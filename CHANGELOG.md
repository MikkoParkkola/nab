# Changelog

All notable changes to nab will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
