# nab Architecture

This document describes the internal architecture of nab, a token-optimized web microfetch tool with HTTP/3, TLS impersonation, JavaScript execution, cookie authentication, anti-fingerprinting, and an MCP server for LLM tool integration.

## Design Philosophy

**Token-Optimized for LLM Consumption**: Every design decision optimizes for minimal token usage while maximizing information density:
- Markdown output by default (25x token savings vs HTML)
- Structured JSON for parsing use cases
- Compact formats for status reporting

**Zero Friction Authentication**: Automatically detect and use browser cookies, 1Password credentials, and OTP codes without manual configuration.

**HTTP Performance**: Leverage HTTP/2 multiplexing, HTTP/3 (QUIC) with 0-RTT resumption, TLS 1.3, and modern compression (Brotli, Zstd).

**Anti-Fingerprinting**: Generate realistic browser fingerprints and TLS profiles to avoid bot detection.

## High-Level Architecture

```
                    ┌─────────────────────────────────────┐
                    │       MCP Server (nab-mcp)          │
                    │  12 tools: fetch, fetch_batch,      │
                    │  submit, login, auth_lookup,        │
                    │  fingerprint, validate, benchmark,  │
                    │  analyze, watch_*                   │
                    │  stdio transport, outputSchema,     │
                    │  task-augmented execution,           │
                    │  elicitation, server icons           │
                    └────────────────┬────────────────────┘
                                     │
┌────────────────────────────────────┼────────────────────────────────────┐
│                         CLI (main.rs)                                   │
│  Commands: fetch, fetch_batch, submit, login, auth, cookies, otp,      │
│  spa, stream, analyze, annotate, fingerprint, bench, validate,         │
│  export-rules, context                                                  │
└────────────────────────────┬───────────────────────────────────────────┘
                             │
        ┌────────────────────┼────────────────────────────────┐
        │                    │                                │
┌───────▼──────────┐  ┌─────▼───────────┐  ┌────────────────▼──────────┐
│  HTTP Clients    │  │ Content Pipeline │  │ Site Extraction           │
│                  │  │                  │  │                           │
│ AcceleratedClient│  │ ContentRouter    │  │ SiteRouter                │
│  (HTTP/2, pool)  │  │  HtmlHandler     │  │  Rule providers (TOML)   │
│ Http3Client      │  │  PlainHandler    │  │  Hardcoded providers      │
│  (QUIC, 0-RTT)  │  │  PdfHandler      │  │  CSS extractor plugins    │
│ ImpersonateClient│  │  readability     │  │                           │
│  (BoringSSL TLS) │  │  quality scoring │  │ linkedin/ google/ github  │
│                  │  │  budget/focus    │  │ hackernews reddit         │
└───────┬──────────┘  │  diff tracking   │  │ twitter youtube wikipedia │
        │             │  snapshot store   │  │ mastodon stackoverflow   │
        │             │  spa_extract      │  │ instagram                │
        │             └──────────────────┘  └──────────────────────────┘
        │
┌───────▼──────────────────────────────────────────────────────────────┐
│                        Core Infrastructure                            │
│                                                                       │
│  Auth Stack          Fingerprinting       Sessions                    │
│  - 1Password         - Chrome/Firefox/    - LRU store (32 slots)     │
│  - Browser cookies     Safari profiles    - Cookie seeding            │
│  - OTP retrieval     - Auto-update        - Pinned profiles           │
│  - Login engine      - TLS fingerprints                               │
│                                           Security                    │
│  JS Engine           Plugin System        - SSRF protection           │
│  - QuickJS (ES2020)  - CSS selectors      - Rate limiting             │
│  - DOM injection     - Binary plugins     - Form CSRF handling        │
│  - Fetch polyfill    - plugins.toml cfg                               │
└──────────────────────────────────────────────────────────────────────┘
        │
┌───────┼──────────────────────────────────────────────────┐
│       │                    │                              │
│ ┌─────▼───────────┐ ┌─────▼──────────────┐ ┌───────────▼───────────┐
│ │ Streaming       │ │ Video Analysis     │ │ Video Annotation      │
│ │                 │ │                    │ │                       │
│ │ HLS/DASH        │ │ Transcription      │ │ Subtitle generation   │
│ │ Native parser   │ │ Speaker diarization│ │ Speaker label overlays│
│ │ ffmpeg backend  │ │ Vision (Claude)    │ │ ffmpeg composition    │
│ │ NRK/SVT/DR/Yle │ │ Emotion detection  │ │                       │
│ └─────────────────┘ └────────────────────┘ └───────────────────────┘
└──────────────────────────────────────────────────────────────────────┘
```

## Core Modules

### 1. MCP Server (`bin/mcp_server/`)

**Purpose**: Stdio-based MCP server exposing nab's capabilities as LLM tools.

**Key Features**:
- 12 tools: `fetch`, `fetch_batch`, `submit`, `login`, `auth_lookup`, `fingerprint`, `validate`, `benchmark`, `analyze`, `watch_create`, `watch_list`, `watch_remove`
- MCP protocol 2025-11-25 with `outputSchema` on every tool
- Task-augmented execution for `fetch_batch` (non-blocking parallel fetches)
- Elicitation support for interactive credential selection during `login`
- Server icons and structured content metadata

**Architecture**:
```
bin/mcp_server/
├── main.rs              # Server setup, handler, output schema builders
├── helpers.rs           # Shared conversion helpers
├── elicitation.rs       # Interactive credential/MFA prompts
├── structured.rs        # Server icons, structured content metadata
├── tests.rs             # Integration tests
└── tools/
    ├── mod.rs           # Tool exports, shared client singleton
    ├── client.rs        # Shared AcceleratedClient with lazy init
    ├── fetch.rs         # Single URL fetch with content conversion
    ├── fetch_batch.rs   # Parallel multi-URL fetch
    ├── submit.rs        # Form submission with CSRF handling
    ├── login.rs         # Auto-login with 1Password + elicitation
    ├── auth.rs          # Credential/TOTP lookup
    ├── fingerprint.rs   # Browser profile generation
    ├── benchmark.rs     # URL performance benchmarking
    └── validate.rs      # Live website validation suite
```

**Binary**: `nab-mcp` (separate binary target in `Cargo.toml`)

### 2. HTTP Clients (`http_client.rs`, `http3_client.rs`, `impersonate_client.rs`)

**Purpose**: High-performance HTTP/1.1, HTTP/2, HTTP/3, and TLS-impersonated fetching.

**Key Features**:
- HTTP/2 multiplexing (100 concurrent streams per connection)
- HTTP/3 (QUIC) with 0-RTT connection resumption
- TLS 1.3 with session caching
- Brotli, Zstd, Gzip compression auto-negotiation
- DNS caching + Happy Eyeballs (IPv4/IPv6 racing)
- Connection pooling with 90s idle timeout
- TLS fingerprint impersonation via BoringSSL (`rquest`) for Chrome/Safari/Firefox profiles

**Impersonation** (`impersonate_client.rs`, feature-gated `impersonate`):
Sites like LinkedIn check TLS fingerprints at the CDN edge and reject non-browser TLS stacks with HTTP 999. The impersonation client uses `rquest` (reqwest fork with BoringSSL) to produce Chrome 136 TLS fingerprints that pass JA3/JA4 checks. Domain detection is automatic via `needs_impersonation()`.

**Data Flow**:
```
URL → AcceleratedClient::fetch_text()
    → SSRF validation
    → Check impersonation requirement
    → Apply fingerprint headers (or let rquest set them for impersonated domains)
    → Connection pool lookup
    → HTTP/2 or HTTP/3 request
    → Decompress response
    → Return HTML/JSON
```

**Used By**: All fetch operations, SPA extraction, streaming URL resolution, MCP tools

### 3. Content Processing Pipeline (`content/`)

**Purpose**: Content-type-aware conversion of HTTP responses to markdown for LLM consumption.

**Architecture**:
```
content/
├── mod.rs              # ContentRouter: dispatches by Content-Type
├── html.rs             # HTML → Markdown (via html2md + readability)
├── plain.rs            # Passthrough for text/plain, JSON, markdown, etc.
├── readability.rs      # Mozilla-style article extraction
├── quality.rs          # Extraction quality scoring
├── pdf.rs              # PDF → Markdown (via pdfium, feature-gated)
├── budget.rs           # Token budget: structure-aware P0-P4 truncation
├── focus.rs            # Query-focused extraction: BM25-lite scoring
├── link_extract.rs     # Same-site link graph with eTLD+1 filtering
├── diff.rs             # Content diff tracking between fetches
├── diff_format.rs      # Diff output formatting
├── snapshot_store.rs   # Content snapshot persistence for diff mode
├── spa_extract.rs      # SPA data extraction (__NEXT_DATA__, __NUXT__)
├── structured.rs       # Structured content metadata
├── table.rs            # Table extraction from PDF (feature-gated)
└── types.rs            # Shared types for PDF pipeline (feature-gated)
```

**Key Features**:
- `ContentRouter` dispatches to `HtmlHandler`, `PlainHandler`, or `PdfHandler` based on MIME type
- URL-aware readability heuristics improve extraction on complex sites
- Token budget enforces `max_tokens` with priority-based P0-P4 scoring (never splits mid-block)
- Query-focused extraction via `focus` parameter: BM25-lite top-20% filter with diff-marker exemption
- Link extraction uses Mozilla's public suffix list (`addr` crate) for eTLD+1 domain filtering
- Diff mode tracks content changes between fetches via snapshot store
- Falls back to HTML handler for bytes that look like HTML despite incorrect `Content-Type`

**Data Flow**:
```
Response bytes + Content-Type
    → ContentRouter::convert_with_url()
    → MIME dispatch to handler
    → Handler produces markdown
    → Optional: focus query filtering
    → Optional: token budget truncation
    → Optional: diff against previous snapshot
    → ConversionResult { markdown, page_count, quality }
```

**Used By**: All fetch operations, MCP fetch/fetch_batch tools

### 4. Site-Specific Extraction (`site/`)

**Purpose**: Specialized extractors for platforms where API access or custom parsing yields better content than generic HTML-to-markdown conversion.

**Architecture**:
```
site/
├── mod.rs              # SiteRouter: provider dispatch (first match wins)
├── css_extractor.rs    # CSS selector-based extraction engine
├── github.rs           # GitHub: repos, issues, PRs, code
├── hackernews.rs       # Hacker News: front page, stories, comments
├── reddit.rs           # Reddit: posts, comments (old.reddit.com API)
├── linkedin/           # LinkedIn (7 files, requires TLS impersonation)
│   ├── mod.rs          # Provider entry point
│   ├── auth.rs         # Cookie-based authentication
│   ├── helpers.rs      # Profile/post parsing helpers
│   ├── types.rs        # LinkedIn-specific data types
│   ├── url.rs          # URL pattern matching
│   ├── oembed.rs       # oEmbed API fallback
│   └── tests.rs        # Unit tests
├── google/             # Google Workspace document extraction
│   ├── mod.rs          # Provider: Docs, Sheets, Slides via OOXML export
│   └── ooxml/          # OOXML parsing (docx/xlsx/pptx via zip + roxmltree)
└── rules/              # Config-driven rule engine
    ├── mod.rs           # Rule loading: user overrides + embedded defaults
    ├── config.rs        # TOML rule schema (SiteRuleConfig)
    ├── config_tests.rs  # Config parsing tests
    ├── helpers.rs       # Template and extraction helpers
    ├── provider.rs      # ApiRuleProvider: generic rule-based SiteProvider
    ├── provider_tests.rs# Provider integration tests
    ├── template.rs      # Mustache-style template engine for output formatting
    ├── json_path.rs     # Minimal JSON path extraction
    └── defaults/        # 9 embedded rule configs
        ├── twitter.toml
        ├── youtube.toml
        ├── wikipedia.toml
        ├── mastodon.toml
        ├── reddit.toml
        ├── stackoverflow.toml
        ├── instagram.toml
        ├── github-issues.toml
        └── hackernews.toml
```

**Provider Loading Order** (first match wins):
1. Rule-based providers from `~/.config/nab/sites/*.toml` (user overrides)
2. Rule-based providers from embedded defaults (9 rules compiled into binary)
3. Hardcoded Rust providers for platforms not covered by a rule (hackernews, github, google-workspace, linkedin)
4. CSS extractor plugins from `~/.config/nab/plugins.toml`

**Used By**: Fetch pipeline (before generic HTML conversion), MCP fetch tool

### 5. Authentication (`auth/`, `browser_detect.rs`)

**Purpose**: Zero-config authentication via browser cookies, 1Password, and OTP retrieval.

**Key Components**:
- **Cookie Extraction** (`auth/cookies/`): Auto-detect default browser (Brave, Chrome, Firefox, Safari, Edge, Dia) and extract cookies from SQLite/binary storage. Submodules: `mod.rs` (lookup), `crypto.rs` (AES-128-CBC decryption via PBKDF2-SHA1 + macOS Keychain), `db.rs` (SQLite helpers), `tests.rs`
- **1Password Integration**: Retrieve credentials, TOTP codes, and passkeys via `op` CLI
- **OTP Retrieval**: SMS (Beeper MCP), Email (Gmail API), TOTP (1Password)

**Data Flow**:
```
URL → detect_default_browser()
    → Extract cookies from browser DB
    → Inject into HTTP client cookie jar
    → Requests auto-authenticated
```

**Used By**: All fetch operations with `--cookies` flag, MCP `auth_lookup` tool, session cookie seeding

### 6. Login Engine (`login.rs`)

**Purpose**: Automated form-based login with credential retrieval and MFA handling.

**Key Features**:
- Fetches login page and detects form fields via `form.rs`
- Retrieves credentials from 1Password (`auth/`)
- Handles multi-factor authentication challenges (`mfa.rs`): TOTP, SMS, Email, Push
- Optional browser-based login via Chrome DevTools Protocol (feature-gated `browser`)
- Session persistence in `~/.nab/sessions/`

**Data Flow**:
```
Login URL → Fetch page → Detect form
         → Retrieve credentials (1Password)
         → Submit form with CSRF token
         → Handle MFA challenge if present
         → Store session cookies
         → Return authenticated page content
```

**Used By**: `login` command, MCP `login` tool (with elicitation for interactive credential selection)

### 7. Browser Fingerprinting (`fingerprint/`)

**Purpose**: Generate realistic browser fingerprints to avoid bot detection.

**Architecture**:
```
fingerprint/
├── mod.rs           # Profile generation: chrome, firefox, safari, random
├── autoupdate.rs    # Fetch latest browser versions weekly
└── tests.rs         # Fingerprint validation tests
```

**Key Features**:
- Chrome, Firefox, Safari profile generation
- Auto-update from real browser version APIs (stored in `~/.nab/fingerprint_versions.json`)
- Realistic TLS client hello fingerprints
- Consistent User-Agent, sec-ch-ua, Accept headers

**Used By**: All HTTP requests, session profile pinning, MCP `fingerprint` tool

### 8. Plugin System (`plugin/`)

**Purpose**: User-defined extraction plugins without recompiling.

**Architecture**:
```
plugin/
├── mod.rs           # Public API: LoadedPlugins, PluginConfig, CssPluginConfig
├── config.rs        # TOML config parser for ~/.config/nab/plugins.toml
└── runner.rs        # Binary plugin subprocess runner
```

**Two plugin types**:
1. **Binary plugins**: External binaries that receive a URL on stdin (JSON) and return markdown + metadata on stdout
2. **CSS extractor plugins** (`type = "css"`): In-process extractors defined entirely in `plugins.toml` using CSS selectors, with optional `remove` selectors and metadata extraction

**Configuration** (`~/.config/nab/plugins.toml`):
```toml
# CSS extractor (no external binary)
[[plugins]]
name     = "internal-wiki"
type     = "css"
patterns = ["wiki\\.internal\\.corp/.*"]

[plugins.content]
selector = "div.wiki-content"
remove   = ["nav", ".ads"]

[plugins.metadata]
title = "h1.page-title"
```

**Used By**: `SiteRouter` (appended after built-in providers)

### 9. Sessions (`session.rs`)

**Purpose**: Persistent named sessions with isolated cookie jars and pinned browser profiles.

**Key Features**:
- LRU eviction at 32 slots (`MAX_SESSIONS`)
- Cookie seeding from browser jars at session creation (synthesises `Set-Cookie` headers scoped to domain/path)
- Pinned `BrowserProfile` per session for fingerprint consistency
- Thread-safe with `tokio::sync::RwLock`

**Used By**: MCP server (sessions persist across tool calls), MCP `fetch` operations with the `session` parameter

### 10. SSRF Protection (`ssrf.rs`)

**Purpose**: Block requests to private/reserved IP ranges, preventing Server-Side Request Forgery.

**Key Features**:
- Comprehensive deny lists covering 16 IPv4 and 14 IPv6 RFC special-use ranges
- IPv4-mapped/embedded IPv6 detection (catches `::ffff:127.0.0.1` bypass attempts)
- DNS pinning via `resolve_and_validate()` to prevent DNS rebinding attacks
- Redirect target validation before following each hop
- Returns `NabError::SsrfBlocked` with descriptive reason

**Used By**: All HTTP client fetch operations (validated before connection)

### 11. Streaming (`stream/`)

**Purpose**: HLS/DASH streaming with provider-specific extractors and multiple playback backends.

**Architecture**:
```
stream/
├── mod.rs              # Public API
├── backend.rs          # Backend trait
├── provider.rs         # Provider trait
├── backends/
│   ├── native_hls.rs   # Pure Rust HLS parser
│   ├── ffmpeg.rs       # ffmpeg subprocess backend
│   └── streamlink.rs   # Streamlink wrapper (deprecated)
└── providers/
    ├── yle.rs          # Yle Areena (Finnish)
    ├── nrk.rs          # NRK (Norwegian)
    ├── svt.rs          # SVT Play (Swedish)
    ├── dr.rs           # DR TV (Danish)
    └── generic.rs      # Generic HLS/DASH
```

**Data Flow**:
```
URL → Provider::extract_stream_info()
    → Resolve master playlist
    → Select quality variant
    → Backend::stream_to_output()
    → Output to file/pipe/player
```

**Used By**: `stream` command

### 12. Video Analysis (`analyze/`)

**Purpose**: Multimodal video analysis with transcription, speaker diarization, and vision understanding.

**Architecture**:
```
analyze/
├── mod.rs           # Pipeline orchestration
├── transcribe.rs    # Audio to text (Whisper/Parakeet)
├── diarize.rs       # Speaker segmentation
├── vision.rs        # Visual understanding (Claude API)
├── extract.rs       # Scene/frame extraction
├── fusion.rs        # Merge transcription + vision
└── report.rs        # Generate reports (JSON/Markdown/SRT)
```

**Used By**: `analyze` command

### 13. Video Annotation (`annotate/`)

**Purpose**: Generate subtitles and visual overlays for videos.

**Architecture**:
```
annotate/
├── mod.rs           # Public API
├── subtitle.rs      # SRT/ASS generation
├── overlay.rs       # Visual overlay positioning
├── compositor.rs    # ffmpeg composition
└── pipeline.rs      # End-to-end pipeline
```

**Used By**: `annotate` command

### 14. Error Handling (`error.rs`)

**Purpose**: Typed error hierarchy for stable public API.

**`NabError`** enum with 10 semantic variants: `InvalidUrl`, `SsrfBlocked`, `ProviderError`, `ConversionError`, `AuthError`, `LoginError`, `SessionError`, `NetworkError`, `BudgetExceeded`, `Other`. Public functions return `Result<T, NabError>` at library boundaries; internal code uses `anyhow`.

### 15. Rate Limiting (`rate_limit.rs`)

**Purpose**: Per-domain rate limiting for concurrent HTTP fetching.

Enforces a configurable minimum delay between consecutive requests to the same domain. Different domains are independent. Thread-safe via `tokio::sync::Mutex`.

**Used By**: `fetch_batch` (CLI and MCP), any multi-URL operation

### 16. Prefetch (`prefetch.rs`)

**Purpose**: Connection warming and Early Hints (HTTP 103) support.

- Preconnect: DNS + TCP + TLS handshake ahead of time
- Early Hints (103): Extract `Link` preload hints from informational responses
- Same-site link prefetching from HTML content
- Tracks warmed hosts to avoid duplicate work

### 17. Supporting Modules

**`api_discovery.rs`**: Discover API endpoints in SPA JavaScript code via pattern matching.

**`arena.rs`**: Bump allocator (`bumpalo`) for efficient HTTP response buffering.

**`fetch_bridge.rs`**: Inject synchronous fetch polyfill into JavaScript engine for XMLHttpRequest/fetch compatibility.

**`form.rs`**: HTML form detection and field parsing for login and submit flows.

**`js_engine.rs`**: QuickJS runtime (ES2020, ~1MB footprint, 32MB memory limit) for SPA data extraction.

**`mfa.rs`**: Detect and handle MFA challenges (TOTP, SMS, Email, Push notifications).

**`websocket.rs`**: WebSocket client with JSON-RPC convenience wrapper.

## CLI Commands (`cmd/`)

The CLI layer in `src/cmd/` maps each subcommand to its implementation:

```
cmd/
├── mod.rs              # Command dispatch
├── fetch.rs            # Single URL fetch
├── fetch_batch.rs      # Parallel multi-URL fetch
├── submit.rs           # Form submission
├── login.rs            # Auto-login flow
├── auth.rs             # Credential lookup
├── cookies.rs          # Browser cookie extraction
├── fingerprint.rs      # Profile generation display
├── bench.rs            # Performance benchmarking
├── validate.rs         # Live website validation
├── otp.rs              # OTP code retrieval
├── analyze.rs          # Video analysis
├── annotate.rs         # Video annotation
├── stream.rs           # Media streaming
├── spa.rs              # SPA data extraction
├── context.rs          # Context/session management
├── export_rules.rs     # Export embedded rule configs
└── output.rs           # Output formatting (markdown/JSON/compact)
```

## Data Flow: Typical Fetch Operation

```
1. User: nab fetch https://example.com --cookies brave
         |
2. CLI parsing (main.rs) -> cmd/fetch.rs
         |
3. SSRF validation (ssrf.rs)
         |
4. Detect browser cookies (browser_detect.rs -> auth/cookies/)
         |
5. Generate fingerprint (fingerprint/mod.rs)
         |
6. Check TLS impersonation requirement (impersonate_client.rs)
         |
7. Create HTTP client with cookies + headers (http_client.rs)
         |
8. Try site-specific extraction (site/mod.rs -> SiteRouter)
         |  -> Rule providers -> Hardcoded providers -> CSS plugins
         |
9. If no site match: fetch HTML (HTTP/2 or HTTP/3)
         |
10. Content pipeline (content/mod.rs -> ContentRouter)
         |  -> HTML handler -> readability -> quality scoring
         |
11. Diff tracking (content/diff.rs, if --diff set)
         |
12. Output to stdout (markdown/JSON/compact format)
```

MCP `fetch` layers query-focused extraction (`content/focus.rs`) and token budget truncation (`content/budget.rs`) via the `focus` and `max_tokens` parameters.

## Data Flow: MCP Fetch

```
1. Client: tools/call { name: "fetch", arguments: { url, cookies, focus } }
         |
2. MicroFetchHandler::handle_call_tool_request()
         |
3. FetchTool::run() -> same pipeline as CLI steps 3-13
         |
4. Return CallToolResult with outputSchema-conformant JSON:
   { url, status, content_type, content, timing_ms, has_diff }
```

## Configuration

**No config files required** -- smart defaults:
- Auto-detect default browser for cookies
- Markdown output by default
- Realistic fingerprints auto-generated
- HTTP/3 enabled by default
- TLS impersonation enabled by default

**Optional configuration files** (in `~/.config/nab/`):
- `plugins.toml`: CSS extractor and binary plugin definitions
- `sites/*.toml`: User overrides for built-in site rules

**Optional environment variables**:
- `RUST_LOG=nab=debug`: Enable debug logging
- `ANTHROPIC_API_KEY`: For vision analysis in `analyze` command

**Persistent state** (in `~/.nab/`):
- `fingerprint_versions.json`: Cached browser version data for auto-updates
- `sessions/`: Login session data

## Performance Characteristics

**Typical Response Time**: ~50ms with HTTP/3 and 0-RTT resumption

**Connection Pooling**: 10 idle connections per host, 90s timeout

**Memory Usage**:
- Base client: ~5MB
- JsEngine: 32MB limit per instance
- Streaming: Minimal buffering, uses pipes
- Sessions: 32 max (LRU eviction)

**Token Efficiency**: 25x savings (Markdown vs raw HTML)

## Extension Points

1. **New streaming provider**: Implement `StreamProvider` trait in `stream/providers/`
2. **New auth method**: Extend `CredentialRetriever` or `OtpRetriever` in `auth.rs`
3. **New fingerprint profile**: Add profile function in `fingerprint/mod.rs`
4. **New output format**: Add to `OutputFormat` enum in `cmd/output.rs`
5. **New site rule**: Add TOML file to `~/.config/nab/sites/` or `site/rules/defaults/`
6. **New CSS extractor plugin**: Add entry to `~/.config/nab/plugins.toml` with CSS selectors
7. **New site provider (Rust)**: Implement `SiteProvider` trait, register in `SiteRouter::new()`
8. **New content handler**: Implement `ContentHandler` trait, register in `ContentRouter::new()`
9. **New MCP tool**: Add tool struct in `bin/mcp_server/tools/`, register in `tool_box!()` macro
10. **New TLS impersonation domain**: Add domain to `IMPERSONATION_DOMAINS` in `impersonate_client.rs`

## Testing Strategy

- **Unit tests**: In module files (`#[cfg(test)] mod tests`)
- **Integration tests**: `tests/` directory
- **Real-world validation**: `nab validate` command tests against live websites
- **Benchmarks**: `nab bench` for performance testing; `criterion` benchmarks in `benches/`

## Dependencies

Key external dependencies:

- **reqwest**: HTTP/1.1 and HTTP/2 client with cookie jar, compression, and connection pooling
- **rquest / rquest-util**: BoringSSL-backed HTTP client for TLS fingerprint impersonation (feature-gated)
- **quinn / h3 / h3-quinn**: HTTP/3 and QUIC with 0-RTT (feature-gated)
- **rust-mcp-sdk**: MCP server runtime with stdio transport, tool macros, and task store
- **rquickjs**: JavaScript engine bindings (QuickJS, ES2020)
- **scraper**: CSS selector-based HTML DOM manipulation (Servo's html5ever)
- **readability**: Mozilla-style article extraction
- **html2md**: HTML to Markdown conversion
- **addr**: eTLD+1 domain extraction via Mozilla's public suffix list
- **1Password CLI (`op`)**: Credential lookup, TOTP retrieval, and passkey discovery via subprocess integration
- **tokio**: Async runtime
- **chromiumoxide**: Chrome DevTools Protocol for browser automation (feature-gated)
- **pdfium-render**: PDF extraction via Chromium's pdfium library (feature-gated)
- **zip / roxmltree**: OOXML parsing for Google Workspace document export

See `Cargo.toml` for complete list with feature flags.

## Feature Flags

| Flag | Default | Description |
|------|---------|-------------|
| `cli` | yes | CLI binary (`nab`) with clap argument parsing |
| `http3` | yes | HTTP/3 + QUIC via quinn |
| `impersonate` | yes | TLS fingerprint impersonation via rquest + BoringSSL |
| `pdf` | no | PDF to Markdown conversion via pdfium |
| `browser` | no | Browser automation via Chrome DevTools Protocol |
