# Getting started with nab

This guide takes you from zero to a working nab install in five minutes, then walks through the four main commands (`fetch`, `analyze`, `watch`, `nab-mcp`) with real examples.

## Install

Pick one of the following.

### Homebrew (macOS, recommended)

```bash
brew tap MikkoParkkola/tap
brew install nab
```

### crates.io

```bash
cargo install nab
```

Requires Rust 1.95 or newer. Install Rust via [rustup.rs](https://rustup.rs/) if you don't already have it.

### Pre-built binary

```bash
cargo binstall nab
```

Or grab the binary for your platform from [GitHub Releases](https://github.com/MikkoParkkola/nab/releases).

### From source

```bash
git clone https://github.com/MikkoParkkola/nab.git
cd nab
cargo install --path .
```

### Verify the install

```bash
nab --version
```

You should see something like `nab 0.7.0`.

## First fetch

The simplest possible nab invocation:

```bash
nab fetch https://example.com
```

You will see clean markdown on stdout. No JavaScript was rendered, no headless browser was launched, and no API key was needed. nab made a single HTTP/3 request, parsed the HTML, removed boilerplate, and converted the body to markdown.

### Fetch with browser cookies

By default, `nab fetch` auto-detects your default browser and uses its session cookies for the request. If you are logged in to GitHub in Brave, this works:

```bash
nab fetch https://github.com/notifications --cookies brave
```

You stay logged in. nab does not run a browser — it reads the cookie store, inserts the cookies into its own request, and pulls the page. Supported browsers: `brave`, `chrome`, `firefox`, `safari`, `edge`, `dia`.

### Fetch with 1Password auto-login

If you have the 1Password CLI installed and unlocked:

```bash
nab fetch https://internal.company.com --1password
```

nab looks up the credentials for the URL, follows the login form, handles CSRF tokens and TOTP/MFA, then fetches the target page.

### Query-focused extraction

Use the MCP `fetch` tool with `focus` and `max_tokens` to get only the parts of the page relevant to your question:

```json
{
  "url": "https://docs.anthropic.com/en/api/messages",
  "focus": "what does the streaming response look like",
  "max_tokens": 2000
}
```

nab applies BM25-lite scoring to the extracted markdown, keeps the top sections, and respects a strict token budget that never splits mid-block (headings, code blocks, tables stay intact).

### Output formats

```bash
nab fetch https://example.com                  # markdown (default)
nab fetch https://example.com --format json    # JSON with confidence scores
nab fetch https://example.com --format compact # one-line summary
nab fetch https://example.com --raw-html       # bypass markdown conversion
```

## First analyze

`nab analyze` transcribes audio and video. It runs locally on your machine. There is no cloud API in the default path.

### Install the model

The default backend on macOS arm64 is FluidAudio (Parakeet TDT v3). Download it once:

```bash
nab models fetch fluidaudio
```

This pulls the FluidAudio binary plus the Parakeet TDT v3 weights (~600 MB total) into `~/.local/share/nab/models/`. You only need to do this once.

### Transcribe

```bash
nab analyze interview.mp4
```

Output is a transcript with segment timestamps. On a 2-hour English audio file, this typically completes in about a minute on Apple Silicon.

### Add diarization

```bash
nab analyze interview.mp4 --diarize
```

Each segment now has a `speaker` field (`SPEAKER_00`, `SPEAKER_01`, ...). Diarization uses the FluidAudio offline VBx clustering with PyAnnote community-1 weights.

### Word-level timestamps

```bash
nab analyze talk.mp4 --word-timestamps
```

Each segment now contains a `words` array with one entry per word, including start, end, and confidence.

### Force a language

```bash
nab analyze finnish_podcast.mp3 --language fi
```

Without `--language`, nab auto-detects. Pass a BCP-47 code to skip detection or override an incorrect guess.

### Active reading

```bash
nab analyze interview.mp4 --active-reading
```

This requires running `nab analyze` from inside an MCP client (Claude Code, Continue, Zed, ...) so that nab can call back to the host LLM. nab sends transcript chunks to the LLM via `sampling/createMessage`, asks it to identify references (papers, people, claims), looks up each reference via `nab fetch`, and inlines the result as a footnote in the transcript.

The transcript stops being a wall of text and starts being annotated with citations.

### Output JSON

```bash
nab analyze podcast.mp3 --format json > transcript.json
```

## First watch

`nab watch` is RSS for the entire web. Add a URL, set an interval, and nab will check it on schedule and notify you when it changes.

```bash
nab watch add https://news.ycombinator.com --interval 10m
```

You will see a watch ID. List your watches:

```bash
nab watch list
```

Inspect a watch's recent check log:

```bash
nab watch logs <id>
```

Remove a watch:

```bash
nab watch remove <id>
```

### Selectors

For pages with a lot of noise, scope the watch to a CSS selector:

```bash
nab watch add https://example.com/pricing \
  --interval 1h \
  --selector "table.pricing" \
  --notify-on regression
```

`--notify-on regression` only fires when the price changes (not when an unrelated banner updates).

### How it works

The watch poller iterates all watches every minute and fetches the ones whose interval has elapsed. It uses conditional GETs (`If-None-Match` and `If-Modified-Since`), so 304 responses cost effectively nothing — they don't even count as a check.

When a watch fires, the change is delivered two ways:

1. **Watch log** on disk (visible via `nab watch logs <id>`)
2. **MCP notification** if the watch was created from inside an MCP client — the client receives `notifications/resources/updated` for the `nab://watch/<id>` resource and reads the diff via `resources/read`

## MCP integration

`nab-mcp` is a Model Context Protocol server. It exposes everything nab can do as MCP tools, prompts, and resources.

### Claude Code

Add to `~/.config/claude/mcp.json`:

```json
{
  "mcpServers": {
    "nab": {
      "command": "nab-mcp"
    }
  }
}
```

Restart Claude Code. You should see nab's tools (`fetch`, `analyze`, `watch_create`, ...) appear in the tool palette.

### Continue, Zed, Cursor, Windsurf

Same shape. Each editor has its own MCP config file location, but the structure is identical: point `command` at the `nab-mcp` binary on your `PATH`.

### HTTP transport

For multi-client setups or when running nab on a server:

```bash
nab-mcp --http 127.0.0.1:8765
```

This starts a Streamable HTTP MCP endpoint on localhost. The transport is fully spec-compliant: origin checks, `MCP-Protocol-Version` header validation, session IDs via `MCP-Session-Id`, resumability via `Last-Event-ID`, DELETE for session termination.

### Available tools

Once nab-mcp is configured, your MCP client gets 12 tools:

| Tool | Use it for |
|------|-----------|
| `fetch` | Get a URL as markdown |
| `fetch_batch` | Fetch many URLs in parallel (async with progress) |
| `submit` | Submit a form |
| `login` | 1Password auto-login |
| `auth_lookup` | Check 1Password for a URL's credentials |
| `fingerprint` | Generate browser fingerprint profiles |
| `validate` | Run the test suite |
| `benchmark` | Time URL fetches |
| `analyze` | Transcribe audio or video |
| `watch_create` | Create a URL watch and subscribe |
| `watch_list` / `watch_remove` | Manage watches |

Plus 4 prompts (including `match-speakers-with-hebb` for cross-tool composition with the [hebb](https://github.com/MikkoParkkola/hebb) memory server).

## Common recipes

### Fetch with browser cookies and a session

Use the MCP `fetch` tool with a named `session`:

```json
{
  "url": "https://app.example.com/dashboard",
  "cookies": "brave",
  "session": "work-app"
}
```

The session persists cookies across requests. Subsequent MCP fetches with the same `session` value reuse the saved jar.

### Analyze with diarization and export embeddings for hebb

```bash
nab analyze interview.mp4 \
  --diarize \
  --include-embeddings \
  --format json > interview.json
```

The output JSON contains 256-dim WeSpeaker embeddings per speaker turn. Pipe these to `hebb voice_match` to identify the speakers if you have a voiceprint database.

### Watch a price page

```bash
nab watch add "https://example.com/products/foo" \
  --interval 1h \
  --selector ".product-price" \
  --notify-on regression
```

### Batch fetch a list of URLs

```bash
echo "https://example.com" > urls.txt
echo "https://news.ycombinator.com" >> urls.txt
echo "https://en.wikipedia.org/wiki/Rust_(programming_language)" >> urls.txt

nab fetch --batch urls.txt --parallel 4
```

### Fetch a Google Doc with comments

```bash
nab fetch --cookies brave \
  "https://docs.google.com/document/d/DOCID/edit"
```

nab uses the Google Workspace export API plus OOXML parsing, so you get markdown content with comments and suggested edits inline.

### Fetch a PDF

```bash
nab fetch https://arxiv.org/pdf/2301.00001 -o paper.md
```

PDF conversion uses pdfium for character-level positioning and reconstructs lines, paragraphs, and tables.

## Troubleshooting

### `nab models fetch fluidaudio` fails

Check that you have at least 1 GB free in `~/.local/share/nab/models/`. The download is resumable — re-run the command if it was interrupted.

### `nab analyze` says "model not installed"

Run `nab models fetch fluidaudio` first. Verify with `nab models list`.

### `nab fetch --cookies brave` returns an empty response

Brave (and Chrome) lock the cookie database while the browser is running. Try closing the browser, or use a different `--cookies` source. nab will print a warning when it detects a locked store.

### `nab analyze --active-reading` says "sampling not available"

Active reading requires the host LLM to support MCP sampling. Claude Code does. If you're running `nab analyze` from a plain terminal (not an MCP client), the LLM is unreachable and nab falls back to passive transcription.

### Watch poller seems idle

The poller iterates every minute, then picks watches whose `last_check_at + interval` is less than `now`. A 1-hour-interval watch will wait up to a minute past the hour before firing. Use shorter intervals for testing.

### MCP client doesn't see nab's tools

Verify the binary is on `PATH` and executable: `which nab-mcp && nab-mcp --version`. Restart the MCP client after editing its config.

## Where to go next

- [README.md](../README.md) — feature reference
- [docs/sovereign-stack.md](sovereign-stack.md) — composing nab with hebb
- [docs/ARCHITECTURE.md](ARCHITECTURE.md) — internal architecture
- [docs/design/](design/) — recent design proposals (analyze v2, URL watch resources, active reading, MCP spec closure)
