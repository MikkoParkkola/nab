# nab — API-first web-task engine + single LLM contact point (DESIGN DRAFT)

Status: RATIFIED 2026-06-05 by operator (Mikko Parkkola). Direction approved; implementation phased per §7. Ratified decisions recorded in §8.
Date: 2026-05-31 · Ticket: MIK-5359 · Author: claude-elite session

## 1. Vision delta

nab evolves from **"fetch a URL"** to **"be the LLM's single contact point for any
web task"** — where nab is a **router** that completes the goal through the
cheapest backend and only reaches for a browser as a last resort. The unit of
action is an **API / `fetch()` call**, not a browser interaction. Same north
star: losslessly-compressed, token-minimal LLM access to the reachable web,
public *or* authenticated.

The "nab is not a browser" locked decision **stands**: nab does not render or
bundle Chromium on the default path. When a task truly needs a browser, nab
*orchestrates an external one over CDP* (the existing opt-in `browser`/
chromiumoxide feature — "no Chromium bundled"). nab is the **conductor**, not the
instrument.

One-liner amendment (RATIFIED 2026-06-05; propagation to public copy deferred until `nab task` ships):
`multimodal web microfetch` → `multimodal web microfetch + API-first web-task completion`.

## 2. nab as the single contact point (router architecture)

The LLM talks to **one** surface: `nab` CLI / `nab-mcp` tools. It does NOT choose
the backend — nab routes through an escalation ladder, cheapest first, applying
its moat (auth, fingerprint, YARA, content-shaping) uniformly at every rung:

```
LLM: "complete <goal> at <url>"  →  nab routes:

  rung 0  FETCH      nab fetch         static page answers it?            → DONE (markdown, ~50ms)
  rung 1  API-FIRST  nab spa+QuickJS   discover + call JSON API directly  → DONE (no browser)
  rung 2  FORM       nab submit        CSRF form POST                     → DONE (no browser)
  rung 3  BROWSER    opt-in CDP        drive EXTERNAL Chrome (last resort)→ DONE
          (nab orchestrates; does not bundle. Webwright loop pattern; Playwright/CDP backend.)
```

nab picks the lowest rung that completes the goal and **never escalates without
need**. The LLM is insulated from the machinery — it asked nab to do a web task;
nab decided fetch vs API vs browser. This is the product position: **the universal,
auth-aware, token-efficient web-task interface for LLMs** — one tool spanning
cheap-fetch → API-task → browser, with the moat applied at every rung.

### 2.1 How Webwright is used (NOT embedded)

Webwright (microsoft/Webwright) is **not a code dependency of nab**:

1. **Pattern donor** — nab's task loop adapts Webwright's *code-as-action* shape
   (plan → emit one action → observe → repair → self-verify); the "action" is an
   authenticated `fetch()`/API call (QuickJS+`fetch_bridge`) or, at rung 3, a CDP
   command — never bundled Playwright code.
2. **Benchmark to beat** — Online-Mind2Web 86.7% GPT-5.4 / 84.7% Opus; Odysseys
   60.1%; the 422K-vs-3.3M token gap. `nab task` must win on the API-backed subset.
3. **Interactive fallback** — at rung 3, nab drives an external browser over CDP
   (its existing opt-in `browser` feature). For host-side cases, the
   `webwright-elite` claude-elite skill can still drive Playwright, but the
   single-contact-point target is nab-orchestrates-CDP so the LLM only talks to nab.

### 2.2 Bidirectional moat — nab enhances the browser rung too

When rung 3 fires, nab wraps the browser (auth in, efficient context out):

| nab feature | Enhances the browser rung by | Win |
|---|---|---|
| `cookies export --format playwright` + `login`/`otp` | CDP session starts logged in; 1Password/TOTP/WebAuthn | auth-gated tasks a fresh browser can't reach |
| `fingerprint` / `waf` | stealthier launch profile | fewer bot blocks |
| content pipeline (`--focus`, readability, budget) | rendered HTML → tight query-focused markdown, not DOM dump | dominant token saving |
| Apple Vision OCR | screenshot → exact text (~10-50ms) | cheaper than vision tokens |
| `response_classifier` | flags JS-shell/thin/blocked | act/wait/escalate, not ingest noise |
| YARA-X engine | screens content before the loop acts | injection defense |
| `api_discovery` mid-task | re-run on current page → switch clicks to API calls | collapse rest of task to HTTP |

## 3. Substrate already in nab (no new heavy deps)

| Capability | Module | Note |
|---|---|---|
| JS execution | `src/js_engine.rs` (rquickjs, ~1MB, ES2020, 32MB cap) | minimal, not a browser |
| fetch() bridge | `src/fetch_bridge.rs` | page `fetch()` → authenticated Rust client |
| API discovery | `src/api_discovery.rs` + `nab spa` | static regex + QuickJS fallback |
| Auth | `src/auth/cookies/*`, `nab login`, `nab otp` | cookies + 1Password + TOTP/WebAuthn |
| Stealth | `src/fingerprint/`, `src/waf/` | anti-bot |
| Form POST | `src/cmd/submit.rs` | CSRF-aware |
| Browser backend | opt-in `browser` = chromiumoxide CDP (external Chrome) | NOT default; nab orchestrates, does not bundle |
| Security | `crates/nab-yara-engine` | injection/exfil screening on ingest |
| Multimodal | `src/content/ocr`, `src/analyze/*` | OCR/ASR on encountered media |
| Brain (no key) | MCP server `sampling.rs` | host LLM drives the loop |
| Content shaping | `src/content/*` (readability, focus, budget, classifier) | LLM-shaped output |

Only NEW code: the router/loop orchestration + a Playwright/CDP cookie-export format.

## 4. Control flow (`nab task "<goal>" <url>`)

```
1. SEED      nab fetch <url> (markdown, YARA-screened) + nab spa (discover APIs)
2. PLAN      MCP sampling: host LLM proposes next ACTION as a structured step
             { kind: api_call | js_eval | submit | extract | needs_browser | done,
               url, method, headers, body, extract_query }
3. ROUTE+ACT execute at the lowest rung:
               api_call -> AcceleratedClient (cookies+fingerprint+HTTP/3)
               js_eval  -> QuickJS + fetch_bridge
               submit   -> cmd::submit (CSRF)
               needs_browser -> escalate to rung 3: opt-in CDP to external Chrome,
                                with nab cookies + fingerprint pre-applied
4. OBSERVE   YARA-screen response; shape via content pipeline to token budget; feed back
5. REPAIR    on error/empty, host LLM revises the action (bounded retries)
6. DONE      LLM-shaped result + reusable task script (code-as-action artifact)
```

Bounded (max steps, token budget, wall-clock). Rungs 0-2 launch no browser. Rung
3 only on explicit `needs_browser` and only with the opt-in feature compiled in;
otherwise nab returns `delegate_to_browser` for the host skill to handle.

## 5. Identity guardrails (do-not-violate)

- Default build: NO Chromium. `browser`/chromiumoxide stays opt-in, as today.
- nab orchestrates an EXTERNAL browser over CDP; it never renders or bundles one.
- Output: LLM-shaped markdown/JSON, never raw HTML dumps.
- No API keys on the default path — brain is the host via MCP sampling.
- Escalate only on need; rung 3 is the last resort, not the default.

## 6. Acceptance criteria / kill-gates

- export.1: `nab cookies export --format playwright` → valid storage_state JSON.
- task.1: `nab task` completes a 3-step API-backed goal end-to-end, host-driven,
  default build (no Chromium), authenticated where needed.
- task.2: every loop step YARA-screened; bounded steps/tokens/wall-clock.
- route.1: router provably stops at the lowest rung that completes the goal
  (telemetry `rung=0|1|2|3`); rung 3 never fires when an API path exists.
- bench.1: on a 20-task API-backed subset, beat vanilla Webwright on median
  tokens AND latency. **KILL-GATE: if it cannot beat a browser agent on the
  API-backed subset, it is just a worse browser agent — stop.**

### 6.1 bench.1 protocol (how the kill-gate is actually run)

The gate has two axes and two halves of evidence. Do not conflate them.

**Metrics (both required to pass).** Over the 20-task corpus, compute the
*median* (not mean — resists outlier pages) of:
1. **Tokens** — total tokens fed to the LLM across the whole task (every step's
   ingested context + the final answer). nab must have the lower median.
2. **Latency** — wall-clock from task start to a correct answer. nab must have
   the lower median.

**Corpus (20 tasks).** Auth-gated and/or API-backed sites — nab's moat. Each
task is `{ goal, seed_url, success_check }`; the API endpoint nab should discover
is recorded for analysis but NOT given to either agent (both must discover it).
Tasks are chosen when the gate is run; bias toward sites with a clean JSON API
behind a heavy HTML page (the gap is widest there) and toward auth-gated flows a
fresh browser cannot reach without nab's cookie/1Password/WebAuthn path.

**nab measurement.** `nab task "<goal>" <seed_url>` driven by a host LLM over MCP
sampling (the self-contained loop). Record the LLM's total prompt+completion
tokens and wall-clock. No per-site API keys — auth is browser cookies + nab's
login path.

**Webwright baseline.** Vanilla `microsoft/Webwright` (Playwright backend) driven
by the SAME LLM model on the SAME tasks. Record the same two numbers. This is a
live run — it needs a browser, the LLM, and per-site credentials, so it is
neither deterministic nor CI-runnable.

**Pass/fail.** Pass iff `median(nab_tokens) < median(ww_tokens)` AND
`median(nab_latency) < median(ww_latency)`. Any other outcome trips the
kill-gate: stop and reconsider the thesis.

#### Evidence shipped vs. remaining

- **Shipped (deterministic, CI-runnable): the token-axis proxy.**
  `nab::task::token_gap` / `median_ratio` measure nab's REAL shaping output
  (readability markdown + API JSON) against a **conservative browser baseline**:
  the raw page HTML a DOM-dumping agent feeds its LLM. The baseline is favorable
  to the browser — real agents add accessibility trees, screenshots, and a fresh
  DOM dump per step, none of which are counted here. Unit-proven that nab's path
  is strictly cheaper on chrome-heavy pages. This is the moat *mechanism* shown
  to hold, not the gate.
- **Remaining (the actual kill-gate): the live head-to-head.** A real
  20-task corpus, a running Webwright + LLM brain, per-site auth, and the
  **latency** axis — none of which the token-axis proxy covers. Until that run
  exists, **bench.1 is NOT cleared** and `nab task` does NOT graduate to the
  default MCP surface (per §7 phasing).

## 7. Phasing

1. export.1 (small, standalone-useful, aligned today).
2. task.1 MVP behind a `task` feature flag; rungs 0-2 only; dogfood via skill.
3. route.1 + bench.1 gate; only then promote `nab task` to default MCP surface.
4. rung 3 (browser orchestration) behind the existing opt-in `browser` feature.

## 8. Operator decisions (ratified 2026-06-05)

- **Vision one-liner amendment (§1): RATIFIED.** Direction approved. Public-facing
  positioning copy (README, GitHub About, CLAUDE.md) is NOT changed until `nab task`
  actually ships — the product leads the marketing, not the reverse (Rams #6 honest).
- **Single contact point: MCP-tool-first.** Expose `task` via nab-mcp first; the CLI
  surface follows once the loop is proven.
- **Rung-3 model: nab-orchestrates-CDP (chromiumoxide).** nab drives an external
  browser itself; the `webwright-elite` skill is Phase-1 scaffold only and is retired
  once rung 3 lands (§10). The LLM only ever talks to nab.
- **Bench target subset: deferred to the Phase-3 bench.1 gate.** Prefer auth-gated +
  API-backed sites (nab's moat); the exact 20-site list is chosen when bench.1 is built.

## 9. Self-contained via MCP (the LLM installs ONLY nab)

Goal: the LLM never needs to install or invoke any other tool. nab-mcp runs the
whole agentic loop **inside the server**, using the latest MCP capabilities so the
client supplies only intelligence — nab supplies execution, memory, and safety.
The `webwright-elite` skill is demoted to a **Phase-1 reference/dogfood harness
only**; the end state has no host skill and nothing else to install.

| MCP capability | Role in nab's self-contained loop | Effect |
|---|---|---|
| **sampling** (`sampling/createMessage`) | the loop's BRAIN — nab asks the connected LLM "what's the next action?" each step | no API key; nab borrows the host's model; the LLM that called `nab.task()` drives nab's own backends |
| **resources** (`nab://task/<id>/…`) | expose intermediate state — trajectory, discovered-apis, screenshots, current-extract — as subscribable resources | PULL not PUSH: the LLM reads only what it needs; nab never dumps the page/DOM into a tool result |
| **elicitation** (`elicitation/create`) | when nab hits a human-only wall — 2FA approve, CAPTCHA, "which account?", consent for a destructive step | nab stays autonomous-but-safe; asks the USER through the client UI, then continues |
| **progress** (`notifications/progress`) | stream "step 4/12: calling API…" on long-horizon tasks | live UX without token cost |
| **roots** | respect the client's workspace for task artifacts (`final_runs/`) | clean file placement, no guessing |
| **structured output** | the final result as a typed object | LLM-shaped, parse-free |

### 9.1 The self-contained loop

```
LLM → nab.task(goal, url)                       # the ONLY call the LLM makes
  nab (internally, no other tool):
    loop:
      action = sampling/createMessage(           # brain = connected LLM, no key
                 ctx = seed + trajectory-so-far + discovered-apis)
      run action at lowest rung (fetch | api | js_eval | submit | cdp-browser)
      observe → YARA screen → content-shape → append to trajectory resource
      if needs_human: elicitation/create(...) ; resume
      emit notifications/progress
      if done: break
  → return structured result   (+ nab://task/<id>/* resources for drill-down)
```

Everything — planning, execution, the browser rung (external CDP), memory,
security — happens inside nab. The browser is nab's own opt-in CDP backend, not a
separate install. Webwright contributes only the loop *pattern* and the benchmark.

### 9.2 Capability-aware fallback (honest constraint)

MCP **sampling requires client support**. nab detects this at `initialize`:
- **Client supports sampling** → fully self-contained loop above (target path).
- **Client lacks sampling** → nab returns one structured "next action" per call and
  the host LLM drives the loop turn-by-turn (the Phase-1 model). Same backends,
  same moat; just host-driven control flow. nab never requires a second tool either way.

### 9.3 Why this beats "delegate to a host skill"

- **One install, one surface.** The LLM connects to nab; that's the entire setup.
  No skill to ship, no Playwright for the LLM to manage, no second tool.
- **No API key.** Sampling reuses the host subscription.
- **Token-disciplined by construction.** Resources are pull-based; nab shapes every
  observation through its content pipeline before the model ever sees it.
- **Portable.** Any sampling-capable MCP client (Claude Code, etc.) gets the full
  agent for free; non-sampling clients degrade gracefully to host-driven, still
  needing only nab.

## 10. Rung 3 — interactive flows are nab-native (chromiumoxide), NOT Playwright

Correction to any earlier "delegate to host Playwright" wording: interactive
execution is **nab's own**, via the `chromiumoxide` CDP driver (already an opt-in
dep) against the user's EXISTING Chrome/Brave. Webwright/Playwright never executes
in the end state — it is pattern + benchmark only. Two independent axes:

| Axis | Options | End state |
|---|---|---|
| Control flow (who runs the loop) | MCP sampling (nab self-driven) ↔ host-driven (no-sampling clients) | sampling primary; host-driven degrade |
| Browser execution (what clicks) | **nab chromiumoxide** ↔ ~~Playwright/Webwright~~ | always nab chromiumoxide |

The `webwright-elite` skill is Phase-1 scaffold only; retired once rung 3 lands.

### 10.1 Rung-3 action schema (what the sampling loop emits)

```jsonc
// one action per sampling step; nab executes via chromiumoxide against external Chrome
{ "rung": 3,
  "action": "navigate|click|type|select|wait|scroll|eval|screenshot|extract|done|needs_human",
  "selector": "css-or-text",      // for click/type/select/wait
  "text": "...",                  // for type
  "url": "...",                   // for navigate
  "wait_for": "selector|networkidle|timeout-ms",
  "extract_query": "what to pull",// extract -> nab content pipeline (readability/focus/budget)
  "reason": "1 line"
}
```

Execution contract: nab pre-applies cookies (§2.2) + fingerprint to the CDP
session; every observation is YARA-screened + content-shaped before returning to
the loop; `needs_human` triggers MCP elicitation (2FA/CAPTCHA/consent); the run
is bounded (max steps / token budget / wall-clock). Default build excludes
chromiumoxide; rung 3 requires the opt-in `browser` feature.

### 10.2 chromiumoxide vs Playwright (honest parity note)

chromiumoxide covers the same CDP primitives but is less battle-hardened than
Playwright (auto-waiting, selector ergonomics, community breadth). Acceptable
because rung 3 is the rare last resort, not the hot path. The only true-self-
containment alternative would be shelling to a Playwright sidecar — which breaks
"the LLM installs only nab." chromiumoxide keeps that promise; that decides it.



## 11. Slice-1 implementation scoping (2026-06-05, mapped against live source)

Concrete integration points for the first build slice (`task` feature flag,
action schema, `nab task` wired to rung 0). Verified against the current tree.

**CLI wiring**
- `enum Commands` — `src/main.rs:72`. Add a `Task { goal: String, url: String, ... }` variant.
- dispatch `match cli.command` — `src/main.rs:940`. Add the `Commands::Task { .. }` arm.
- `pub mod task;` in `src/cmd/mod.rs` (or a new top-level `src/task/`).
- New `task` feature in `Cargo.toml [features]` (gate the command + module like
  `analyze`/`browser`); keep it OUT of `default` until the loop is proven.

**The regression-sensitive prerequisite (do FIRST, with care)**
- Rung 0 needs the screened markdown as a RETURNED VALUE. Today there is none:
  - `cmd_fetch(cfg: &FetchConfig) -> Result<()>` (`src/cmd/fetch.rs:142`) prints to stdout.
  - `convert_body_to_markdown(..)` (`src/cmd/fetch.rs:823`) is private/async.
  - `guard_fetch_output(body, surface, url) -> Result<String>` (`src/security/fetch_yara.rs:19`) is the YARA screen.
- Extract `fetch_to_screened_markdown(cfg: &FetchConfig) -> Result<String>` from
  `cmd_fetch`; make `cmd_fetch` a thin printer over it. This touches nab's hot
  path (`nab fetch`), so: the full existing fetch test suite MUST stay green, and
  add tests asserting the extracted core returns the same bytes the printer emits.

**Schema (self-contained, fully tested)**
- `src/task/schema.rs`: the §4 action enum as serde types —
  `{ kind: api_call | js_eval | submit | extract | needs_browser | done, url,
  method, headers, body, extract_query }` + a `TaskOutcome` result type.

**Slice-1 acceptance**
- `nab task "<goal>" <url>` (feature-gated) fetches via the extracted core, YARA-
  screens, returns shaped markdown as a `TaskOutcome`. Bounded (single step at rung 0).
- No regression to `nab fetch`. New unit tests for schema + the extraction parity.
- Rungs 1-2 (API/submit) + the sampling loop land in slices 2-3 (see §7).

## 12. Slices 2-3 shipped + Slice-4 scoping (2026-06-06, mapped against live source)

**Shipped on `main`:**
- **Slice 2** (#148): rung-1 API *discovery* — `discover_apis` surfaces
  `DiscoveredApi` leads in `TaskOutcome` (the host LLM sees callable endpoints).
- **Slice 3** (#150): rung-1 API *execution* — `execute_action(action, format)
  -> ActionObservation` runs a `TaskAction::ApiCall` via `fetch_screened`
  (`fetch_config_for_api_call` maps method/headers/body onto `FetchConfig`).
  CLI: `nab task <goal> <url> --action '<json>'` drives one step (§9.2
  host-driven). `submit`/`extract` return an honest `Incomplete` (deferred);
  `cmd_fetch` untouched.

### 12.1 The binary-boundary constraint (load-bearing — verified 2026-06-06)

The task engine today lives entirely in **`src/cmd/task.rs` + `src/cmd/fetch.rs`**,
which compile **only into the `nab` CLI binary** (`src/cmd/` is not in `src/lib.rs`).
`fetch_screened` and `execute_action` are therefore **unreachable from the
`nab-mcp` binary**.

Critically, `nab-mcp` already has its **own** fetch path: `FetchTool`
(`src/bin/mcp_server/tools/fetch.rs`) builds directly on **library** primitives —
`nab::AcceleratedClient`, `nab::content::*`, `nab::security::guard_fetch_output`,
`nab::SafeFetchConfig` — and never calls `cmd::fetch`. The two binaries run
**parallel fetch implementations** over a shared library substrate.

This decides slice 4. The self-contained sampling loop (§9.1) can only live in
`nab-mcp` (only it holds a client runtime to call `sampling/createMessage`, via
the existing `src/bin/mcp_server/sampling.rs` — `is_supported` + `create_message`
+ a `MockRuntime` test harness). That loop **cannot** call
`cmd::task::execute_action` across the binary boundary, and moving the flagship
`cmd_fetch` stack into the library to make it reachable is exactly the
zero-flagship-regression line we will not cross.

### 12.2 Decision — promote the task core to `nab::task` (library)

Build slice 4 on the **library substrate the MCP fetch tool already uses**, not
on `cmd::fetch`:

1. **Schema → lib (pure, zero-risk).** Move `TaskAction`, `TaskOutcome`,
   `ActionObservation`, `DiscoveredApi`, `TaskStatus` from `src/cmd/task.rs` to a
   new `src/task/` library module (`nab::task`), behind the `task` feature. These
   are plain serde types — the move is mechanical and both binaries then share one
   schema (today the schema is binary-only, so `nab-mcp` would otherwise have to
   duplicate it).
2. **Lib executor on shared primitives.** A `nab::task::execute_action` built on
   `nab::AcceleratedClient` + cookie resolution (`nab::util::resolve_cookie_header_for_url`)
   + `nab::ApiDiscovery` + `nab::security::guard_fetch_output` + `nab::content`
   budget/focus. This gives the loop the core moat (HTTP/3 + fingerprint + browser
   cookies + YARA screen + token budget) **without** depending on `cmd_fetch`.
   The CLI's `cmd::task` becomes a thin wrapper over the lib core (or keeps its
   `fetch_screened` path — both are acceptable; the lib core is the SSOT the loop uses).
3. **Loop = pure, mock-tested.** `run_task_loop(goal, seed, sampler, bounds)`
   where `sampler` is a trait abstracting `create_message` — testable with a
   scripted mock sampler (the `MockRuntime` pattern), no real LLM. Bounded by max
   steps / token budget / wall-clock (§4). Seeds with the lib fetch + `discover_apis`,
   then: sample → parse `TaskAction` → `execute_action` → append trajectory →
   check done/bounds.
4. **MCP tool = thin wiring.** A `task` tool in `src/bin/mcp_server/tools/` that
   calls `run_task_loop` with a real sampler over `sampling.rs`; falls back to
   one-action-per-call when `is_supported` is false (§9.2).

### 12.3 Slice-4 DoR — open items to resolve before building

- **Moat scope of the lib executor.** `cmd_fetch` carries extras the lib executor
  would NOT inherit for free: `SiteRouter` providers (lib — reusable), the
  issue-#117 cookie-profile fallback, and WAF handling (some binary-local). Decide
  per-extra: reuse from lib, promote, or accept a thinner rung-1 path for the loop.
  Rung-1 `api_call` needs little of this (direct JSON endpoints); document what is
  intentionally out.
- **`submit` (rung 2)** still needs its own value-returning extraction from
  `cmd_submit` (which prints + returns `Result<()>`), mirroring the `fetch_screened`
  carve-out. Scope it as its own regression-sensitive step, not a free add.
- **`route.1` + `bench.1` kill-gates** (§6/§7 Phase 3) become assertable once the
  loop exists: `route.1` proves the router stops at the lowest rung (telemetry the
  `ActionObservation.rung` field already carries); `bench.1` is the 20-task
  median-tokens-AND-latency gate vs a browser agent — **KILL-GATE**.

### 12.4 Slice-4 acceptance

- `nab::task` library module (schema + executor + loop), `task`-feature-gated;
  default `-Dwarnings` build clean (no dead-code leak), `cmd_fetch` untouched.
- `nab-mcp` exposes a `task` tool: sampling-capable clients get the self-contained
  loop; non-sampling clients get host-driven one-action-per-call (§9.2).
- Loop is bounded and mock-sampler-tested end-to-end (no real LLM in CI).
- `task.1` (3-step API-backed goal, host-driven, default build, authenticated) and
  `route.1` (rung telemetry proves lowest-rung stop) demonstrated.
