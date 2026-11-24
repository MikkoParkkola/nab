# Fetch fidelity: binary `-o` saves and wrong-block extraction (MIK-9520, MIK-8684, MIK-9504)

Status: revision 4, 2026-11-15, after two rounds of independent design review (findings folded in). Scope: `nab fetch` CLI main path. **Implemented so far: Part 2 only** (branch `fix/mik-8684-extraction`). Part 1 (binary saves) and Part 3 (link-dense lists) are not yet shipped.

## Problem

Agents read every URL through `nab fetch`. Three defects make it return the wrong thing while reporting success:

1. **MIK-9520, binary `-o`.** `nab fetch URL -o FILE` cannot save a non-text response. A 13,504-byte PNG is saved as 21 bytes of placeholder text (`[Image: PNG 544×184]`), or, with `--raw-html`, as 24,189 bytes in which every byte invalid as UTF-8 became U+FFFD. Cause: the body is decoded with `String::from_utf8_lossy` (`src/cmd/fetch.rs:396`) and every file write takes `&str` (`src/cmd/output.rs:35`, `src/cmd/fetch.rs:1180`).
2. **MIK-8684, wrong block wins.** Reproduced 2026-11-15 on nab 0.12.6 (= `origin/main` 65b1dca) on 5 of 8 saved pages (Flipboard is a YARA false positive, out of scope).
3. **MIK-9504, dropped sections.** Part 3, separate piece.

Out of scope: YARA policy for text output; the cookie default (MIK-9169); MCP-server and batch fetch paths; Flipboard (YARA false positive, tracked with the MIK-8975 family); JS-rendered pages that need a browser; streaming saves larger than the 10 MiB body cap.

## Part 1: binary `-o` (MIK-9520)

### Destination intent

The final extension of the `-o` path, compared ASCII-case-insensitively:

- **Text destination:** no extension, or one of `md`, `markdown`, `txt`, `html`, `htm`, `json`, `xml`, `csv`. Behaviour is exactly today's, for every response type and status.
- **File destination:** any other extension (`png`, `pdf`, `mp4`, `zip`, …).

### Body class

Tested on the response (lowercased MIME, parameters stripped):

1. HTML/XHTML, `text/*`, JSON, XML, JavaScript, or a `+json`/`+xml` suffix other than `image/svg+xml` → **text**.
2. `application/octet-stream` (also what `http_client.rs:494` substitutes for a missing header) → **text** if, after an optional UTF-8 BOM and ASCII whitespace, the body starts with `<`, or the body is valid UTF-8; otherwise **binary**.
3. Anything else (`application/pdf`, `image/*` including SVG, `audio/*`, `video/*`, archives) → **binary**.

### Rule for a file destination

Site providers and media auto-transcription (both run before the request, `fetch.rs:266-325`) are skipped, so the real response is classified. Then:

| Response | Result |
|---|---|
| 2xx, binary | bytes written unchanged |
| 2xx, text | error `response is <mime>, not a file; use a .md name to save its text`; no file created |
| non-2xx | error naming the status; no file created; status lines, `--show-headers` and the JSON object still print |
| body length reaches `DEFAULT_MAX_BODY_SIZE` | error naming the 10 MiB cap; no file created (a file of exactly the cap size is refused too; accepted) |
| YARA match on the bytes | error naming the rules; no file created, under both redact and refuse policies, since bytes cannot be redacted; audited as for text; `NAB_YARA_BYPASS=1` bypasses as today |

`--raw-html` (new alias `--raw`) with a text destination keeps today's raw-text behaviour.

### Output contract

One helper writes the file from bytes, before anything is printed, for all three formats. Full and Compact print their usual lines plus `💾 Saved N bytes (unchanged, <mime>) to FILE`. JSON prints the usual object with `"markdown": null`, `metadata.title: null`, `"saved_to"` and `"saved_bytes"`. Markdown conversion, OCR, the hebb URL cache and thin-content diagnostics are skipped for bytes. Nothing binary is printed to stdout.

### Tests (fail before, pass after)

`tests/cli_fetch.rs` local proxy helper, generalised to send bytes. For a PNG containing bytes 0x89 and 0xFF: `x.png` is byte-identical (sha256) in Full, Compact and JSON formats; `x.md`, `X.MD` and an extensionless name get today's placeholder text; `x.cmd` is a file destination. PDF: `x.pdf` byte-identical, `x.md` extracted text. HTML 200 to `x.pdf`: error, no file. 404 PNG to `x.png`: error, no file, nonzero exit. Missing Content-Type: binary body saved, HTML body to `x.png` refused. YARA match (test rule via the existing test hook) to `x.png`: error, no file. Unit tests for the destination and body-class functions.

## Part 2: extraction picks the wrong block (MIK-8684)

### Evidence

Stage-by-stage diagnostic on the saved pages (byte lengths):

| Page | SPA candidate (source) | readability | direct | returned today | right answer |
|---|---|---|---|---|---|
| engineering.fb.com | 1,101 WordPress query JSON (generic assignment) | 12,913 | 32,839 | SPA | readability |
| salesforce.com | 998 author bio (generic assignment) | 22,169 | 38,901 | SPA | readability |
| thenerve.news (home) | 24,651 font CSS (generic assignment) | 459 | 25,037 | SPA | direct |
| threadreaderapp.com | none | 187 (2022 teaser) | 7,067 | teaser | direct |
| gradium.ai | none | 1,462 (image credits) | 24,505 | credits | direct |

All three bad SPA candidates come from the catch-all scraper `extract_content_from_generic_inline_assignments` (`spa/inline.rs:84`), which takes the longest string from any `= {…}` assignment in any script. The named framework assignments in the same file (`window.__NEXT_DATA__`, `__NUXT__`, …) and every other extractor did not misfire.

### Cause A fix: the catch-all scraper only fills an empty result

`extract_spa_data` gains a sibling that reports the source as an enum: `Framework` (every extractor and the named-pattern assignments, unchanged) or `GenericAssignment` (the catch-all only). In `extract_inline_script_json`, named patterns are tried across all scripts before the catch-all runs on any script.

In `html_to_markdown_with_url_and_sources`:

- `Framework` candidates: unchanged, including the early return.
- `GenericAssignment` candidates: never early-return. They replace the chosen HTML result only when it is the whole-page conversion (no article was picked), `prefer_readability` is false, the result is under 800 bytes, and the candidate is longer than it. An article an extractor picked stays, however short.

### Cause B fix: a tiny generic-readability pick on a large page yields to the full conversion

`Article` gains a source enum (`Substack`, `WordPress`, `Generic`; `Generic` covers the readability crate, semantic `<article>`/`<main>` and text-density extraction). The direct conversion is returned instead of readability when all hold: `prefer_readability` is false; the article source is `Generic` (site-specific extractors are trusted); the page is not Next.js metadata-only (`is_nextjs_metadata_only`, so the existing chunk recovery still triggers); readability is under 2,000 bytes and under 10% of the direct conversion.

The thin-content warning and opt-in Jina fallback are evaluated on the readability result when the fallback applies, so their signal is not masked, unless an SPA candidate then replaced the result, in which case they are evaluated on that candidate. The chosen source and candidate lengths are logged at debug level.

### Tests

Committed, synthetic (no third-party text):

- Cause A, HTML fixtures: an article page with a `var settings = {...}` holding a 1,000-byte bio string must return the article; the same with a CSS string; a JS-shell page (body under 800 bytes) whose only content is a generic assignment must return it; a page with `window.__NEXT_DATA__ = {...}` article and 2,500 bytes of navigation must return the article.
- Cause B, injected candidates (the selection step takes readability and direct results as inputs after a small refactor), one test per evidence row at the measured lengths, plus: `--readability` and a token budget keep readability; a `Substack`-source result of 1,500 bytes on a heavy page stays; a Next.js metadata-only page is exempt.

Local before merge (third-party pages, not committed): the 5 broken pages must return the right answer; holdouts must keep their source and length within ±5%: a2ui (readability, about 11,300), substack (Substack extractor, about 21,500), lesswrong (about 35,000), simonwillison.net short post (about 550 bytes of content). No Ghost or Next.js HTML holdout could be obtained; the synthetic Next.js fixture stands in.

### Risks

- Thresholds come from 5 broken and 4 healthy pages; logged sources and lengths allow later measurement.
- A page whose only real content is in a generic assignment but whose HTML has 800+ bytes of chrome now returns the chrome. Accepted; previously such pages handed bios and CSS to other sites' readers.

## Part 3: link-dense lists dropped (MIK-9504)

The "Get started" heading survives readability but its 3-item `<ul>` (every item a link) is removed, consistent with readability's link-density cleanup. Separate piece after Parts 1-2.
