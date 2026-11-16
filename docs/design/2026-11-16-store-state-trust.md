# App-wide state stores do not replace server-rendered pages

MIK-8684 (Flipboard markdown case). Status: design, for review before code.

## Problem

`nab fetch` on the Flipboard collection page `https://flipboard.com/@news/how-tech-companies-are-shaping-global-politics-thh095uc0gle39a0` returns one line: "TODAY, TODAY Show, TODAY.com, top news stories, …". The page's HTML (2.47 MB) holds 34 server-rendered `<article>` cards with 35 `<h3>` headlines and 134 links, plus a 2.27 MB `window.__PRELOADED_STATE__` script. The line is the `keywords` field of one profile entry inside that state (byte 301,088).

Path, read at origin/main 6fdde661f:

1. `spa_extract::spa::inline::extract_inline_script_json_with_source` matches `window.__PRELOADED_STATE__` among its named patterns and returns `find_longest_string(state, 200)` labelled `SpaSource::Framework`.
2. `html::html_to_markdown_with_url_and_sources` returns a `Framework` candidate immediately unless `is_thin_content(html_len, candidate_len)`. `is_thin_content` is false for any text of 800 characters or more, and the keywords string is about 1,600, so the HTML is never extracted. No "html selection" debug line is logged on this fetch, which confirms the early return.

The longest string in an app-wide store is arbitrary: a Redux or Apollo store holds every entity the app loaded (profiles, sections, ads, navigation), and its longest string is whichever field happens to be longest. `__NEXT_DATA__` is different: it carries the current page's own props.

## Design

D1. Only the current page's own props stay trusted. Page data, still `SpaSource::Framework` (allowed to beat page chrome): `window.__NEXT_DATA__`, `self.__NEXT_DATA__`, `__NEXT_DATA__`. Everything else becomes a store, labelled `SpaSource::GenericAssignment` (used only to fill a thin HTML result): `window.__INITIAL_STATE__`, `window.__PRELOADED_STATE__`, `window.__APOLLO_STATE__`, `window.__APP_STATE__`, `window.__STORE_STATE__`, `window.__DATA__`, and also:
- `window.__NUXT__`: a JSON `__NUXT__` carries the Vuex `state` beside the page `data`, and `find_longest_string` walks both, so its longest string can be store content exactly as on Flipboard.
- `window.___GATSBY`: the loader bootstrap, not the page envelope. Gatsby page data is read from `data-gatsby-ssr` tags by `extract_gatsby_data` (`spa/gatsby.rs:16`), which this change does not touch.

D2. Three ordered passes over all inline scripts, so script order cannot decide trust:
1. Page data: the first `__NEXT_DATA__` assignment that yields text is returned as `Framework`.
2. Stores: otherwise the first store assignment that yields text, labelled `GenericAssignment`. This is today's first-match order with a new label, so a store still outranks any generic hit, and a multi-megabyte store is never rescanned at each `=` inside its strings.
3. Generic: otherwise the longest generic `= {...}` hit on the page, as today. It runs whenever no store yielded text, including when a store assignment is present but unparseable or under 200 characters.

A named pattern counts only as an assignment: the name, optional whitespace, then `=`. Today `script_text.find(pattern)` followed by the next `=` anywhere later accepts a store string that merely mentions `__NEXT_DATA__`, which would keep the Flipboard failure under a page-data label.

Audit of the other `Framework` producers in `extract_spa_candidate` (`spa_extract/mod.rs:74`): `script#__NEXT_DATA__`, `script#__NUXT_DATA__`/`#__nuxt-data`, SvelteKit, Gatsby `data-gatsby-ssr`, Angular transfer state, JSON-LD and hidden `<code>` JSON. Each reads its own script tag or element; none reads a `window.` store assignment, so none can carry a demoted store.

D3. `merge_spa_candidate` is unchanged: a `GenericAssignment` candidate still replaces the HTML result when the pick is the direct conversion, readability is not preferred, the result is under 800 characters and the candidate is longer. A shell that renders only from a store keeps working (`html_to_markdown_keeps_spa_result_that_carries_article_body`, an `__APOLLO_STATE__` body beside `<div id="root">Loading</div>`).

Accepted loss: a page whose article text lives only in a store now loses it when the HTML result is 800 or more characters of chrome, or when readability picked a short server-rendered teaser (a readability pick is never filled). No such page is known; the live check below looks for one, and finding one stops the merge.

Out of scope: rendering a store's item lists as markdown; listing-page quality beyond not being replaced by a store string; restricting the `__NEXT_DATA__` longest-string fallback to `props.pageProps`, which can still return app-wide state embedded in the Next.js envelope (follow-up ticket); MIK-9504 (readability dropping sections).

## Tests, written failing first

| ID | Test | Fails today because |
|---|---|---|
| T1 | 12 `<article>` cards (heading, link, two-sentence summary; at least 800 characters of markdown) beside a `window.__PRELOADED_STATE__` whose longest string is a 1,600-character keyword list built from a sentinel word that appears in no card: the page without the script converts to at least 800 characters (baseline), and with it the output has the card headlines and no sentinel | the store string returns early |
| T2 | T1's page once per store pattern (all eight in D1) | same |
| T3 | Shell (`<div id="root"></div>`, no text) with a `window.__PRELOADED_STATE__` holding a 1,200-character body: output is that body | guards D3; passes today only through the `Framework` early return, so it guards the thin fill once the label changes |
| T4 | Existing `named_next_data_assignment_still_beats_page_chrome` (60 navigation links, body only in `__NEXT_DATA__`), kept as is | guards D1's trusted branch; passes today, fails if `__NEXT_DATA__` is demoted |
| T5 | Source label for all eleven patterns: `Framework` for the three `__NEXT_DATA__` forms, `GenericAssignment` for the eight stores | stores are `Framework` today |
| T6 | A `__PRELOADED_STATE__` script before a `window.__NEXT_DATA__` script, each holding a different long body: result is the `__NEXT_DATA__` body, labelled `Framework` | today the first script wins with the store body |
| T7 | Shell with a `__PRELOADED_STATE__` script holding a 1,200-character body and another script with a longer generic `= {...}` string, in both script orders: output is the store body | guards pass order 2 before 3; passes today |
| T8 | Shell whose only script assigns `window.__PRELOADED_STATE__` with a short (under 200 characters) store, then a generic `= {...}` with a 1,200-character body: output is the generic body | guards pass 3 running after a store that yields nothing; passes today |
| T9 | 12-card page plus a store whose string mentions `__NEXT_DATA__` and later holds `= {"k":"<1,600-character string>"}`: output has the card headlines and not that string | today the unanchored match labels it `Framework` |

Before/after check, recorded in the PR: old and new binary convert the same saved copies of real pages, served from localhost, so network variance and live-site drift drop out.
- Flipboard collection page (`__PRELOADED_STATE__`, 2.47 MB): markdown contains several card headlines and not the keyword list. Conversion time for both binaries is recorded, without a threshold: the new path runs the HTML extraction the old early return skipped, and pass 2 keeps the generic scan off store scripts exactly as today.
- One Flipboard article page and the Medium article behind the `__APOLLO_STATE__` fixtures: new output keeps the article body.
- Counter-sample, pages carrying a demoted pattern: Bilibili and Xiaohongshu (`__INITIAL_STATE__`), Medium (`__PRELOADED_STATE__`, `__APOLLO_STATE__`), nuxtjs.org and Juejin (`__NUXT__`). Any page whose main text disappears stops the merge and reopens D1.

Sabotage: put `__PRELOADED_STATE__` back among page data (T1, T5 fail); return early for `GenericAssignment` too (T1, T2 fail); drop the thin fill (T3 fails); collapse the passes into one first-match loop (T6 fails); pool stores with generic hits by length (T7 fails); skip pass 3 whenever a store name is present (T8 fails); drop the assignment anchor (T9 fails).
