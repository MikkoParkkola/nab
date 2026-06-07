#!/usr/bin/env python3
"""bench.1 scorer — read per-task nab + browser results, compute the kill-gate.

Pass iff median(nab_tokens) < median(browser_tokens) AND
        median(nab_latency) < median(browser_latency).

Reads results/<id>.nab.json and results/<id>.browser.json. A task missing either
side is reported and excluded from the medians (never silently dropped).

Usage: ./score.py [results_dir]
"""
import json
import os
import statistics
import sys

RESULTS = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "results")


def load(side):
    out = {}
    for fn in os.listdir(RESULTS):
        if fn.endswith(f".{side}.json"):
            d = json.load(open(os.path.join(RESULTS, fn)))
            out[d["id"]] = d
    return out


def main():
    nab = load("nab")
    browser = load("browser")
    ids = sorted(set(nab) & set(browser))
    missing = sorted((set(nab) | set(browser)) - set(ids))

    print(f"{'task':<20} {'nab_tok':>9} {'br_tok':>9} {'tok_x':>6}  {'nab_ms':>8} {'br_ms':>8} {'lat_x':>6}")
    print("-" * 76)
    nt, bt, nl, bl = [], [], [], []
    for i in ids:
        n, b = nab[i], browser[i]
        ntok, btok = n["nab_tokens"], b["browser_tokens"]
        nlat, blat = n["nab_latency_ms"], b["browser_latency_ms"]
        nt.append(ntok); bt.append(btok); nl.append(nlat); bl.append(blat)
        tx = btok / ntok if ntok else float("inf")
        lx = blat / nlat if nlat else float("inf")
        print(f"{i:<20} {ntok:>9} {btok:>9} {tx:>5.1f}x  {nlat:>8} {blat:>8} {lx:>5.1f}x")

    if not ids:
        print("\nNo paired tasks. Run both run_nab.sh and the browser baseline first.")
        return 2

    mnt, mbt = statistics.median(nt), statistics.median(bt)
    mnl, mbl = statistics.median(nl), statistics.median(bl)
    print("-" * 76)
    print(f"{'MEDIAN':<20} {mnt:>9.0f} {mbt:>9.0f} {mbt/mnt:>5.1f}x  {mnl:>8.0f} {mbl:>8.0f} {mbl/mnl:>5.1f}x")

    tokens_win = mnt < mbt
    latency_win = mnl < mbl
    passed = tokens_win and latency_win
    print()
    print(f"  tokens : nab median {mnt:.0f} {'<' if tokens_win else '>='} browser {mbt:.0f}  -> {'WIN' if tokens_win else 'LOSS'}")
    print(f"  latency: nab median {mnl:.0f} {'<' if latency_win else '>='} browser {mbl:.0f}  -> {'WIN' if latency_win else 'LOSS'}")
    print()
    print(f"  bench.1 (n={len(ids)}): {'PASS' if passed else 'FAIL'} (both axes required)")
    if missing:
        print(f"  WARNING: unpaired tasks excluded from medians: {missing}")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
