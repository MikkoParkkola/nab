# nab Claude Code Plugin

This plugin packages the nab MCP server with the research, URL insight, Wayback, Internet Archive, and O'Reilly skills that make nab useful as a one-click research-and-archive workflow.

nab is auth-aware fetch infrastructure, not a browser. It uses your existing browser sessions and local credentials to fetch clean markdown from public pages, internal SaaS, Google Workspace, paywalled research, and other URLs you are authorized to access.

## Install

Install nab first:

```bash
brew tap MikkoParkkola/tap
brew install nab
# or: cargo install nab
```

Load the plugin from this repository:

```bash
claude --plugin-dir ./plugin
```

Validate the package:

```bash
claude plugin validate ./plugin
python3 scripts/validate_nab_plugin.py
```

The plugin auto-registers the `nab` MCP server through `plugin/.mcp.json`; `plugin/mcp.json` is kept as a compatibility copy for tools that expect that name. The wrapper resolves `nab-mcp` in this order: `NAB_MCP_BIN`, `PATH`, Homebrew Apple Silicon, Homebrew Intel, then Cargo's `~/.cargo/bin`.

## Command

Use the `/nab` command shape for the workflow:

```text
/nab fetch <url>
/nab fetch --cookies brave <url>
/nab archive <url>
/nab research <topic>
```

Claude Code may namespace plugin commands as `/nab:nab`; the command keeps the same arguments.

## Auth Path

nab distinguishes itself from curl-style fetchers by reusing real local auth:

- `--cookies brave` loads cookies from Brave for pages where you are already signed in.
- Browser cookie injection also supports Chrome, Firefox, Safari, Edge, and Dia from the nab CLI.
- `--1password` uses the 1Password CLI path for login, including TOTP/MFA where nab can automate it.
- WebAuthn/passkey and anti-bot handling stay in nab itself; the plugin only packages the workflow.

The fetch-time YARA-X guard remains active by default in nab. The `nab-yara-edge` hook included here is a WARN stub for MIK-3390 and does not replace nab's built-in scanning.

## Worked Examples

### Authenticated Fetch

```text
/nab fetch --cookies brave https://docs.google.com/document/d/DOCID/edit
```

Expected flow: use nab MCP fetch when available, otherwise run `nab fetch --cookies brave <url>`. Return clean markdown from the authenticated page and note whether the MCP or CLI path was used.

### Archived Snapshot Retrieval

```text
/nab archive https://example.com/research/post
```

Expected flow: use the bundled Wayback skill to save or find a snapshot, fetch the archived URL through nab, then return the snapshot URL plus extracted markdown evidence.

### Multi-Source Research

```text
/nab research "state of MCP plugin distribution for auth-aware web research"
```

Expected flow: combine the bundled `research`, `url-insight`, `wayback`, `ia`, and `oreilly` skills. Fetch URLs with nab, lock durable evidence in Wayback where useful, consult Internet Archive metadata when relevant, and use O'Reilly for practitioner material.

## Bundled Components

- `commands/nab.md` - `/nab` fetch/archive/research command prompt
- `skills/research` - general research routing
- `skills/url-insight` - URL triage and ROI scoring
- `skills/wayback` - Wayback Machine and CDX workflows
- `skills/ia` - Internet Archive item workflows
- `skills/oreilly` - O'Reilly practitioner search
- `.mcp.json` / `mcp.json` - auto-registers `nab-mcp`
- `hooks/nab-yara-edge.sh` - non-blocking YARA-X edge warning stub

## Rollback

Delete `plugin/`. The nab binary, nab MCP server, and upstream Claude Elite skills remain unaffected.
