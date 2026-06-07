#!/usr/bin/env python3
"""bench.1 browser baseline v2 — validity-checked CDP capture.

Fixes the pilot's measurement bug: a render only counts if the page actually
rendered the answer. Bot-walls and empty SPA shells are browser FAILURES, not
cheap token wins.

Per task, against an ISOLATED headless Chrome (own port + temp profile):
  1. navigate to seed_url
  2. wait for the load event, then poll document.body.innerText until it stops
     growing (SPA hydration) or a cap — this is the real "page settled" signal.
  3. validity gate: innerText must exceed MIN_VALID_CHARS and contain none of the
     bot-wall markers; if `browser_answer_contains` is set, it must be present.
  4. record browser_tokens (innerText) + browser_latency_ms + answer_found.

innerText only — conservative (no screenshot/a11y tokens). See browser_baseline.md.
Output: results/<id>.browser.json.

Usage: ./browser_run.py corpus.v2.json
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
PORT = 9334
PROFILE = "/Users/mikko/nab-cc-tmp/chrome_bench_v2"
OUT = os.path.join(HERE, "results")

MIN_VALID_CHARS = 500
BOT_WALL = re.compile(
    r"enable javascript|just a moment|verify you are human|access denied|"
    r"unusual traffic|are you a robot|captcha|cf-browser-verification",
    re.I,
)


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


def read_innertext(ws, mid):
    cdp_send(ws, mid, "Runtime.evaluate",
             {"expression": "document.body ? document.body.innerText : ''",
              "returnByValue": True})
    r = cdp_wait(ws, want_id=mid, timeout=20)
    if r and "result" in r:
        return (r["result"].get("result", {}) or {}).get("value", "") or ""
    return ""


def run_task(t):
    ws, tab_id = new_tab_ws()
    mid = 0
    try:
        mid += 1; cdp_send(ws, mid, "Page.enable"); cdp_wait(ws, want_id=mid)
        t0 = time.time()
        mid += 1; cdp_send(ws, mid, "Page.navigate", {"url": t["seed_url"]})
        cdp_wait(ws, want_id=mid)
        cdp_wait(ws, want_event="Page.loadEventFired", timeout=40)
        # Poll innerText until it stops growing (SPA hydration settled) or a cap.
        prev, stable, text = -1, 0, ""
        deadline = time.time() + 12
        while time.time() < deadline:
            time.sleep(0.8)
            mid += 1
            text = read_innertext(ws, mid)
            if len(text) == prev:
                stable += 1
                if stable >= 2:  # ~1.6s with no growth → settled
                    break
            else:
                stable = 0
            prev = len(text)
        total_ms = int((time.time() - t0) * 1000)

        # Validity gate: a real render of the answer, not a shell or a bot-wall.
        marker = t.get("browser_answer_contains", "")
        valid = (
            len(text) >= MIN_VALID_CHARS
            and not BOT_WALL.search(text)
            and (not marker or marker.lower() in text.lower())
        )
        return {
            "id": t["id"],
            "browser_tokens": toks(text),
            "browser_latency_ms": total_ms,
            "answer_found": bool(valid),
            "innertext_chars": len(text),
            "valid_capture": bool(valid),
        }
    finally:
        ws.close()
        close_tab(tab_id)


def main():
    corpus = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "corpus.v2.json")
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
                       "answer_found": False, "valid_capture": False, "error": str(e)[:120]}
            json.dump(rec, open(f"{OUT}/{t['id']}.browser.json", "w"), indent=2)
            flag = "" if rec.get("valid_capture") else "  [INVALID capture — bot-wall/shell]"
            print(f"  browser_tokens={rec['browser_tokens']} "
                  f"latency_ms={rec['browser_latency_ms']} valid={rec.get('valid_capture')}{flag}")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()
    print(f"[browser] done -> {OUT}/*.browser.json")


if __name__ == "__main__":
    main()
