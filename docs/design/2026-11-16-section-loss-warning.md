# Say when extraction empties a section

MIK-9504. Status: design revision 4; revisions 1 to 3 reviewed by Grok and Muse.

## Problem

On the Google A2UI 1.0 post, `nab fetch` keeps the "Get started" heading but drops its four bullet points, and drops all five image captions. Nothing tells the reader content is missing.

Path, read at main 0d0d240 with a stage probe on the saved page:

| Stage | Length | List | Captions |
|---|---|---|---|
| HTML after nab's noise stripping | 51,969 | kept | kept |
| direct conversion of that HTML | 20,122 | kept | kept |
| `readability` crate 0.3 (`extract_with_readability_crate`) | 11,362 | dropped | dropped |
| nab's scraper fallback | 387 | dropped | dropped |
| final output (readability picked) | 11,301 | dropped | dropped |

The crate's conditional cleaning removes the `<ul>` (each item is mostly links, so it reads as navigation) and the `div.image-wrapper` caption blocks. That is the ticket's fail-fast condition: the readability step classifies the block as boilerplate, so the fix is a section-loss warning, not a rewrite.

Revision 1 proposed restoring content between readability's first and last anchor lines from the direct conversion. Both reviewers rejected it, and the probe confirms the main reason: on Medium five lines readability kept fall outside the matched span and would be lost, mid-article chrome (a subscribe box) would come back, and trimmed-line splicing breaks code blocks and tables.

## Design

D1. One pure function, `emptied_sections(cleaned_html: &str, markdown: &str) -> Vec<String>` in `src/content/html.rs`. It reports a heading that the markdown keeps with nothing under it, while the source had text under it.

Source side, on `cleaned_html`:
- Headings are `h1` to `h6`. A heading's segment is the leaf blocks after it in document order, up to the next heading of any level, so a kept subsection cannot hide its parent's loss.
- The segment stops where the heading's nearest `article`, `main`, `section` or `body` ends, and skips `aside`, `nav`, `footer` and `form` subtrees, so trailing boilerplate cannot make an intentionally empty heading look emptied.
- Leaf blocks are the innermost elements among `p`, `li`, `dt`, `dd`, `th`, `td`, `caption`, `figcaption`, `blockquote`, `pre`, `summary`, plus a `div` holding text directly; an element containing another leaf block is not counted itself, so `li > p` counts once.
- The segment has text when its leaves hold 20 or more characters in total.

Markdown side, on the final markdown:
- Heading lines are ATX (`## Get started`, a closing run of `#` stripped) and nab's converter's setext form: a text line followed, after optional blank lines, by exactly `==========` (`h1`) or `----------` (`h2`). A `---` line is the converter's horizontal rule: neither a heading nor text. Lines inside fenced code are never headings.
- A heading's text is compared after stripping hashes, link markup (keeping the label), emphasis markers and control characters, then collapsing whitespace.
- A heading's interval is the lines after it up to the next markdown heading or the end. It holds text when any line in it is not blank, not a setext underline and not a horizontal rule; an image line counts as text (something was kept).

Pairing: source headings are matched to markdown headings as a monotonic subsequence. Each source heading searches forward from the last matched markdown heading; unmatched headings on either side are skipped without moving the other side.

Reported: a matched heading whose markdown interval holds no text while its source segment has text. The reported text is the normalized heading with control characters removed and cut to 80 characters.

D2. Placement: computed once, on the final markdown after SPA merge and Jina recovery, inside `html_to_markdown_with_url_and_sources`, and only when that markdown came from `cleaned_html` (readability or direct pick, no SPA replacement, no Jina recovery); otherwise empty, and the source is never parsed. Next.js chunk recovery, which replaces the body after conversion (`recover_nextjs_content_chunks` in the CLI, `recover_nextjs_chunks` in MCP), clears the list. The early SPA return yields an empty list. A new private entry point returns `(String, Vec<String>)` and is threaded through `html_to_markdown_with_url_and_fetcher` and `html_to_markdown_sanitized_with_url_and_options` to `HtmlHandler::to_markdown_with_url_and_options`; the public `String`-returning functions keep their signatures. `ConversionResult` gains `emptied_sections: Vec<String>`, empty for every non-HTML handler.

D3. Surfaces, through one formatter `emptied_sections_warning(&[String]) -> Option<String>` (first three headings, quoted, then a count):
- `nab fetch` prints it through the existing `⚠️` diagnostics with `; --raw-html has the full page` appended.
- The MCP fetch tool adds it to its text output without the CLI flag, and `emptied_sections` to its structured output, carried from the conversion result through `FetchDiagnosticMetadata`.

D4. Nothing else changes: no output rewrite, no new option, the response classifier untouched.

Out of scope, recorded on the ticket: partial loss (captions dropped from a section that keeps a paragraph, as on the A2UI figures, cannot be told apart from removed in-article ads without matching body text); restoring dropped blocks; why the scraper fallback returned 387 characters.

## Tests, written failing first

Markdown inputs in E1 to E6 come from nab's own converter (`html_to_markdown`), so they carry its real heading and list spelling.

| ID | Test | Fails today because |
|---|---|---|
| E1 | Source `<h2>Get started</h2><ul>` of four items, then `<h2>Next</h2><p>…</p>`; markdown converted from the same HTML with the `ul` removed: returns `["Get started"]` | function absent |
| E2 | Same source, markdown converted with the list kept: returns nothing | function absent |
| E3 | Markdown converted with the heading and list both removed: returns nothing | function absent |
| E4 | Removed segment of 19 characters in total: nothing; of exactly 20: the heading | function absent |
| E5 | Markdown heading written as a link, `## [Get started](/x)`, list removed: returns `["Get started"]` | function absent |
| E6 | Parent `h2` with its own list removed and a kept `h3` subsection: returns the `h2` only; parent `h2` with under 20 characters of its own text and a kept `h3`: returns nothing | function absent |
| E7 | Pairing: an extra source-only heading before the article and an extra markdown-only title heading, list removed under "Get started": returns `["Get started"]` | function absent |
| E8 | Boundaries: a kept empty heading at the end of `<article>` followed by footer text, and a fenced code block holding `## Not a heading` under a kept heading: returns nothing | function absent |
| E9 | Silence on unchanged conversions: the existing Substack fixture in `readability.rs` and a page with a kept table, each through the full pipeline: nothing | field absent |
| E10 | Pipeline: a controlled drop (readability forced, `ul` of link-only items under a heading) puts the heading on `ConversionResult.emptied_sections`; a shell page whose store body (holding the same heading with nothing under it) replaces the output yields nothing; the same with the injected Jina fetcher supplying the markdown yields nothing | field absent |
| E11 | Surfaces: the formatter's wording and cap; `build_fetch_diagnostics` and the MCP text and structured output carry it for a non-empty list and nothing for an empty one; a heading holding an escape character is printed without it | no such line |

Characterization, not a pass condition: the saved A2UI page through the pipeline names "Get started". If readability starts keeping the list, this flips and is a prompt to revisit, not a regression.

Before/after on saved pages (A2UI, Medium, nuxtjs.org, Flipboard, Bilibili, Xiaohongshu): the line appears on A2UI only, and markdown is byte-identical to main on all six.

Sabotage: drop the "heading kept" condition (E3 fails); compare the floor with `>` instead of `>=` (E4 fails); compare headings without stripping link markup (E5 fails); end source segments at the next equal-or-higher heading (E6 fails); advance both sides on a mismatch (E7 fails); let segments cross the article boundary (E8 fails); treat fenced lines as headings (E8 fails); compute before the SPA merge (E10 fails); skip the Jina check (E10 fails); drop the control-character strip (E11 fails).
