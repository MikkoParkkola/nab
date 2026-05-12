# Design: nab MCP Feature Innovations

**Status**: Implemented (v0.5.0)
**Author**: Mikko Parkkola
**Date**: 2026-03-13
**nab version**: 0.4.0 (design baseline), implemented in 0.5.0

## Executive Summary

nab's MCP server (`nab-mcp`) exposes 12 tools: `fetch`, `fetch_batch`, `submit`, `login`, `auth_lookup`, `fingerprint`, `validate`, `benchmark`, `analyze`, `watch_create`, `watch_list`, and `watch_remove`. These tools fetch URLs and convert content to markdown for LLM consumption, transcribe local media, and monitor URLs for changes.

This document specifies 5 features that make nab the first MCP server designed for token economy. Together they reduce per-call token consumption by 10-50x for common patterns, eliminate sequential round-trips, and extend nab from a stateless fetcher into a session-aware web automation layer.

| # | Feature | Token Savings | Effort (incl. tests) | Priority | Latency Budget |
|---|---------|--------------|----------------------|----------|----------------|
| 2 | Query-Focused Extraction | 10-50x on large pages | ~200 LOC | P0 | +5ms max |
| 3 | Token Budget Enforcement | Prevents context blowouts | ~150 LOC | P0 | +2ms max |
| 4 | Prefetch Link Graph | 3-5x faster research | ~340 LOC | P1 | max(linked) + 10ms |
| 5 | Persistent Sessions | Enables multi-step auth flows | ~200 LOC | P1 | +0ms (lookup) |
| 6 | Site Extraction Registry | User-extensible providers | ~245 LOC | P2 | +1ms (match) |

Total estimated effort: ~1135 LOC (library + MCP wiring + tests).

### Latency Targets

All features are post-processing on already-fetched content. The network fetch dominates (~50-500ms). Latency budgets (in table above) are additive to fetch time. Focus + Budget are pure CPU (<10ms combined for 500 sections). Sessions and Registry are sub-millisecond lookups.


## Feature 2: Query-Focused Extraction

### Problem

A documentation page returns 100KB of markdown. The LLM's query is about one API method. The LLM ingests all 100KB, wasting 95% of context window on irrelevant content. There is no way to tell nab "I only care about the authentication section."

### Solution

Add a `focus` parameter to the `fetch` MCP tool. When set, nab splits the converted markdown into sections (by headings), scores each section against the query using keyword overlap (BM25-lite), and returns only the top-scoring sections. Omitted sections are replaced with `[... N sections omitted ...]` markers that preserve document structure without consuming tokens.

### API

**Tool**: `fetch`
**New parameter**:

```json
{
  "url": "https://docs.example.com/api",
  "focus": "authentication bearer token"
}
```

**Parameter schema**:

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `focus` | `string` | No | Natural-language query. When set, only sections relevant to this query are returned. |

**Response behavior**:
- When `focus` is absent: current behavior (full content).
- When `focus` is set: sections scored and filtered. The `structured_content` gains an `omitted_sections` integer field.

**Example output**:

```
[... 3 sections omitted ...]

## Authentication

Bearer tokens are issued via the `/oauth/token` endpoint...

[... 5 sections omitted ...]

## Token Refresh

Refresh tokens expire after 30 days...

[... 2 sections omitted ...]
```

### Implementation Notes

**New module**: `src/content/focus.rs` (~120 LOC)

1. **Section splitting** -- Split markdown on heading boundaries (`# `, `## `, `### `, etc.). Each section includes its heading and all content until the next heading of equal or higher level. Implementation: iterate lines, detect heading prefixes, accumulate into `Vec<Section>` where `Section { heading: String, level: u8, body: String, char_offset: usize }`.

2. **Scoring** -- BM25-lite scoring against the focus query. Before tokenization, strip markdown link URLs: convert `[text](url)` to just `text` and remove bare URLs. This prevents link-heavy sections (e.g., "See also" or navigation) from diluting term frequency with URL tokens. Tokenize query and sections into lowercase words, compute term frequency per section, apply inverse document frequency across all sections. No external crate needed; the algorithm is ~50 lines. Formula: `score = sum(tf(t,s) * idf(t) for t in query_terms)` where `tf = count / section_len` and `idf = log(N / df)`.

3. **Filtering** -- Keep the top 20% of sections by score, with a minimum of 3 and a maximum of 10 sections. Always keep the first section (page title / intro) regardless of score. Consecutive omitted sections are collapsed into a single `[... N sections omitted ...]` marker.

   **Threshold rationale**: On a 50-section doc, top 20% = 10 sections ≈ 5x reduction. On a 100-section doc, top 20% = 20 sections but capped at 10 ≈ 10x reduction. The cap ensures large documents don't return more than the LLM can usefully process in one shot. The minimum of 3 ensures tiny documents aren't over-filtered.

**Integration point**: `FetchTool::run()` in `src/bin/mcp_server/tools.rs`. After `convert_body_async()` returns the full markdown, call `focus::extract_focused(markdown, query)` to produce the filtered output. The filtered text replaces the full markdown in both the text content and `structured_content.content`.

**⚠️ Site provider bypass**: When a URL matches a built-in site provider (GitHub, Twitter, YouTube, etc.), `FetchTool::run()` returns at `tools.rs:128` BEFORE reaching any post-processing code. This means `focus`, `max_tokens`, `diff`, and `prefetch_links` are silently ignored for provider-matched URLs. **Fix**: after the site provider returns its markdown, the post-processing pipeline (focus → diff → budget) must still run on the provider output. The site provider early-return should be refactored to merge into the main content pipeline. This is a cross-cutting change that affects all features in this document.

**Existing infrastructure used**:
- `content::readability::extract_article` already strips boilerplate before this step
- `content::html::html_to_markdown_with_url` produces heading-structured markdown
- `ContentRouter::convert_with_url` is the entry point unchanged

### Degenerate Inputs

| Input | Behavior |
|-------|----------|
| `focus=""` (empty string) | Ignored; full content returned (same as absent). |
| `focus` matches zero sections | Return all sections with a `[no sections matched focus query]` header. Never return empty content. |
| Page has no headings | Full content returned unchanged. `omitted_sections: 0`. |
| Page is a single heading + paragraph | Return as-is. BM25 scoring skipped for ≤3 sections. |
| `focus` is 1000+ characters | Truncate query to first 200 chars before tokenizing. |

### Acceptance Criteria

- AC1: `fetch(url, focus="authentication")` on a 100KB docs page returns less than 10KB.
- AC2: All returned sections contain at least one query term or are structurally necessary (intro section).
- AC3: Omitted-section markers show accurate counts.
- AC4: When `focus` is absent, behavior is identical to current (no regression).
- AC5: When the page has no headings, full content is returned (graceful degradation).
- AC6: Focus processing on a 500-section document completes in <5ms (benchmark test).

### Estimated LOC

| Component | LOC |
|-----------|-----|
| `src/content/focus.rs` (split + score + filter) | ~120 |
| `src/bin/mcp_server/tools.rs` (FetchTool integration) | ~20 |
| `src/content/mod.rs` (module declaration) | ~2 |
| Tests | ~60 |
| **Total** | **~200** |


## Feature 3: Token Budget Enforcement

### Problem

LLMs have finite context windows. A fetch that returns 200KB of markdown can blow the context, causing the LLM to lose earlier conversation history or fail entirely. There is no way to say "give me at most 4000 tokens of content."

### Solution

Add a `max_tokens` parameter to the `fetch` tool. When set, nab performs structure-aware truncation: it prioritizes headings, first paragraphs of each section, and code blocks over body text. This is not character-level slicing -- it preserves document structure and readability.

### API

**Tool**: `fetch`
**New parameter**:

```json
{
  "url": "https://example.com/long-page",
  "max_tokens": 2000
}
```

**Parameter schema**:

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `max_tokens` | `integer` | No | None (unlimited) | Maximum approximate token count for the returned content. Uses 4 chars/token heuristic. |

**Response behavior**:
- Content is structurally truncated to fit within the budget.
- A `[... truncated to ~N tokens, full page: ~M tokens ...]` footer indicates truncation occurred.
- The `structured_content` gains `truncated: true` and `full_tokens: M` fields.

### Implementation Notes

**New module**: `src/content/budget.rs` (~80 LOC)

1. **Token estimation** -- Use 4 characters per token heuristic (standard for English text with GPT-family tokenizers). No tokenizer dependency needed. `fn estimate_tokens(text: &str) -> usize { text.len() / 4 }`.

2. **Priority scoring** -- Assign priority to each markdown block:
   - Priority 0 (always keep): Page title (first `# ` heading)
   - Priority 1: All headings (`## `, `### `, etc.) — **capped at 30% of budget**. When headings alone exceed 30%, keep only top-level (`##`) headings and collapse deeper headings into `[... N subsections ...]` markers.
   - Priority 2: First paragraph after each heading
   - Priority 3: Code blocks (fenced with triple backticks) — only when page has <20 code blocks. On code-heavy pages, code blocks drop to Priority 4 (treated same as body text).
   - Priority 4: Remaining paragraphs and overflow code blocks

3. **Budget allocation** -- Walk blocks in priority order, accumulating estimated tokens. Stop when budget is reached. Within the same priority level, preserve document order. Heading budget is enforced as a sub-ceiling: even if total budget allows more headings, the 30% cap prevents the "table of contents with no content" failure mode.

**Integration point**: `FetchTool::run()` in `src/bin/mcp_server/tools.rs`. After `convert_body_async()` (and after `focus::extract_focused` if `focus` is set), call `budget::truncate_to_budget(markdown, max_tokens)`. This returns the truncated markdown and metadata about what was cut.

**Interaction with Feature 2**: When both `focus` and `max_tokens` are set, focus filtering runs first (reduces to relevant sections), then budget truncation runs on the filtered output. This is the correct order: filter by relevance, then by size.

**Existing infrastructure used**:
- `structured::truncate_markdown` already does character-level truncation (currently hard-coded to 4000 chars in 6 call sites). This feature replaces it with a structure-aware version.
- `build_fetch_structured` already builds the structured_content map; add `truncated` and `full_tokens` fields.

**Migration from `truncate_markdown`**: The existing `truncate_markdown(text, 4000)` calls in `tools.rs` (6 sites) and `elicitation.rs` (1 site) will be replaced:
- When `max_tokens` is set: use `budget::truncate_to_budget(markdown, max_tokens)` (structure-aware).
- When `max_tokens` is absent: keep `truncate_markdown(markdown, 4000)` for `structured_content` only (backward compat). The text content remains untruncated.
- The `4000` char constant moves to a named `const STRUCTURED_CONTENT_MAX_CHARS: usize = 4000` in `structured.rs`.

**Token estimation accuracy**: The 4 chars/token heuristic is calibrated for English prose. Known deviations:
- Code: ~3 chars/token (overestimates by ~25%)
- CJK: ~1.5 chars/token (underestimates by ~60%)
- URLs/paths: ~6 chars/token (overestimates by ~33%)

Since this is a *budget* (upper bound), overestimating is safe (returns less content than budgeted). Underestimating for CJK means CJK pages may exceed the budget by up to 60%. Acceptable for v1; a future enhancement can detect character ranges and adjust the ratio.

### Degenerate Inputs

| Input | Behavior |
|-------|----------|
| `max_tokens=0` | Return only the truncation footer (no content). |
| `max_tokens=1` | Return page title only (Priority 0 block). |
| `max_tokens=999999` | Content fits within budget; returned unchanged. |
| Content is a single code block | Return the code block whole or truncate footer only. Never split mid-block. |

### Acceptance Criteria

- AC1: `fetch(url, max_tokens=1000)` returns content estimating to 1000 tokens or fewer.
- AC2: Truncated output preserves all headings from the original document.
- AC3: Truncated output includes the first paragraph of each section before including second paragraphs of any section.
- AC4: Code blocks are preserved whole (not split mid-block).
- AC5: When content fits within budget, output is unchanged.
- AC6: Truncation footer shows accurate original and truncated token estimates.
- AC7: Budget enforcement on a 200KB document completes in <2ms (benchmark test).

### Estimated LOC

| Component | LOC |
|-----------|-----|
| `src/content/budget.rs` (parse blocks, prioritize, truncate) | ~80 |
| `src/bin/mcp_server/tools.rs` (FetchTool integration) | ~15 |
| `src/content/mod.rs` (module declaration) | ~2 |
| Tests | ~50 |
| **Total** | **~150** |


## Feature 4: Prefetch Link Graph

### Problem

A common LLM research pattern: fetch a documentation index page, identify 5 relevant links, then fetch each one sequentially. Each fetch is a separate MCP round-trip. With nab's current design, this takes 5 sequential calls after the initial fetch -- typically 2-5 seconds of wall time per call.

### Solution

Add a `prefetch_links` parameter to `fetch`. When set to N, nab returns the main page AND pre-fetches the N most relevant linked pages from the content. All linked pages are fetched concurrently using HTTP/2 multiplexing. Results are returned as a structured array alongside the main content.

**Design alternative rejected**: Separate `fetch_deep` tool — rejected because `prefetch_links` composes naturally with `focus`/`max_tokens`, and tool proliferation is worse than parameter proliferation for MCP clients. The `fetch` tool will have 9 parameters total, within typical range for HTTP client tools.

### API

**Tool**: `fetch`
**New parameter**:

```json
{
  "url": "https://docs.example.com/api/",
  "prefetch_links": 5,
  "focus": "authentication"
}
```

**Parameter schema**:

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `prefetch_links` | `integer` | No | 0 (disabled) | Number of linked pages to pre-fetch. Max: 10. |

**Response behavior**:
- Main page content is returned as usual.
- Linked pages appear in `structured_content.linked_pages` as an array of `{ url, title, content, timing_ms }` objects.
- When combined with `focus`, both the main page and linked pages are filtered by the focus query.
- When combined with `max_tokens`, the budget is split adaptively (see Feature Interaction Matrix).

**Scope**: `prefetch_links` applies to `fetch` only, not `fetch_batch`. `fetch_batch` already handles parallel URL fetching; adding link-graph expansion to each batch URL would create combinatorial explosion (10 URLs × 10 links = 100 fetches). If needed, the LLM can call `fetch` with `prefetch_links` for specific URLs after a `fetch_batch` triage.

Similarly, `focus` and `max_tokens` apply to `fetch` only for v1. Extending them to `fetch_batch` is straightforward (apply per-URL in the batch loop) but deferred to avoid scope creep.

**Example structured output**:

```json
{
  "url": "https://docs.example.com/api/",
  "status": 200,
  "content": "# API Reference\n\n...",
  "linked_pages": [
    {
      "url": "https://docs.example.com/api/auth",
      "title": "Authentication",
      "content": "## Bearer Tokens\n\n...",
      "timing_ms": 45.2
    }
  ],
  "timing_ms": 120.5
}
```

### Implementation Notes

**New module**: `src/content/link_extract.rs` (~80 LOC)

1. **Link extraction** -- Parse the converted markdown for inline links (`[text](url)`). Resolve relative URLs against the fetch URL using `url::Url::join`. Deduplicate by normalized URL. Filter out anchors (`#`), non-HTTP schemes, and external domains (different host) unless the original URL is itself a docs index.

2. **Relevance scoring** -- When `focus` is set, score extracted links by their anchor text and surrounding context against the focus query. When `focus` is absent, rank by document position (earlier links are more likely to be important on index pages).

3. **Concurrent fetch** -- Reuse the existing `AcceleratedClient` and `SafeFetchConfig` for each linked page. Spawn all N fetches via `futures::future::join_all`, but with a **per-host concurrency limit of 3** using `tokio::sync::Semaphore` keyed by host. This prevents hammering a single documentation server with 10 simultaneous requests, which triggers Cloudflare and similar WAFs. HTTP/2 multiplexing reduces TCP connections but does NOT prevent server-side rate limiting. The `PrefetchManager::preconnect_many` call is NOT used — it generates real HEAD requests which create detection risk.

4. **Content conversion** -- Each linked page goes through the same `convert_body_async` pipeline as the main page.

**Integration point**: `FetchTool::run()` in `src/bin/mcp_server/tools.rs`. After the main page is fetched and converted, if `prefetch_links > 0`:
1. Call `link_extract::extract_links(markdown, url, prefetch_links)` to get the top-N URLs.
2. Fetch all N URLs concurrently.
3. Convert each response through `convert_body_async`.
4. Apply `focus` filtering to each linked page (if `focus` is set).
5. Apply per-page `max_tokens` budget (if `max_tokens` is set).
6. Build the `linked_pages` array in `structured_content`.

**Existing infrastructure used**:
- `PrefetchManager::preconnect_many` can pre-warm connections to linked hosts
- `AcceleratedClient::fetch_safe` provides SSRF-safe fetching for linked URLs
- `ContentRouter::convert_with_url` handles all content types for linked pages
- `FetchBatchTool::run()` already demonstrates the concurrent fetch pattern

**Security hardening** (SSRF amplification prevention):
- **No recursive following**: Linked pages are fetched but their content is NOT scanned for further links. Depth is always exactly 1.
- **Same-site policy**: By default, only links with the same registered domain (eTLD+1) as the main URL are followed. This means `docs.example.com` can follow links to `api.example.com` or `cdn.example.com`, but not to `evil.com`. Implementation: use the `publicsuffix` crate (or `addr` crate which wraps it) to correctly extract the registered domain. Naive two-segment suffix comparison is NOT acceptable — it fails on `github.io`, `co.uk`, `amazonaws.com`, and similar multi-part TLDs. New dependency: `addr = "0.15"` (~50KB, pure Rust, embeds the Mozilla public suffix list). Cross-site links are excluded. Rationale: prevents crafted HTML pages from using nab as a port scanner via `<a href="http://10.0.0.x:port">` links, while allowing documentation spread across subdomains.
- **Per-linked-page size cap**: Each linked page fetch has a 1MB response body limit (matches `SafeFetchConfig` defaults). Prevents fetching multi-GB files linked from the main page.
- **Per-linked-page timeout**: 10 seconds per linked page (not per batch). Slow pages are abandoned, not blocking.
- **Link count filtering**: After extraction, links are deduplicated by normalized URL, then the top-N are selected. The extraction itself processes at most 200 links from the document to avoid O(n²) scoring on link-heavy pages.

### Degenerate Inputs

| Input | Behavior |
|-------|----------|
| `prefetch_links=0` | Disabled (default). No link extraction or fetching. |
| `prefetch_links=100` | Clamped to 10. Warning in response. |
| Page has no links | `linked_pages: []`. No error. |
| All linked fetches fail | `linked_pages: []`. Main page content still returned normally. |
| Page has 10,000 links | Only first 200 are scored; top-N selected from those. |
| Linked page is 50MB PDF | Rejected by 1MB body limit. Omitted from results. |

### Acceptance Criteria

- AC1: `fetch(url, prefetch_links=3)` returns the main page plus up to 3 linked pages.
- AC2: Linked pages are fetched concurrently (total time is approximately max(individual times), not sum).
- AC3: Relative URLs in links are resolved correctly against the main page URL.
- AC4: SSRF protection applies to all linked page fetches (no fetching localhost, private IPs).
- AC5: When `focus` is set, linked pages are scored by relevance to the focus query.
- AC6: When a linked page fetch fails, it is omitted from results (not an error for the whole call).
- AC7: `prefetch_links` is capped at 10 to prevent abuse.
- AC8: Linked pages from different registered domains than the main URL are excluded by default (same-site policy).
- AC9: No recursive link following — linked pages' links are never extracted.

### Estimated LOC

| Component | LOC |
|-----------|-----|
| `src/content/link_extract.rs` (extract, resolve, score) | ~80 |
| `src/bin/mcp_server/tools.rs` (FetchTool integration + concurrent fetch) | ~150 |
| `src/bin/mcp_server/main.rs` (outputSchema update for linked_pages) | ~30 |
| `src/content/mod.rs` (module declaration) | ~2 |
| Tests | ~80 |
| **Total** | **~340** |


## Feature 5: Persistent Sessions

### Problem

Multi-step web interactions lose state between MCP calls. An LLM that calls `login(url="github.com")` gets an authenticated response, but the next `fetch(url="github.com/settings")` starts a fresh unauthenticated session. The reqwest `CookieStore` built into `AcceleratedClient` is per-process but the global client singleton means all cookies from all URLs share a flat namespace. There is no way to maintain isolated, named sessions across calls.

### Solution

Add a `session` parameter to `fetch`, `submit`, and `login`. Named sessions persist cookies across MCP calls within the same `nab-mcp` process lifetime. The `login` tool stores the authenticated cookie jar in the session; subsequent `fetch` calls with the same session name use those cookies automatically.

### API

**New parameter on `fetch`, `submit`, `login`**:

```json
{
  "url": "https://github.com/settings",
  "session": "github"
}
```

**Parameter schema**:

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `session` | `string` | No | None | Named session for cookie persistence. Creates the session on first use. |

**Interaction with `cookies` parameter**:
- When both `session` and `cookies` are set: browser cookies are loaded into the session on first use, then session cookies take precedence on subsequent calls.
- When only `session` is set: an empty cookie jar is created and populated by server responses.
- When only `cookies` is set: current behavior (one-shot browser cookie injection).

**Session lifecycle**:
- Sessions are created implicitly on first use.
- Sessions persist for the lifetime of the `nab-mcp` process.
- Future extension: `~/.nab/sessions/{name}.json` for cross-process persistence (out of scope for v1).

### Implementation Notes

**New module**: `src/session.rs` (~100 LOC)

1. **Session store** -- `SessionStore` wraps a `HashMap<String, SessionEntry>` behind a `tokio::sync::RwLock`. `SessionEntry` holds `Arc<reqwest::cookie::Jar>`, `reqwest::Client`, `BrowserProfile`, and `last_used: Instant`. Global singleton accessed via `OnceCell`. Maximum 32 concurrent sessions with **LRU eviction**: when a 33rd session is created, the session with the oldest `last_used` timestamp is evicted (its client and jar are dropped). Every `get_or_create` call updates `last_used`. This prevents unbounded accumulation while keeping active sessions alive.

2. **Session-aware client** -- `SessionStore::get_or_create(name)` returns a `(Client, Arc<Jar>)` pair. The client is built once per session with `ClientBuilder::cookie_provider(jar.clone())`. **Critical**: the `BrowserProfile` (TLS fingerprint, User-Agent, headers) is selected once at session creation and pinned for the session's lifetime. This ensures TLS fingerprint continuity (JA3/JA4 hash) across requests within the same session — a requirement for auth-sensitive sites that use TLS fingerprinting to detect session replay. Connection pools are per-client (unavoidable with reqwest's architecture), but the 32-session cap bounds resource overhead.

3. **Cookie seeding** -- When `cookies` is provided alongside `session`, browser cookies are loaded into the session jar. **Important**: `resolve_cookie_header` returns a `Cookie:` request header (`"SID=abc; HSID=def"`), but `reqwest::cookie::Jar::set_cookies` requires `Set-Cookie` response headers (which include domain, path, and expiry). These are different formats. The seeding implementation must parse the `Cookie:` header string into individual name=value pairs, then synthesize `Set-Cookie` headers with the request URL's domain and path (e.g., `Set-Cookie: SID=abc; Domain=github.com; Path=/`). Without domain scoping, reqwest's Jar will not send the cookies to any URL.

**Integration points**:

- `FetchTool::run()`: When `session` is set, use the session-specific client instead of the global `get_client()`.
- `SubmitTool::run()`: Same pattern.
- `LoginTool::run()`: After successful login, the session's cookie jar automatically contains the authenticated cookies (reqwest populates them from `Set-Cookie` headers).

**Existing infrastructure used**:
- `reqwest::Client::builder().cookie_store(true)` already enables per-client cookie jars
- `reqwest::cookie::Jar` implements `CookieStore` and is `Send + Sync`
- `helpers::resolve_cookie_header` extracts browser cookies for seeding
- `LoginFlow` already supports `cookie_header: Option<String>` -- session cookies can be serialized into this format
- `login.rs::SESSION_DIR` is already defined as `".nab/sessions"` (currently unused) -- ready for future persistence

**Security considerations**:
- **Session isolation**: `nab-mcp` runs as a single process per MCP client. When accessed through mcp-gateway, each client gets its own `nab-mcp` process, so sessions are naturally isolated. If multiple MCP clients share the same `nab-mcp` process (not currently the case), session names would be accessible across clients. For v1 this is acceptable — sessions are per-process state, like any in-memory cookie jar.
- **Session name validation**: Alphanumeric + hyphens only, max 64 chars. Prevents path traversal if session names are later used as filesystem paths for persistence.
- **No session enumeration**: The API provides no tool to list active sessions. Sessions can only be accessed by name.
- **Cookie scope**: Session cookies respect the `reqwest::Jar` domain/path scoping rules. Cookies set for `github.com` are not sent to `evil.com` even within the same session.

### Degenerate Inputs

| Input | Behavior |
|-------|----------|
| `session=""` | Invalid. Error: "session name must be 1-64 alphanumeric/hyphen characters". |
| `session="../../etc"` | Invalid. Rejected by alphanumeric+hyphen validation. |
| `session="a"` repeated 1000 times | Same session reused (idempotent creation). HashMap lookup. |
| 100 different session names | 100 independent cookie jars in memory. ~1KB each empty. |

### Acceptance Criteria

- AC1: `login(url, session="s1")` followed by `fetch(url, session="s1")` uses the authenticated cookies.
- AC2: Two concurrent sessions with different names have independent cookie jars.
- AC3: A session created with `cookies="brave"` includes browser cookies on first use.
- AC4: Omitting the `session` parameter gives current behavior (no regression).
- AC5: Session names are validated (alphanumeric + hyphens only, max 64 chars).
- AC6: The session store is thread-safe under concurrent MCP calls.
- AC7: Session cookies respect domain scoping (no cross-domain leakage within a session).

### Estimated LOC

| Component | LOC |
|-----------|-----|
| `src/session.rs` (SessionStore, session-client builder) | ~100 |
| `src/bin/mcp_server/tools.rs` (FetchTool, SubmitTool, LoginTool changes) | ~40 |
| `src/lib.rs` (module declaration + re-export) | ~3 |
| Tests | ~60 |
| **Total** | **~200** |


## Feature 6: Site Extraction Registry

### Problem

nab has 12 hardcoded site providers in `src/site/` (Twitter, Reddit, HackerNews, GitHub, Google Workspace, Instagram, YouTube, Wikipedia, StackOverflow, Mastodon, LinkedIn, Substack). Adding a new provider requires writing Rust code, rebuilding, and redeploying. Users who need custom extraction for internal sites or niche platforms cannot extend nab without forking.

### Solution

Extend the existing plugin system at `~/.config/nab/plugins.toml` with a `type = "css"` mode. CSS extractors use selectors and content rules instead of external binaries. They are loaded at startup and participate in `SiteRouter` dispatch alongside built-in providers.

### API

**Configuration file**: `~/.config/nab/plugins.toml` (extended, not a new file)

See implementation notes below for the TOML format example.

**Schema (for `type = "css"` entries)**:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Unique extractor name. |
| `patterns` | string[] | Yes | URL regex patterns (same as plugin system). |
| `content.selector` | string | Yes | CSS selector for the main content container. |
| `content.remove` | string[] | No | CSS selectors for elements to strip from content. |
| `metadata.title` | string | No | CSS selector for the title element (text content extracted). |
| `metadata.author` | string | No | CSS selector for the author element. |
| `metadata.published` | string | No | CSS selector for the publication date element. |

### Implementation Notes

**Architecture decision**: Extend the existing plugin system at `~/.config/nab/plugins.toml` rather than creating a parallel config file. The plugin system already has `name`, `binary`, and `patterns` — the exact same concepts. Adding a `type = "css"` mode to the plugin config avoids two nearly-identical config files and reduces user confusion.

**Extended plugin config** (`~/.config/nab/plugins.toml`):

```toml
# Existing binary plugin format (unchanged)
[[plugins]]
name = "my-provider"
binary = "/usr/local/bin/nab-plugin-example"
patterns = ["example\\.com/.*"]

# NEW: CSS extractor plugin (no binary needed)
[[plugins]]
name = "internal-wiki"
type = "css"
patterns = ["wiki\\.internal\\.corp/.*"]

[plugins.content]
selector = "div.wiki-content"
remove = ["div.sidebar", "nav.breadcrumbs"]

[plugins.metadata]
title = "h1.page-title"
```

**New module**: `src/site/css_extractor.rs` (~120 LOC)

1. **Config loading** -- Extend `plugin::config::load_plugins()` to parse entries with `type = "css"`. Entries without `type` default to `"binary"` (backward compatible). Return empty vec for CSS entries if none defined.

2. **CssExtractorProvider** -- A struct implementing `SiteProvider`. `matches()` checks URL against compiled regex patterns. `extract()` receives **already-fetched HTML bytes** as input — it does NOT re-fetch the URL. The `SiteProvider` trait's `extract` method must be extended to accept an optional `&[u8]` response body. When present, the provider uses it instead of fetching. This eliminates the double-fetch latency and duplicate server hits.

3. **Content pipeline** -- After CSS selector extraction and element removal, the resulting HTML fragment is passed through `ContentRouter::convert_with_url` (not `html2md::parse_html` directly). This ensures the full content pipeline runs: readability, SPA data extraction, and all other processing. Using `html2md` directly would bypass nab's entire content pipeline and produce inferior output.

4. **Router integration** -- `SiteRouter::new()` appends CSS extractor providers after the built-in providers. Built-in providers always take precedence.

**Existing infrastructure used**:
- `plugin::config` — extended, not duplicated
- `site::SiteProvider` trait — CssExtractorProvider implements this
- `scraper::Selector::parse` and `scraper::Html` — already dependencies
- `ContentRouter::convert_with_url` — full content pipeline, not raw html2md

**Config format**: TOML, extending existing `plugins.toml`. No new config file. No new dependency.

### Degenerate Inputs

| Input | Behavior |
|-------|----------|
| No `type = "css"` entries in plugins.toml | No CSS extractors loaded. Binary plugins unaffected. |
| Invalid CSS selector in entry | Log warning for that entry. Skip it. Other entries still load. |
| Regex pattern that matches everything (`.*`) | Allowed but user's responsibility. Built-in providers checked first. |
| `content.selector` matches zero elements | Return empty content with metadata header. Not an error. |
| 100 CSS extractor entries | All loaded. Linear scan is fine for <1000 patterns. |
| Entry has `type = "css"` but no `content.selector` | Log warning, skip entry. |

### Acceptance Criteria

- AC1: User adds a `type = "css"` entry to `~/.config/nab/plugins.toml`; `nab fetch` uses it for matching URLs.
- AC2: Built-in providers take precedence over CSS extractors for the same URL.
- AC3: Invalid CSS entries log a warning at startup but do not affect binary plugin entries or crash the server.
- AC4: Invalid CSS selectors in an extractor log a warning and skip that extractor.
- AC5: An extractor with `content.remove` strips the specified elements before markdown conversion.
- AC6: Existing binary plugins continue to work unchanged (backward compatible).
- AC7: Metadata fields are optional; missing selectors result in `None` metadata values.
- AC8: CSS extractors receive already-fetched HTML; they do NOT re-fetch the URL.

### Estimated LOC

| Component | LOC |
|-----------|-----|
| `src/site/css_extractor.rs` (CssExtractorProvider, config parsing) | ~120 |
| `src/plugin/config.rs` (extend for `type = "css"` entries) | ~20 |
| `src/site/mod.rs` (SiteRouter integration) | ~15 |
| Tests | ~80 |
| **Total** | **~235** |


## Feature Interaction Matrix

All new `fetch` parameters can be combined freely. The processing pipeline order is:

```
fetch URL (using session client if session is set)
  → HTTP response (or site provider output — BOTH enter this pipeline)
  → convert_body_async (HTML→markdown)
  → apply_diff (if diff=true)                  ← diff on FULL content, markers tagged
  → focus::extract_focused (if focus)          ← reduces by relevance, diff markers exempt
  → budget::truncate_to_budget (if max_tokens) ← reduces by size, heading cap 30%
  → link_extract + concurrent fetch (if prefetch_links) ← fan-out, 3/host limit
    → each linked page: convert → focus → budget
  → build response (text + structured_content)
```

### Critical interaction: `diff` + `focus`

When both `diff=true` and `focus="query"` are set:
1. **Snapshot stores full content keyed by URL only.** The snapshot store always saves the full page content. This ensures a stable diff baseline regardless of whether or how the focus query changes between calls.
2. **Diff computes on full content, then focus filters the diff output.** The pipeline is: full markdown → diff against previous full snapshot → focus filter on the diff output. Diff markers (`[changed]`, `[added]`, `[removed]`) are **exempt from BM25 scoring** — they are always kept in the output regardless of their score. This prevents the filtering from discarding the very information the user asked for with `diff=true`.
3. The `has_diff` flag reflects whether the full page changed, not whether the focused subset changed. This is correct: if the page changed but only in sections the focus query doesn't match, the LLM should still know the page was updated (it can then broaden its focus query).
4. When `focus` is absent and `diff` is true: current behavior unchanged (diff on full content).
5. **Design alternative rejected**: Keying snapshots by `(url, focus_query)` was considered but rejected because every new focus query resets the diff baseline, making diff useless for page-monitoring use cases where the LLM varies its focus query.

### Critical interaction: `prefetch_links` + `max_tokens`

When both are set, the token budget is split in two phases:
- **Phase 1 (pre-fetch)**: Main page budget = `budget / 2`. Linked page pool = `budget / 2`.
- **Phase 2 (post-fetch)**: After linked pages return, the linked pool is divided among the pages that actually succeeded. If 2 of 3 linked fetches fail, the 2 survivors each get `pool / 2` instead of `pool / 3`. Main page surplus (if main used less than `budget / 2`) is added to the linked pool before division.
- This two-phase approach avoids penalizing the main page for linked page failures, and avoids wasting budget on pages that never arrive.

### Full combination example

```json
{
  "url": "https://docs.example.com/api/",
  "focus": "authentication",
  "max_tokens": 4000,
  "prefetch_links": 3,
  "diff": true,
  "session": "docs-research"
}
```

Processing: session client → fetch → diff (full, markers tagged) → focus (markers exempt from scoring) → budget (2000 main) → extract links → fetch 3 linked (3/host limit) → focus each → budget (surplus-aware split among survivors) → build response.

**No interaction**: `session` and `cookies` are pre-fetch (client selection). All other params are post-fetch (content processing). Orthogonal by design.


## Feature Dependencies

- **Feature 4 depends on 2 + 3**: Uses focus scoring for link relevance ranking and budget enforcement for token splitting. Can ship with simpler heuristics if needed, but full design requires both.
- **Features 2 and 3**: Independent but complementary. Focus filters by relevance, budget by size. Focus runs first when both set.
- **Features 5 and 6**: Fully independent of all other features and each other.


## Implementation Order

### Phase 0: Pipeline Unification (prerequisite)

Refactor `FetchTool::run()` so site provider output enters the same post-processing pipeline as standard fetches. Currently providers return early at `tools.rs:109-131`, bypassing all new features. Also extend `SiteProvider::extract()` signature with `prefetched_html: Option<&[u8]>` (mechanical update to 12 providers).

**Deliverables**:
- Refactored `FetchTool::run()` (~30 LOC)
- Updated `SiteProvider` trait + 11 provider signatures (~15 LOC)

### Phase 1: Token Economy (Features 2 + 3)

Implement Features 2 and 3 together. They are independent, small, and deliver the highest ROI. Both modify `FetchTool::run()` in the same code path, so implementing them together avoids duplicate refactoring.

**Deliverables**:
- `src/content/focus.rs`
- `src/content/budget.rs`
- Updated `FetchTool` with `focus` and `max_tokens` parameters
- Updated `fetch_output_schema()` with new fields

**Estimated time**: 1-2 days.

### Phase 2: Multi-page Intelligence (Feature 4)

Implement Prefetch Link Graph after Phase 1. It depends on both focus scoring and budget enforcement for its full design.

**Deliverables**:
- `src/content/link_extract.rs`
- Updated `FetchTool` with `prefetch_links` parameter
- Updated output schema with `linked_pages` array
- Concurrent fetch integration

**Estimated time**: 2-3 days.

### Phase 3: Session State (Feature 5)

Implement Persistent Sessions. No dependencies on other features. Can be done in parallel with Phase 2.

**Deliverables**:
- `src/session.rs`
- Updated `FetchTool`, `SubmitTool`, `LoginTool` with `session` parameter

**Estimated time**: 1 day.

### Phase 4: Extensibility (Feature 6)

Implement Site Extraction Registry. Lowest priority -- most users will not need custom extractors. Can be done at any time.

**Deliverables**:
- `src/site/css_extractor.rs`
- Extended `src/plugin/config.rs` for `type = "css"` entries
- Updated `SiteRouter::new()` to load CSS extractor providers
- Updated `plugins.toml` documentation in README

**Estimated time**: 1-2 days.


## Breaking Changes & Migration

### SiteProvider trait change (Feature 6, affects all phases)

Feature 6 requires extending `SiteProvider::extract()` to accept optional pre-fetched HTML:

```rust
// Current signature
async fn extract(&self, url: &str, client: &AcceleratedClient, cookies: Option<&str>) -> Result<SiteContent>;

// New signature
async fn extract(&self, url: &str, client: &AcceleratedClient, cookies: Option<&str>, prefetched_html: Option<&[u8]>) -> Result<SiteContent>;
```

**Impact**: All 11 built-in providers must add `_prefetched_html: Option<&[u8]>` to their signature and ignore it. This is a mechanical change (~1 line per provider, ~15 LOC total). Only `CssExtractorProvider` actually uses the parameter.

**Timing**: This trait change should land in Phase 1 as a preparatory refactor (before Features 2+3), even though Feature 6 ships in Phase 4. Reason: the site provider early-return refactor (making provider output enter the post-processing pipeline) is a Phase 1 prerequisite anyway.

### Site provider pipeline refactor (cross-cutting, Phase 1 prerequisite)

The current `FetchTool::run()` returns early at `tools.rs:109-131` when a site provider matches, bypassing all post-processing (focus, budget, diff, prefetch). This must be refactored so provider output merges into the main pipeline. Estimated: ~30 LOC change in `FetchTool::run()`.

This refactor is **blocking for Features 2, 3, and 4**. Without it, `focus`, `max_tokens`, and `prefetch_links` silently do nothing for GitHub, Twitter, YouTube, and 8 other provider-matched URLs.


## Rollback Strategy

All new features are parameter-gated with backward-compatible defaults:

| Feature | Parameter | Default | Rollback |
|---------|-----------|---------|----------|
| 2 | `focus` | absent (full content) | Don't pass `focus` |
| 3 | `max_tokens` | absent (unlimited) | Don't pass `max_tokens` |
| 4 | `prefetch_links` | 0 (disabled) | Don't pass `prefetch_links` |
| 5 | `session` | absent (global client) | Don't pass `session` |
| 6 | `type = "css"` | absent (binary plugin) | Remove entry from `plugins.toml` |

**No feature flags needed.** Absent parameters = identical behavior to v0.4.0.

**Version strategy**: Ship as **v0.5.0** (minor bump — new features, no breaking API changes for MCP clients). The `SiteProvider` trait change is internal (not part of the public library API). Each phase can be a separate commit but ships as one release.

**Emergency rollback**: If a feature causes production issues, the MCP client (Claude Code, gateway) can stop passing the problematic parameter. No nab binary rollback needed unless the pipeline refactor introduces a regression — in that case, `git revert` the pipeline refactor commit and rebuild.


## Documentation Update Plan

Each phase must update the following before merging:

| Document | Updates Required |
|----------|-----------------|
| `README.md` → MCP Server section | Add `focus`, `max_tokens`, `prefetch_links`, `session` to fetch tool parameter table |
| `README.md` → Features section | Add "Query-focused extraction", "Token budget", "Link graph prefetch", "Persistent sessions" |
| `CHANGELOG.md` | One "Added" entry per feature |
| `src/bin/mcp_server/main.rs` | Update `fetch_output_schema()` with new optional fields |
| `src/bin/mcp_server/tools.rs` | Update `#[mcp_tool]` description strings for `fetch`, `submit`, `login` |
| `~/.config/nab/plugins.toml` docs | Document `type = "css"` extractor format (Phase 4 only) |


## Testing Strategy

### Unit Tests (per feature, in-module)

Each feature module (`focus.rs`, `budget.rs`, `link_extract.rs`, `session.rs`, `registry.rs`) includes unit tests covering:
- Happy path with representative inputs
- All degenerate inputs from the tables above
- Edge cases (empty input, single-element input, maximum-size input)

### Integration Tests (MCP protocol compliance)

In `src/bin/mcp_server/tests.rs`:
- **Schema conformance**: Verify that `structured_content` produced by each new parameter combination validates against the declared `outputSchema`. Use `serde_json::from_value` with the schema type to ensure no field mismatches.
- **Parameter combinations**: Test the full interaction matrix (diff + focus, focus + budget, all-params-combined).
- **Backward compatibility**: Every existing test continues to pass unchanged. No new parameters alter default behavior.

### Benchmark Tests

In `benches/`:
- **Focus scoring**: BM25 on 100/500/1000 sections × 1-10 query terms. Target: <5ms for 500 sections.
- **Budget truncation**: Structure-aware truncation on 10KB/100KB/1MB documents. Target: <2ms for 200KB.
- **Link extraction**: Regex matching + URL resolution on documents with 10/100/1000 links. Target: <1ms for 100 links.

### Manual Validation

Verify qualitative output against: Python docs, Rust std docs, MDN (focus); Wikipedia, RFCs (budget); API index pages (prefetch); GitHub login (sessions); internal wiki (registry).


## Observability

Every `fetch` call with new parameters emits a `tracing::info!` span with:

```
nab_mcp.fetch{url_host, has_focus, max_tokens, prefetch_links, has_session, diff}
  full_tokens: usize,       // estimated tokens of full content
  returned_tokens: usize,   // estimated tokens of returned content
  omitted_sections: usize,  // sections removed by focus filter
  truncated: bool,           // whether budget truncation occurred
  linked_fetched: usize,    // linked pages successfully fetched
  linked_failed: usize,     // linked pages that errored/timed out
  processing_ms: f64,       // post-fetch processing time (focus + budget + diff)
```

Enables: token savings validation (`1 - returned/full`), threshold tuning, prefetch hit rate monitoring, latency budget enforcement. Uses existing `tracing` crate.

**Privacy**: The span logs `url_host` (hostname only, not full URL) and `has_focus` (boolean, not the query text). Full URLs and focus queries are NOT logged — they may contain confidential business context, internal hostnames, or authentication tokens in query parameters. Session names are also NOT logged (they may reveal workflow intent).


## Non-Goals / Out of Scope

The following are explicitly not part of this design:

1. **Semantic embedding-based focus** -- Feature 2 uses BM25-lite keyword scoring, not vector embeddings. Embedding models would add a large dependency and latency. BM25 is sufficient for keyword-based section filtering.

2. **Cross-process session persistence** -- Feature 5 sessions live in memory for the `nab-mcp` process lifetime. Persisting to `~/.nab/sessions/` is a future extension flagged in the code (`login.rs:SESSION_DIR`) but not implemented here.

3. **JavaScript rendering for linked pages** -- Feature 4 fetches linked pages via HTTP only. SPA-rendered linked pages will get the same thin-content treatment as the main page. This is acceptable because documentation sites (the primary use case) are typically server-rendered.

4. **Global rate limiting for prefetch** -- Feature 4 enforces per-host concurrency (3 concurrent requests per host via semaphore) but does NOT implement cross-call rate limiting (e.g., "max 10 requests/second to any host"). Per-host concurrency prevents WAF triggers within a single call; cross-call rate limiting is deferred.

5. **Streaming/incremental delivery** -- All features return complete results. While MCP 2025-11-25 supports task-augmented execution (which nab uses for `fetch_batch`), streaming partial content within a single tool response is not supported. Prefetched linked pages are returned as a batch, not incrementally.

6. **LLM-in-the-loop extraction** -- Feature 6 uses CSS selectors for extraction, not LLM prompts. Sending page content to an LLM for extraction would create circular dependencies and unpredictable latency.

7. **Registry hot-reload** -- Feature 6 loads extractors at startup. Changes to `extractors.toml` require restarting `nab-mcp`. File watching and hot-reload are deferred.

8. **Output schema versioning** -- Adding `omitted_sections`, `truncated`, `full_tokens`, and `linked_pages` to `structured_content` is additive. MCP's `outputSchema` is advisory (clients SHOULD NOT reject extra fields). However, `fetch_output_schema()` in `main.rs` must be updated to declare the new optional fields so strict-validating clients can accept them. No separate versioning mechanism is needed for v1 — all new fields are optional with sensible defaults (absent = not applicable).

9. **Token counting accuracy** -- Feature 3 uses 4 chars/token as a heuristic. Exact tokenization (e.g., via tiktoken) would require a tokenizer dependency. The heuristic is within 25% for English prose, overestimates for code (~33%), and underestimates for CJK (~60%). Since this is an upper-bound budget, overestimation is safe. CJK underestimation is a known limitation accepted for v1 (see Feature 3 implementation notes).
