# bench.1 — nab task-engine kill-gate harness

The go/no-go for the task engine (design `docs/design/2026-05-31-nab-task-engine.md`
§6, §6.1): **on an API-backed task subset, beat a live browser agent on median
tokens AND latency.** If nab cannot, it is just a worse browser agent — stop.

This directory is the harness. It is honest about what it measures: a real
head-to-head on both axes, with a browser baseline biased *in the browser's favor*
(see `browser_baseline.md`).

## Layout

| File | Role |
|---|---|
| `corpus.pilot.json` | 6 public API-backed tasks (heavy HTML page + clean JSON API — nab's moat case). Pilot before the full 20. |
| `run_nab.sh` | nab side: drives `nab task` host-driven (seed -> rung-1 api_call), times both legs, records tokens nab feeds the brain. Deterministic + reproducible. |
| `browser_baseline.md` | The live protocol for the browser side (webwright/Playwright, same LLM brain). |
| `score.py` | Reads both sides, computes median token + latency ratios, prints PASS/FAIL. Pairs by task id; reports (never hides) unpaired tasks. |
| `results/` | Per-task `<id>.nab.json` + `<id>.browser.json`, plus browser screenshots. |

## Run

```bash
# 1. nab side (needs a `--features task` build at ../../target/release/nab)
TMPDIR=/Users/mikko/nab-cc-tmp ./run_nab.sh corpus.pilot.json

# 2. browser side — follow browser_baseline.md live with the webwright skill,
#    writing results/<id>.browser.json per task.

# 3. score
./score.py
```

## Why the browser side is not in CI

The token-axis primitive in `nab::task::token_gap` is unit-tested and CI-green —
that proves the moat *mechanism*. The full kill-gate needs a live browser + an LLM
brain + (for auth tasks) real sessions, which is neither deterministic nor
CI-runnable. This harness is how the live gate is actually run and recorded; the
verdict is committed under `results/` as the evidence trail.

## Phasing

bench.1 must PASS before `nab task` graduates to the default MCP surface (design
§7) and before rung 3 (browser orchestration) is worth building. The pilot
de-risks the methodology on 6 tasks before scaling the corpus to 20.
