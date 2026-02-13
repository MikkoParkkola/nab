# Rust Excellence Engineer - Project Memory

## nab Project Structure

### SiteProvider Framework (Phase 1: Twitter/X)

**Architecture**:
- `src/site/mod.rs`: Core framework with `SiteProvider` trait, `SiteRouter`, `SiteContent`, `SiteMetadata`
- `src/site/twitter.rs`: Twitter/X provider using FxTwitter API
- Integration: Wired into both CLI (`cmd_fetch`) and MCP server (`FetchTool::run`)

**Key Patterns**:
- Site providers checked BEFORE HTTP fetch for specialized handling
- `SiteRouter::try_extract()` returns `Option<SiteContent>` (None on no match or error)
- Errors logged as `tracing::warn` but don't block fallback to normal HTTP fetch
- URL matching: case-insensitive, strips query params
- All async with `async_trait`

**Testing**:
- 16 unit tests covering URL matching, parsing, formatting
- Zero clippy warnings
- Follows existing patterns from `stream::provider` architecture

**FxTwitter API**:
- Endpoint: `https://api.fxtwitter.com/{user}/status/{id}`
- Returns JSON with `tweet.text` or `tweet.article.content.blocks[]` for long-form
- Provides clean engagement metrics (likes, retweets, replies, views)
- More reliable than scraping HTML

**Integration Points**:
- CLI: Check providers before HTTP fetch, output markdown directly if matched
- MCP: Same pattern, adds "from specialized provider" notice
- Both use same `SiteRouter::new()` and `try_extract()` flow

**10 Providers** (as of 2026-02-13):
- Twitter (private mod), Reddit, HackerNews, GitHub, Instagram, YouTube, Wikipedia, StackOverflow, Mastodon, LinkedIn
- Twitter module is `mod twitter` (NOT `pub mod`), so `TwitterProvider` not directly benchmarkable
- All other providers are `pub mod`, structs accessible as `nab::site::<mod>::<Provider>`

**Provider Bug Fixes** (2026-02-13):
- **Reddit**: `AcceleratedClient` uses `http2_prior_knowledge()` which forces H2 without ALPN.
  Reddit's JSON API returns HTML instead of JSON via this path. Fix: build a fresh `reqwest::Client`
  without `http2_prior_knowledge` in `extract()`. Also: Reddit API returns `created_utc` as `f64`
  (not `u64`) and `score` can be negative (`i64`). Use `#[serde(default)]` on all fields for resilience.
- **Instagram**: Meta restricts oEmbed API (500 errors, non-JSON). Fix: try oEmbed first, fall back
  to extracting `og:title`, `og:description`, `og:image` from HTML `<meta>` tags using `scraper` crate.
- **Lesson**: Always test deserialization against REAL API responses, not hand-crafted JSON.
  Numeric types in JSON APIs are often floats even when they look like integers.

### Benchmarks (2026-02-13)

**Three criterion suites** in `benches/`:
1. `arena_benchmark` - Arena vs Vec allocation (existing)
2. `content_bench` - HTML-to-markdown at 1KB/10KB/50KB/200KB, ContentRouter dispatch
3. `router_bench` - SiteProvider URL matching per-provider, batch, construction

**Key numbers**:
- HTML conversion: 28us (1KB), 162us (10KB), 760us (50KB), 5.6ms (200KB)
- Provider URL matching: 337-468ns per 3 URLs (hit), 500ns-1.14us (miss)
- Router construction: 13-17ns
- Arena 2.2x faster than Vec for realistic responses
- Binary: nab 11MB, nab-mcp 9.5MB (release, LTO, stripped)
- 15 duplicate dep pairs, mostly passkey/quinn ecosystem lag

**Optimization opportunities** (identified, not implemented):
- `is_boilerplate()` allocates per-line via `to_lowercase()` -- use case-insensitive compare
- `format_number()` duplicated in twitter/reddit/hackernews
- `matches()` pattern (lowercase + split) duplicated across 6 providers
- `http_client.rs` has 3 builder methods with ~25 lines duplication each

### Stream/HTTP/3/WebSocket Module Review (Phase 3)

**Files reviewed and improved** (14 files, +857/-175 lines):
- `src/http3_client.rs`: QUIC/H3 client with quinn + h3 crates
- `src/prefetch.rs`: Connection warming + Early Hints (103) parser
- `src/websocket.rs`: WebSocket + JSON-RPC client with tungstenite
- `src/stream/`: Provider/backend architecture for media streaming

**Key improvements made**:
- Added `anyhow::Context` to ALL error paths (HTTP requests, process spawn, JSON parse)
- 105 new tests added (187 -> 292 total), zero clippy warnings
- Fixed unnecessary `Vec::clone` in `WebSocket::send_binary`
- Flattened nested if-let chains using let-else in `supports_h3`
- Added `PartialEq`/`Eq` derives and `is_binary`/`is_close` to `WebSocketMessage`
- `StreamProvider::name()` trait now returns `&'static str` (matches impls)
- Comprehensive `///` docs on all public types

**Lessons learned**:
- `Http3Client::new` needs tokio runtime (uses native cert loading) - tests must be `#[tokio::test]`
- bench/ warnings are outside scope - only library clippy matters
- Test CLI tests depend on 1Password - expect failures in test env
- `rustls::crypto::ring::default_provider().install_default()` is idempotent (returns `Err` on duplicate, but `let _ =` discards it safely)

**Stream architecture** (two-layer):
- **Providers** (`StreamProvider` trait): Yle, SVT, NRK, DR, Generic - extract metadata from APIs
- **Backends** (`StreamBackend` trait): NativeHls, Ffmpeg, Streamlink - handle actual data transfer
- Provider gets manifest URL -> Backend downloads segments
