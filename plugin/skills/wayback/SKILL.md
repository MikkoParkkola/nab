---
name: wayback
version: 2026.05.05
effort: low
triggers: wayback|wayback machine|web archive|archive.org snapshot|link rot|archived url|cdx|save page now|spn2|snapshot history|when was this page|preserved version|forensic timeline|page disappeared|404 dead link|prior art evidence|durable evidence
description: >
  Internet Archive Wayback Machine routing. Detects link rot, fetches
  closest snapshots, runs CDX timeline analysis, and submits new
  captures via Save Page Now (SPN2). All routes through mcp-gateway
  fulcrum tools (free for read; auth-aware for higher SPN2 limits).
  Paired skill: `ia` (full archive.org item upload/download/search via
  CLI). Complements: research, url-insight, supercharge.
---

# Wayback Machine Skill

Five tools wrapped via `mcp-gateway`. All anonymous; `wayback_save`
optionally uses `OREILLY_API_KEY`-style auth via `IA_ACCESS_KEY` +
`IA_SECRET_KEY` for the higher rate ceiling.

## Decision tree

```
Q: "Is this URL archived?"            → fulcrum:wayback_availability
Q: "Show me snapshots of X over time" → fulcrum:wayback_cdx
Q: "Has this page changed?"           → fulcrum:wayback_cdx (collapse=digest)
Q: "Capture this URL now"             → fulcrum:wayback_save
Q: "What new items appeared?"         → fulcrum:internet_archive_changes
Q: "Pull the metadata of an item"     → fulcrum:internet_archive_metadata
Q: "Search the IA catalog"            → fulcrum:internet_archive_search
Q: "Upload/download with `ia` CLI"    → use sibling skill `ia`
```

## High-leverage workflows

### 1. Patent-grade evidence locking (MIK-3344 line)

```python
# Lock in adversarial-resistant proof at triage time
gateway_execute("fulcrum:wayback_save", {
  "url": "<source>",
  "capture_screenshot": "1",
  "if_not_archived_within": "1d"  # idempotent
})
# job returns; then 30 sec later:
gateway_execute("fulcrum:wayback_availability", {
  "url": "<source>",
  "timestamp": "<job-time>"
})
# returns court-admissible Wayback URL with timestamp
```

### 2. Paper-revision detection across prior-art portfolio (MIK-3355)

```python
# Weekly diff for each tracked arxiv paper:
rows = gateway_execute("fulcrum:wayback_cdx", {
  "url": "arxiv.org/abs/<id>",
  "collapse": "digest",       # one row per unique content hash
  "from": "<last-check-ts>",
  "output": "json"
})
# different digest = paper was revised; alert + re-read body
```

Operational contract:

1. Keep the tracked paper list in
   `~/.claude/data/portfolio/cdx_paper_diff_config.json`.
2. Run `~/.claude/data/portfolio/cdx_paper_diff.py` weekly. The script
   calls `fulcrum:wayback_cdx` via the local `mcp-gateway` CLI using
   `collapse: "digest"` and `from: <last-check-ts>` for each configured
   arXiv ID.
3. Persist state in `~/.claude/data/portfolio/cdx_paper_diff_state.json`.
   First run is a baseline and must not alert on newly observed digests.
4. On later runs, a digest change emits a Linear comment on the configured
   patent/prior-art ticket. If a paper is intentionally untracked, enable
   `linear.create_untracked` in the config to file a new MIK ticket.
5. Schedule manifest:
   `~/.claude/data/portfolio/cdx_paper_diff_schedule.json` uses cron
   expression `0 9 * * 1` (Mondays 09:00 UTC, Mondays 12:00 Helsinki
   during UTC+03) for remote-agent scheduling.
6. If `failed_or_empty / papers` exceeds `max_error_rate` (default 5%),
   treat the run as needing fallback to slower per-paper
   `wayback_availability` probes before trusting the weekly digest gate.

### 3. Link-rot prophylaxis at URL-triage

Wire into `url-insight` skill: every fetched URL also gets
`wayback_save({if_not_archived_within: "7d"})`. Cheap insurance
against rot.

### 4. Forensic timeline of a domain

```python
gateway_execute("fulcrum:wayback_cdx", {
  "url": "<competitor.com>",
  "matchType": "host",
  "filter": "statuscode:200,mimetype:text/html",
  "collapse": "timestamp:8",   # one snapshot per day
  "limit": 100
})
```

## Auth tiers

| Capability                               | Anonymous      | With IA keys |
| ---------------------------------------- | -------------- | ------------ |
| availability/CDX/metadata/search/changes | ✅ full access | same         |
| `wayback_save` sync                      | ~5/min         | ~30/min      |
| `wayback_save` async + outlinks          | ❌             | ✅           |

Keys live in `~/.claude/secrets.env` + `~/.mcp-gateway/.env` +
1Password "Claude Elite" → "Internet Archive S3 Keys".
Recovery: `op item get "Internet Archive S3 Keys" --vault="Claude Elite" --fields username,credential --reveal`

## Confidence levels

V (verified ≥2 sources): all 5 tools pass live smoke tests
2026-05-04 against gateway commit 24ea7b31.
Live trace IDs: `gw-a6b8578b…` (auth), `gw-beac5e7c…` (search),
`gw-f84d661d…` (auth save).

## Composition cross-links

- **Upstream**: `url-insight` (auto-snapshot URLs at triage)
- **Downstream**: `linear` (file evidence URL on patent tickets)
- **Sibling**: `ia` (CLI-based upload/download via `internetarchive`
  package — installed via `internet-archive-skills` plugin)
- **Replaces**: nothing — pure additive layer

## Versioning

- 2026.05.05 — initial 5-tool wrap. CDX digest-diff workflow validated.
