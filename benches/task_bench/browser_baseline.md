# bench.1 browser baseline — live protocol

The browser side of the kill-gate. Driven LIVE (not CI) by the same LLM brain
that drives nab, using the `webwright` skill (Playwright). Per task, record
`results/<id>.browser.json` with `{ id, browser_tokens, browser_latency_ms,
answer_found, steps }`.

## What "browser_tokens" means (and why it is conservative)

A browser agent feeds its LLM the page content it must reason over at each step.
We count the **rendered visible text** (`document.body.innerText`) of every page
the agent has to read to reach the answer, summed across steps, at 4 chars/token
(same heuristic as `nab::content::budget::estimate_tokens`, so both sides use one
ruler).

This is deliberately **favorable to the browser**:
- A real vision-driven agent also sends a **screenshot** per step (~1k–2k image
  tokens each) — not counted here.
- A DOM/accessibility-tree agent sends the full a11y tree or raw HTML, which is
  larger than `innerText` — we count the smaller `innerText`.
- We count only the pages on the **happy path**; real agents pay for misclicks
  and re-reads.

So if nab still wins on this baseline, it wins by **at least** this much.

## Latency

Wall-clock from first navigation to the step where the answer is visible/extracted,
summed across the agent's steps (page loads + renders + any interaction waits).

## Per-task procedure (webwright)

1. `navigate(seed_url)` — start the browser at the seed page (no API hint given).
2. Capture `t_start`.
3. At each step, read the page the way a browser agent must:
   `innerText` length -> tokens; add to `browser_tokens`. Screenshot for the
   action log (evidence), not counted.
4. Take the minimal action sequence a competent agent would to reach the answer
   (scroll/click/extract). Each navigation/interaction adds its load time to
   `browser_latency_ms`.
5. Stop when the answer is on screen / extracted. Capture `t_end`.
6. Record the result JSON; save screenshots under `results/<id>.browser/`.

## Honesty rules

- Same goal, same answer, same brain as the nab run.
- Do not optimize the browser path with API knowledge nab had to discover — the
  browser agent reaches the answer through the rendered UI, as a browser agent does.
- If a task cannot be completed by either agent, mark `answer_found=false` and
  exclude it from the medians (the scorer reports exclusions, never hides them).
