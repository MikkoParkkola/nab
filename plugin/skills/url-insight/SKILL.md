---
name: url-insight
description: Auto-triage pasted URLs into signal-tagged, ROI-scored, DoR-compliant portfolio insights. Fetches via nab, classifies (paper/product/repo/blog/docs/benchmark), cross-maps to 21-repo portfolio, dispatches specialist agent panels, runs devil + steelman critiques, scores ROI, auto-files P0-P2 issues via linear skill. Dedupes via hebb. Auto-fires on URL batches, "analyze these urls", "what's interesting in", "should we adopt X", "competitive signal", "worth a spike", multi-URL pastes.
triggers: analyze these urls|what's interesting in|should we adopt|competitive signal|worth a spike|triage urls|url batch|read these links|insight from|digest these|landscape scan|what did I miss|portfolio signal
effort: medium
version: 2026.05.06-v5
---

# url-insight — URL batches → portfolio insights → DoR issues

**Fires on**: multi-URL paste · "analyze these urls" · "should we adopt X" · "competitive signal" · "worth a spike" · arxiv/HN/ProductHunt/GitHub URLs in bulk.
**Purpose**: compress a firehose of URLs into ranked, evidence-backed actions against the 21-active-repo portfolio. Noise → IGNORE. Signal → specialist agent panel → Linear ticket with devil + steelman reckoning.

Design: Fukasawa (paste URL → pipeline fires) · Rams #10 (8 labels, not 30) · Honest (IGNORE is first-class) · Calibrated (devil + steelman together).

---

## 0 · MANDATORY protocol (non-negotiable, like hebb/linear)

1. **Before fetching any URL**: run the dedup pre-check in §0.1. If hebb or Linear finds a prior verdict, **skip fetch**, **skip nab**, and report `Already triaged: <MIK-id>, verdict=<ADOPT|IGNORE|INSPIRE|SUPERCHARGE|HUMAN-REVIEW>, ROI=<n>x` with the issue link.
2. **After each URL is classified + scored**: write the hebb dedup pin in §0.1 with `project="url-insight"` and key schema `url-<sha256-of-canonical-url>-<iso-date>`.
3. **Before filing any issue**: run devil's advocate (§6) AND steelman (§6.5). Quote both verbatim in issue body.
4. **For SUPERCHARGE**: dispatch innovator agent FIRST (serial gate). If kill → drop signal. If proceed → parallel fan-out: moonshot-architect + business-panel + research patent-prior-art.
5. **For FAIL-FAST SPIKE**: surface user prompt "file issue / also spawn <1h spike agent?" (unless `--auto-spike` flag).
6. **Emit innovation-tracker queue entry** per actionable insight (§10).
7. **Wayback evidence capture (BLOCKING for non-IGNORE)**: after a successful fetch, call `fulcrum:wayback_save` with `if_not_archived_within="7d"` and `capture_screenshot="1"`; persist `wayback_url` or `wayback_save_failed=true` in the run JSON (§1.1). Capture failure is evidence debt, not a triage blocker.
8. **Dual output**: JSON artifact `~/.claude/data/url-insight/run-TIMESTAMP.json` + markdown report.
9. **Living-document update (BLOCKING for non-IGNORE)**: append delta line(s) with `[^N]` citation(s) to `~/.claude/data/portfolio/market-positioning-living.md` per §0.8. No skipping. No "I'll do it later". The doc rots without per-URL writes.

Confidence: V (verified ≥2 sources) · I (inferred from 1) · A (assumed). Mark every ROI claim.

Token budget per URL (hard cap 2M total per batch, 50 URL hard cap):
SUPERCHARGE ~80K · INSPIRE/REPOSITION ~40K · FAIL-FAST/INTEGRATE ~20K · ADOPT ~10K · IGNORE 0.
Override flags: `--shallow` (skip panels, 1-pass) · `--deep` (full panel even for INTEGRATE).

---

## 0.1 · Dedup pre-check (MIK-3372, FIRST PHASE before nab-fetch)

This is the first phase after URL parsing and before any fetch, `nab_fetch`, browser, or agent analysis. Duplicate detection must be cheap and deterministic.

1. **Canonicalize URL** with `skills/url-insight/dedup_precheck.py`:
   - lower-case the host
   - strip trailing slash from non-root paths
   - remove fragments
   - strip `utm_*`, `utm_source`, `utm_medium`, `utm_campaign`, `utm_term`, `utm_content`, `fbclid`, `gclid`, `ref`, and `source`
   - compute `sha256(canonical_url)`
2. **hebb recall**:
   - query: canonical hash from step 1
   - project: `url-insight`
   - limit: `3`
   - matching prior pin key format: `url-<sha256-of-canonical-url>-<iso-date>`
3. **Linear search**:
   - search title/body for the canonical host and the strongest path or repo component
   - example terms from `dedup_precheck.py --url "$URL" --json`: `linear_search_terms`
4. **Short-circuit on hit**:
   - print `Already triaged: <MIK-id>, verdict=<ADOPT, IGNORE, INSPIRE, SUPERCHARGE, HUMAN-REVIEW>, ROI=<n>x` plus the Linear link
   - exit the URL pipeline for that URL
   - do **not** call `nab_fetch`, browser fetch, Wayback save, classifier agents, or Linear issue creation for the duplicate
5. **Pin after successful fresh triage**:
   - call `mcp__hebb__remember` with `project="url-insight"`, tags including `url-insight`, `url-canonical-hash`, the signal label, and the canonical host
   - key schema: `url-<sha256-of-canonical-url>-<iso-date>`
   - body must include at least: original URL, canonical URL, canonical hash, Linear ticket identifier if filed or reused, verdict, ROI, and run artifact path

Helper command:

```bash
python3 skills/url-insight/dedup_precheck.py --url "$URL" --json
```

False-positive safety: if the dedup check blocks 3 legitimate re-triages in 7 days, tighten canonicalization and add a `--force` path before further blocking changes.

---

## 0.8 · Living-document update mandate (NON-NEGOTIABLE — ships every URL)

**File**: `~/.claude/data/portfolio/market-positioning-living.md` — the canonical investor / partner / new-agent primer for the portfolio's market posture.

**Rule**: Every URL classified as **anything other than IGNORE / IGNORE-WATCHLIST** MUST result in at least one delta line appended to the living document. No exceptions. The living doc decays into stale opinion-art the moment we let an analysis ship without writing back.

**Section-mapping (where the delta goes)**:

| Classification result                          | Target section in living doc      | Delta content                                           |
| ---------------------------------------------- | --------------------------------- | ------------------------------------------------------- |
| SUPERCHARGE filed                              | §6 SUPERCHARGE candidates table   | new row with Linear ID + EXCEED axis + status           |
| New cumulative-axis signal (per MIK-3244)      | §5 Cumulative axis signals table  | append signal to existing axis row OR create new row    |
| Vocabulary critique / external messaging audit | §4 Vocabulary                     | new AVOID / USE entry with source citation              |
| New competitor + product launch                | §3 Market thesis (relevant wedge) | competition list update + reference link                |
| Honest-weak-spot reveal                        | §7 Honest weak spots              | numbered entry with mitigation + Linear ticket if filed |
| Wedge TAM revision                             | §3 Market thesis (relevant wedge) | TAM range update with new evidence link                 |
| Active-ticket re-surfacing (per §4.1 gate)     | §11 Source ledger                 | one-liner cross-ref                                     |

**Citation requirement**: every delta line MUST include at least one `[^N]` reference resolving to §12 of the living doc. If §12 does not yet contain the source, add it in the same edit. **No claim ships without provenance.** A delta with no `[^N]` is a protocol violation — the user has explicitly directed: "if there is no valid links someone could think it is just a collection of opinions. we are having this fact based + our own conclusions" (2026-05-03).

**§12 entries** must carry: source URL (V-fetched at addition time), one-line summary, confidence marker (V / I / A), fetch date. Confidence-`I` and `-A` entries MUST be upgraded to `V` within 30 days plus a second corroborating source, OR the dependent claim is removed.

**Operational sequence per URL**:

1. Run pipeline steps 1–13 (§1).
2. If signal != IGNORE: identify target section per table above.
3. `Read` `market-positioning-living.md` (last-edit check + section anchor).
4. `Edit` to append delta line with `[^N]` reference.
5. If new source: `Edit` §12 with full reference (URL + summary + V/I/A + date).
6. Increment doc version at footer (`v1.x` → `v1.x+1`) on every edit, even single-line. Stale version = stale doc.
7. Commit message MUST cite the originating URL hash + Linear ticket(s) for audit.

**Quarterly consolidation** (first Monday Jan/Apr/Jul/Oct): operator (or `/loop` ticked task) merges duplicate axis-signal rows, prunes >6mo-stale signals with no re-trigger, refreshes TAM ranges, upgrades `(I)` / `(A)` markers.

**Audit failure modes**:

- Delta added without `[^N]` → protocol violation; revert + redo.
- §12 reference added without V-fetch → mark `(A)` + log a 30-day promotion task in Linear.
- Wedge TAM revised without external evidence → roll back the TAM revision; keep evidence link only.
- Footer version not bumped → CI / pre-commit reformat will flag.

This subsection is BLOCKING the same way §0 is BLOCKING — skipping it leaves the living doc lying to its readers, which destroys its value as an investor primer and as new-agent ramp-up material.

---

## 1 · Pipeline (single pass, 13 steps)

```
1.  PARSE   URLs from message (regex \bhttps?://\S+\b)
2.  DEDUP   canonical URL hash + hebb recall + Linear search (§0.1); skip fetch if already triaged
3.  FETCH   nab_fetch MCP (PREFER); nab_fetch_batch for ≥3 URLs
            Fallback: `nab fetch URL` CLI (--cookies brave / --1password)
            NEVER WebFetch (~50K token waste; see linear skill §5)
3b. ARCHIVE successful 2xx fetches with fulcrum:wayback_save (§1.1)
4.  CLASSIFY  URL type: paper · product-launch · blog · repo · docs · benchmark · company · other
5.  ROUTE   enrichment by type (§3)
6.  EXTRACT single-pass: claim · differentiator · numbers · date · surprise
7.  CREDIBILITY  tier S/A/B/C/D (§5); multiplies ROI probability
8.  CROSS-MAP  to 21 active repos (§4); v1 descriptions, v2 CLAUDE.md
9.  CLASSIFY SIGNAL  → 1 of 8 labels (§2)
10. DISPATCH AGENTS  by signal (§6); parallel fan-out, normalized JSON return
11. DEVIL + STEELMAN  reconciliation (§6.5); calibrated verdict
12. SCORE ROI  + compose DoR-compliant issue body (§7) → linear skill (§8)
13. REPORT  sorted table + IGNORE list + JSON + queue emit (§9)
```

Staleness: if `published_date > 24mo ago` → flag `[DATED, check SOTA]`, cap ROI probability at 0.4.

---

### 1.1 · Wayback evidence capture (MIK-3354)

Run immediately after each successful 2xx fetch, before classification:

```python
archive = gateway_execute("fulcrum:wayback_save", {
    "url": url,
    "if_not_archived_within": "7d",
    "capture_screenshot": "1",
})
```

Capture results are never allowed to block classification, agent fan-out, or Linear filing. They do change evidence quality:

- If the tool returns an archive/timestamp URL, store it as `wayback_url` on the URL row in `~/.claude/data/url-insight/run-TIMESTAMP.json`.
- If the tool returns a pending job, store `wayback_job_id` and follow with `fulcrum:wayback_availability`; fill `wayback_url` when availability returns a timestamp URL.
- If capture or availability fails, store `wayback_save_failed=true` and `wayback_error` with a short sanitized reason. Do not log credentials, cookies, request headers, or page bodies.
- For every non-IGNORE Linear issue/comment, include both `Source URL` and `Wayback URL` when `wayback_url` exists. If it does not exist, include `Wayback capture: attempted, failed non-fatally` so the evidence gap is visible.
- Report artifacts must include `wayback_url` or `wayback_save_failed` for every non-IGNORE URL.

This is source-of-record preservation, not content validation. The fetched body remains the analytical input; Wayback is the durable evidence trail.

---

## 2 · Signal schema — 8 labels (Rams #10)

| Label               | Definition                                                                                       | Default Priority |
| ------------------- | ------------------------------------------------------------------------------------------------ | ---------------- |
| **SUPERCHARGE**     | Combine with existing repo strength → 10x moat. **Highest value, often overlooked.**             | P0               |
| **REPOSITION**      | Competitive landscape shift → update value prop / docs / messaging                               | P1               |
| **INSPIRE**         | Build our own superior version                                                                   | P1               |
| **INTEGRATE**       | Wire in as dependency (MCP server, SDK, service)                                                 | P2               |
| **ADOPT**           | Use directly (library, tool, API)                                                                | P2               |
| **FAIL-FAST SPIKE** | Cheap <1-day test to validate before commit                                                      | P2               |
| **RESEARCH**        | Multi-day empirical validation required                                                          | P3               |
| **IGNORE**          | Noise, no portfolio signal. **Terminates pipeline — no agent dispatch, no issue, no cross-map.** | —                |

**Rule of least regret**: SUPERCHARGE vs INSPIRE → pick SUPERCHARGE (10x beats 1x). INSPIRE vs IGNORE → pick IGNORE (noise kills signal).

**Repeat-competitor escalation**: same domain ≥3× across batches (via `hebb.recall(query=DOMAIN, tag="url-insight")`) → auto-add REPOSITION alongside original. "Competitor shipped 3 things this month, that is a market signal."

---

## 3 · URL-type routing (enrichment strategy)

| Type           | Host patterns                                     | Additional enrichment                                                               |
| -------------- | ------------------------------------------------- | ----------------------------------------------------------------------------------- |
| paper          | arxiv.org, doi.org, biorxiv, researchgate, \*.edu | `research` skill: semantic_scholar (citations+influence), openalex (author h-index) |
| product-launch | news.ycombinator.com (Show HN), producthunt.com   | Fetch linked comparison pages / docs                                                |
| repo           | github.com, gitlab.com, codeberg.org              | `gh api /repos/OWNER/NAME` → stars, last-commit, topics, license                    |
| blog           | medium.com, substack.com, \*.dev, personal blogs  | Author credibility (openalex + ror if org-attributed)                               |
| docs           | readthedocs.io, mdbook, _.docs, docs._            | Version + last-updated                                                              |
| benchmark      | paperswithcode.com, llm-leaderboard, openreview   | SOTA delta vs our numbers if repo-adjacent                                          |
| company        | .com home, pitch decks, investor pages            | ror_search_organization → Fortune500/FAANG/indie tier                               |
| other          | anything else                                     | credibility tier only; often IGNORE                                                 |

---

## 4 · Cross-map to portfolio (21 active repos)

**Source of truth**: `~/.claude/data/url-insight/portfolio.json` (generated artifact).
Schema: `{generated_at, source, filter, count, repos: [{name, path, description, tagline, visibility, language, pushed_at, commits_30d, has_claude_md, local_clone}, ...]}`.
Filter: owned (`isFork=false`) + not archived + `pushedAt ≥ CUTOFF` (currently 2026-03-01) + strategic (sites/profile READMEs dropped).

**Refresh** (weekly; stale if `generated_at` > 14 days old):

```bash
python3 ~/.claude/skills/url-insight/refresh_portfolio.py
# — or one-liner —
gh repo list MikkoParkkola --limit 200 --json name,description,isArchived,isFork,pushedAt,visibility,primaryLanguage \
  | python3 -c "import json,sys,pathlib,subprocess,re,datetime; \
raw=json.load(sys.stdin); H=pathlib.Path('/Users/mikko/github'); C='2026-03-01'; \
DROP={'MikkoParkkola','parkkola-website','revaluator-legal','revaluator-website','domain-tracker'}; \
a=[r for r in raw if not r['isArchived'] and not r['isFork'] and (r['pushedAt'] or '')>=C and r['name'] not in DROP]; \
# ... see refresh_portfolio.py for full enrichment logic"
```

**v1 match** (always available): token-overlap between extracted claim and `description` field. Top-3 repos by Jaccard, min-score ≥ 0.15 to qualify.

**v2 match** (when `has_claude_md=true`): grep the local CLAUDE.md (first 2000 chars). Weight v2 match 2× v1.

**Inline Python helper** (Rams #10 — consumes `payload["repos"]`):

```python
import json, re, pathlib
payload = json.loads(pathlib.Path("~/.claude/data/url-insight/portfolio.json").expanduser().read_text())
portfolio = payload["repos"]
def cross_map(claim: str, top_k=3):
    claim_toks = set(re.findall(r'\w{4,}', claim.lower()))
    scored = []
    for r in portfolio:
        desc = (r.get("description") or "").lower()
        v1_toks = set(re.findall(r'\w{4,}', desc))
        v1 = len(claim_toks & v1_toks) / max(len(claim_toks | v1_toks), 1)
        cmd_path = pathlib.Path(r.get("path") or "") / "CLAUDE.md" if r.get("has_claude_md") else None
        v2 = 0.0
        if cmd_path and cmd_path.exists():
            text = cmd_path.read_text(errors="replace")[:2000].lower()
            v2_toks = set(re.findall(r'\w{4,}', text))
            v2 = 2 * len(claim_toks & v2_toks) / max(len(claim_toks | v2_toks), 1)
        score = max(v1, v2)
        if score >= 0.15:
            scored.append((r["name"], score, r["tagline"]))
    return sorted(scored, key=lambda x: -x[1])[:top_k]
```

Empty result → **NOT yet IGNORE**. Run the Linear-active-tickets gate below before discarding.
Staleness guard: if `payload["generated_at"]` > 14d old, warn + trigger refresh before cross-mapping.

**Companion context**: `~/.claude/data/url-insight/portfolio-context.md` holds the strategic + architectural portfolio narrative — cross-repo dep graph, roadmap themes, ADR inventory, companion-bundle pattern, open-issue highlights. Consult when a URL match needs grounding a one-line description cannot provide (e.g. "does this paper relate to the metacognition G4 sibling-wiring gate?").

### 4.1 · Linear active-ticket gate (BLOCKING before IGNORE)

**Lesson learned 2026-04-24 (MIK-2997) and re-surfaced 2026-05-03 (Vision Banana, Vikas Chandra video roadmap)**: portfolio.json one-line descriptions DO NOT tokenize active research-direction keywords. Repos like `botnaut-server` describe themselves at the product level; their actual research scope (multimodal, metacog, world-model, mesh-shard, diffusion-paradigm) lives in CLAUDE.md integration matrices AND in active Linear umbrella tickets (MIK-3209 multimodal, MIK-3094 N3 mesh-shard, MIK-3015 cost plane, MIK-3110 sovereign policy, etc).

**Rule**: BEFORE classifying any URL as IGNORE / IGNORE-WATCHLIST, query Linear for active backlog tickets matching the URL's domain keywords. If any active ticket scope overlaps, classify as REPOSITION / INSPIRE / RESEARCH (with cross-link comment) instead of IGNORE.

**Implementation** (inline, in pipeline step 8):

```python
# After v1+v2 cross-map returns empty/weak, BEFORE classifying as IGNORE:
# Extract 3-5 domain keywords from URL claim (e.g. "multimodal", "video", "diffusion", "vision-foundation-model")
domain_kws = extract_domain_keywords(claim)  # use noun-phrase extraction

# Query Linear for active tickets matching any keyword
# fulcrum:linear_search_issues query="<kw1> OR <kw2> OR <kw3>"
# OR: fulcrum:linear_list_issues teamKey=MIK state=backlog (filter to recent + match in title/description)
matching_tickets = linear_search_active(domain_kws, max_results=5)

if matching_tickets:
    # DO NOT classify as IGNORE. Re-route to:
    # - REPOSITION + INSPIRE if competitor/parallel work to active ticket
    # - RESEARCH if architecture/paradigm signal informing active ticket
    # - Comment on the matching ticket(s), do NOT file new ticket unless scope is genuinely new
    cross_map_result = matching_tickets
    signal = decide_signal(claim, matching_tickets, credibility_tier)
else:
    # Now IGNORE / IGNORE-WATCHLIST is honest
    pass
```

**Active-ticket keyword index** (refresh quarterly; current as of 2026-05-03):

| Domain keyword                                             | Active umbrella tickets                                                     |
| ---------------------------------------------------------- | --------------------------------------------------------------------------- |
| multimodal / video / vision-foundation / VLM / on-device   | MIK-3209 (Qwen Thinker/Talker), MIK-3324 (Vikas roadmap), MIK-3209 sub-area |
| memory / persistent-memory / consolidation                 | MIK-3323 (stash competitor), hebb portfolio                                 |
| cost / budget / token-spend / quota / cost-control         | MIK-3015 (Attested External Cost Plane)                                     |
| sovereignty / sovereign-stack / vendor-lock / closed-API   | MIK-3110, MIK-3011                                                          |
| diffusion / generative-pretraining / RGB-output / Marigold | MIK-3311 (zlab/dflash), MIK-3244 (cumulative axis)                          |
| voice / STT / TTS / speech-tags / WER                      | MIK-3313 (Grok Omni), MIK-3114                                              |
| metacognition / CFSR / continuous-microtraining            | MIK-3243 (LADIR), MIK-3244, botnaut-metacog                                 |
| mesh-shard / N3 / expert-routing / MoE                     | MIK-3094 (N3 EPIC), MIK-3097                                                |
| compute-rationing / capex / GW-scale / EUV                 | MIK-3015 (Lee.aao evidence), MIK-3011                                       |
| attestation / signed-state / .state                        | MIK-2850 (botnaut-attestation), MIK-3110                                    |
| RL / scaling-laws / RLHF / DPO                             | MIK-2997, MIK-3243                                                          |
| quantization / NVFP4 / W4A16 / ParetoQ / KV-cache          | MIK-3324 (cross-link), nvfp4-mojo repo                                      |
| Apple-Metal / on-device-runtime / ExecuTorch               | MIK-3324, bnaut-metal                                                       |

**Refresh cadence**: this index goes stale fast. After each url-insight session, append any new umbrella tickets created. Quarterly: prune closed tickets + re-derive from active backlog.

**Failure mode this prevents**: classifying a paradigm-shift signal as IGNORE because portfolio.json says "botnaut-server: planner-worker-judge agent server" without tokenizing "multimodal" / "video" / "vision-foundation" — the actual active research direction.

---

## 5 · Credibility tier (S/A/B/C/D)

Prevents hype-driven false-positive SUPERCHARGE. Multiplies ROI probability.

| Tier | Factor | Signals                                                                           |
| ---- | ------ | --------------------------------------------------------------------------------- |
| S    | 1.0    | Top venue (NeurIPS/ICML/ICLR/Nature) · FAANG/DeepMind/OpenAI · author h-index >50 |
| A    | 0.8    | arxiv top-1% cited · Fortune500 eng blog · h-index 20-50                          |
| B    | 0.6    | arxiv default · well-known company · h-index 5-20                                 |
| C    | 0.4    | personal blog (established author) · small startup · new PhD                      |
| D    | 0.2    | random substack · anonymous · no verifiable affiliation                           |

Lookups (free, via `research` skill / fulcrum MCP):

- `orcid_get_person` + `openalex_get_author` → h-index
- `ror_search_organization` → org tier
- arxiv category + submission pattern → venue proxy

Default when unresolvable: **C** (0.4). Don't assume S without evidence.

---

## 6 · Agent dispatch by signal (REPLACES simulated personas)

Claude Code has real specialist agents. Dispatch via Agent tool (parallel fan-out, `run_in_background=true`). Don't simulate panels.

| Signal          | Agents (parallel unless noted)                                                                                                          | Token budget |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------- | ------------ |
| IGNORE          | none (skip)                                                                                                                             | 0            |
| ADOPT           | polyglot-code-reviewer                                                                                                                  | ~10K         |
| INTEGRATE       | polyglot-code-reviewer + security-auditor                                                                                               | ~20K         |
| INSPIRE         | innovator + chief-product-officer + fact-checker-investigator                                                                           | ~40K         |
| FAIL-FAST SPIKE | innovator + codex-reasoner (codex designs cheapest kill-test)                                                                           | ~20K         |
| RESEARCH        | market-tech-researcher + codebase-intelligence-researcher + data-detective                                                              | ~40K         |
| SUPERCHARGE     | **SERIAL** innovator (kill/proceed gate) → if proceed: parallel moonshot-architect + business-panel-experts + research patent-prior-art | ~80K         |
| REPOSITION      | business-panel-experts + brand-customer-experience-lead + growth-hacker                                                                 | ~40K         |

**Domain specialist additions** (detect via URL extension / arxiv category / page keywords):

- Rust → rust-excellence-engineer
- Mojo / ML kernels → mojo-ml-architect
- iOS / Android → mobile-native-expert
- Distributed systems claims → distributed-systems-architect
- macOS SwiftUI → macos-swiftui-coder
- Tests / QA → comprehensive-test-engineer

**Each agent receives**: URL + fetched content + cross-map repos + initial claim + credibility tier + hebb-known history.

**Each agent returns** (normalized JSON, enforced via prompt template):

```json
{
  "agent": "AGENT-NAME",
  "verdict": "proceed | kill | iterate",
  "finding": "one sentence",
  "question": "one risk / open item",
  "action": "one concrete next step",
  "confidence": 0.0
}
```

**Synthesis**: majority verdict; ties broken by innovator/ceo/devil. Compose issue body with all panel quotes verbatim under collapsible sections.

### 6.5 · Devil + Steelman (mandatory for every non-IGNORE)

Run as TWO distinct sections in one agent call (mode-switch, don't collapse). Devil alone biases status-quo (incumbents always find 3 reasons to dismiss disruption: x86 → ARM, MySpace → FB, Blockbuster → Netflix). Steelman alone biases FOMO. Together they calibrate.

**Devil's advocate**:

1. 3 reasons NOT to adopt (concrete, not generic).
2. Cheapest way this could be wrong (<1h evidence to kill it).
3. Who tried this and failed (research skill: arxiv negative results, semantic_scholar forward citations, HN criticism search).

**Steelman ("what if they're right?")**:

1. If we started today, would we build differently because of this?
2. What 3 assumptions of OURS would have to be FALSE for them to be right?
3. What evidence are we discarding to keep our current view?
4. Who on our team has stake in dismissing this? (NIH / sunk-cost defender?)
5. Is this the Christensen pattern — disruptive toy dismissed by incumbent until too late?
6. Cost of ignoring IF they're right — asymmetric downside?

**Reconciliation table**:

| Devil              | Steelman | Net action                                                      |
| ------------------ | -------- | --------------------------------------------------------------- |
| kill               | kill     | IGNORE — both agree it's noise                                  |
| kill               | proceed  | **HUMAN-REVIEW tag — NIH bias risk, highest-alpha**             |
| proceed            | proceed  | signal stands, confidence +0.2                                  |
| proceed            | kill     | signal stands but drop one tier (weak novelty)                  |
| iterate            | iterate  | force FAIL-FAST SPIKE (experiment resolves)                     |
| any other mismatch |          | add `mismatch-review` tag, keep original signal, flag in report |

**Mismatch cases are where REAL alpha lives.** Most people only do devil → confirmation bias for status quo. Surface mismatches explicitly.

---

## 7 · DoR-compliant issue template

Pre-fills the 3 DoR signals (testable AC, priority/ROI, reversibility). **Caveat (I)**: gateway-wrapped Linear calls do not fire `dor-issue-guard.py` (linear skill §4). Pre-fill is **self-discipline**.

Template (placeholders marked with `__FIELD__` to avoid stub-detector false positives):

```markdown
## ROI: **NX** | Signal: **LABEL** | Cross-repo target(s): **REPOS**

P**N** — value/cost: $**VAL**/yr / $**COST** → **RATIO**x · Credibility: **TIER**

### Source

- URL: **URL**
- Wayback URL: **WAYBACK_URL_OR_ATTEMPTED_FAILED**
- Author/Org: **AUTHOR_ORG**
- Published: **DATE**
- Fetched: **TODAY**
- Staleness: fresh / DATED Nmo (verify SOTA)

### Main claim

**CLAIM_ONE_SENTENCE**

### Why it matters for us

**VISION_MAPPING** (cite CLAUDE.md line/section if v2)

### Acceptance Criteria (testable)

- [ ] **OUTCOME_1**
- [ ] **OUTCOME_2**
- [ ] **OUTCOME_3**

### Agent panel (§6 — verbatim returns)

<details><summary>innovator</summary>__JSON_BLOCK__</details>
<details><summary>chief-product-officer</summary>__JSON_BLOCK__</details>
<details><summary>fact-checker-investigator</summary>__JSON_BLOCK__</details>

### Devil's advocate (§6.5)

**3 kill-reasons**:

1. **REASON_1**
2. **REASON_2**
3. **REASON_3**
   **Cheapest way wrong**: **ONE_LINE_REFUTATION**
   **Prior failures**: **ARXIV_OR_HN_LINK_OR_NONE**
   **Verdict**: keep / downgrade / IGNORE

### Steelman ("what if they're right?")

**3 assumptions of ours that'd have to be false**:

1. **ASSUMPTION_1**
2. **ASSUMPTION_2**
3. **ASSUMPTION_3**
   **Evidence we'd have to discard**: **ITEMS**
   **Cost of ignoring if right**: $**VAL**/yr asymmetric
   **Christensen-check**: disruptive-for-incumbents? yes/no — **WHY**
   **Verdict**: kill / proceed / iterate

### Reconciliation

- Devil verdict: **V1**
- Steelman verdict: **V2**
- Net action (per §6.5 table): **ACTION**
- Confidence: **PCT**%

### Reversibility / Rollback

yes/no + how — revert commit / feature flag off / N/A because **REASON**

### ROI breakdown

- value: $**VAL**/yr (**JUSTIFICATION**)
- probability: **PCT**% (credibility-adjusted: base × **FACTOR**)
- cost: **K** tokens + **HOURS**h
- score: **NX**

### Approach

adopt-directly / integrate-as-dep / build-superior / fail-fast-spike / empirical-research / reposition

### Next step

**SINGLE_CONCRETE_ACTION**
```

---

## 8 · Filing — delegate to `linear` skill

See `~/.claude/skills/linear/SKILL.md` for full pattern. Summary:

```python
# 1. Read cache (NEVER hardcode UUIDs)
import json, pathlib
cache = json.loads((pathlib.Path.home()/".claude/data/linear/teams-cache.json").read_text())
mik = cache["teams"]["MIK"]

# 2. Map url-insight priority → Linear priority (INVERTED — gotcha from linear §4)
#    url-insight: P0 best, P3 worst    Linear: 0=none 1=urgent 2=high 3=normal 4=low
prio_map = {"P0": 1, "P1": 2, "P2": 3, "P3": 4}

# 3. Labels: ["auto-generated", "url-insight", signal.lower()]
#    + "mismatch-review" / "human-review" if reconciliation flagged
label_ids = [mik["labels"][l] for l in label_keys if l in mik["labels"]]

# 4. Filing rule
#    P0, P1, P2: auto-file
#    P3:        surface in report, ASK user before filing
#    HUMAN-REVIEW tag: ALWAYS surface to user, do NOT auto-file
arguments = {
  "teamId":         mik["id"],
  "title":          f"[ROI:{roi}x] [{signal}] {claim_summary}",
  "description":    dor_body,
  "priority":       prio_map[priority],
  "stateId":        mik["states"]["Backlog"]["id"],
  "labelIds":       label_ids,
  "createAsUser":   "Claude Elite",
  "displayIconUrl": "https://www.anthropic.com/images/icons/safari-pinned-tab.svg",
}
# mcp__gateway__gateway_execute(tool="fulcrum:linear_create_issue", arguments=arguments)
```

For SUPERCHARGE with no prior art found → also add `P0-patent` label + comment tagging `/!:innovate` and `/!:discover`.

---

## 9 · Report format (to user)

**Sorted**: SUPERCHARGE > REPOSITION > INSPIRE > INTEGRATE > ADOPT > FAIL-FAST SPIKE > RESEARCH > IGNORE.

```markdown
## url-insight run — N URLs · TIMESTAMP

| #   | URL (short)      | Signal  | Target repo(s) | Cred | ROI | Devil   | Steelman | Mismatch? | Issue     | Notes                  |
| --- | ---------------- | ------- | -------------- | ---- | --- | ------- | -------- | --------- | --------- | ---------------------- |
| 1   | arxiv/2507.21474 | INSPIRE | hebb           | C    | 30x | proceed | proceed  | no        | (dry-run) | trace-viz narrow scope |
| ... |

### Discarded (IGNORE)

- URL — 1-line reason

### Mismatch / Human-review queue (REVIEW THESE)

- URL — devil=kill steelman=proceed → potential NIH bias, decide manually

### Artifacts

- JSON: ~/.claude/data/url-insight/run-TS.json
- Queue: ~/.claude/data/innovations/queue/HASH.json × N
- Memories: hebb × N (project=url-insight)
- Wayback: archived proof URL per non-IGNORE URL, or non-fatal capture-failure marker
```

---

## 10 · Innovation-tracker queue schema

Emit per actionable insight to `~/.claude/data/innovations/queue/URL_HASH.json`:

```json
{
  "source": "url-insight",
  "url": "FULL_URL",
  "wayback_url": "WAYBACK_URL / null",
  "wayback_save_failed": false,
  "url_hash": "SHA256_PREFIX12",
  "signal": "LABEL",
  "cross_map": ["repo_a", "repo_b"],
  "evidence_url": "PRIMARY_SOURCE_URL",
  "claim": "MAIN_CLAIM",
  "credibility": "S/A/B/C/D",
  "roi": 0,
  "priority": "P0/P1/P2/P3",
  "linear_issue": "MIK-NNN / null",
  "devils_advocate": {
    "kill_reasons": ["r1", "r2", "r3"],
    "cheapest_wrong": "ONE_LINE",
    "prior_failures": "URL / NONE",
    "verdict": "keep / downgrade / ignore"
  },
  "steelman": {
    "false_assumptions": ["a1", "a2", "a3"],
    "discarded_evidence": "ITEMS",
    "asymmetric_cost": 0,
    "christensen": "yes/no",
    "verdict": "kill / proceed / iterate"
  },
  "reconciliation": {
    "net_action": "STRING",
    "confidence": 0.0,
    "mismatch": false
  },
  "agents": [
    { "agent": "NAME", "verdict": "V", "finding": "...", "action": "..." }
  ],
  "timestamp": "RFC3339"
}
```

Matches `innovation-tracker.sh` schema (see `~/.claude/hooks/PostToolUse/innovation-tracker.sh`). `/!:innovate` rescores alongside git-commit innovations.

---

## 11 · Worked example — arxiv/2507.21474 (V, fetched 2026-04-24)

Input: `https://arxiv.org/abs/2507.21474`

**Dedup**: `hebb.recall("2507.21474", project="url-insight")` returned 0 hits — first analysis. Proceed.

**Fetch**: `nab fetch https://arxiv.org/abs/2507.21474` returned 47,705 bytes in 677ms (cookies auto-loaded). Status 200.

**Wayback capture**: `fulcrum:wayback_save` with 7d idempotence and screenshot capture returns a timestamp URL or a pending job. The run JSON records `wayback_url` when available, otherwise `wayback_save_failed=true` plus a sanitized reason. This does not block the rest of the analysis.

**Classify**: `paper` (arxiv.org).

**Route**: research skill → semantic_scholar citation count, openalex author h-index.

**Extract**:

- Claim: ENN (Engram Neural Network) is an RNN with an explicit differentiable Hebbian memory matrix + sparse attention-driven retrieval.
- Differentiator: interpretability via observable memory dynamics (trace visualizations); accuracy comparable to GRU/LSTM on MNIST/CIFAR-10/WikiText-103.
- Numbers: 3 benchmarks; converges to similar accuracy/perplexity as baselines (no SOTA claim).
- Date: 2025-07-29 (9mo old, fresh).
- Surprise: LOW on idea (Hebbian+NN is classic territory); MEDIUM on execution (explicit differentiable memory + sparse retrieval + transparency angle in one architecture).

**Credibility**: Daniel Szelogowski — solo author, no org listed on abstract page, arxiv-only venue → tier **C** (factor 0.4).

**Cross-map**:

- v2 (hebb/CLAUDE.md exists): keywords {memory, hebbian, engram, recall, decay, plasticity, neuroscience, trace} overlap strongly → top match is **hebb**.
- v1 candidates (weaker): botnaut-server (neural inference), metacognition (reasoning).
- Verified: hebb's CLAUDE.md line 5 reads "world-class memory system built to the quality bar" + "Hebbian synaptic plasticity, surprisal gating, reconsolidation".

**Initial signal**: between INSPIRE and SUPERCHARGE.

- hebb already has BGE-M3 + HNSW + BM25 + RRF, thermodynamic decay, surprisal gating — far more sophisticated than ENN's memory matrix.
- ENN's training-time differentiable memory + observable trace visualizations is the novel piece hebb doesn't have.
- Not SUPERCHARGE because ENN's core mechanism is weaker than hebb's existing stack — no 10x moat.
- Initial classification: **INSPIRE** (build superior version of trace-viz angle).

### Agent dispatch (INSPIRE budget ~40K)

Three agents in parallel, each receives URL + fetched abstract + cross-map=[hebb] + credibility=C:

```
innovator                  -> "proceed", finding: "trace-viz interpretability is net-new for hebb, narrow scope means low-cost spike", action: "branch hebb-trace-viz, render activation heatmap"
chief-product-officer      -> "proceed", finding: "interpretability is a stated hebb moat — visualizing recall decisions strengthens the I1 trust-chain story", action: "ship as feature behind --debug flag"
fact-checker-investigator  -> "iterate", finding: "no peer review, no GitHub link, 0 citations checked via semantic_scholar — high reproducibility risk", action: "verify ENN claims with PyTorch reimpl before adopting any architecture"
```

Synthesis: 2 proceed + 1 iterate → proceed with iterate-modifier (narrow scope further).

### Devil's advocate

1. Solo-author arxiv-only paper, no peer review, no GitHub code linked — reproducibility risk HIGH.
2. "Comparable to RNN/GRU/LSTM" is not a SOTA win. 2026 baselines are Transformers/Mamba/Hyena — ENN isn't benchmarked against them.
3. Training-time Hebbian memory is not hebb's inference-time architecture — different problem class, trace-viz concept may not transfer.

Cheapest wrong: 1h semantic_scholar check — if 0 citations 9mo post-submission, signal dies.
Prior failures: Hebbian-in-NN graveyard (Hopfield nets, Kanerva memory, DNC). Must explain how ENN avoids DNC's read-head-inspection-nobody-uses failure mode.
**Devil verdict**: proceed (no kill-class evidence; risks are about scope, not feasibility).

### Steelman ("what if they're right?")

1. **3 assumptions of ours that'd have to be false**:
   - "hebb's BGE-M3 + HNSW retrieval is sufficient for interpretability" — false if users can't trace WHY a memory ranked top-3.
   - "Surprisal-gate logs are enough debugging telemetry" — false if a visual heatmap surfaces patterns logs hide.
   - "Inference-time vs training-time memory are unrelated" — false if ENN's trace mechanism transfers as a debugging UI primitive regardless of when memory was learned.
2. **Evidence we'd discard**: hebb users repeatedly ask "why did this memory rank?" in issues/Slack — log-based answers are unsatisfying.
3. **Asymmetric cost if right**: ~$5K/yr in user trust + a sticky differentiator vs Mem0/MemGPT competitors.
4. **Christensen-check**: YES — interpretability is the kind of "toy feature" incumbents (vector DB vendors) dismiss until users defect to the visible-thinking alternative.

**Steelman verdict**: proceed.

### Reconciliation

- Devil = proceed, Steelman = proceed → **signal stands, confidence +0.2** (INSPIRE confirmed, no mismatch).
- Net action: INSPIRE — narrow-scope trace-viz spike.
- Confidence: 70%.

### ROI

- value: $5K/yr (hebb recall debugging UX, interpretability feature, Christensen-defense).
- probability: 30% × 0.4 (credibility C) = 12% — but +0.2 from clean reconciliation → about **25%**.
- cost: ~2K tokens (fetched) + 4h dev for spike.
- score: ($5000 × 0.25) / ~$40 compute equiv ≈ **30x** → **P2**.

### Composed issue body (would be filed, not actually sent)

```markdown
## ROI: 30x | Signal: INSPIRE | Cross-repo target(s): hebb

P2 — value/cost: $5000/yr / $40 → 30x · Credibility: C

### Source

- URL: https://arxiv.org/abs/2507.21474
- Wayback URL: WAYBACK_URL_OR_ATTEMPTED_FAILED
- Author/Org: Daniel Szelogowski (solo, no affiliation)
- Published: 2025-07-29
- Fetched: 2026-04-24
- Staleness: fresh (9mo)

### Main claim

Engram Neural Network (ENN): RNN variant with explicit differentiable Hebbian memory matrix + sparse attention retrieval, achieving comparable accuracy to GRU/LSTM on MNIST/CIFAR-10/WikiText-103 while offering observable memory dynamics (trace visualizations) for interpretability.

### Why it matters for us

hebb's vision (CLAUDE.md line 5) is a "world-class memory system built to the quality bar". hebb has superior retrieval (BGE-M3 + HNSW + BM25 + RRF) vs ENN's matrix, but lacks ENN's trace-visualization angle. The interpretability-via-observable-dynamics idea could become a hebb recall-debugging UI feature — net-new, not a retrofit of our memory core.

### Acceptance Criteria (testable)

- [ ] Render memory activation heatmap for a single hebb recall query on a sample WikiText prompt.
- [ ] Overlay surprisal-gate decisions on the heatmap (green=stored, red=suppressed).
- [ ] Produce 1 debugging insight not previously visible (e.g. "stored memory N had near-zero activation but ranked top-3 — why?").

### Agent panel (verbatim)

<details><summary>innovator</summary>{"verdict":"proceed","finding":"trace-viz interpretability is net-new for hebb, narrow scope means low-cost spike","action":"branch hebb-trace-viz, render activation heatmap","confidence":0.7}</details>
<details><summary>chief-product-officer</summary>{"verdict":"proceed","finding":"interpretability is a stated hebb moat, visualizing recall decisions strengthens the I1 trust-chain story","action":"ship as feature behind --debug flag","confidence":0.65}</details>
<details><summary>fact-checker-investigator</summary>{"verdict":"iterate","finding":"no peer review, no GitHub link, citation count not yet verified, high reproducibility risk","action":"verify ENN claims via reimplementation before adopting architecture","confidence":0.55}</details>

### Devil's advocate

1. Solo-author arxiv-only paper, no peer review, no GitHub linked — reproducibility risk HIGH.
2. "Comparable to RNN/GRU/LSTM" is not a SOTA win. 2026 baselines are Transformers/Mamba — ENN isn't benchmarked against them.
3. Training-time Hebbian memory is not hebb's inference-time memory — trace-viz concept may not transfer.
   **Cheapest wrong**: 1h semantic_scholar check — if 0 citations 9mo post-submission, signal dies.
   **Prior failures**: Hebbian-in-NN graveyard (Hopfield, Kanerva, DNC). Must explain how trace-viz beats DNC's read-head inspection (which nobody uses).
   **Verdict**: proceed (risks are scope, not feasibility).

### Steelman ("what if they're right?")

**3 assumptions of ours that'd have to be false**:

1. hebb's BGE-M3 + HNSW retrieval is sufficient for interpretability.
2. Surprisal-gate logs are enough debugging telemetry.
3. Inference-time vs training-time memory are unrelated.
   **Evidence discarded**: hebb users repeatedly ask "why did this memory rank?" — logs are unsatisfying.
   **Cost of ignoring if right**: ~$5K/yr in user trust + sticky differentiator vs Mem0/MemGPT.
   **Christensen-check**: YES — interpretability is a "toy feature" incumbents (vector DB vendors) dismiss until users defect.
   **Verdict**: proceed.

### Reconciliation

- Devil verdict: proceed
- Steelman verdict: proceed
- Net action: signal stands, confidence +0.2 → INSPIRE confirmed, narrow-scope trace-viz spike.
- Confidence: 70%

### Reversibility / Rollback

Yes — spike on `hebb-trace-viz` branch. No merge if spike fails. Revert = delete branch. Zero prod impact.

### ROI breakdown

- value: $5K/yr (hebb recall debugging UX, Christensen-defense)
- probability: 25% (12% base × +0.2 reconciliation bonus)
- cost: ~2K tokens + 4h dev
- score: 30x

### Approach

fail-fast-spike — 1-day implementation on `hebb-trace-viz` branch.

### Next step

Branch `hebb-trace-viz` off main. Implement activation heatmap for `hebb recall "<sample query>"`. Eyeball debugging value. Kill / promote within 4h.
```

### Report table (single-URL run)

```markdown
| #   | URL              | Signal  | Target | Cred | ROI | Devil   | Steelman | Mismatch? | Issue     | Notes                  |
| --- | ---------------- | ------- | ------ | ---- | --- | ------- | -------- | --------- | --------- | ---------------------- |
| 1   | arxiv/2507.21474 | INSPIRE | hebb   | C    | 30x | proceed | proceed  | no        | (dry-run) | trace-viz narrow scope |
```

### Counterfactual SUPERCHARGE path (illustrative)

If credibility had been S (e.g. DeepMind authors, NeurIPS venue) AND the paper had introduced a memory primitive hebb lacks (not just trace-viz):

1. Initial signal would have been **SUPERCHARGE** (their idea × hebb's existing strength = 10x moat).
2. SERIAL dispatch: innovator first. If kill → drop to INSPIRE.
3. If innovator proceeds → parallel fan-out: moonshot-architect + business-panel-experts + research-skill patent-prior-art search.
4. If patent-prior-art returns empty → escalate to **P0 [PATENT]**, tag `/!:innovate` `/!:discover`.
5. If prior art exists → downgrade to INSPIRE, name competitor.

Token budget would have been ~80K vs the ~40K spent here.

---

## 12 · Worked example 2 — product launch (illustrative, not fetched)

Input: `https://www.producthunt.com/posts/mem0-long-term-memory-for-ai-agents`

Classify: `product-launch`. Route: fetch comparison page if linked, credibility via ror on Mem0 Inc.

Cross-map: **hebb** (direct competitor, long-term memory for AI agents).

Initial signal: **REPOSITION** (hebb must update value prop vs Mem0 messaging). Possible secondary **INSPIRE** (what does Mem0 do that hebb doesn't?).

Agent dispatch (REPOSITION ~40K): business-panel-experts + brand-customer-experience-lead + growth-hacker.

Devil must include: "Is Mem0 actually better on LoCoMo? hebb leads LoCoMo #1 per CLAUDE.md — if Mem0's numbers are worse, this is REPOSITION (lead with our win), not threat."
Steelman must include: "What if Mem0's distribution wins regardless of benchmark? Worse-but-marketed beats better-but-quiet (Christensen)."

---

## 13 · Anti-patterns (NEVER)

1. **NEVER use WebFetch** — ~50K token waste per call (25× nab overhead). nab MCP preferred, nab CLI fallback.
2. **NEVER skip devil's-advocate AND steelman** — both, every non-IGNORE. Devil alone = status-quo bias. Steelman alone = FOMO bias. Together = calibrated.
3. **NEVER auto-file P3 issues without user confirmation** — surface in report, await "yes file it".
4. **NEVER auto-file HUMAN-REVIEW reconciliation cases** — these are the highest-alpha mismatches; user must decide.
5. **NEVER miss the SUPERCHARGE case** — highest-value label AND easiest to under-classify. When a URL maps to a repo you already lead in, ask: "their idea × our strength = 10x moat?" If yes → SUPERCHARGE → P0.
6. **NEVER skip hebb dedup** — analyzing same URL twice in a week is a protocol failure.
7. **NEVER hardcode Linear UUIDs** — always read `~/.claude/data/linear/teams-cache.json` (linear skill §3).
8. **NEVER assume S-tier credibility without evidence** — default C when unresolvable.
9. **NEVER pre-fill "to be determined" in DoR sections** — if you can't answer, downgrade signal until you can.
10. **NEVER let IGNORE silently leak into actionable** — IGNORE terminates the pipeline, period. No agent dispatch, no cross-map, no issue, no queue entry. One line in the report.
11. **NEVER simulate panel personas (CTO/CPO/CMO) inline** — DISPATCH to real agents via Agent tool. Simulated personas add tokens without independent perspectives.
12. **NEVER skip innovator's serial-gate for SUPERCHARGE** — kill-verdict at gate saves the ~60K parallel panel cost.
13. **NEVER auto-spike without user consent** unless `--auto-spike` explicitly present.
14. **NEVER exceed 50 URL or 2M token batch cap** — hard halt, surface partial results.

---

## 14 · Cross-refs

| Skill / hook / MCP                                  | Why                                                                                                                             |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `~/.claude/skills/linear/SKILL.md`                  | Issue filing; DoR template; Linear priority inversion gotcha                                                                    |
| `~/.claude/skills/research/SKILL.md`                | arxiv / semantic_scholar / openalex / patent prior-art                                                                          |
| `~/.claude/skills/supercharge/SKILL.md`             | Invoked when signal=SUPERCHARGE — runs the 11-phase synthesis pipeline (DECOMPOSE → ... → TELEMETRY) instead of file-and-forget |
| `~/.claude/skills/wayback/SKILL.md`                 | Source-of-record preservation; URL triage must call `fulcrum:wayback_save` after successful fetch                               |
| `~/.claude/skills/apple-calendar/SKILL.md`          | If URL implies a calendar event ("I'm attending X") — rare                                                                      |
| `~/.claude/skills/ctx-mgmt/SKILL.md`                | Session snapshot after large runs                                                                                               |
| `nab` MCP / CLI                                     | URL fetch (PREFER over WebFetch)                                                                                                |
| `mcp__hebb__{recall,remember}`                      | Dedup + cross-session insight graph                                                                                             |
| `Agent` tool                                        | Real specialist dispatch (replaces simulated personas)                                                                          |
| `~/.claude/hooks/PostToolUse/innovation-tracker.sh` | Downstream consumer of queue entries                                                                                            |
| `~/.claude/hooks/PreToolUse/dor-issue-guard.py`     | DoR gate (does NOT fire on gateway-wrapped Linear calls)                                                                        |
| `/!:innovate` `/!:discover`                         | Patent-pipeline escalation for SUPERCHARGE + no-prior-art                                                                       |

---

## 15 · Files

| Path                                                           | Purpose                                                                                                 |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| `~/.claude/skills/url-insight/SKILL.md`                        | this file                                                                                               |
| `~/.claude/skills/url-insight/session-learnings-2026-05-02.md` | session learnings: portfolio facts, hook gotchas, fan-out preamble — MUST read before fresh-session run |
| `~/.claude/data/url-insight/portfolio.json`                    | cached `gh repo list` (weekly refresh)                                                                  |
| `~/.claude/data/url-insight/run-TIMESTAMP.json`                | per-run JSON artifact                                                                                   |
| `~/.claude/data/innovations/queue/HASH.json`                   | innovation-tracker queue entry                                                                          |

---

_url-insight v2026.05.05 (adds §1.1 Wayback evidence capture for MIK-3354) + session-learnings-2026-05-02 — 8 signals, real-agent panels, devil + steelman calibrated, 0 hype, Rams #10. Paste URLs → ranked actions in one pass. Fresh-session runs MUST read `session-learnings-2026-05-02.md` before classification (portfolio facts, hook gotchas, fan-out preamble). §4.1 BLOCKING: query Linear active tickets before classifying as IGNORE. §1.1 BLOCKING for non-IGNORE evidence: record `wayback_url` or `wayback_save_failed`._
