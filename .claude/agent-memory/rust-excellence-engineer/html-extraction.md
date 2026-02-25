# HTML Extraction Improvements (2026-02-18)

## Bugs Fixed

### BUG 1: Hardcoded URL in readability
`readability::extract_article(html, "https://example.com")` — always used wrong URL.
Fix: thread real URL from `cmd_fetch.rs` through `ContentRouter::convert_with_url` -> `HtmlHandler::to_markdown_with_url` -> `readability::extract_article`.

**Key pattern**: `ContentRouter::convert()` now delegates to `convert_with_url(bytes, ct, None)`.
`convert_with_url()` is the hot path; HTML content gets special-cased before the handler loop.

### BUG 2: Comment section bleed-through (LessWrong)
Readability was scoring comment divs as article content.
Fix: `strip_comment_sections()` in `content/html.rs` — parses DOM, finds comment containers
by class/id pattern, rebuilds HTML without those nodes. Uses `element_hash()` to identify
elements across two DOM passes (scraper doesn't support mutation).

Comment markers: `comment-section`, `comments-section`, `comments-container`, `comment-list`,
`comment-thread`, `disqus`, `discussion-section`, `replies-section`.

### BUG 3: No SPA extraction (Cohere/Next.js)
Next.js embeds content in `<script id="__NEXT_DATA__">` JSON.
Fix: `extract_spa_data()` tries `__NEXT_DATA__` and `__NUXT_DATA__` before readability.
`extract_nextjs_content()` recursively searches `props.pageProps` for content fields.
Min content length: 100 chars (not 200 — too restrictive).

## Architecture Changes

- `ContentRouter::convert()` → delegates to `convert_with_url(url=None)`
- `ContentRouter::convert_with_url(url: Option<&str>)` — new URL-aware method
- `HtmlHandler::to_markdown_with_url(url: Option<&str>)` — passes URL to readability
- `html_to_markdown_with_url()` — public fn; pipeline: SPA → strip comments → readability → fallback
- `html_to_markdown_with_readability()` — backward-compat wrapper, calls with `url=None`

## Wire-up Points

- `cmd/fetch.rs`: `router.convert_with_url(&bytes, &ct, Some(&fetch_url))`
- `bin/mcp_server.rs`: same pattern with `url_clone = self.url.clone()`

## Rust 2024 Edition Issues (pre-existing)

- `gen` is reserved keyword — must use `r#gen` or rename variable
  - `subtitle.rs` tests: `let gen = ...` → `let generator = ...`
  - `fingerprint/mod.rs`: `rng.gen()` → `rng.r#gen()`
- `ref mut` in implicit borrow pattern → just `mut binding` (edition 2024 ergonomics)
  - `fusion.rs`: `if let (Some(ref mut t1), ...)` → `if let (Some(t1), ...)`
- Move semantics in loops: `if let Some(cb) = progress` in a loop moves the option
  → use `if let Some(cb) = &progress` to borrow

## Hook Behavior

The PostToolUse `rust-format.py` hook runs `cargo clippy --fix` on every Write.
It auto-applies clippy suggestions. The system reminders show WHAT the hook changed
(not a revert — it's improving the code). Do not be confused by the hook output.

## Clippy Pedantic Patterns

- `const` inside function after `let` → move `const` before `let` statements
  (clippy::items_after_statements)
- `.map(...).unwrap_or_else(...)` → `.map_or_else(|| ..., |x| ...)`
- Missing `# Errors` on `Result`-returning pub fns
- Missing `# Panics` when `unwrap()` used
- `#[must_use]` on functions whose return value is always meaningful
- Backtick site names in doc comments: `LessWrong` not LessWrong
- `#[allow(clippy::unwrap_used)]` when unwrap is provably safe (selector fallback)
