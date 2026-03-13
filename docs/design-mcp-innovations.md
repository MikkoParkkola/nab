# Design: nab MCP Feature Innovations

**Status**: Proposed
**Author**: Mikko Parkkola
**Date**: 2026-03-13
**nab version**: 0.4.0

## Executive Summary

nab's MCP server (`nab-mcp`) currently exposes 8 tools: `fetch`, `fetch_batch`, `submit`, `login`, `auth_lookup`, `fingerprint`, `validate`, and `benchmark`. These tools fetch URLs and convert content to markdown for LLM consumption, but they treat every response as an opaque blob -- the full content is always returned regardless of what the LLM actually needs.

This document specifies 5 features that make nab the first MCP server designed for token economy. Together they reduce per-call token consumption by 10-50x for common patterns, eliminate sequential round-trips, and extend nab from a stateless fetcher into a session-aware web automation layer.

| # | Feature | Token Savings | Effort | Priority |
|---|---------|--------------|--------|----------|
| 2 | Query-Focused Extraction | 10-50x on large pages | ~150 LOC | P0 |
| 3 | Token Budget Enforcement | Prevents context blowouts | ~100 LOC | P0 |
| 4 | Prefetch Link Graph | 3-5x faster research | ~300 LOC | P1 |
| 5 | Persistent Sessions | Enables multi-step auth flows | ~150 LOC | P1 |
| 6 | Site Extraction Registry | User-extensible providers | ~200 LOC | P2 |

Total estimated effort: ~900 LOC of library code, ~200 LOC of MCP wiring.


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

2. **Scoring** -- BM25-lite scoring against the focus query. Tokenize query into lowercase words, compute term frequency per section, apply inverse document frequency across all sections. No external crate needed; the algorithm is ~40 lines. Formula: `score = sum(tf(t,s) * idf(t) for t in query_terms)` where `tf = count / section_len` and `idf = log(N / df)`.

3. **Filtering** -- Keep sections scoring above the median score, or at minimum the top 3 sections. Always keep the first section (page title / intro). Consecutive omitted sections are collapsed into a single `[... N sections omitted ...]` marker.

**Integration point**: `FetchTool::run()` in `src/bin/mcp_server/tools.rs`. After `convert_body_async()` returns the full markdown, call `focus::extract_focused(markdown, query)` to produce the filtered output. The filtered text replaces the full markdown in both the text content and `structured_content.content`.

**Existing infrastructure used**:
- `content::readability::extract_article` already strips boilerplate before this step
- `content::html::html_to_markdown_with_url` produces heading-structured markdown
- `ContentRouter::convert_with_url` is the entry point unchanged

### Acceptance Criteria

- AC1: `fetch(url, focus="authentication")` on a 100KB docs page returns less than 10KB.
- AC2: All returned sections contain at least one query term or are structurally necessary (intro section).
- AC3: Omitted-section markers show accurate counts.
- AC4: When `focus` is absent, behavior is identical to current (no regression).
- AC5: When the page has no headings, full content is returned (graceful degradation).

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
   - Priority 1: All headings (`## `, `### `, etc.)
   - Priority 2: First paragraph after each heading
   - Priority 3: Code blocks (fenced with triple backticks)
   - Priority 4: Remaining paragraphs

3. **Budget allocation** -- Walk blocks in priority order, accumulating estimated tokens. Stop when budget is reached. Within the same priority level, preserve document order.

**Integration point**: `FetchTool::run()` in `src/bin/mcp_server/tools.rs`. After `convert_body_async()` (and after `focus::extract_focused` if `focus` is set), call `budget::truncate_to_budget(markdown, max_tokens)`. This returns the truncated markdown and metadata about what was cut.

**Interaction with Feature 2**: When both `focus` and `max_tokens` are set, focus filtering runs first (reduces to relevant sections), then budget truncation runs on the filtered output. This is the correct order: filter by relevance, then by size.

**Existing infrastructure used**:
- `structured::truncate_markdown` already does character-level truncation (currently used with 4000 char limit). This feature replaces it with a structure-aware version.
- `build_fetch_structured` already builds the structured_content map; add `truncated` and `full_tokens` fields.

### Acceptance Criteria

- AC1: `fetch(url, max_tokens=1000)` returns content estimating to 1000 tokens or fewer.
- AC2: Truncated output preserves all headings from the original document.
- AC3: Truncated output includes the first paragraph of each section before including second paragraphs of any section.
- AC4: Code blocks are preserved whole (not split mid-block).
- AC5: When content fits within budget, output is unchanged.
- AC6: Truncation footer shows accurate original and truncated token estimates.

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
- When combined with `max_tokens`, the budget is split: 40% for the main page, 60% divided among linked pages.

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

3. **Concurrent fetch** -- Reuse the existing `AcceleratedClient` and `SafeFetchConfig` for each linked page. Spawn all N fetches concurrently via `futures::future::join_all`, matching the pattern already used in `FetchBatchTool::run()`.

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

### Acceptance Criteria

- AC1: `fetch(url, prefetch_links=3)` returns the main page plus up to 3 linked pages.
- AC2: Linked pages are fetched concurrently (total time is approximately max(individual times), not sum).
- AC3: Relative URLs in links are resolved correctly against the main page URL.
- AC4: SSRF protection applies to all linked page fetches (no fetching localhost, private IPs).
- AC5: When `focus` is set, linked pages are scored by relevance to the focus query.
- AC6: When a linked page fetch fails, it is omitted from results (not an error for the whole call).
- AC7: `prefetch_links` is capped at 10 to prevent abuse.

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

1. **Session store** -- `SessionStore` wraps a `HashMap<String, Arc<reqwest::cookie::Jar>>` behind a `tokio::sync::RwLock`. Global singleton accessed via `OnceCell`, matching the pattern used for `CLIENT` in `tools.rs`.

2. **Session-aware client** -- `SessionStore::get_or_create(name)` returns an `Arc<Jar>`. The jar is injected into a session-specific `reqwest::Client` built with `ClientBuilder::cookie_provider(jar)`. This means session cookies are isolated from the global client and from other sessions.

3. **Cookie seeding** -- When `cookies` is provided alongside `session`, `resolve_cookie_header` extracts browser cookies as a string, which is parsed into `Set-Cookie` entries and injected into the session's `Jar`.

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

### Acceptance Criteria

- AC1: `login(url, session="s1")` followed by `fetch(url, session="s1")` uses the authenticated cookies.
- AC2: Two concurrent sessions with different names have independent cookie jars.
- AC3: A session created with `cookies="brave"` includes browser cookies on first use.
- AC4: Omitting the `session` parameter gives current behavior (no regression).
- AC5: Session names are validated (alphanumeric + hyphens only, max 64 chars).
- AC6: The session store is thread-safe under concurrent MCP calls.

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

nab has 11 hardcoded site providers in `src/site/` (Twitter, Reddit, HackerNews, GitHub, Google Workspace, Instagram, YouTube, Wikipedia, StackOverflow, Mastodon, LinkedIn). Adding a new provider requires writing Rust code, rebuilding, and redeploying. Users who need custom extraction for internal sites or niche platforms cannot extend nab without forking.

### Solution

A YAML-based extraction registry at `~/.config/nab/extractors.yaml` lets users define custom site extractors using CSS selectors and content rules. These extractors are loaded at startup and participate in the `SiteRouter` dispatch alongside built-in providers.

### API

**Configuration file**: `~/.config/nab/extractors.yaml`

```yaml
extractors:
  - name: internal-wiki
    patterns:
      - "wiki.internal.corp/.*"
    content:
      selector: "div.wiki-content"
      remove:
        - "div.sidebar"
        - "nav.breadcrumbs"
        - "div.page-actions"
    metadata:
      title: "h1.page-title"
      author: "span.last-editor"
      published: "time.last-modified"

  - name: substack
    patterns:
      - ".*\\.substack\\.com/p/.*"
    content:
      selector: "div.body"
      remove:
        - "div.subscription-widget"
        - "div.footer-wrap"
    metadata:
      title: "h1.post-title"
      author: "a.frontend-pencraft-Text-module"
```

**Schema**:

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

**New module**: `src/site/registry.rs` (~150 LOC)

1. **Config loading** -- Parse `~/.config/nab/extractors.yaml` at startup using `serde_yaml` (or `serde_yml`). Follow the same pattern as `plugin::config::load_plugins()` which loads `~/.config/nab/plugins.toml`. Return empty vec if file doesn't exist.

2. **RegistryProvider** -- A struct implementing `SiteProvider` that is parameterized by the YAML config. `matches()` checks URL against compiled regex patterns. `extract()`:
   a. Fetch the URL using the `AcceleratedClient`.
   b. Parse HTML with `scraper::Html::parse_document`.
   c. Select the content container via `content.selector`.
   d. Remove elements matching `content.remove` selectors.
   e. Convert remaining HTML to markdown via `html2md::parse_html`.
   f. Extract metadata fields by their CSS selectors.

3. **Router integration** -- `SiteRouter::new()` calls `registry::load_extractors()` and appends the resulting `Vec<Box<dyn SiteProvider>>` after the built-in providers. Built-in providers always take precedence (they are checked first).

**Existing infrastructure used**:
- `site::SiteProvider` trait -- RegistryProvider implements this exactly
- `site::SiteRouter` -- append registry providers to the `providers` vec
- `plugin::config` pattern -- same config-loading convention (`~/.config/nab/`)
- `scraper::Selector::parse` and `scraper::Html` -- already dependencies, used throughout `content/`
- `html2md::parse_html` -- already used by `content::html`
- `content::readability::is_unlikely_candidate` -- filtering logic can be reused

**New dependency**: `serde_yml` (or reuse `toml` format for consistency with `plugins.toml`). If using TOML instead of YAML, the config format changes slightly but semantics are identical.

### Acceptance Criteria

- AC1: User creates `~/.config/nab/extractors.yaml` with a valid extractor; `nab fetch` uses it for matching URLs.
- AC2: Built-in providers take precedence over registry providers for the same URL.
- AC3: Invalid YAML logs a warning at startup but does not crash the server.
- AC4: Invalid CSS selectors in an extractor log a warning and skip that extractor.
- AC5: An extractor with `content.remove` strips the specified elements before markdown conversion.
- AC6: Missing config file results in no registry providers (no error).
- AC7: Metadata fields are optional; missing selectors result in `None` metadata values.

### Estimated LOC

| Component | LOC |
|-----------|-----|
| `src/site/registry.rs` (config types, RegistryProvider, loader) | ~150 |
| `src/site/mod.rs` (SiteRouter integration) | ~15 |
| Tests | ~80 |
| **Total** | **~245** |


## Feature Dependencies

```
Feature 2 (Focus) -----> Feature 4 (Prefetch)
                    |         uses focus scoring for link relevance
                    |
Feature 3 (Budget) -----> Feature 4 (Prefetch)
                              uses budget splitting for linked pages

Feature 5 (Sessions) ---> standalone, no deps

Feature 6 (Registry) ---> standalone, no deps
```

- **Feature 4 depends on Features 2 and 3**: Prefetch Link Graph uses focus scoring (Feature 2) to rank extracted links by relevance, and uses budget enforcement (Feature 3) to split token budgets between the main page and linked pages. Feature 4 can be built without these dependencies by using simpler heuristics, but the full design requires them.
- **Features 2 and 3 are independent** of each other but complement each other. Focus filtering reduces by relevance; budget enforcement reduces by size. When both are set, focus runs first.
- **Features 5 and 6 are fully independent** of all other features and of each other.


## Implementation Order

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
- `src/site/registry.rs`
- Updated `SiteRouter::new()` to load registry providers
- Documentation for `extractors.yaml` format

**Estimated time**: 1-2 days.


## Non-Goals / Out of Scope

The following are explicitly not part of this design:

1. **Semantic embedding-based focus** -- Feature 2 uses BM25-lite keyword scoring, not vector embeddings. Embedding models would add a large dependency and latency. BM25 is sufficient for keyword-based section filtering.

2. **Cross-process session persistence** -- Feature 5 sessions live in memory for the `nab-mcp` process lifetime. Persisting to `~/.nab/sessions/` is a future extension flagged in the code (`login.rs:SESSION_DIR`) but not implemented here.

3. **JavaScript rendering for linked pages** -- Feature 4 fetches linked pages via HTTP only. SPA-rendered linked pages will get the same thin-content treatment as the main page. This is acceptable because documentation sites (the primary use case) are typically server-rendered.

4. **Rate limiting for prefetch** -- Feature 4 does not throttle concurrent fetches to the same host. HTTP/2 multiplexing handles this at the protocol level. If abuse becomes an issue, per-host concurrency limits can be added later.

5. **Streaming/incremental delivery** -- All features return complete results. MCP does not support streaming tool responses (as of protocol 2025-11-25), so streaming is not possible.

6. **LLM-in-the-loop extraction** -- Feature 6 uses CSS selectors for extraction, not LLM prompts. Sending page content to an LLM for extraction would create circular dependencies and unpredictable latency.

7. **Registry hot-reload** -- Feature 6 loads extractors at startup. Changes to `extractors.yaml` require restarting `nab-mcp`. File watching and hot-reload are deferred.

8. **Token counting accuracy** -- Feature 3 uses 4 chars/token as a heuristic. Exact tokenization (e.g., via tiktoken) would require a tokenizer dependency. The heuristic is within 20% for English text, which is sufficient for budget enforcement.
