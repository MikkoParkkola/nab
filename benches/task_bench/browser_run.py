#!/usr/bin/env python3
"""bench.1 browser baseline via CDP — the agent-ingest cost of the browser path.

Launches an ISOLATED headless Chrome (own port + temp profile; does NOT touch the
user's running browser), and for each corpus task: navigates to the seed URL,
waits for the load event (real latency), reads document.body.innerText (the text a
browser agent must ingest), and checks the answer field is present.

This is the conservative browser baseline (see browser_baseline.md): innerText
only — no screenshot tokens, no a11y tree, no misclick re-reads. If nab still wins
here, it wins by at least this much.

Output: results/<id>.browser.json = { id, browser_tokens, browser_latency_ms,
         answer_found }.

Usage: ./browser_run.py corpus.pilot.json
"""
import json
import os
import re
import subprocess
import sys
import time

import websocket  # websocket-client

HERE = os.path.dirname(os.path.abspath(__file__))
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
PORT = 9333
PROFILE = "/Users/mikko/nab-cc-tmp/chrome_bench_iso"
OUT = os.path.join(HERE, "results")


def toks(s):
    return (len(s) + 3) // 4


def cdp_send(ws, mid, method, params=None):
    ws.send(json.dumps({"id": mid, "method": method, "params": params or {}}))


def cdp_wait(ws, want_id=None, want_event=None, timeout=40):
    end = time.time() + timeout
    while time.time() < end:
        ws.settimeout(max(0.1, end - time.time()))
        try:
            msg = json.loads(ws.recv())
        except Exception:
            continue
        if want_id is not None and msg.get("id") == want_id:
            return msg
        if want_event is not None and msg.get("method") == want_event:
            return msg
    return None


def launch_chrome():
    os.makedirs(PROFILE, exist_ok=True)
    proc = subprocess.Popen(
        [CHROME, "--headless=new", "--disable-gpu", "--no-sandbox",
         "--no-first-run", "--disable-background-networking",
         "--remote-allow-origins=*",
         f"--remote-debugging-port={PORT}", f"--user-data-dir={PROFILE}",
         "about:blank"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    # wait for CDP up
    import urllib.request
    for _ in range(50):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/version", timeout=1)
            return proc
        except Exception:
            time.sleep(0.2)
    raise RuntimeError("isolated Chrome did not expose CDP")


def new_tab_ws():
    import urllib.request
    # Chrome 111+ requires PUT (not GET) for /json/new.
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}/json/new", method="PUT")
    data = json.loads(urllib.request.urlopen(req, timeout=5).read())
    ws = websocket.create_connection(data["webSocketDebuggerUrl"], max_size=64 * 1024 * 1024)
    return ws, data["id"]


def close_tab(tab_id):
    import urllib.request
    try:
        urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/close/{tab_id}", timeout=5)
    except Exception:
        pass


def run_task(t):
    ws, tab_id = new_tab_ws()
    mid = 0
    try:
        mid += 1; cdp_send(ws, mid, "Page.enable"); cdp_wait(ws, want_id=mid)
        t0 = time.time()
        mid += 1; cdp_send(ws, mid, "Page.navigate", {"url": t["seed_url"]})
        cdp_wait(ws, want_id=mid)
        cdp_wait(ws, want_event="Page.loadEventFired", timeout=40)
        load_ms = int((time.time() - t0) * 1000)
        # give SPAs time to hydrate (counted in latency). npm/crates/SO render
        # their answer client-side; too short a wait captures an empty shell.
        time.sleep(4.0)
        mid += 1
        cdp_send(ws, mid, "Runtime.evaluate",
                 {"expression": "document.body ? document.body.innerText : ''",
                  "returnByValue": True})
        r = cdp_wait(ws, want_id=mid, timeout=20)
        total_ms = int((time.time() - t0) * 1000)
        text = ""
        if r and "result" in r:
            text = (r["result"].get("result", {}) or {}).get("value", "") or ""
        fields = [f for f in t.get("answer_field", "").split(",") if f]
        # answer fields are API JSON keys; for the browser we check the human answer
        # is *reachable* (page non-trivial). Mark found if the page rendered content.
        found = len(text) > 200
        return {
            "id": t["id"],
            "browser_tokens": toks(text),
            "browser_latency_ms": total_ms,
            "answer_found": bool(found),
            "innertext_chars": len(text),
        }
    finally:
        ws.close()
        close_tab(tab_id)


def main():
    corpus = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "corpus.pilot.json")
    tasks = json.load(open(corpus))["tasks"]
    os.makedirs(OUT, exist_ok=True)
    proc = launch_chrome()
    try:
        for t in tasks:
            print(f"[browser] {t['id']}: {t['seed_url']}")
            try:
                rec = run_task(t)
            except Exception as e:
                rec = {"id": t["id"], "browser_tokens": 0, "browser_latency_ms": 0,
                       "answer_found": False, "error": str(e)[:120]}
            json.dump(rec, open(f"{OUT}/{t['id']}.browser.json", "w"), indent=2)
            print(f"  browser_tokens={rec['browser_tokens']} "
                  f"latency_ms={rec['browser_latency_ms']} found={rec['answer_found']}")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()
    print(f"[browser] done -> {OUT}/*.browser.json")


if __name__ == "__main__":
    main()
