# nab Claude Code Plugin

This plugin is one directory. Claude Code loads it with `claude --plugin-dir ./plugin` and starts the nab MCP server by running Node on a launcher that lives inside the plugin. The launcher downloads the pinned nab-mcp release for the current platform, caches that binary under the plugin folder, and spawns it directly. It does not search for nab or nab-mcp, and it does not require a prior install of the nab CLI.

nab is auth-aware fetch infrastructure, not a browser. It can read cookies from the local browser so a fetch reuses a session you already have. Fetching stays on this machine. Use it for public pages, pages you are signed in to, archived copies, and research notes you are allowed to read.

## Install

Load this directory. The first MCP start downloads nab-mcp 0.12.3 from the GitHub release that matches your operating system and CPU.

```bash
claude --plugin-dir ./plugin
```

Validate the package:

```bash
claude plugin validate ./plugin
```

The plugin registers the `nab` MCP server through `plugin/.mcp.json`. `plugin/mcp.json` is the same document for tools that expect that filename. The command is Node, and the only argument is the launcher inside this folder.

## Command

Use the `/nab` command shape for the workflow:

```text
/nab fetch <url>
/nab fetch --cookies brave <url>
/nab archive <url>
/nab research <topic>
```

Claude Code may namespace plugin commands as `/nab:nab`; the command keeps the same arguments.

## Auth path

`--cookies brave` loads cookies from Brave for pages where you are already signed in. Browser cookie injection also supports Chrome, Firefox, Safari, Edge, and Dia from nab. `--1password` uses the 1Password CLI path for login, including TOTP where nab can automate it. WebAuthn and anti-bot handling stay in nab itself; the plugin only packages the workflow.

The fetch-time YARA-X guard remains active by default in nab. The `nab-yara-edge` hook included here is a warning stub and does not replace nab's built-in scanning.

## Worked examples

### Example: Fetch a URL

```text
/nab fetch https://example.com/research/post
```

Ask nab to fetch the URL and return clean markdown. The MCP server does the fetch. You get the page text, not a live browser session.

### Example: Fetch with the user's browser cookies

Authenticated Fetch uses the cookies already stored for you.

```text
/nab fetch --cookies brave https://docs.google.com/document/d/DOCID/edit
```

Use this when you are already signed in. nab reads the local browser cookie store and sends those cookies only to the site you named.

### Example: Read an archive or Wayback page

Archived Snapshot Retrieval finds a stored copy, then reads it.

```text
/nab archive https://example.com/research/post
```

Use the bundled Wayback skill to find or save a snapshot, then fetch the archived URL through nab. Return the snapshot URL and the extracted markdown.

Multi-Source Research still combines the bundled research, url-insight, wayback, ia, and oreilly skills when the question needs more than one page.

## Bundled components

- `commands/nab.md` documents the `/nab` fetch, archive, and research command prompt
- `skills/research` routes general research
- `skills/url-insight` triages a URL before you spend a fetch
- `skills/wayback` covers Wayback Machine and CDX workflows
- `skills/ia` covers Internet Archive item workflows
- `skills/oreilly` covers practitioner book search
- `.mcp.json` registers nab through Node and `bin/launch.js`
- `hooks/nab-yara-edge.sh` is a non-blocking YARA-X edge warning stub

## Rollback

Remove the `plugin/` directory from the Claude Code plugin list. The downloaded binary sits in that directory, so removing the directory removes the cache too.

## What this plugin runs and where data goes

The launcher downloads one binary from `https://github.com/MikkoParkkola/nab/releases/download/v0.12.3/`. GitHub may redirect that download to a `githubusercontent.com` host. The file is cached under `plugin/bin/cache/` and then started. This folder does not call the npm registry.

Once a day the launcher POSTs to `https://telemetry.revaluator.ai/v1/heartbeat`. The JSON fields are `project` (`nab`), `event` (`heartbeat`), `version`, `runtime`, `install_id`, `install_date`, and `machine_id`. `install_date` is the UTC day that install id was created. `machine_id` is a second random id shared by the products on this machine. It is not a name, an email, or an account. The body has no cookie and no page URL. The published build pinned by this folder does not send that POST. The launcher and a later build share `~/.nab/telemetry`. The shared machine id file is `~/.revaluator/machine-id`. Set `NAB_NO_TELEMETRY`, `NO_TELEMETRY`, or `DO_NOT_TRACK` to a value other than `0` or `false` to stop it. The full text is [PRIVACY.md](PRIVACY.md).

The downloaded program fetches the URL you name. It can read cookies from a local browser. Those cookie values are sent only to the site you named. They are not sent to the author. A Wayback helper in this folder can request `web.archive.org`. The bundled notes also name `archive.org`, `docs.google.com`, `arxiv.org`, `www.gnu.org`, `www.anthropic.com`, `www.producthunt.com`, and `karpour.github.io`. Those notes do not add a second telemetry destination. An O'Reilly search runs only when you ask for it, through a separate gateway, not from this launcher at startup.
