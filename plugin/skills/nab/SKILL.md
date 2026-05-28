---
name: nab
version: 2026.05.26
effort: low
triggers: fetch|fetch url|url to markdown|read this url|grab this page|scrape|authenticated fetch|behind login|paywall|paywalled|cookies|brave cookies|1password|totp|mfa|sso|internal dashboard|google workspace doc|saas page|webfetch|web fetch|batch fetch|form submit|csrf|clean markdown|token budget
description: >
  How and WHY to use nab — the preferred URL->markdown microfetch tool for agents.
  Decision: nab (auth + cookies + anti-bot, ~50ms, LLM-shaped markdown) > jina (clean MD fallback)
  > NEVER WebFetch (~50K tokens/call, ~25x waste). Prefer the nab MCP tools (fetch, fetch_batch,
  submit, login, auth_lookup) when the plugin's nab server is loaded; fall back to the `nab` CLI.
  Auto-picks for "fetch this URL", "read the page", authenticated / paywalled / SaaS / Google
  Workspace content, multi-URL batches, and form submits. nab is a fetch tool, NOT a browser.
composes_with: research, url-insight, wayback, ia, oreilly
---

# nab Skill — Auth-Aware URL→Markdown Microfetch

> nab is the **default** way to turn any URL into clean, token-tight markdown — including
> pages behind cookies, 1Password/TOTP, passkeys, and anti-bot WAFs. It is **not a browser**:
> it reads your existing browser session, returns LLM-shaped markdown, and ships nothing else.

## Prime directive (Fukasawa — without thought)

```
URL -> markdown      : nab (MCP fetch) -> nab CLI -> jina (FREE clean MD) -> NEVER WebFetch
authenticated URL    : nab --cookies brave  (existing session)  OR  nab --1password (login + TOTP)
3+ URLs              : nab fetch_batch (MCP)  /  nab fetch --batch file --parallel N (CLI)
form / login flow    : nab submit (CSRF-aware)  /  nab login (1Password auto-login)
WebFetch             : NEVER — ~50K tokens/call (~25x the nab cost in context)
```

Why nab wins: ~50ms, HTTP/3, real browser-cookie injection, 1Password auto-login with
TOTP/MFA, WebAuthn/passkey, fingerprint spoofing, WAF evasion, 12 site providers (official
APIs / stable structured data, not broad HTML scraping), BM25-lite query-focused extraction,
and a structure-aware token budget. WebFetch dumps raw, unbudgeted HTML into context.

## MCP-first, CLI-fallback

Prefer the **nab MCP tools** when the plugin's `nab` MCP server is loaded (registered as `nab`
in `plugin/.mcp.json`; the host surfaces tools as `mcp__nab__<tool>`). The 12-tool surface
includes these load-bearing tools (V — from `README.md` MCP table):

| MCP tool      | Use for                                                                 |
| ------------- | ----------------------------------------------------------------------- |
| `fetch`       | One URL → markdown, with cookies, query focus, token budget, session    |
| `fetch_batch` | Parallel multi-URL fetch with task-augmented async execution (3+ URLs)  |
| `submit`      | Submit a form with CSRF + smart field extraction                        |
| `login`       | 1Password auto-login with TOTP support                                  |
| `auth_lookup` | Look up 1Password credentials for a URL                                 |

> Naming note (V — source-verified): the repo registers these under **bare** names via the
> derive macro, e.g. `#[mcp_tool(name = "fetch")]` and `#[mcp_tool(name = "auth_lookup")]`
> (`src/bin/mcp_server/tools/fetch.rs:39`, `…/auth.rs:16`; full map in `…/tools/mod.rs`). There
> is **no `nab_` prefix anywhere** in `src/bin/mcp_server/`. When surfaced through a host the
> tools appear server-qualified — `mcp__nab__fetch` (Claude Code) or `nab:fetch` (mcp-gateway).
> The operator's earlier `nab_fetch` shorthand was a convention, not the wire name. Canonical
> set: `fetch`, `fetch_batch`, `submit`, `login`, `auth_lookup`, `analyze`, `fingerprint`,
> `validate`, `benchmark`, `watch_create` / `watch_list` / `watch_remove`.

If the MCP server is unavailable, fall back to the **CLI** (V — from `README.md`):

```bash
nab fetch <url>                                  # plain fetch → markdown
nab fetch <url> --cookies brave                  # use your existing Brave session
nab fetch <url> --1password                      # 1Password login (TOTP/MFA supported); alias: --op
nab fetch <url> --batch urls.txt --parallel 8    # batch CLI fetch
nab fetch <url> --format json                    # structured output instead of markdown
nab fetch <url> --render --browser-cdp-url wss://... # JS-heavy SPA via external CDP (opt-in)
nab otp <domain>                                 # pull a one-time code: 1Password TOTP → SMS → email
```

`--cookies` accepts: `auto`, `brave`, `chrome`, `firefox`, `safari`, `edge`, `none` (V).

> TOTP/MFA (V — source-verified): two paths exist. (1) Inline — `login` / `--1password` handle
> TOTP during auto-login (README: "1Password auto-login with TOTP support"). (2) Standalone —
> `nab otp <domain>` **does exist** as a CLI subcommand (`src/main.rs:361` `Otp { domain }` →
> `src/cmd/otp.rs` `cmd_otp`). It resolves a one-time code by searching, in order: 1Password
> TOTP → SMS via Beeper → Email via Gmail. Use `nab otp <domain>` to pull a code out-of-band;
> use `login` / `--1password` for an end-to-end authenticated flow.

## When to use which (decision table)

| Situation                                            | Reach for                                  | Conf |
| ---------------------------------------------------- | ------------------------------------------ | ---- |
| Public page, just need the text                      | `fetch` (MCP) / `nab fetch <url>`          | V    |
| Page where you are already signed in (browser)       | `fetch` + cookies / `--cookies brave`      | V    |
| SaaS / internal dashboard / paywalled research       | `--cookies <browser>` or `--1password`     | V    |
| Google Workspace doc you can open in your browser    | `--cookies brave` on the doc URL           | V    |
| 3+ URLs / a research fan-out                          | `fetch_batch` / `--batch file --parallel N`| V    |
| Submitting a form (search, login form, action)       | `submit` (CSRF-aware)                      | V    |
| Need credentials resolved for a URL before fetch     | `auth_lookup`                              | V    |
| JS-only SPA returning thin content                   | `--render` + external CDP (opt-in)         | V    |
| Need structured fields, not prose                    | `--format json`                            | V    |

## Cross-links — siblings in this plugin

nab is the fetch primitive; these bundled skills layer intent on top of it (all V — dirs exist
in `plugin/skills/`):

- **research** — academic/literature orchestration (arxiv, semantic_scholar, openalex, …).
  Use for "find papers on X", prior art, SOTA maps. It still routes URL reads through nab.
- **url-insight** — URL triage + ROI scoring before you spend a fetch.
- **wayback** — Wayback Machine / CDX: lock durable evidence, fetch a prior version, then
  hand the archived URL to nab for clean markdown.
- **ia** — Internet Archive item metadata, uploads, downloads, collection search.
- **oreilly** — practitioner-grade books/chapters for production/operator guidance.

Composition pattern: `url-insight` (is it worth it?) → `nab fetch`/`fetch_batch` (get it) →
`wayback` (pin it) → `research`/`oreilly` (widen it). The `/nab` command wires these together.

## Anti-patterns (NEVER)

| Anti-pattern                                            | Do instead                                        |
| ------------------------------------------------------- | ------------------------------------------------- |
| `WebFetch` a URL                                        | nab (MCP) → nab CLI → jina; ~25x cheaper in tokens|
| Pasting raw HTML into context when markdown suffices    | let nab return budgeted markdown (`fetch`)        |
| Looping single `fetch` calls over a URL list            | `fetch_batch` / `--batch --parallel`              |
| `NAB_YARA_BYPASS=1` for convenience                     | leave the YARA-X fetch guard ON (audited bypass only) |
| Treating nab as public-web only                         | use `--cookies` / `--1password` for authed reach  |
| Calling / describing nab as a "browser" or "engine"     | it reads your browser's cookies; it is a fetch tool |
| Scraping a site nab already has a provider for           | nab uses official APIs / structured data for 12 sites |

## Security note

nab scans fetched bodies with a fetch-time YARA-X guard by default (prompt-injection / exfil /
secret signatures). Keep it on. `NAB_YARA_BYPASS=1` is an audited emergency operator escape
hatch only; `NAB_YARA_ACTION=refuse` blocks instead of redacting matched sections (V — CLAUDE.md).

---

Confidence: V = verified in nab `README.md` / `CLAUDE.md` / `plugin/.mcp.json` (v0.10.3) **or
source** (`src/main.rs`, `src/cmd/otp.rs`, `src/bin/mcp_server/tools/`) | I = schema/convention
argument | A = operator-config assertion not confirmed in this repo. Both prior open flags
(`nab otp` existence, MCP tool naming) are now source-verified V (2026-05-26).

**2026.05.26** | closes the "no skill teaches nab itself" gap in the nab plugin
