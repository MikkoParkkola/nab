# nab Architecture

This document describes the internal architecture of nab, a ultra-minimal browser engine with HTTP/3, JavaScript execution, cookie authentication, and anti-fingerprinting.

## Design Philosophy

**Token-Optimized for LLM Consumption**: Every design decision optimizes for minimal token usage while maximizing information density:
- Markdown output by default (25× token savings vs HTML)
- Structured JSON for parsing use cases
- Compact formats for status reporting

**Zero Friction Authentication**: Automatically detect and use browser cookies, 1Password credentials, and OTP codes without manual configuration.

**HTTP Performance**: Leverage HTTP/2 multiplexing, HTTP/3 (QUIC) with 0-RTT resumption, TLS 1.3, and modern compression (Brotli, Zstd).

**Anti-Fingerprinting**: Generate realistic browser fingerprints to avoid bot detection.

## High-Level Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                          CLI (main.rs)                           │
│  Commands: fetch, spa, stream, analyze, annotate, auth, otp...   │
└────────────┬─────────────────────────────────────────────────────┘
             │
             ├──────────────────────────────────────────────────┐
             │                                                  │
┌────────────▼──────────┐  ┌──────────────┐  ┌────────────────▼────┐
│  HTTP Clients         │  │ Auth Stack   │  │ JS Engine           │
│                       │  │              │  │                     │
│ • AcceleratedClient   │  │ • 1Password  │  │ • QuickJS (ES2020)  │
│   (HTTP/2, pooling)   │  │ • Cookies    │  │ • DOM injection     │
│ • Http3Client         │  │ • OTP codes  │  │ • Console capture   │
│   (QUIC, 0-RTT)       │  │ • Passkeys   │  │ • Fetch polyfill    │
└───────────┬───────────┘  └──────┬───────┘  └──────────┬──────────┘
            │                     │                    │
            │         ┌───────────▼────────────────────▼───────┐
            │         │   Browser Fingerprinting               │
            │         │  • Chrome/Firefox/Safari profiles      │
            │         │  • Auto-update from real versions      │
            │         │  • Realistic headers, TLS configs      │
            └─────────┴────────────────────────────────────────┘
                                  │
       ┌──────────────────────────┼──────────────────────────┐
       │                          │                          │
┌──────▼──────────┐  ┌───────────▼──────────┐  ┌───────────▼─────────┐
│ Streaming       │  │ Video Analysis       │  │ SPA Extraction      │
│                 │  │                      │  │                     │
│ • HLS/DASH      │  │ • Transcription      │  │ • __NEXT_DATA__     │
│ • Native parser │  │ • Speaker diarization│  │ • __NUXT__          │
│ • ffmpeg backend│  │ • Vision (Claude)    │  │ • Custom patterns   │
│ • VLC/mpv pipe  │  │ • Emotion detection  │  │ • 80% success rate  │
└─────────────────┘  └──────────────────────┘  └─────────────────────┘
                                  │
                     ┌────────────▼──────────────┐
                     │   Video Annotation        │
                     │ • Subtitle generation     │
                     │ • Speaker label overlays  │
                     │ • ffmpeg composition      │
                     └───────────────────────────┘
```

## Core Modules

### 1. HTTP Clients (`http_client.rs`, `http3_client.rs`)

**Purpose**: High-performance HTTP/1.1, HTTP/2, and HTTP/3 fetching with connection pooling.

**Key Features**:
- HTTP/2 multiplexing (100 concurrent streams per connection)
- HTTP/3 (QUIC) with 0-RTT connection resumption
- TLS 1.3 with session caching
- Brotli, Zstd, Gzip compression auto-negotiation
- DNS caching + Happy Eyeballs (IPv4/IPv6 racing)
- Connection pooling with 90s idle timeout

**Data Flow**:
```
URL → AcceleratedClient::fetch_text()
    → Apply fingerprint headers
    → Connection pool lookup
    → HTTP/2 or HTTP/3 request
    → Decompress response
    → Return HTML/JSON
```

**Used By**: All fetch operations, SPA extraction, streaming URL resolution

### 2. JavaScript Engine (`js_engine.rs`)

**Purpose**: Execute JavaScript for SPA data extraction and dynamic content rendering.

**Key Features**:
- QuickJS runtime (ES2020, ~1MB footprint)
- 32MB memory limit
- Fetch API polyfill injection via `fetch_bridge.rs`
- Console output capture for debugging

**Data Flow**:
```
HTML → JsEngine::new()
     → Load HTML + inject fetch polyfill
     → Execute embedded <script> tags
     → Extract window.__NEXT_DATA__ or similar
     → Return JSON data
```

**Used By**: `spa` command, API discovery

### 3. Authentication (`auth/`, `browser_detect.rs`)

**Purpose**: Zero-config authentication via browser cookies, 1Password, and OTP retrieval.

**Key Components**:
- **Cookie Extraction** (`auth/cookies/`): Auto-detect default browser (Brave, Chrome, Firefox, Safari, Edge, Dia) and extract cookies from SQLite/binary storage. Submodules: `mod.rs` (lookup), `crypto.rs` (AES-128-CBC decryption), `db.rs` (SQLite helpers)
- **1Password Integration**: Retrieve credentials, TOTP codes, and passkeys via `op` CLI
- **OTP Retrieval**: SMS (Beeper MCP), Email (Gmail API), TOTP (1Password)

**Data Flow**:
```
URL → detect_default_browser()
    → Extract cookies from browser DB
    → Inject into HTTP client cookie jar
    → Requests auto-authenticated
```

**Used By**: All fetch operations with `--cookies` flag

### 4. Browser Fingerprinting (`fingerprint/mod.rs`, `fingerprint/autoupdate.rs`)

**Purpose**: Generate realistic browser fingerprints to avoid bot detection.

**Key Features**:
- Chrome, Firefox, Safari profile generation
- Auto-update from real browser version APIs
- Realistic TLS client hello fingerprints
- Consistent User-Agent, sec-ch-ua, Accept headers

**Data Flow**:
```
Request → random_profile() or chrome_profile()
        → Generate headers (User-Agent, sec-ch-ua, Accept, etc.)
        → Apply to HTTP client
        → TLS fingerprint matching
```

**Auto-Update**: Fetches latest Chrome/Firefox versions weekly, stores in `~/.nab/fingerprint_versions.json`

**Used By**: All HTTP requests

### 5. Streaming (`stream/`)

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

### 6. Video Analysis (`analyze/`)

**Purpose**: Multimodal video analysis with transcription, speaker diarization, and vision understanding.

**Architecture**:
```
analyze/
├── mod.rs           # Pipeline orchestration
├── transcribe.rs    # Audio → text (Whisper/Parakeet)
├── diarize.rs       # Speaker segmentation
├── vision.rs        # Visual understanding (Claude API)
├── extract.rs       # Scene/frame extraction
├── fusion.rs        # Merge transcription + vision
└── report.rs        # Generate reports (JSON/Markdown/SRT)
```

**Data Flow**:
```
Video → extract audio → transcribe → diarize
     ↓
Extract frames → vision analysis → emotions/objects
     ↓
Fusion → timestamp alignment → JSON/Markdown output
```

**Used By**: `analyze` command

### 7. Video Annotation (`annotate/`)

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

**Data Flow**:
```
Analysis JSON → subtitle generation (SRT/ASS)
             → overlay positioning
             → ffmpeg filter_complex
             → Composited video with subtitles
```

**Used By**: `annotate` command

### 8. Error Handling (`error.rs`)

**Purpose**: Typed error hierarchy for stable public API.

**`NabError`** enum with 9 semantic variants: `InvalidUrl`, `SsrfBlocked`, `ProviderError`, `ConversionError`, `AuthError`, `LoginError`, `SessionError`, `NetworkError`, `BudgetExceeded`. Public functions return `Result<T, NabError>` at library boundaries; internal code uses `anyhow`.

### 9. Content Intelligence (`content/focus.rs`, `content/budget.rs`, `content/link_extract.rs`)

**Purpose**: Token-optimized content processing for LLM consumption.

- **Query-focused extraction** (`focus.rs`): BM25-lite scoring extracts sections relevant to a `focus` query; top-20% filter with diff-marker exemption
- **Token budget** (`budget.rs`): Structure-aware truncation via `max_tokens`; priority-based P0-P4 scoring, never splits mid-block
- **Link graph** (`link_extract.rs`): Same-site link extraction with eTLD+1 filtering (Mozilla PSL via `addr` crate) and relevance scoring

### 10. Sessions (`session.rs`)

**Purpose**: Persistent named sessions with LRU eviction (32 slots), cookie seeding from browser jars, pinned browser profiles.

### 11. Supporting Modules

**`api_discovery.rs`**: Discover API endpoints in SPA JavaScript code via pattern matching.

**`fetch_bridge.rs`**: Inject synchronous fetch polyfill into JavaScript engine for XMLHttpRequest/fetch compatibility.

**`mfa.rs`**: Detect and handle MFA challenges (TOTP, SMS, Email, Push notifications).

**`prefetch.rs`**: Parse Early Hints (HTTP 103) and extract link preload hints for performance optimization.

**`websocket.rs`**: WebSocket client with JSON-RPC convenience wrapper.

## Data Flow: Typical Fetch Operation

```
1. User: nab fetch https://example.com
         ↓
2. CLI parsing (main.rs)
         ↓
3. Detect browser cookies (browser_detect.rs)
         ↓
4. Generate fingerprint (fingerprint/mod.rs)
         ↓
5. Create HTTP client with cookies + headers (http_client.rs)
         ↓
6. Fetch HTML (HTTP/2 or HTTP/3)
         ↓
7. Convert HTML → Markdown (html2md)
         ↓
8. Output to stdout (compact/JSON/full format)
```

## Data Flow: SPA Extraction

```
1. User: nab spa https://nextjs-app.com
         ↓
2. Fetch HTML with cookies + fingerprint
         ↓
3. Initialize JsEngine (js_engine.rs)
         ↓
4. Inject fetch polyfill (fetch_bridge.rs)
         ↓
5. Execute embedded <script> tags
         ↓
6. Extract window.__NEXT_DATA__ or __NUXT__
         ↓
7. Parse and format JSON
         ↓
8. Output structured data
```

## Configuration

**No config files required** — smart defaults:
- Auto-detect default browser for cookies
- Markdown output by default
- Realistic fingerprints auto-generated
- HTTP/3 enabled by default

**Optional environment variables**:
- `RUST_LOG=nab=debug`: Enable debug logging
- `ANTHROPIC_API_KEY`: For vision analysis in `analyze` command

**Persistent state** (in `~/.nab/`):
- `fingerprint_versions.json`: Cached browser version data for auto-updates

## Performance Characteristics

**Typical Response Time**: ~50ms with HTTP/3 and 0-RTT resumption

**Connection Pooling**: 10 idle connections per host, 90s timeout

**Memory Usage**:
- Base client: ~5MB
- JsEngine: 32MB limit per instance
- Streaming: Minimal buffering, uses pipes

**Token Efficiency**: 25× savings (Markdown vs raw HTML)

## Extension Points

Want to add a new feature? Common extension patterns:

1. **New streaming provider**: Implement `StreamProvider` trait in `stream/providers/`
2. **New auth method**: Extend `CredentialRetriever` or `OtpRetriever` in `auth.rs`
3. **New fingerprint**: Add profile function in `fingerprint/mod.rs`
4. **New output format**: Add to `OutputFormat` enum in `main.rs`

## Testing Strategy

- **Unit tests**: In module files (`#[cfg(test)] mod tests`)
- **Integration tests**: `tests/` directory
- **Real-world validation**: `nab validate` command tests against live websites
- **Benchmarks**: `nab bench` for performance testing

## Dependencies

Key external dependencies:
- **reqwest**: HTTP/1.1 and HTTP/2 client
- **quinn/h3/h3-quinn**: HTTP/3 and QUIC
- **rquickjs**: JavaScript engine bindings
- **html5ever/scraper**: HTML parsing
- **passkey**: WebAuthn/passkey support
- **tokio**: Async runtime

See `Cargo.toml` for complete list with feature flags.

## Future Architecture Considerations

- **Async JavaScript execution**: Currently synchronous, could add async support
- **Custom TLS fingerprints**: More browsers beyond Chrome/Firefox/Safari
- **Distributed tracing**: Add OpenTelemetry for observability
