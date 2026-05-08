---
name: oreilly
version: 2026.05.05
effort: low
triggers: oreilly|o'reilly|safari|safari books|learning platform|practitioner book|production guide|safari online|book on X|ISBN lookup|video course|tutorial book|manning book|packt|pearson|microsoft press|live event learning|hands-on guide
description: >
  O'Reilly Learning Platform search routing. Two tools wrapped via
  mcp-gateway: chapter-level (v2) and whole-book (v1) search across
  60K+ books, 100K+ video segments, 200K+ live events. Coverage spans
  Manning, Packt, Pearson, Apress, Wiley, Microsoft Press, MIT Press,
  IBM Redbooks. Use as the practitioner-grade complement to academic
  research (arxiv/semantic-scholar) when the question is "how do
  people actually build/operate X" rather than "what's the SOTA".
---

# O'Reilly Learning Platform Skill

## Decision tree

```
Q: "What's the canonical book on X?"        → fulcrum:oreilly_books_search (v1)
Q: "Find chapters/segments about X"         → fulcrum:oreilly_search (v2)
Q: "ISBN of <title>"                        → fulcrum:oreilly_books_search
Q: "Latest 2025 books on rust async"        → fulcrum:oreilly_search + sort=date
Q: "Manning books on LLM systems"           → fulcrum:oreilly_search + publishers
Q: "Hands-on patterns for KV cache"         → fulcrum:oreilly_search
Q: "Compare academic vs practitioner view"  → run alongside research skill
```

## High-leverage workflows

### 1. Practitioner counterpart to academic research

When `research` skill returns a SOTA paper, run a parallel
`oreilly_search` for the same topic. The arxiv view tells us where
the frontier is; the O'Reilly view tells us what's already in
production books / vendor-shipped patterns.

```
research (arxiv + S2)            → "what's novel?"
oreilly_search (parallel)        → "what's the deployed practice?"
gap between them                 → moat opportunity
```

### 2. ISBN/citation enrichment

```python
gateway_execute("fulcrum:oreilly_books_search", {
  "query": "<title> <first-author-lastname>",
  "limit": 3
})
# returns ISBN-13 + canonical archive_id for citation graphs
```

### 3. Topic landscape mapping

```python
# Get format facets + topic facets in one call
res = gateway_execute("fulcrum:oreilly_search", {
  "query": "kv cache attention",
  "limit": 10,
  "sort": "date"
})
# res["facets"] -> top topics, top publishers, format breakdown
```

### 4. Pre-arxiv expert knowledge

Many architecture lessons (production reliability, ops patterns,
cost-engineering, hardware tradeoffs) are book-only. Books predate
arxiv coverage in: SRE, observability, cost engineering, security
operations, distributed systems engineering. Use this skill when
the topic is operational, not novel.

## Auth

`X-API-Key: $OREILLY_API_KEY`. Stored in:

- `~/.claude/secrets.env`
- `~/.mcp-gateway/.env`
- 1Password "Claude Elite" → "O Reilly Learning Platform API"

Recovery:

```bash
op item get "O Reilly Learning Platform API" --vault="Claude Elite" --fields credential --reveal
```

## Tool surface

| Tool                           | Endpoint          | When                                          |
| ------------------------------ | ----------------- | --------------------------------------------- |
| `fulcrum:oreilly_search`       | `/api/v2/search/` | Chapter-level, format facets, recency-tunable |
| `fulcrum:oreilly_books_search` | `/api/v1/search/` | Whole-book, ISBN, full description            |

## Confidence

V (verified live): both endpoints smoke-tested 2026-05-04 against
gateway `24ea7b31`+. v2 returned 108 hits for "hebbian memory";
v1 returned ISBN 9781098166304 for Chip Huyen's _AI Engineering_.

## Composition cross-links

- **Upstream**: `research` (parallel pattern A: arxiv + S2 + oreilly)
- **Downstream**: `linear` (citation enrichment on tickets)
- **Sibling**: `wayback` + `ia` (when source is offline / open-archive)
- **Replaces**: ad-hoc oreilly.com browser searches

## Versioning

- 2026.05.05 — initial 2-tool wrap. v2 chapter + v1 books.
