# Readability chooses the article's bounds, not its contents

MIK-9504. Status: design draft, not reviewed; the direction needs the operator's choice because the ticket's fail-fast asks for a warning instead.

## Problem

On the Google A2UI 1.0 post, `nab fetch` keeps the "Get started" heading but drops its four bullet points, and drops all five image captions ("Figure 1." to "Figure 4.", "A2UI Playground by Copilotkit"). Both sit inside the article.

Path, read at main 0d0d240 with a stage probe on the saved page:

| Stage | Length | List | Captions |
|---|---|---|---|
| HTML after nab's noise stripping | 51,969 | kept | kept |
| direct conversion of that HTML | 20,122 | kept | kept |
| `readability` crate 0.3 (`extract_with_readability_crate`) | 11,362 | dropped | dropped |
| nab's scraper fallback | 387 | dropped | dropped |
| final output (readability picked) | 11,301 | dropped | dropped |

The crate's conditional cleaning removes the `<ul>` (each item is mostly links, so its link density reads as navigation) and the `div.image-wrapper` caption blocks. The crate is a crates.io dependency, not vendored.

## Design

Readability is good at finding where the article starts and ends, and lossy inside it. So:

1. Run readability and the direct conversion as today.
2. Anchor lines: readability output lines of 40 or more characters that occur exactly once in the direct conversion. The first and last anchor mark the article's span in the direct conversion.
3. Output: readability's lines before the first anchor, then the direct conversion's lines from the first anchor to the last, then readability's lines after the last anchor.
4. No anchors, or anchors out of order: readability's output unchanged.

Edges stay trimmed: navigation, share bars and related posts sit outside the span. Only what readability deleted between two blocks it kept comes back.

## Evidence (probe, saved pages, not yet code)

| Page | Lines restored | What |
|---|---|---|
| Google A2UI | 16 | 4 list items, 5 images with captions, 2 pull quotes |
| Medium, Karpathy "Software 2.0" | 0 | unchanged |
| nuxtjs.org installation docs | 0 | unchanged |

## Open

- Readability lines inside the span that the direct conversion renders differently (2 on A2UI, 5 on Medium, 4 on nuxtjs) need a rule: point 3 keeps those before the first anchor and after the last; one inside the span would be replaced by the direct rendering of the same block.
- Risk: an ad or signup box between two article paragraphs comes back. nab's noise stripping runs first and removes the known patterns.
- Wider counter-sample before merge.
