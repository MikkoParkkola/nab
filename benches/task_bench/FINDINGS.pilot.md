# bench.1 pilot findings — 2026-06-07

First live head-to-head of `nab task` (host-driven) vs a real browser (headless
Chrome via CDP), 6 public API-backed tasks. The pilot's job (per the operator's
"pilot first" choice) was to **de-risk the methodology before the full 20** — and
it did, by surfacing two measurement bugs and two product/corpus levers. Raw data:
`results/pilot-raw-2026-06-07/`.

## Verdict (raw, n=6)

```
task               nab_tok  br_tok  tok_x   nab_ms  br_ms  lat_x
crates-io          105347     708   0.0x     1603   4363   2.7x
gh-stars             2530     800   0.3x     1433   5429   3.8x
hn-item             10408    7240   0.7x     2008   4976   2.5x
npm-version           797      66*  0.1x     4861   4468   0.9x
stackoverflow-q      2808      67*  0.0x     2825   4447   1.6x
wikipedia-summary     624   18434  29.5x     1289   4702   3.6x
MEDIAN               2669     754   0.3x     1806   4585   2.5x
* invalid browser capture (SPA/bot-wall shell — see Bug 1)

LATENCY: nab WINS  (median 1806ms vs 4585ms, 2.5x; faster on every task)
TOKENS : nab LOSES (median 2669 vs 754) -> bench.1 FAIL (both axes required)
```

## What is real

- **Latency is a clean, decisive nab win.** Every task, ~2.5x median. HTTP+API
  beats browser launch + render. This axis holds.
- **Wikipedia is the moat in one line:** nab 624 tok vs browser 18434 (29.5x).
  When nab's readability pipeline shapes a heavy page, the token win is enormous.

## Bug 1 — browser baseline under-renders SPA / bot-walled pages

npm (66 tok) and StackOverflow (67 tok) returned near-empty shells even at a 4s
hydration wait: npmjs.com hydrates client-side and SO bot-walls headless Chrome.
These captures are **invalid** (the answer never rendered) yet scored low, biasing
the token axis *against* nab. A real browser agent (non-headless, the user's
session) would ingest far more on these pages. Fix: wait for network-idle + assert
the answer string is actually present before accepting a browser capture; treat
bot-walls as browser failures, not cheap wins.

## Lever 1 (product) — `api_call` returns RAW, unshaped responses

The biggest token sink. `crates-io` (105347 tok) and `hn-item` (10408) are nab
handing the brain the **entire** API response — every serde version, the full HN
comment tree — with no shaping. In the autonomous loop the brain *reads* each
observation, so this raw dump **is** the token cost (the `extract` rung shapes
what is carried forward, not what was already ingested).

nab's product promise is *token-minimal* web access. The `api_call` path currently
violates it: nab's own content pipeline (`focus`, `budget::truncate_to_budget`)
and the schema's existing `extract_query` field are **not applied to API
responses**. Applying them is implementing the thesis the bench just proved is
necessary — not gaming the metric. **This is the fix that decides the token axis.**

## Lever 2 (corpus) — single-fact public lookups are the worst case for nab

The pilot tasks ("how many stars", "latest version") have the answer inline on the
rendered page — the *best* case for read-the-page, the *worst* case for shaped
markdown + a separate API call (nab pays for a seed README it does not need, then
a verbose API object, to surface one number a browser reads in situ). The design's
intended corpus is **auth-gated, multi-step, data-heavy** tasks (the moat: reach +
extraction across pages a browser must click through and a fresh browser cannot
even authenticate into). The full-20 corpus must reflect that, or the bench tests
the wrong thing.

## Consequence for phasing

- bench.1 is **NOT passed**. nab wins latency, loses tokens on this corpus.
- Before the full 20: (a) fix Bug 1 (valid browser renders), (b) ship Lever 1
  (shape `api_call` responses via focus + budget), (c) rebuild the corpus per
  Lever 2 (auth-gated / multi-step / data-heavy).
- Per design §7, rung 3 (browser orchestration) is gated behind bench.1 passing.
  The honest status: rung 3 is *buildable* (Chrome+CDP available) but not yet
  *justified by the gate*. Build the token-axis fix and a fair corpus first; if
  nab then wins both axes, rung 3 earns its place.

## What the pilot delivered

A working two-sided harness (`run_nab.sh`, `browser_run.py`, `score.py`), a real
head-to-head on both axes, and a precise map of what must change before the
full-20 run. That is a successful pilot: it spent 6 tasks to avoid spending 20 on
the wrong methodology.

## Post-shaping re-run (Lever 1 implemented)

`api_call` responses now pass through `shape_api_response` (focus by
`extract_query` + a hard `API_RESPONSE_TOKEN_BUDGET` cap). The verbose-API
outliers collapsed:

```
task          raw_nab_tok -> shaped_nab_tok   browser_tok
crates-io        105347   ->    4032             708
hn-item           10408   ->    4096            7240
(others unchanged; all under the cap)
MEDIAN nab tokens 2669 (unchanged)   |  latency median 1624ms vs 4585ms (2.8x WIN)
```

The cap did exactly what it should — no more 105k-token dumps — and latency
widened to a 2.8x median win (up to 9x on gh-stars/wikipedia). But the **token
median did not move**: it is set by `gh-stars` (2530) and `stackoverflow` (2808),
neither an outlier. So the shaping fix is a real product improvement, **not** a
gate-flipper — confirming the token verdict is driven by the two issues above
(invalid browser captures + single-fact corpus), not by raw-API dumping.

## Honest verdict

- **Latency: nab wins decisively** (2.8x median, every task). Holds.
- **Tokens: inconclusive on this pilot.** 2 of 6 browser captures invalid
  (npm/SO shells under-count the browser); the corpus is single-fact lookups
  (nab's worst case); on the one heavy page both rendered validly (Wikipedia) nab
  wins 29.5x. Not a clean pass, not a clean fail — the methodology must be fixed
  before the number means anything.
- bench.1 is **NOT passed**, and will not be until: valid browser renders
  (network-idle + answer-present assertion; treat bot-walls as browser failures)
  and a moat-representative corpus (auth-gated / multi-step / data-heavy, not
  trivial public single-fact lookups).

