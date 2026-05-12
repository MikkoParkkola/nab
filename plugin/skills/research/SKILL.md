---
name: research
version: 2026.05.05
effort: medium
triggers: arxiv|paper|research paper|citation|SOTA|prior art|doi|literature|literature review|preprint|study|publication|h-index|scholarly|academic|semantic scholar|openalex|pubmed|europepmc|crossref|dblp|unpaywall|internet archive|oreilly|find papers|who cites|survey of|novel claim|orcid|ror
description: >
  Academic / literature research orchestrator. Routes intent -> best free tool first
  (arxiv, semantic_scholar, openalex, crossref, europepmc, pubmed, unpaywall,
  orcid, ror, dblp, openlibrary, internet_archive_search, oreilly_search),
  escalates to paid only when free < 3 hits.
  Auto-picks for "find papers on X", "prior art", "SOTA map", "who cites Y",
  "OA PDF for DOI", "author disambiguation", "literature review". NEVER WebFetch.
composes_with: autoresearch, elite-loop, "!:discover"
---

# Research Skill — Academic & Literature Orchestrator

> Replaces the 1-line `Academic: context7 | arXiv` routing in `core-minimal.md`
> with a decision matrix and tested chain patterns.
> All tools listed are **FREE** unless marked `$`. All via `mcp__gateway__gateway_execute`,
> server prefix `fulcrum:`.

## Prime directive

```
free-first -> escalate only when free<3 hits | NEVER WebFetch (~50K/call waste)
<3 hits on free  -> try exa_search($0.005, semantic)
<3 hits on exa   -> parallel_search($0.004, multi-hop)
still nothing    -> widen query / drop filters before paying more
```

## Research-type -> best-tool matrix (V evidence tested 2026-05-05)

| #   | Intent                                                       | Primary (FREE)                                                                                                                   | Secondary                                                            | Why                                                                                                                                   | Conf |
| --- | ------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- | ---- |
| 1   | Fresh preprints (last 6mo ML/CS)                             | `arxiv_search` sortBy=submittedDate                                                                                              | `semantic_scholar` year=2025-2026                                    | arxiv indexes preprints day-0; S2 lags ~weeks                                                                                         | V    |
| 2   | Citation graph / influence                                   | `semantic_scholar` fields=citationCount                                                                                          | `openalex_search_works`                                              | S2 returns clean citationCount inline                                                                                                 | V    |
| 3   | Specific author's works                                      | `openalex_get_author`                                                                                                            | `semantic_scholar` + name                                            | OpenAlex returns h-index + works_count + ORCID in one call; S2 needs paper->author hop                                                | I    |
| 4   | OA PDF for a DOI                                             | `unpaywall_find_open`                                                                                                            | `europepmc_search` (for biomed)                                      | Unpaywall is THE canonical "is this free?" DB (~50M); does NOT index arXiv DOIs                                                       | V    |
| 5   | Biomedical / clinical                                        | `europepmc_search`                                                                                                               | `pubmed_search`+`pubmed_fetch_article`                               | EPMC returns hasPDF/inEPMC/OA inline; PubMed only gives PMID list                                                                     | V    |
| 6   | CS-specific (established work)                               | `semantic_scholar`+`arxiv_search`                                                                                                | `dblp_search`                                                        | DBLP only indexes formally-published venues (0 hits on "Hebbian LLM memory" 2026-04-24); trap for fresh queries                       | V    |
| 7   | Math-specific                                                | `arxiv_search` cat:math.\*                                                                                                       | `crossref_search_works`                                              | arXiv is the math preprint standard; crossref for published journals                                                                  | I    |
| 8   | Historical / archived / pre-arXiv terminology                | `internet_archive_search`                                                                                                        | `openlibrary_search` (books) / `crossref_search_works` (DOI lineage) | IA finds archived texts, old arXiv mirrors, proceedings, patents, and pre-current-keyword material; OpenLibrary is book metadata only | V    |
| 9   | Multi-hop "cites X AND disputes Y"                           | `parallel_search` ($0.004)                                                                                                       | `exa_search` type=neural                                             | parallel_search built for chained multi-hop reasoning                                                                                 | I    |
| 10  | Prior art (patent-claim grade)                               | chain: `arxiv_search` + `semantic_scholar` + `internet_archive_search` + `exa_search`                                            | `parallel_search`                                                    | No single source; must triangulate preprint+published+archived web/text                                                               | I    |
| 11  | SOTA map of a whole field                                    | chain: `arxiv_search` (fresh) + `semantic_scholar` (influential, top-k by citations) + `openalex_search_works` (concept breadth) | `exa_search` fill gaps                                               | 3 axes: recent / influential / conceptual cluster                                                                                     | V    |
| 12  | Novel-claim validation (cross-discipline)                    | `openalex_search_works` (concepts graph)                                                                                         | `semantic_scholar`                                                   | OpenAlex concepts span 250M works + cross-field topics                                                                                | I    |
| 13  | Author disambiguation / institution                          | `orcid_get_person` + `ror_search_organization`                                                                                   | `openalex_get_author`                                                | ORCID=canonical ID, ROR=canonical institution                                                                                         | I    |
| 14  | Production engineering pattern / practitioner implementation | `oreilly_search`                                                                                                                 | `oreilly_books_search` for book-level bibliography                   | O'Reilly surfaces book/chapter-level implementation guidance and operator patterns that paper indexes often omit                      | V    |

Confidence: V=tested today with real query | I=schema-level argument | A=assumption.

## 3 chain patterns (Rams #10 — less but better)

Pick one of these by intent. Default is pattern A.

### A. "Find papers on X" / survey (DEFAULT for 90% of asks)

```
1. fulcrum:arxiv_search  {search_query: "all:X AND all:Y", sortBy: "submittedDate", max_results: 10}
2. fulcrum:semantic_scholar {query: "X Y", limit: 10, fields: "title,authors,year,citationCount,externalIds,abstract"}
3. Merge: dedupe by DOI/ArXiv ID. Sort by (year desc, citationCount desc).
4. If historical, pre-arXiv, old terminology, or prior-art adjacent:
   fulcrum:internet_archive_search {q: "mediatype:texts AND (\"X\" OR \"Y\")", rows: 10, fl: "identifier,title,creator,date,mediatype,downloads,subject", sort: "downloads desc"}
5. If the ask includes production engineering, agent/product implementation, operational memory, deployment, eval, or system-design practice:
   fulcrum:oreilly_search {query: "X Y", formats: "book", limit: 10, sort: "relevance"}
6. Skip O'Reilly for pure citation counting, DOI/OA lookup, author disambiguation, or biomed-only searches; skip IA for fresh-only queries with a tight recent-year filter.
7. If free scholarly + IA/O'Reilly fanout still returns < 3 useful hits: fulcrum:exa_search {query: "...", category: "research_paper", num_results: 5}
```

### B. "Prior art" / SOTA map

```
1. A + openalex_search_works {search: "X", per_page: 10, filter: "publication_year:2023-2026", select: "id,title,publication_year,doi,cited_by_count,authorships"}
   (always pass 'select' — default response is ~35KB/result from inverted-index abstract)
2. For top-5 influential: semantic_scholar citations graph (paperId -> references + citations).
3. If patent-grade: add internet_archive_search for archived/pre-current-keyword evidence before paid web search.
4. If still under-covered: add parallel_search for non-paper web evidence.
```

### C. "Who is author X" / "I have DOI Y"

```
Author:   orcid_get_person <ORCID>  ||  openalex_get_author <id_or_ORCID>
DOI->OA:  unpaywall_find_open <doi> <email>   (NOT for arXiv DOIs — 404)
DOI->md:  crossref_get_doi <doi>
```

## Parameter gotchas (save 5-10 rediscovery calls)

```
arxiv_search          -> search_query (NOT query),  sortBy, sortOrder, max_results
dblp_search           -> q (NOT query),             h (NOT limit)
openalex_search_works -> search (NOT query),        per_page, filter, select (USE IT)
pubmed_search         -> term (NOT query),          retmax, sort
semantic_scholar      -> query, limit, fields, year
europepmc_search      -> query, pageSize, cursorMark
crossref_search_works -> query, rows, filter
exa_search            -> query, category (research_paper), num_results, type
internet_archive_search -> q (Lucene), rows, fl, sort; use mediatype:texts for papers/books/proceedings
oreilly_search        -> query, formats, limit, page, sort; use formats=book for book/chapter results
oreilly_books_search  -> query, limit, page; use for whole-book bibliography/ISBN, not narrow chapter evidence
unpaywall_find_open   -> doi, email REQUIRED; use parm@iki.fi; fails on 10.48550/arXiv.* (use publisher DOI)
openalex: pass `select: "id,title,publication_year,doi,cited_by_count,authorships"` or response is ~35KB/hit
```

## Output format (citation-ready)

One line per hit. Year desc. Drop duplicates by DOI/ArXiv ID.

```
<Title> — <Authors> (<Year>) arXiv:<id> | doi:<doi> | cites:<n>
```

Example:

```
Hebbian Memory-Augmented Recurrent Networks — Szelogowski (2025) arXiv:2507.21474 | doi:10.48550/arXiv.2507.21474 | cites:2
HeLa-Mem: Hebbian Learning and Associative Memory for LLM Agents — Zhu, Li, Zhang et al. (2026) arXiv:2604.16839 | ACL 2026 | cites:0
```

## Cost ceiling rules

```
FREE budget      -> unlimited (arxiv, S2, openalex, crossref, EPMC, pubmed, unpaywall, orcid, ror, dblp, openlibrary, internet_archive_search, oreilly_search/oreilly_books_search)
$0.005 exa       -> OK if free returned <3 hits OR semantic query needs neural
$0.004 parallel  -> OK only for multi-hop chains (pattern B with patent-grade ask)
$0.01 tavily     -> last resort only
WebFetch         -> NEVER (~50K tokens = $0.25 equivalent in context)
```

## Test evidence (2026-04-24, query: "Hebbian learning LLM memory")

| Tool                                                             | Hits      | Top result                                                                                                                                                  | Cost                                  |
| ---------------------------------------------------------------- | --------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------- |
| arxiv_search (sort=submittedDate)                                | 2         | HeLa-Mem (2026-04-18, ACL 2026)                                                                                                                             | FREE                                  |
| semantic_scholar                                                 | 11,990    | Hebbian Memory-Augmented RNN (2025, cites:2)                                                                                                                | FREE                                  |
| openalex_search_works (2024-26)                                  | 261       | HeLa-Mem                                                                                                                                                    | FREE but 113KB payload — USE `select` |
| crossref_search_works                                            | 3,080,234 | Short-term Hebbian ~ transformer attention (Ellwood 2023, PLoS)                                                                                             | FREE                                  |
| europepmc_search                                                 | 5,553     | Anti-Hebbian replay (Hippocampus 2026) — all biomed                                                                                                         | FREE                                  |
| pubmed_search                                                    | 539       | biomed-only, PMID list only                                                                                                                                 | FREE                                  |
| dblp_search                                                      | **0**     | — (DBLP too conservative for fresh preprint queries)                                                                                                        | FREE                                  |
| exa_search category=research_paper                               | 2         | HeLa-Mem arxiv abs URL                                                                                                                                      | $0.007                                |
| unpaywall_find_open (arXiv DOI)                                  | 404       | — (doesn't index arXiv DOIs)                                                                                                                                | FREE                                  |
| unpaywall_find_open (publisher DOI 10.1371/journal.pcbi.1011843) | gold OA   | PLoS PDF + PMC mirror + DOAJ                                                                                                                                | FREE                                  |
| internet_archive_search mediatype:texts                          | 93        | archived patents/proceedings and older neural-memory/Hebbian-learning texts, e.g. dHAN associative thought (2007) and visual-cortex Hebbian learning (2005) | FREE                                  |
| oreilly_search `"LLM memory" OR "AI agents memory"`              | 28        | AI Agents and Applications ch.14 "Productionizing AI agents: Memory, guardrails, and beyond"; Hands-On GenAI appendix "LLM Memory Requirements"             | FREE                                  |

**Winning combo for this query**: Pattern A core (`arxiv_search` + `semantic_scholar`)
plus conditional free fanout. `internet_archive_search` adds older/pre-current-keyword
and patent/proceedings context; `oreilly_search` adds practitioner LLM/agent-memory
chapters. No paid escalation needed when these free fanouts produce useful context.
dblp was 0-hit noise; crossref found a 2023 paper the others missed via keyword ranking
so it's worth as a 3rd cross-check for survey/prior-art.

## Composition

- Metric-hill-climb of a target file -> use `autoresearch` skill (different tool).
- Debate/multi-perspective decisions -> use `swarm-templates/research-council`.
- Autonomous discovery loop -> use `/!:discover` or `/!:autoresearch` (slash command).
- This skill = **literature search orchestration only**.

## Quick recipes

```
# "SOTA map of hebbian memory"
-> Pattern B with year=2023-2026, top-10 by (year, cites)

# "Find papers by Hinton on forward-forward"
-> openalex_get_author "Geoffrey Hinton" -> filter works by title contains "forward-forward"
   OR semantic_scholar query "forward-forward Hinton"

# "Is DOI 10.1038/nature12373 open access?"
-> unpaywall_find_open doi=10.1038/nature12373 email=parm@iki.fi
   (returns best_oa_location.url_for_pdf if OA)

# "Recent CS papers about X (last 30 days)"
-> arxiv_search search_query="all:X AND cat:cs.*" sortBy=submittedDate max_results=20
```

**2026.05.05** | supersedes `Academic: context7 | arXiv` line in core-minimal.md
