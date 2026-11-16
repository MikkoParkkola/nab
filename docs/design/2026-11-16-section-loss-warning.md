# Say when extraction empties a section

MIK-9504. Status: design revision 3, for review before code.

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

D1. One pure function, `emptied_sections(cleaned_html: &str, markdown: &str) -> Vec<String>` in `src/content/html.rs`. It reports a heading that the markdown keeps with nothing under it, while the source had text under it:
- Source side: walk `cleaned_html` in document order. Headings are `h1` to `h6`. A heading's own segment is the leaf text blocks (`p`, `li`, `figcaption`, `blockquote`, `pre`, `td`, `dd`) after it up to the next heading of any level, so a kept subsection cannot hide its parent's loss. The segment counts as having text when its leaves hold 20 or more characters in total.
- Markdown side: heading lines are ATX (`## Get started`) and setext (a line followed by `---` or `===`). A heading's text is compared after stripping the hashes, link markup (keeping the label), emphasis markers and surrounding whitespace, then collapsing inner whitespace. Source headings are matched to markdown headings in order, so a repeated heading text pairs occurrence by occurrence.
- Reported: a matched heading whose markdown interval (the lines after it up to the next markdown heading or the end) holds no text, while its source segment has text.

No body text is matched, so list bullets, table pipes, code fences and wrapped lines cannot cause a false alarm. A heading that disappears with its section is not reported: removing a whole "Related posts" block is intended. Partial loss, where some text survives under the heading, is not reported either (see Out of scope).

D2. Placement: computed inside the HTML pipeline, beside the pick in `html_to_markdown_with_url_and_sources`, on the same `cleaned_html` readability and the direct conversion read and on the markdown before security screening, focus and the token budget. It is skipped, returning nothing, when the final markdown did not come from that HTML: an SPA candidate replaced it or Jina recovery supplied it. The private pipeline function returns the headings beside the markdown; the public `String`-returning functions are unchanged. `ConversionResult` gains `emptied_sections: Vec<String>`, empty for every non-HTML handler.

D3. Surfaces, one line each:
- `nab fetch` adds a diagnostic through the existing `⚠️` channel: `extraction dropped the content under "Get started"; --raw-html has the full page` (more than three headings: the first three and a count).
- The MCP fetch tool adds `⚠️ Extraction dropped the content under "Get started".` to its text output, without the CLI-only flag, and `emptied_sections` to its structured output, carried through `FetchDiagnosticMetadata` from the conversion result.

D4. Nothing else changes: no output rewrite, no new option, the response classifier untouched.

Out of scope, recorded on the ticket: partial loss (captions dropped from a section that keeps a paragraph, as on the A2UI figures, cannot be told apart from removed in-article ads without matching body text); restoring dropped blocks; why the scraper fallback returned 387 characters.

## Tests, written failing first

Markdown inputs in E1 to E5 come from nab's own converter (`html_to_markdown`), so they carry its real heading and list spelling.

| ID | Test | Fails today because |
|---|---|---|
| E1 | Source `<h2>Get started</h2><ul>` of four items, then `<h2>Next</h2><p>…</p>`; markdown converted from the same HTML with the `ul` removed: returns `["Get started"]` | function absent |
| E2 | Same source, markdown converted with the list kept: returns nothing | function absent |
| E3 | Markdown converted with the heading and list both removed: returns nothing | function absent |
| E4 | Segment of 19 characters in total, removed from the markdown: returns nothing | function absent |
| E5 | Markdown heading written as a link, `## [Get started](/x)`, with the list removed: returns `["Get started"]` | function absent |
| E6 | Parent `h2` whose own list is removed and whose `h3` subsection is kept: returns the `h2` only | function absent |
| E7 | Silence on unchanged conversions: the existing Substack fixture in `readability.rs` and a page with a kept table, each through the full pipeline: returns nothing | function absent |
| E8 | A controlled drop through the full pipeline (readability forced, `ul` of link-only items under a heading): `ConversionResult.emptied_sections` names the heading; and a shell page whose store body replaces the output returns nothing | field absent |
| E9 | `build_fetch_diagnostics` and the MCP text both carry the line when the vector is non-empty, and nothing when it is empty | no such line |

Characterization, not a pass condition: the saved A2UI page through the pipeline names "Get started". If readability starts keeping the list, this flips and is a prompt to revisit, not a regression.

Before/after on saved pages (A2UI, Medium, nuxtjs.org, Flipboard, Bilibili, Xiaohongshu): the line appears on A2UI only, and markdown is byte-identical to main on all six.

Sabotage: drop the "heading kept" condition (E3 fails); drop the 20-character floor (E4 fails); compare headings without stripping link markup (E5 fails); end segments at the next equal-or-higher heading instead of any heading (E6 fails); return headings when an SPA candidate replaced the output (a pipeline case in E8 fails).
