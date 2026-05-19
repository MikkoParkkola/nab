# Toby Ord — Are AI Agent Costs Also Rising Exponentially?

**Source.** https://www.tobyord.com/writing/hourly-costs-for-ai-agents (2025-12-22, HN front page 2026-04-18, 257 points)
**Author.** Toby Ord (philosopher, Future of Humanity Institute).
**Ticket.** MIK-2952. References: MIK-2937 (Botnaut effective-cost), MIK-2940 (Davis 24/7 fleet), MIK-2948 (Darkbloom benchmark), MIK-2938 (airlok routing).

## Extracted quantitative claims

| # | Claim (verbatim or near-verbatim) | Source location |
|---|-----------------------------------|-----------------|
| C1 | "The size of the models (parameter count) has grown by 4,000x and the number of times they are run in each task (tokens generated) has grown by about 100,000x" over the 7-year METR window. | §"Over those 7 years AI systems have grown exponentially…" |
| C2 | Sweet-spot hourly rates from the annotated METR/GPT-5 chart: **human SWE ≈ $120/hr**; **o3 ≈ $40/hr**; **Grok 4 ≈ $0.40/hr**; **Sonnet 3.5 ≈ $0.40/hr**. Sweet-spot costs span **~100x** while horizon length spans only **~15x**. | §"We can see that the human software engineer is at best $120 per hour…" |
| C3 | Off-sweet-spot blow-up: **Grok 4 $0.40 → $13/hr** at start of plateau; **GPT-5 $13/hr at 45 min → $120/hr at 2 hr** horizon; **o3 $350/hr** at its full 1.5-hr horizon (above human price). Per-hour cost rises **10-100x** above sweet spot when pushed near plateau. | §"For instance, Grok 4 is at $0.40 per hour…" |
| C4 | Claude 4.1 Opus 50% time horizon = **2 hours** of human SWE work. | §"Claude 4.1 Opus's 50% time horizon is 2 hours" |
| C5 | Cross-model correlation: **time horizon and hourly cost both rise together** across the model fleet, at both sweet-spot and saturation-point. "Hourly costs for some models are now close to human costs." Implication: METR trend is **partly driven by unsustainably increasing inference compute**, so real-world deployment will **lag the headline horizon trend by increasing amounts**. | §"Again, there is a correlation…" and Conclusions list |

## Assessment vs Botnaut portfolio positioning

| Claim | MIK-2937 effective-cost | MIK-2940 24/7 fleet | MIK-2948 Darkbloom | Verdict |
|-------|-------------------------|---------------------|--------------------|---------|
| C1 (4,000x params, 100,000x tokens) | Refines. Reinforces that the absolute-cost denominator has exploded; effective-cost claims must be benchmarked per-task, not per-token. | Validates. 24/7 fleet thesis only holds if per-task cost is bounded; growing token counts mean fleet ROI demands aggressive model/route selection. | Refines. Darkbloom comparison must control for token budget, not just wall-clock latency. | Refines |
| C2 (sweet-spot spread 100x; cheap models exist at $0.40/hr) | **Validates strongly.** A Sonnet-3.5-class agent at $0.40/hr sweet spot is the empirical floor Botnaut routes to; effective-cost positioning is defensible *if* we operate at the sweet spot, not at plateau. | Validates. 24/7 fleet of cheap-sweet-spot agents is economically feasible; expensive agents are not. | Validates. Darkbloom-style benchmarks should explicitly report sweet-spot vs plateau cost. | Validates |
| C3 (plateau blow-up 10-100x) | **Refines — load-bearing.** Effective-cost claims collapse if the router pushes a model past its sweet spot. Routing/budget caps are not a nice-to-have, they are the entire thesis. | **Refines — load-bearing.** A 24/7 fleet that lets agents grind to plateau will pay 10-100x; the fleet must have hard token-budget kill switches. | Refines. Any benchmark that lets competitors run to plateau will look artificially expensive; report cost at matched horizon. | Refines |
| C4 (Claude 4.1 Opus 2-hr horizon) | Validates. Sets the upper bound for what a single agent step should attempt before decomposition. | Validates. Fleet planning should assume ~2 hr unit-of-work for top-tier models in late 2025. | Neutral data point. | Validates |
| C5 (cost rising with horizon; deployment lags METR trend) | **Refutes the naive "agents get cheaper forever" narrative;** validates the Botnaut framing that the *practical* deployable frontier is well below the headline frontier. The moat is in operating economically on the deployable frontier, not chasing the headline. | Validates. 24/7 fleet ROI improves precisely because the headline frontier is uneconomic; cheap-route fleets capture the deployable surplus. | Refines. Darkbloom positioning should explicitly call out the headline-vs-deployable gap as a structural opportunity, not a temporary one. | Refines (with one sub-refutation of the naive narrative) |

**Net.** Ord's piece is **net-validating with one critical refinement**: it confirms cheap sweet-spot operation is real (validates effective-cost + 24/7 fleet theses) but warns that any system allowed to grind to plateau loses 10-100x. The Botnaut moat is the routing + budget discipline that keeps every call near its sweet spot. No claim outright refutes our positioning; C3 + C5 sharpen it into a non-trivial defensible thesis.

## nab cost implication

nab is a URL→Markdown fetcher with anti-bot, auth, and cookies. Its competitive moat against WebFetch (~50K tokens, $0.025/fetch at Opus prices) is the **same sweet-spot vs plateau pattern Ord describes for agents**:

- **WebFetch = plateau operation.** Pays full model cost to read and summarize the entire DOM regardless of relevance. ~25x token bloat per fetch.
- **nab = sweet-spot operation.** Cleans HTML deterministically (~50ms, sub-cent), returns structured MD, lets the agent decide what to read. Effective cost per useful byte is ~1/25th of WebFetch.

Ord's framework gives nab a **defensible economic story for portfolio positioning**: the headline "agent can browse the web" capability (WebFetch-style) is the F1-of-AI metric — possible but uneconomic. nab is the deployable-frontier instrument. The same 10-100x off-sweet-spot blow-up Ord measured for LLM agents shows up in URL-fetch tooling. As model token costs continue to rise per C1, the nab advantage **widens, not narrows**, because the WebFetch tax scales with model price while nab's deterministic fetch cost is flat.

**Action items for nab roadmap.** (1) Publish a nab-vs-WebFetch effective-cost table using Ord's sweet-spot framing. (2) Add a "budget cap" mode to nab batch that mirrors the kill-switch discipline Ord's C3 demands. (3) Cite Ord's piece in nab's README sovereign-stack section as third-party validation of the cost-routing thesis.

## Confidence

V — quantitative claims are direct extractions with paragraph-level source location; assessment classifications use the rubric defined in MIK-2952 AC. Ord's underlying data is METR's GPT-5 chart (one snapshot, ~10 model points), so absolute numbers carry that dataset's caveats (Ord himself flags OpenAI cost-estimate uncertainty). Portfolio implications are I-level (one strong source, internally consistent with MIK-2937/2940/2948 framing).
