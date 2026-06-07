# bench.1 v2 — fair-corpus verdict (2026-06-07)

The methodology-fixed, fair-corpus run of the kill-gate. Continues
`FINDINGS.pilot.md`. **The kill-gate fired: nab fails the token axis.** This is the
honest result the gate exists to produce — reported straight, not tuned away.

## What changed from the pilot

1. **Browser baseline is now validity-checked** (`browser_run.py`): poll innerText
   until it settles (SPA hydration), then a render only counts if it exceeds
   500 chars, contains no bot-wall markers, and contains the task's expected
   answer string. Bot-walls and shells are browser **failures**, excluded from the
   medians (reported, never hidden). This removes the pilot's invalid captures.
2. **nab tokens are loop-accurate** (`run_nab.sh`): the autonomous loop (the mode
   the kill-gate tests, §9.1) feeds the brain at most 4000 chars of seed via
   `build_prompt`, not the full host-driven `TaskOutcome.content`. We measure what
   the loop ingests. (The host-driven full seed being unbounded is a separate
   product gap — see "Two bugs the bench found".)
3. **Corpus is moat-shaped** (`corpus.v2.json`): data-heavy / multi-item tasks
   (list the releases / dependencies / sections / top stories), not trivial
   single-fact lookups.

## Verdict (combined v1+v2, valid captures, n=11)

```
LATENCY: nab WINS — median 1510ms vs 4468ms (3.0x). Robust across both corpora.
TOKENS : nab LOSES — median 2959 vs 1064. bench.1 FAIL (both axes required).
```

Per-task token ratio (browser_tok / nab_tok; >1 = nab cheaper):

```
nab WINS tokens (content-heavy pages):
  wikipedia-summary   29.5x     wikipedia-sections  6.2x
  hn-item              1.8x     gh-releases         1.6x
nab LOSES tokens (terse / structured pages):
  hn-topstories        0.2x     crates-tokio-deps   0.6x
  pypi-releases        0.3x     gh-stars            0.3x     crates-io 0.2x
```

## Why nab loses tokens — the precise root cause

**API JSON carries the full object per item; the rendered page shows only the
displayed fields.** For `hn-topstories`, the Algolia `front_page` response returns
title + url + author + points + num_comments + id + tags + timestamps for ~30
hits (capped at the 4000-token budget); the rendered HN front page shows terse
titles + points → 1064 tokens. For list/structured tasks the API is **more**
verbose than the page, and nab's byte-cap shaping cannot fix that.

The token win survives only where the rendered DOM is genuinely heavy (an article)
and the API is lean (Wikipedia: 18434 rendered vs 2959 API).

## The one capability that would flip the token axis

**JSON-aware field extraction on API responses** — return only the goal-relevant
fields, not the whole object graph. The `extract_query` field already exists in
the `ApiCall` schema but is wired to the markdown-section focus pipeline, which is
useless on flat JSON. A JSON-path / field-selecting extractor (driven by
`extract_query`, or by the brain naming the fields) would collapse the verbose-API
responses to the handful of values the task needs — turning the 0.2-0.6x losses
into wins. This is the missing moat piece the bench has now pinpointed.

### Status: BUILT + unit-proven; empirical re-run PENDING

`shape_api_response` now does JSON field projection: when `extract_query` names
fields and the response is JSON, `project_to_fields` prunes the tree to just the
subtrees leading to those fields (keeps `hits[].title`, drops url/author/points/
nbHits), then budget-caps. Unit-proven (`shape_api_response_projects_json_to_requested_fields`
and `_multi_field_and_nonjson_fallback`): title-only projection of a 2-hit
response keeps the titles, drops the noise, and shrinks the body >2x. The runner
now passes `answer_field` as `extract_query` (the brain naming what it wants).

**Honest caveat — not yet end-to-end confirmed.** The empirical re-run (does
projection flip the *median* token verdict?) is PENDING: the release rebuild was
blocked this session by a hostile build environment (a concurrent cross-project
cargo build contending the global lock + a temp-sweep repeatedly wedging cargo).
The capability is proven in isolation; whether it flips the gate depends on the
margin — nab still pays a ~1000-token seed cost (build_prompt cap), and for the
terse pages (HN front page ≈ 1064 browser tokens) the seed alone is close to the
browser total. So projection clearly helps a lot on the obs leg (4024 → ~300 for
HN titles) but may not, on its own, flip every terse-page task. The seed leg is
the second lever (bound the host-driven seed; bug #1 below). **Do not record
bench.1 as passed until the empirical re-run on a clean build confirms both axes.**


Until that ships, the honest positioning: **nab is a decisive LATENCY win and a
REACH win (auth-gated pages a fresh browser can't even load), and a TOKEN win on
content-heavy pages — but NOT a token win on terse structured pages.** The
"API-first is always token-minimal" framing does not survive contact with verbose
APIs.

## Two bugs the bench found (both real, both fixable)

1. **Unbounded host-driven seed.** `nab task <goal> <url>` (no `--action`, no
   sampling client) returns the FULL seed markdown — the HN front page came back
   as **1,093,161 tokens**. The autonomous loop caps it (build_prompt), but a
   no-sampling client gets the firehose. Fix: bound `TaskOutcome.content` to a
   seed budget in the seed path (mirrors the api_call shaping already shipped).
2. **No JSON field extraction** (above) — the token-axis blocker.

## Honest gate status

bench.1 is **FAILED on the token axis**, robustly, across two corpora with fixed
methodology. Per the design's own rule ("if it cannot beat a browser agent on the
API-backed subset … stop"), the disciplined response is to STOP tuning the bench
and make a strategic call:

- **Build JSON field extraction** (the named fix), then re-gate. Highest-leverage:
  it directly targets the proven root cause.
- **Reposition the claim**: nab `task` is a latency + reach + content-page-token
  play, not a universal token play. Honest, ships today, no new capability.

Either is legitimate. What is NOT legitimate is declaring bench.1 passed. It isn't.
