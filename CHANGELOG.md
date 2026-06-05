# Changelog

All notable changes to nab will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.11.0] - 2026-05-25

### Added

- Added `nab doctor`, an environment diagnostic that lists every `nab` binary on `PATH` in precedence order with versions, and warns when an older install shadows a newer one (the duplicate Homebrew + `cargo install` case from #105). The warning names the install channel that wins `PATH` (`brew uninstall nab` / `cargo uninstall nab`) so the fix is unambiguous.

## [0.11.0] - 2026-05-25

### Added

- Added explicit browser rendering via `nab browser <url>` plus `nab fetch --render/--interactive`. The new path connects only to a configured external CDP WebSocket endpoint (`NAB_BROWSER_CDP_WS` or `--cdp-url`/`--browser-cdp-url`), keeps default `nab fetch` on the fast HTTP path, and adds thin-content hints that suggest browser rendering without silently falling through to a remote provider.
- Added a first-class Substack site provider for `*.substack.com/p/*` posts, generic `nab fetch --readability`, CLI `--max-output-tokens`, and MCP `fetch.readability`. HTML conversion now prefers readability when raw markdown exceeds a caller token budget, and CLI/MCP budgeted output uses 80% of `max_tokens` for response headroom.
- Added `src/cmd/cookies.rs` www-domain cookie inclusion for bare-domain queries (MIK-3068).
- Added browser engine routing to site-rules (#91).
- Added `nab-core` library crate scaffolding to workspace.
- Added auto-merge CI workflow for green PRs (MIK-4621).

### Security

- Added `nab-yara-engine`, a YARA-X fetch-time signature guard wired into CLI `nab fetch`, batch fetch, media fetch output, site-provider output, and MCP `fetch`. It is default-on, redacts matched sections with a clear `NAB YARA SANITIZED` marker, supports `NAB_YARA_ACTION=refuse`, and supports audited emergency bypass with `NAB_YARA_BYPASS=1`.
- Safe fetch now uses the SSRF-validated socket address for the actual request connection, strips credential-bearing headers across cross-origin redirects, keeps MCP Tor fetches on the configured SOCKS proxy for manually validated redirect hops, and leaves remote `r.jina.ai` thin-content recovery disabled unless `--remote-fallback` is explicitly passed.
- Secure Ingestion now reports WebMCP manifest advertisements as `DirectiveKind::WebMcpManifest` with `Info` severity for both HTML `<link rel="mcp">` and `/.well-known/mcp.json` manifest bodies. Default behavior remains non-blocking, while `NAB_WEBMCP_STRICT=true` refuses unopted WebMCP ingestion unless the source matches `NAB_WEBMCP_OPT_IN`.

### Changed

- `--no-fallback` on `nab fetch` is now a hidden deprecated no-op. Remote fallback via `r.jina.ai` is disabled by default; use the new explicit `--remote-fallback` flag to opt in. Scripts that passed `--no-fallback` are unaffected (remote calls were already off), but the flag will be removed in a future release.

## [0.10.3] - 2026-04-26

### Fixed

- **Windows MSVC release build broke when `impersonate` feature was off** (`src/cmd/linkedin.rs`): the v0.10.2 release CI failed because `use nab::impersonate_client::{...}` was imported at module scope without a `#[cfg(feature = "impersonate")]` gate, but the `impersonate_client` module is itself feature-gated in `lib.rs`. Default Windows MSVC release builds in CI do not enable the `impersonate` feature, so the import was unresolved (E0432). Same pattern for `tokio::io::AsyncWriteExt`, `std::time::{Duration, Instant}`, and `anyhow::Context` — all only used inside impersonate-gated functions but imported at module scope. Fix: gate all impersonate-only imports behind `#[cfg(feature = "impersonate")]`, and gate the now-dead `resolve_output_path` / `fmt_elapsed` helpers + their tests behind the same feature. Verified with `cargo build --no-default-features --features cli` (clean build, only one harmless dead-fields warning) and the full impersonate suite (1293/1293 tests passing).

## [0.10.2] - 2026-04-26

### Fixed

- **LinkedIn activity-feed renderer no longer attributes Comments / reshares / reactions to the user as posts** (`src/site/linkedin/voyager.rs`): the prior recursive `scan_commentary` walker collected text from any node where the enclosing `actor.name.text` matched the resolved profile name. That filter was necessary but not sufficient: LinkedIn's `included[]` array carries `social.Comment` entities (the user's own comments on others' posts, surfaced in `/recent-activity/all/`), `Profile` entities, `SocialDetail` entities, and `feed.Update` entities all interleaved. The walker leaked Comment text — one-line reactions like "Exactly what I need!", "Legend!", "you are welcome!" — into the rendered post list, falsely framing them as user posts. **Fix**: typed-pass parser that filters strictly on `$type == "com.linkedin.voyager.dash.feed.Update"`. Reshares are detected via the `resharedUpdate` field and tagged `[RESHARE of <author>]` rather than carrying the original poster's commentary. Each rendered post now also includes an engagement summary (impressions / reactions / comments / reposts) joined from `SocialActivityCounts` via the activity URN, plus the per-post URL. Output is sorted newest-first by activity URN (snowflake-style monotonic id). 9 unit tests cover the new parser including engagement join, reshare attribution, sort order, Comment-entity exclusion, and per-post URL emission.
- **LinkedIn HTTP 999 bot-detection bypass restored** (`src/impersonate_client.rs`): Chrome 137 emulation (was Chrome 136), and stopped overriding `Accept` / `Accept-Language` headers — `wreq-util`'s Chrome emulation already sets the canonical Chrome values for those, and our shorter overrides were creating a TLS-vs-header fingerprint mismatch that LinkedIn was rejecting with HTTP 999. Added top-level-navigation hints (`upgrade-insecure-requests: 1`, `sec-fetch-user: ?1`, `cache-control: max-age=0`) that real Chrome top-level navigations always send. Authenticated LinkedIn fetches succeed again with valid browser cookies.
- **`--raw-html` bypasses site routers** (`src/cmd/fetch.rs`): the LinkedIn / GitHub / Reddit / etc. site providers always rewrite the response into structured markdown; that conflicts with the explicit user request for wire HTML. `--raw-html` now skips the site router entirely so the generic fetch path emits the unmodified HTML body.
- **LinkedIn activity feed reads end-to-end without a browser** (`src/site/linkedin/voyager.rs`): wired the modern GraphQL endpoint `/voyager/api/graphql?queryId=voyagerFeedDashProfileUpdates.HASH` ahead of the historical REST candidates. Five queryId hashes discovered 2026-04-26 by downloading the activity-page bundle chunks (`https://static.licdn.com/aero-v1/sc/h/…`) and grepping `voyagerFeedDashProfileUpdates\.[a-f0-9]+` from the 23 MB voyager-web chunk. All five return 200 with the typed Voyager response shape today; we try them in order and return the first 2xx with a non-empty body. `nab fetch https://www.linkedin.com/in/{handle}/recent-activity/all/ --cookies brave` now returns the actual posts as markdown — no chromiumoxide, no agent-browser, no DevTools capture required. When all five hashes start failing simultaneously (rotation), re-discover by repeating the chunk-grep procedure documented inline.

### Added

- **`NAB_DUMP_VOYAGER` env var** (`src/site/linkedin/voyager.rs`): when set to a path, dumps the raw Voyager response body to that file before parsing. Used during the typed-pass fix to capture ground-truth envelope shape; kept in tree as a triage helper for future hash rotations or shape changes.
- **`src/site/linkedin/voyager.rs`** — Voyager API client scaffolding for `LinkedIn` activity feeds. Wires `extract_csrf_token` (from `JSESSIONID` cookie) and `parse_voyager_activity` (already in helpers) plus `VoyagerActivityResponse` (already in types) into an actual XHR path. Step 1 (resolve `/in/{handle}` → profile URN via `voyager/api/identity/dash/profiles`) is verified working. Step 2 (fetch share feed) currently calls the historical REST endpoints; in 2026 LinkedIn answered them with 400/404 because activity feeds moved to a GraphQL endpoint whose `queryId` hash rotates per release. The module ships auth + URN resolution pieces; `queryId` discovery from lazy JS chunks is the next step (tracked in MIK-3059).

- **`nab linkedin export` subcommand** (Track F per `docs/anti-bot-strategy.md`, MIK-3069): automates the official LinkedIn data archive request (Settings → Data Privacy → "Get a copy of your data"). Resolves cookies for `www.linkedin.com`, derives csrf-token from `JSESSIONID`, POSTs the request to `/mysettings-api/settingsApiDataExport/`, polls the settings page on exponential backoff (60s → 600s capped), and downloads the signed ZIP. Two archive kinds: `--kind fast` (~10 min, contacts + history) and `--kind full` (~24 h, posts + articles + messages). Override flags `--form-url`, `--request-url`, `--body-override` give the user an escape hatch when LinkedIn rotates its internal paths or field names. `--poll-only` resumes polling for an export fired from the web UI. Endpoint discovered 2026-04-25 by grepping the SPA bundle; the current default body is rejected with HTTP 400 — capture the live JSON via Chrome DevTools and pass via `--body-override` until the next release pins the new field name. New module: `src/site/linkedin/export.rs` (12 unit tests, all green). New helper: `nab::impersonate_client::request_impersonated` adds POST support to the existing GET-only `fetch_impersonated`.

## [0.10.1] - 2026-04-25

### Changed

- Documentation in `src/security/ingestion_guard.rs` and `src/security/mod.rs` now describes the threat in publisher-neutral terms rather than naming a specific website. The technique exists in the open as a semantic-web research practice; the same shape exists, or will exist, in less benign hands. The defensive layer is the same regardless of intent.
- CHANGELOG entry for v0.10.0 below rephrased to remove a specific URL from the released-on disk; the binary capability is unchanged.

## [0.10.0] - 2026-04-25

### Added

- **Secure Ingestion guard** (`nab::security::ingestion_guard`, MIK-3035): public Rust API for detecting and stripping machine-targeted markup before HTML reaches an LLM agent. Five detector kinds:
  - AI-addressed HTML comments (e.g. `<!-- Machine Intelligence Notice: ... -->`)
  - Machine-only attribute payloads (`data-dim`, `data-ai`, `data-mcp`, `data-agent`, `data-machine`)
  - Machine-class elements (`<span class="m" ...>`)
  - `display:none` text containers (severity `Block`)
  - `aria-hidden="true"` text containers (severity `Block`)
- `detect(html)` returns a `DetectionReport` with per-kind counts and excerpt samples; `sanitize(html)` returns `(cleaned, report)` with conservative strip rules (visible text preserved, machine-only attributes stripped, hidden text removed).
- Unit tests include a golden-corpus regression seeded from a verbatim `<!-- Machine Intelligence Notice ... -->` block observed in the wild on a public research website.
- Wired Secure Ingestion into the default HTML-to-markdown conversion path so hidden machine-targeted directives are stripped before readability and html2md extraction.
- `examples/scan_html.rs` — scan any HTML file: `cargo run --example scan_html -- page.html`. Pass `--sanitize` to emit cleaned HTML to stdout.
- Live verification at release time against three public pages that openly publish the technique: 8 / 45 / 48 detections respectively (about-page / two semantic-web blog posts).

This module is licensed under PolyForm Noncommercial 1.0.0 per the v0.9.0 dual-licensing decision (Path C).

## [0.9.0] - 2026-04-25

### Changed

- **Dual licensing introduced** (Path C, MIK-3035 / MIK-3036): designated Enterprise Edition modules — `src/auth/`, `src/fingerprint/`, `src/waf/`, `src/site/`, `src/security/` — are now licensed under PolyForm Noncommercial 1.0.0; everything else remains MIT. See [LICENSE.md](LICENSE.md), [LICENSE-EE.md](LICENSE-EE.md), and the License section of the README for details.
- Every EE-designated source file now carries an `// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0` header.
- Releases prior to v0.9.0 remain entirely MIT and stay MIT forever; the new license terms apply only to commits in v0.9.0 and later that touch EE-designated paths.

### Fixed

- MCP tool schemas no longer emit invalid `"nullable": true` keyword, which had blocked all Claude Code sub-agents from loading nab's tool list. Five `Option<String>` fields (`analyze.language`, `analyze.backend`, `watch_create.selector`, `watch_create.interval`, `watch_create.diff_kind`) converted to `String` with `#[serde(default)]` and empty-string sentinel. API semantics unchanged — omitted field still means "use default". Closes [#61](https://github.com/MikkoParkkola/nab/issues/61).

## [0.8.1] - 2026-04-16

### Changed
- `nab upgrade` command for seamless binary updates
- README rewritten with agent-first install as the recommended setup path
- MCP tool count corrected from 11 to 12

## [0.7.0] - 2026-04-07

### Added

#### Multilingual ASR pipeline (`nab analyze` v2)
- **FluidAudio subprocess backend** — Parakeet TDT v3 on Apple Neural Engine, 131x realtime on a 2 h 09 m English clip, 97.18 % mean confidence ([`6fa7164`](https://github.com/MikkoParkkola/nab/commit/6fa7164))
- **`AsrBackend` trait** — pluggable transcription backends with consistent `TranscriptionResult` shape (segments, language, duration, model, backend, rtfx)
- **Word-level timestamps** with per-word confidence scores
- **Speaker diarization** via FluidAudio + PyAnnote community-1
- **Multilingual** — 25 EU languages first-class, optional Qwen3-ASR (CoreML beta) for Chinese, Japanese, Korean, Vietnamese
- **`analyze` MCP tool** — full structured output schema, task-augmented async execution
- **Speaker embedding export** — `--include-embeddings` surfaces 256-dim WeSpeaker vectors per speaker turn ([`aa2a250`](https://github.com/MikkoParkkola/nab/commit/aa2a250))
- **`match-speakers-with-hebb` prompt** — guides MCP clients to match nab speaker embeddings against hebb's voiceprint database
- **Active reading via MCP sampling** — `--active-reading` flag asks the host LLM to identify references in the transcript and inlines lookups as footnotes ([`ebc68b2`](https://github.com/MikkoParkkola/nab/commit/ebc68b2))

#### URL watch (`nab watch`)
- **Watch subsystem** — `nab watch add/list/remove/logs` with per-watch interval, CSS selector, diff kind, notify mode ([`52f950d`](https://github.com/MikkoParkkola/nab/commit/52f950d))
- **MCP subscribable resources** — every watch is exposed as a `nab://watch/<id>` resource. MCP clients call `resources/subscribe` and receive `notifications/resources/updated` when content changes
- **Conditional GETs** — `If-None-Match` and `If-Modified-Since` make 304 responses effectively free
- **Semantic diff** — three diff kinds: text, semantic, DOM
- **Adaptive backoff** on 429 / 503; auto-mute after 5 consecutive failures
- **`watch_create`, `watch_list`, `watch_remove`** MCP tools

#### Models management (`nab models`)
- **`nab models fetch fluidaudio`** — persistent install of FluidAudio binary + Parakeet TDT v3 weights ([`8818317`](https://github.com/MikkoParkkola/nab/commit/8818317))
- **`nab models list/update/verify`** — version tracking and integrity checks
- Phase 3 will add `whisper` and `sherpa-onnx` subcommands

#### Apple Vision OCR
- **`nab::content::ocr`** — Apple Vision framework OCR engine via `objc2-vision` ([`63878b4`](https://github.com/MikkoParkkola/nab/commit/63878b4))
- 15 languages, Apple Neural Engine accelerated, ~10-50 ms per image
- macOS only — Linux and Windows fall back to Tesseract (Phase 3)

#### MCP 2025-11-25 spec closure
- **Streamable HTTP transport** — `nab-mcp --http <bind>` with origin checks, `MCP-Protocol-Version` header validation, session IDs, SSE resumability via `Last-Event-ID`, DELETE for session termination ([`4c0100a`](https://github.com/MikkoParkkola/nab/commit/4c0100a))
- **Sampling helper** — nab calls back to the host LLM via `sampling/createMessage` for active reading, focus extraction, form auto-fill
- **Roots helper** — `roots/list` queried for workspace-scoped saves; `nab fetch file://path` validated against advertised roots
- **Structured logging** — `notifications/message` with RFC 5424 levels, replacing stderr-only `tracing`
- **Argument completion** — `completion/complete` for tool arguments
- **Elicitation form mode + URL mode** — interactive credential input; OAuth/SSO redirects for Google, GitHub, Microsoft, Apple, Facebook, and 8 more
- 12 tools, 4 prompts, 2+N resources

### Changed
- `nab analyze` migrated from monolithic transcribe path to `AsrBackend` trait architecture
- MCP server now exposes 12 tools (was 8): added `analyze`, `watch_create`, `watch_list`, `watch_remove`
- MCP `resources` capability now declares `subscribe: true`
- All tools advertise structured output schemas, annotations, and validation errors

### Deprecated
- `ParakeetTranscriber`, `Transcriber`, `VllmTranscriber` — superseded by `AsrBackend` trait. Will be removed in 0.8.0.

### Removed
- Dead code path referencing nonexistent `parakeet.cpp` binary in old `analyze/transcribe.rs`

## [Deferred] — WASM Marketplace + misc (not yet released)

### Added
- **WASM provider marketplace** — sandboxed third-party site extractors via wasmtime runtime (#19)
  - Zero-trust sandbox: no filesystem, no network, bounded CPU (fuel metering), bounded memory
  - Guest ABI: `alloc` + `extract` exports over linear memory, JSON-encoded response
  - Manifest format: `manifest.toml` with name, version, description, URL regex patterns
  - Provider directory: `~/.config/nab/wasm_providers/<name>/` with hot-reload
  - 1,047 lines of implementation (`wasm_provider.rs` + `wasm_manifest.rs`)
- **CLI commands**: `nab provider list`, `nab provider install`, `nab provider remove`, `nab provider test`
- **Feature-gated**: `--features wasm-providers` (wasmtime 42, cranelift JIT backend)
- **Provider SDK documentation** with example Rust guest targeting `wasm32-unknown-unknown`
- **WebMCP discovery mode** — `/.well-known/mcp.json` and HTML `<link>` tag detection (#17)
- **Qwen3-ASR transcription backend** via vLLM — 4x faster, 30 language support (#11)
- **Parakeet.cpp transcription backend** — default backend with `--gpu --fp16` enabled by default

### Fixed
- Content extraction from JS-rendered Next.js SPA pages with jina fallback
- Full content extraction from X Articles and Ghost CMS blogs
- Windows release build: `which` crate moved to general deps
- CI: gate `VoyagerProfileResponse` re-export behind feature flag
- CI: disable `impersonate` feature for cross/Windows release builds
- CLI auth tests rewritten to tolerate 1Password CLI blocking

## [0.6.6] - 2026-03-16

### Added
- **Extraction quality scoring**: `QualityScore` with 4 weighted signals — content density (0.35), structure (0.25), completeness (0.25), encoding quality (0.15)
- CLI `fetch --json`: include top-level `confidence` field + detailed `quality` breakdown
- Content pipeline: `ImageHandler` for image URLs, `PdfLightHandler` (no-pdfium fallback)
- Release CI: Windows x86_64 build target (`x86_64-pc-windows-msvc`)

### Fixed
- `starts_with_ordered_list` now handles multi-digit numbers (e.g., `42. `, `100. `)
- SPA integration test: check stderr (not stdout) for status messages (broken since v0.6.0)

### Changed
- Clippy: remove unnecessary raw string literal hashes
- docs/ARCHITECTURE.md: comprehensive rewrite — MCP server, content pipeline, plugins, all site modules
- CONTRIBUTING.md: expand module list from 8 to 25+ entries

## [0.6.5] - 2026-03-16

### Changed
- MCP server: extract truncation constants (`TOOL_TRUNCATION_LIMIT`, `BATCH_PREVIEW_LIMIT`)
- MCP server: consolidate schema property helpers via `schema_prop()`
- README: remove Windows binary from install table (not built in CI)
- CONTRIBUTING: add all 5 feature flags (was missing `impersonate`, `pdf`, `browser`)

## [0.6.4] - 2026-03-16

### Fixed
- Dockerfile: use Rust 1.93 (was 1.87), add build-essential/cmake/clang for crypto deps
- Dockerfile: use `--no-default-features` to skip BoringSSL/QUIC for faster Docker builds

### Changed
- MCP tools `submit`, `login`, `validate` now return `structured_content` alongside text (consistency with `fetch`, `fetch_batch`, `auth_lookup`, `fingerprint`, `benchmark`)
- MCP `benchmark` tool reports error count per URL instead of silently dropping failed iterations

## [0.6.3] - 2026-03-15

### Added
- HTML extraction: `strip_hidden_sections()` removes `<details>`, `<noscript>`, `<dialog>` elements
- HTML extraction: `strip_noise_sections()` removes vulnerability advisories, cookie banners, newsletter signups
- HTML extraction: `clean_markdown_noise()` strips base64 data URIs and empty link artefacts
- Readability/direct fallback: picks whichever produces richer output
- Glama MCP server directory listing (`glama.json`, Dockerfile, badge)
- 10 new HTML extraction tests

## [0.6.2] - 2026-03-14

### Changed
- All dependencies updated to latest versions

## [0.6.1] - 2026-03-14

### Fixed
- CI formatting fix for Rust 2024 edition

## [0.6.0] - 2026-03-14

### Added
- `NabError` typed error hierarchy with 9 semantic variants (`InvalidUrl`, `SsrfBlocked`, `ProviderError`, `ConversionError`, `AuthError`, `LoginError`, `SessionError`, `NetworkError`, `BudgetExceeded`) — replaces bare `anyhow::Error` at public API boundaries

### Changed
- **Codebase restructuring** — 5 monolithic files decomposed into focused submodules:
  - `site/linkedin.rs` (1915 LOC) → 7 files (mod + auth + helpers + types + url + oembed + tests)
  - `site/rules/provider.rs` (2172 LOC) → provider (738) + helpers (289) + tests (1181)
  - `site/rules/config.rs` (1460 LOC) → config (561) + tests (898)
  - `auth/cookies.rs` (1074 LOC) → cookies/ directory (mod + crypto + db + tests)
  - `bin/mcp_server/tools.rs` (1033 LOC) → tools/ directory (10 files, max 346 LOC)
- Public API surface reduced — 13 internal re-exports removed, internal modules marked `#[doc(hidden)]`
- SSRF validation functions now return `NabError` variants instead of opaque `anyhow::Error`
- Config structs replace positional parameters across all `cmd/` functions (10 structs)
- 6 shared helpers consolidated in `cmd/mod.rs` (cookie resolution, domain extraction, referer building)

### Fixed
- **Silent test bug**: `concurrent_fetch_custom_item_limit` used wrong TOML field (`item_limit` vs `max_items`) — serde silently ignored unknown field
- **UTF-8 truncation panics** eliminated across cmd/ layer (now uses `floor_char_boundary`)
- ~10 `unwrap()`/`expect()` calls in library code replaced with proper error propagation
- 2 stale `clippy::too_many_lines` suppressions removed (functions were already under threshold)
- Production `expect()` in `cmd_fetch_batch` replaced with `?` error
- SPA command status messages moved from `stdout` to `stderr` (data-only on stdout)

## [0.5.0] - 2026-03-13

### Added
- **Query-focused extraction** — BM25-lite scoring extracts only sections relevant to a `focus` query; top-20% filter with diff-marker exemption
- **Token budget enforcement** — structure-aware truncation via `max_tokens` that never splits mid-block (headings, code blocks, tables); priority-based P0-P4 scoring
- **Prefetch link graph** — same-site link extraction from fetched markdown with eTLD+1 filtering (Mozilla PSL via `addr` crate) and relevance scoring
- **Persistent named sessions** — `SessionStore` with LRU eviction (32 slots), cookie seeding from browser jars, pinned browser profiles; `session` parameter on fetch/submit/login tools
- **CSS extractor plugins** — define custom site extractors in `plugins.toml` using CSS selectors (`type = "css"`), no Rust code required; content goes through full `ContentRouter` pipeline
- **MCP protocol 2025-11-25** — upgraded from 2025-06-18 via rust-mcp-sdk 0.9
- **URL elicitation** for OAuth/SSO login flows (Google, GitHub, Microsoft, Apple, Facebook, and 8 more providers)
- **Task-augmented execution** for `fetch_batch` — returns immediately with task ID, fetches in background with push notifications
- **Multi-select cookie elicitation** — pick multiple browser cookie stores (Brave, Chrome, Firefox, Safari) at once
- **Structured content** (`structured_content`) on 5 tools alongside human-readable text
- **Output schemas** (`outputSchema`) on fetch, fetch_batch, auth_lookup, fingerprint, benchmark
- **Tool annotations** on all 8 tools (read_only_hint, destructive_hint, open_world_hint)
- **Server icons** — globe SVG in light and dark themes
- Google Workspace site provider: extract Google Docs (markdown), Sheets (markdown table), and Slides (plain text) using browser cookie authentication
- Comments and suggested edits extraction from OOXML (docx/xlsx/pptx) for Google Workspace documents

### Changed
- MCP server module split into 6 files (tools, elicitation, helpers, structured, tests) for maintainability

### Fixed
- `stream --duration` flag now works for file output (was only working for player piping)
- `analyze` command now properly detects audio-only files and skips video frame extraction

### Changed
- Native HLS backend respects duration limit via segment counting
- FFmpeg backend passes duration via `-t` flag
