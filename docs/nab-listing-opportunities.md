# nab Listing Opportunities

Researched 2026-03-13. Actionable listings where nab can submit TODAY.

---

## TIER 1: MCP Server Directories (highest relevance -- nab IS an MCP server)

### 1. punkpeye/awesome-mcp-servers (LARGEST, 30K+ stars)
- **URL**: https://github.com/punkpeye/awesome-mcp-servers
- **Web directory**: https://mcpservers.org (synced from this repo)
- **Requirements**: No star minimum. Fork, edit README.md, submit PR. Follow alphabetical order within category. Include server name linked to repo + brief description.
- **nab qualifies**: YES
- **How to submit**: Fork repo, add nab under appropriate category (e.g., "Web Browsing" or "Web Search"), submit PR titled "Add nab"
- **Category fit**: "Web Browsing" -- nab fetches web content with anti-fingerprinting and cookie auth
- **CONTRIBUTING.md**: https://github.com/punkpeye/awesome-mcp-servers/blob/main/CONTRIBUTING.md

### 2. appcypher/awesome-mcp-servers (~10K stars)
- **URL**: https://github.com/appcypher/awesome-mcp-servers
- **Requirements**: No star minimum. PR with link + brief description. Alphabetical order. One PR per suggestion.
- **nab qualifies**: YES
- **How to submit**: Fork, add to bottom of relevant category, submit PR
- **CONTRIBUTING.md**: https://github.com/appcypher/awesome-mcp-servers/blob/main/CONTRIBUTING.md

### 3. wong2/awesome-mcp-servers (~8K stars)
- **URL**: https://github.com/wong2/awesome-mcp-servers
- **Requirements**: No formal CONTRIBUTING.md found. Just submit a PR following existing format.
- **nab qualifies**: YES
- **How to submit**: Fork, add entry, submit PR

### 4. mcp.so (GitHub issue submission)
- **URL**: https://mcp.so
- **Requirements**: Submit by "creating a new issue in our GitHub repository" (chatmcp/mcpso)
- **nab qualifies**: YES
- **How to submit**: Create issue at https://github.com/chatmcp/mcpso with server details

### 5. Smithery.ai (MCP server registry/marketplace)
- **URL**: https://smithery.ai
- **Submission page**: https://smithery.ai/new
- **Requirements**: Sign in with GitHub, link your repo. No star requirement.
- **nab qualifies**: YES (needs to be a valid MCP server, which it is)
- **How to submit**: Sign in at smithery.ai/new, link the nab GitHub repo
- **CLI alternative**: `smithery mcp publish <url> -n nab`

### 6. PulseMCP (auto-updated directory, 9000+ servers)
- **URL**: https://www.pulsemcp.com/servers
- **Requirements**: Appears to auto-crawl. Contact hello@pulsemcp.com for manual submission.
- **nab qualifies**: YES
- **How to submit**: Email hello@pulsemcp.com or wait for auto-discovery

### 7. Glama.ai MCP Server Directory
- **URL**: https://glama.ai/mcp/servers
- **Requirements**: Auto-picks up servers. No manual submission needed per Reddit (r/mcp).
- **nab qualifies**: YES
- **How to submit**: Should auto-discover. May need to "claim" the listing afterward.

### 8. modelcontextprotocol/servers (Official Anthropic list)
- **URL**: https://github.com/modelcontextprotocol/servers
- **Requirements**: Has a "Community Servers" section in README. Higher bar -- Anthropic-maintained. Submit PR.
- **nab qualifies**: YES (community section)
- **How to submit**: PR to add nab to the "Community Servers" third-party section
- **Note**: This is the official list. Being here carries significant weight.

### 9. Additional MCP Directories (lower traffic but easy)
| Directory | URL | Submit | nab qualifies |
|-----------|-----|--------|---------------|
| mcpservers.com | https://mcpservers.com | Appears crawled | YES |
| mcpserverfinder.com | https://www.mcpserverfinder.com | Curated, unclear process | YES |
| mcpserverdirectory.org | https://mcpserverdirectory.org | Likely crawled | YES |
| mcpmarket.com | https://mcpmarket.com | Unclear | YES |
| mcp-awesome.com | https://mcp-awesome.com | Unclear | YES |
| MobinX/awesome-mcp-list | https://github.com/MobinX/awesome-mcp-list | PR | YES |
| TensorBlock/awesome-mcp-servers | https://github.com/TensorBlock/awesome-mcp-servers | PR | YES |
| ever-works/awesome-mcp-servers | https://github.com/ever-works/awesome-mcp-servers | PR | YES |
| abordage/awesome-mcp | https://github.com/abordage/awesome-mcp | PR | YES |

---

## TIER 2: CLI Tool Lists (nab as a CLI tool -- curl alternative)

### 10. agarrharr/awesome-cli-apps (15K+ stars, on sindresorhus/awesome)
- **URL**: https://github.com/agarrharr/awesome-cli-apps
- **Requirements**: Open source, >20 GitHub stars, >90 days old, easy to install, well documented.
- **nab qualifies**: MAYBE -- check if nab has >20 stars and is >90 days old
- **How to submit**: PR titled "Add nab" with format `[nab](URL) - Description.`
- **Category**: "Web" or "HTTP" section
- **Blocker**: Requires >20 stars. If nab is too new, wait.

### 11. toolleeo/cli-apps (2100+ tools, largest CLI list)
- **URL**: https://github.com/toolleeo/cli-apps
- **Requirements**: Edit CSV files (not README). No explicit star minimum. Accepts PRs.
- **nab qualifies**: YES
- **How to submit**: Edit the CSV file, add nab entry, submit PR
- **Note**: README says "you must edit the CSV files, not the README!"
- **Category**: Would go under "Networking" or a web-fetching subcategory

---

## TIER 3: Python Awesome Lists (nab as a Python package)

### 12. vinta/awesome-python (230K+ stars -- THE Python list)
- **URL**: https://github.com/vinta/awesome-python
- **Requirements**: 100-500 stars preferred; <100 requires strong justification. Repo must be 6+ months old with consistent activity. Must demonstrate real-world usage.
- **nab qualifies**: UNLIKELY today -- needs significant star growth
- **How to submit**: PR with justification
- **Category**: "HTTP Clients" or "Web Crawling"
- **Strategy**: Target later when nab reaches 100+ stars

### 13. ml-tooling/best-of-web-python (auto-ranked)
- **URL**: https://github.com/ml-tooling/best-of-web-python
- **Requirements**: Submit via PR to their `projects.yaml` file. Auto-ranked by stars/activity.
- **nab qualifies**: YES (will rank low but gets listed)
- **How to submit**: Add entry to projects.yaml, submit PR
- **Category**: "HTTP Clients" section

---

## TIER 4: Web Scraping Lists

### 14. lorien/awesome-web-scraping (7K+ stars)
- **URL**: https://github.com/lorien/awesome-web-scraping
- **Requirements**: STANDALONE SOFTWARE ONLY (no web services). Project must be >6 months old.
- **nab qualifies**: MAYBE -- must be >6 months old. No web services rule is OK (nab is standalone).
- **How to submit**: PR to python.md with format `* [nab](URL) - description`
- **Blocker**: "ANY PROJECT WHICH AGE IS LESS THAN HALF A YEAR WILL BE REJECTED"

### 15. luminati-io/Awesome-Web-Scraping (Bright Data)
- **URL**: https://github.com/luminati-io/Awesome-Web-Scraping
- **Requirements**: PR-based, follows similar contribution guidelines
- **nab qualifies**: YES
- **How to submit**: PR to python.md adding nab under HTTP clients section

---

## PRIORITY ACTION PLAN (submit today)

### Immediate (no barriers):
1. **punkpeye/awesome-mcp-servers** -- Biggest MCP list, PR to README.md
2. **appcypher/awesome-mcp-servers** -- Second biggest, straightforward PR
3. **wong2/awesome-mcp-servers** -- Third major list, PR
4. **mcp.so** -- GitHub issue at chatmcp/mcpso
5. **Smithery.ai** -- Sign in at smithery.ai/new, link repo
6. **modelcontextprotocol/servers** -- PR to community servers section
7. **toolleeo/cli-apps** -- Edit CSV, submit PR (no star requirement)
8. **ml-tooling/best-of-web-python** -- Add to projects.yaml
9. **luminati-io/Awesome-Web-Scraping** -- PR to python.md
10. All the smaller MCP directories (MobinX, TensorBlock, ever-works, abordage) -- quick PRs

### After reaching 20+ stars:
11. **agarrharr/awesome-cli-apps** -- needs >20 stars

### After reaching 6+ months age:
12. **lorien/awesome-web-scraping** -- needs >6 months old

### After reaching 100+ stars:
13. **vinta/awesome-python** -- needs 100+ stars with justification

---

## SAMPLE PR ENTRY

For MCP server lists:
```
- [nab](https://github.com/MikkoParkkola/nab) - Fast web fetcher with anti-fingerprinting, browser cookie auth, and 1Password integration. Also an MCP server for AI assistants.
```

For CLI lists:
```
- [nab](https://github.com/MikkoParkkola/nab) - Web fetcher like curl with anti-fingerprinting, browser cookie extraction, and 1Password integration.
```
