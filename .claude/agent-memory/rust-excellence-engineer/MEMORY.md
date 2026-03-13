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

**11 Providers** (as of 2026-02-17):
- Twitter (private mod), Reddit, HackerNews, GitHub, GoogleWorkspace, Instagram, YouTube, Wikipedia, StackOverflow, Mastodon, LinkedIn
- Twitter module is `mod twitter` (NOT `pub mod`), so `TwitterProvider` not directly benchmarkable
- All other providers are `pub mod`, structs accessible as `nab::site::<mod>::<Provider>`

**Google Workspace Provider** (2026-02-17):
- `src/site/google.rs` — Docs (HTML+OOXML), Sheets (CSV+OOXML), Slides (TXT+OOXML)
- Requires browser cookies — fails fast with helpful error if none provided
- `SiteProvider::extract()` signature extended: `cookies: Option<&str>` added (breaking change)
- All 10 existing providers updated with `_cookies: Option<&str>`, `plugin/runner.rs` also updated
- Cookie loading moved BEFORE `try_extract()` call in both `cmd/fetch.rs` and `mcp_server.rs`
- `SiteRouter::try_extract()` now takes `cookies: Option<&str>`

**LazyLock vs once_cell::Lazy (2026-02-17)**:
- `clippy::non_std_lazy_statics` (pedantic) flags `once_cell::sync::Lazy` — use `std::sync::LazyLock` (stable Rust 1.80+)
- Replace `Lazy::new(|| ...)` with `LazyLock::new(|| ...)`; import `use std::sync::LazyLock;` (drop `once_cell` import)

**`format_push_string` clippy fix**:
- `combined.push_str(&format!("## Tab: {}\n\n", name))` → `let _ = write!(combined, "## Tab: {}\n\n", name);`
- Requires `use std::fmt::Write as _;` (underscore avoids collision with `std::io::Write`)

**roxmltree namespace attribute gotcha**:
- `node.attribute("w:author")` returns `None` for namespaced attributes
- Must use `node.attribute(("http://schemas.openxmlformats.org/wordprocessingml/2006/main", "author"))`
- Pattern: try namespace-qualified first, fall back to unqualified: `.or_else(|| node.attribute("author"))`

**OOXML parsing** (zip + roxmltree):
- `.docx`: comments in `word/comments.xml` (w:comment elements), suggestions in `word/document.xml` (w:ins/w:del)
- `.xlsx`: modern in `xl/threadedComments/*.xml`, legacy in `xl/comments*.xml`
- `.xlsx` sheet data: workbook in `xl/workbook.xml` (<sheet name="..." sheetId="N"/>), sheets in `xl/worksheets/sheetN.xml` (1-based), shared strings in `xl/sharedStrings.xml` (<si><t>string</t></si>)
- `.xlsx` cell types: `t="s"` → shared string index in <v>; `t="b"` → boolean (1=TRUE, 0=FALSE); `t="inlineStr"` → text in <is><t>; default → numeric raw value in <v>
- `.pptx`: comments in `ppt/comments/*.xml` (cm or comment elements)
- Always use `let Ok(doc) = roxmltree::Document::parse(xml) else { return vec![]; }` (let-else)

**Google Docs multi-tab limitation** (2026-02-17):
- Tab IDs are opaque strings (e.g. `t.abc123`), rendered ONLY by JavaScript — absent from initial HTML
- Sequential IDs (`t.0`, `t.1`) do not exist in the Google export API
- PDF/HTML export without `&tab=` returns only the default/first tab
- NO viable approach for multi-tab export without browser JS execution
- Solution: always use single export path; document limitation in code comment

**Google Sheets multi-sheet fix** (2026-02-17):
- Old: `discover_sheets()` scraped editor HTML for `"sheetId":N` regex — never matched (JS-rendered)
- New: download xlsx once, parse `xl/workbook.xml` for sheet names, parse each `xl/worksheets/sheetN.xml`
- Benefit: single HTTP request (vs 1 editor + N CSV per sheet), correct sheet names, handles gaps in data
- Fall back to CSV export only when xlsx parsing returns empty content
- `xlsx_to_all_sheets_markdown()` is the main entry; reuses xlsx bytes for comment parsing too

**Clippy pedantic patterns learned**:
- `map_or(false, |x| ...)` → `.is_some_and(|x| ...)` (unnecessary_map_or)
- `.ends_with(".xml")` → use `Path::extension().is_some_and(|e| e.eq_ignore_ascii_case("xml"))` (case_sensitive_file_extension_comparisons)
- `match { Ok(d) => d, Err(_) => return vec![] }` → `let Ok(d) = ... else { return vec![]; }`
- `format!(..)` appended to String → `push_str` + separate operations
- `.filter_map(|x| Some(...))` that always returns Some → `.map(|x| ...)` (redundant_closures / unnecessary filter_map)
- `.take_while(|b| b.is_ascii_alphabetic())` → `.take_while(u8::is_ascii_alphabetic)` (redundant_closure_for_method_calls)

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

### Browser Automation (Phase 3: CDP Integration - 2026-02-15)

**Feature**: Optional Chrome DevTools Protocol integration for SPA login and CAPTCHA handling

**Architecture**:
- `src/browser.rs`: CDP client wrapper (330 lines, feature-gated)
- `BrowserLogin::connect(port)` - Connect to Chrome on port 9222
- `BrowserLogin::login(url, credential)` - Automated login with CAPTCHA detection
- `BrowserLogin::extract_cookies()` - Get session cookies from browser
- Integration: `src/login.rs` adds `with_browser()` and `browser_login()` methods
- CLI: `--browser` flag in `src/cmd/login.rs` and `src/main.rs`

**Key Patterns**:
- Everything behind `#[cfg(feature = "browser")]` - zero impact when disabled
- Default build unchanged: 11 MB, 320 tests pass
- With browser: 15 MB (+4 MB), 326 tests pass
- `chromiumoxide` crate for CDP via WebSocket
- `futures::StreamExt` required for `handler.next().await`

**Form Field Detection**:
- Multiple CSS selectors tried for username/password (exact match first, then substring)
- Arrays NOT references in for loops to avoid `&&str` type issues
- `input[name='username']`, `input[type='email']`, etc. (6 patterns each)

**CAPTCHA Detection**:
- Checks for `.g-recaptcha`, `.h-captcha`, `.cf-turnstile`, iframes
- 60-second pause when detected for manual solving
- Error messages contextual: suggest `--browser` when feature enabled

**Cookie Handling**:
- Extract all cookies for domain from CDP
- Convert to HTTP `Cookie` header format: `name1=value1; name2=value2`
- Used for subsequent requests after login

**Testing**:
- 6 unit tests in browser module (all pass)
- Test without feature: `cargo test --lib` (320 tests)
- Test with feature: `cargo test --lib --features browser` (326 tests)
- No actual Chrome needed for unit tests

**Build Commands**:
- Default: `cargo build --release --features pdf` (11 MB)
- With browser: `cargo build --release --features pdf,browser` (15 MB)

**Lessons Learned**:
- Feature gating must cover struct fields, methods, use statements, CLI args
- Iterator type matters: `for x in &array` gives `&&str`, `for x in array` gives `&str`
- Conditional compilation in main.rs needs careful block scoping for variables
- `#[cfg(not(feature = "browser"))]` useful for fallback paths
- StreamExt trait must be in scope for `.next()` on futures
- Doc comments should mention feature requirement: `/// (requires browser feature)`

**Documentation**:
- `docs/browser-automation.md` - Comprehensive guide (300+ lines)
- `PHASE3_IMPLEMENTATION.md` - Implementation summary

### Chromium Cookie Decryption Bug Fix (2026-03-12)

**Two root-cause bugs** in `src/auth/cookies.rs` that caused ALL cookies to decrypt as garbage:

**Bug 1 — Wrong AES IV**:
- Old (wrong): `AES_CBC_IV = [0u8; 16]` (16 zero bytes)
- Correct: `AES_CBC_IV = [b' '; 16]` (16 space/0x20 bytes)
- Source: `chromium/components/os_crypt/os_crypt_mac.mm`, `OSCryptImpl::DecryptString`
- Python reference: `self.iv = b' ' * 16` in `browser_cookie3/ChromiumBased.__init__`

**Bug 2 — Missing schema v24+ domain-integrity prefix**:
- Chromium cookie DB schema version 24 (Chrome 130+, Brave 1.70+) prepends `SHA-256(host_key)` (32 bytes) to every decrypted plaintext
- Query `SELECT value FROM meta WHERE key='version'` to get schema version
- If version >= 24: strip first 32 bytes after PKCS7 unpadding
- Both Brave and Chrome on this machine report schema version 24
- Reference: https://issues.chromium.org/issues/40185252

**Fix**:
- `query_db_schema_version()` function added — queries sqlite3 CLI for `meta.version`
- `decrypt_cookie_value(encrypted, key, has_domain_tag: bool)` — added `has_domain_tag` param
- `decrypt_rows(rows, key, has_domain_tag: bool)` — threaded through
- `get_cookies_native()` queries schema version, sets `has_domain_tag = version >= 24`
- Added `sha2 = "0.10"` to `Cargo.toml` (was only transitive before)
- 26 unit tests pass, 537/537 lib tests pass
- Verified live: 19 Brave cookies decrypted for linkedin.com, zero UTF-8 errors
