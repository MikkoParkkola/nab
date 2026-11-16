# App-wide state stores do not replace server-rendered pages

MIK-8684 (Flipboard markdown case). Status: design, for review before code.

## Problem

`nab fetch` on the Flipboard collection page `https://flipboard.com/@news/how-tech-companies-are-shaping-global-politics-thh095uc0gle39a0` returns one line: "TODAY, TODAY Show, TODAY.com, top news stories, …". The page's HTML (2.47 MB) holds 34 server-rendered `<article>` cards with 35 `<h3>` headlines and 134 links, plus a 2.27 MB `window.__PRELOADED_STATE__` script. The line is the `keywords` field of one profile entry inside that state (byte 301,088).

Path, read at origin/main 6fdde661f:

1. `spa_extract::spa::inline::extract_inline_script_json_with_source` matches `window.__PRELOADED_STATE__` among its named patterns and returns `find_longest_string(state, 200)` labelled `SpaSource::Framework`.
2. `html::html_to_markdown_with_url_and_sources` returns a `Framework` candidate immediately unless `is_thin_content(html_len, candidate_len)`. `is_thin_content` is false for any text of 800 characters or more, and the keywords string is about 1,600, so the HTML is never extracted. No "html selection" debug line is logged on this fetch, which confirms the early return.

The longest string in an app-wide store is arbitrary: a Redux or Apollo store holds every entity the app loaded (profiles, sections, ads, navigation), and its longest string is whichever field happens to be longest. `__NEXT_DATA__` and `__NUXT__` are different: they carry the current page's own props.

## Design

D1. The named patterns split in two. Page data, still `SpaSource::Framework` (trusted to beat page chrome): `window.__NEXT_DATA__`, `self.__NEXT_DATA__`, `__NEXT_DATA__`, `window.__NUXT__`, `window.___GATSBY`. App-wide stores, now `SpaSource::GenericAssignment` (used only to fill a thin HTML result): `window.__INITIAL_STATE__`, `window.__PRELOADED_STATE__`, `window.__APOLLO_STATE__`, `window.__APP_STATE__`, `window.__STORE_STATE__`, `window.__DATA__`.

D2. Order is unchanged: a page-data match anywhere on the page still outranks everything; a store match now ranks with the generic assignments, and the longest store-or-generic hit is the candidate, as `GenericAssignment` already does.

D3. Nothing else changes. `merge_spa_candidate` still lets a `GenericAssignment` candidate replace the HTML result when the pick is the direct conversion, readability is not preferred, the result is under 800 characters and the candidate is longer. An SPA shell that renders only from a store therefore keeps working: the existing test `html_to_markdown_keeps_spa_result_that_carries_article_body` (an `__APOLLO_STATE__` body beside `<div id="root">Loading</div>`) must still pass. Flipboard's 34 cards extract to well over 800 characters, so they stay.

Out of scope: rendering a store's item lists (titles and links) as markdown; listing-page extraction quality beyond not being replaced by a store string; MIK-9504 (readability dropping sections).

## Tests, written failing first

| ID | Test | Fails today because |
|---|---|---|
| T1 | HTML with 12 `<article>` cards (heading, link, two-sentence summary) and a `window.__PRELOADED_STATE__` whose longest string is a 1,600-character keyword list: the output contains the card headlines and not the keyword list | the store string returns early |
| T2 | The same with `window.__INITIAL_STATE__` and `window.__APOLLO_STATE__` | same |
| T3 | An SPA shell (`<div id="root"></div>`, no text) with a `window.__PRELOADED_STATE__` holding a 1,200-character article body: the output is that body | guards D3; passes today |
| T4 | A page with `__NEXT_DATA__` whose page props hold an article body, beside navigation chrome: the output is the body | guards D1's page-data branch; passes today |
| T5 | The source label: `extract_inline_script_json_with_source` returns `Framework` for `__NEXT_DATA__` and `GenericAssignment` for `__PRELOADED_STATE__` | label is `Framework` for both today |

Live check before merge, recorded in the PR: the Flipboard collection page's markdown contains several of its article headlines and not the keyword list.

Sabotage: put `__PRELOADED_STATE__` back among page data (T1, T5 fail); return early for `GenericAssignment` too (T1 fails); drop the thin fill (T3 fails).
