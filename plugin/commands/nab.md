---
description: Fetch, archive, or research URLs with nab
argument-hint: "fetch [--cookies brave|--1password] <url> | archive <url> | research <topic>"
disable-model-invocation: true
---

# nab

Input: $ARGUMENTS

Accept these entry points exactly:

- `/nab fetch <url>`
- `/nab fetch --cookies brave <url>`
- `/nab archive <url>`
- `/nab research <topic>`

When Claude Code namespaces plugin commands, this command may appear as `/nab:nab`; keep the same argument contract and behavior.

## Dispatch

Parse `$ARGUMENTS` as one of the supported subcommands.

### fetch

Use nab MCP fetch tools first when the plugin's `nab-mcp` server is available. If the MCP server is unavailable, run the local CLI:

```bash
nab fetch <url>
```

Preserve auth flags and pass them through to the CLI when needed:

```bash
nab fetch --cookies brave <url>
nab fetch --1password <url>
```

Do not use generic web fetching unless both nab MCP and nab CLI are unavailable. Keep nab's fetch-time YARA-X guard enabled; do not set `NAB_YARA_BYPASS=1` unless the user explicitly requests an audited emergency bypass.

### archive

Use the bundled `wayback` and `ia` skills for archive routing:

1. Submit the URL to Wayback Save Page Now through the available gateway or Wayback MCP path.
2. Retrieve the closest archived snapshot.
3. Fetch the archived URL with nab so the final output is clean markdown.

If no Wayback or Internet Archive tool is available, report the missing archive dependency and still offer a direct `nab fetch` result.

### research

Use the bundled `research`, `url-insight`, `wayback`, `ia`, and `oreilly` skills together:

1. Form a compact research plan for the topic.
2. Prefer nab MCP or `nab fetch` for every URL.
3. Use Wayback for durable evidence and prior versions when source stability matters.
4. Use Internet Archive for item metadata, uploads, downloads, or collection search.
5. Use O'Reilly for practitioner-grade books, chapters, and production guidance.
6. Return a concise synthesis with sources, confidence, and follow-up actions.

## Output

Return the fetched markdown, archive evidence, or research synthesis directly. Include command/tool evidence when a fallback was needed.
