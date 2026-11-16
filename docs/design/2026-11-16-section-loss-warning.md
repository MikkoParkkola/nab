# Say when extraction empties a section

MIK-9504. Status: design revision 2, for review before code.

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

D1. One pure function, `nab::content::html::emptied_sections(html: &str, markdown: &str) -> Vec<String>`, run on the source HTML after the pipeline's hidden-section and noise stripping. For each `h1` to `h4`, its section is the following sibling elements up to the next heading of the same or higher level. The heading is reported when all three hold:
- its text appears as a line of the markdown's plain text (links, emphasis and heading underlines removed);
- its section has at least one block with 20 or more characters of text;
- no such block's first 24 characters (whitespace collapsed) appear in the markdown's plain text.

A heading that disappears with its section is not reported: readability removing a whole "Related posts" block is intended.

D2. `nab fetch` adds one diagnostic line through `build_fetch_diagnostics`, printed like the existing ones: `⚠️  extraction dropped the content under 1 heading ("Get started"); --raw-html has the full page`. More than three headings are listed as the first three plus a count.

D3. The MCP fetch tool adds the same line to its text output, and an `emptied_sections` array to its structured output (empty when nothing was dropped).

D4. Nothing else changes: no output rewrite, no new option, the response classifier untouched.

Out of scope, recorded on the ticket: captions dropped from a section that keeps other text are not detected (partial loss cannot be told apart from removed in-article ads without a rewrite); restoring dropped blocks; why the scraper fallback returned 387 characters (`try_semantic_extraction` takes the first `article` or `main` over 100 characters).

## Tests, written failing first

| ID | Test | Fails today because |
|---|---|---|
| E1 | `emptied_sections` on HTML with `<h2>Get started</h2><ul>` of four items and markdown holding the heading but no item: returns `["Get started"]` | function absent |
| E2 | Same HTML, markdown holding the heading and the items: returns nothing | function absent |
| E3 | Markdown without the heading or its items: returns nothing | function absent |
| E4 | Section whose only block is under 20 characters: returns nothing | function absent |
| E5 | Heading text inside a link in the markdown (`## [Get started](/x)`) still counts as kept | function absent |
| E6 | `build_fetch_diagnostics` with the A2UI shape returns the warning line naming "Get started" | no such line |
| E7 | Trimmed A2UI fixture through the full HTML pipeline: the heading is kept, the list is absent, and `emptied_sections` names "Get started" | pins the shape; fails if readability starts keeping the list, which turns this into a prompt to revisit |

Before/after on saved pages (A2UI, Medium, nuxtjs.org, Flipboard, Bilibili): the warning fires on A2UI only; markdown is byte-identical to main on all five.

Sabotage: drop the "heading kept" condition (E3 fails); drop the 20-character floor (E4 fails); match on raw markdown instead of plain text (E5 fails).
