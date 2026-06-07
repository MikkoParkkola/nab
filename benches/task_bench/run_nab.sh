#!/usr/bin/env bash
# bench.1 — nab-side runner. For each corpus task: drive `nab task` host-driven
# (seed fetch + one rung-1 api_call), time both legs, record the tokens nab feeds
# the LLM brain (seed content + API observation) and whether the answer appeared.
#
# The brain (LLM operator) chooses the api_call URL between legs; here we script
# it from the recorded api_hint so the run is reproducible. Token estimate = the
# same 4-chars/token heuristic as nab::content::budget::estimate_tokens.
#
# Output: results/<id>.nab.json = { id, nab_tokens, seed_tokens, obs_tokens,
#         nab_latency_ms, answer_found }.
#
# Usage: TMPDIR=/Users/mikko/nab-cc-tmp ./run_nab.sh corpus.pilot.json
set -uo pipefail
cd "$(dirname "$0")"
CORPUS="${1:-corpus.pilot.json}"
NAB="${NAB:-../../target/release/nab}"
OUT=results
mkdir -p "$OUT"

python3 - "$CORPUS" "$NAB" "$OUT" <<'PY'
import json, subprocess, sys, time, re

corpus, nab, out = sys.argv[1], sys.argv[2], sys.argv[3]
tasks = json.load(open(corpus))["tasks"]

def toks(s): return (len(s) + 3) // 4

def run(args, timeout=45):
    t0 = time.time()
    try:
        p = subprocess.run([nab, "task", *args, "--json"],
                           capture_output=True, text=True, timeout=timeout)
        ms = int((time.time() - t0) * 1000)
        return p.stdout, ms, p.returncode
    except subprocess.TimeoutExpired:
        return "", int((time.time() - t0) * 1000), 124

for t in tasks:
    tid, goal, seed, api = t["id"], t["goal"], t["seed_url"], t["api_hint"]
    print(f"[nab] {tid}: {goal}")
    seed_out, seed_ms, rc1 = run([goal, seed])
    action = json.dumps({"kind": "api_call", "url": api, "method": "GET"})
    obs_out, obs_ms, rc2 = run([goal, seed, "--action", action])

    seed_content = obs_content = ""
    try: seed_content = json.loads(seed_out).get("content", "")
    except Exception: pass
    try: obs_content = json.loads(obs_out).get("content", "")
    except Exception: pass

    # answer heuristic: the recorded answer_field key(s) appear in the obs JSON.
    fields = [f for f in t.get("answer_field", "").split(",") if f]
    found = all(re.search(re.escape(f), obs_content) for f in fields) if fields else bool(obs_content)

    rec = {
        "id": tid,
        "nab_tokens": toks(seed_content) + toks(obs_content),
        "seed_tokens": toks(seed_content),
        "obs_tokens": toks(obs_content),
        "nab_latency_ms": seed_ms + obs_ms,
        "answer_found": bool(found),
        "rc": [rc1, rc2],
    }
    json.dump(rec, open(f"{out}/{tid}.nab.json", "w"), indent=2)
    print(f"  nab_tokens={rec['nab_tokens']} latency_ms={rec['nab_latency_ms']} "
          f"found={rec['answer_found']} (seed={rec['seed_tokens']} obs={rec['obs_tokens']} rc={rec['rc']})")

print(f"[nab] done -> {out}/*.nab.json")
PY
