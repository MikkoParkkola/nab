# nab

[![CI](https://github.com/MikkoParkkola/nab/actions/workflows/ci.yml/badge.svg)](https://github.com/MikkoParkkola/nab/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/nab.svg)](https://crates.io/crates/nab)
[![Downloads](https://img.shields.io/crates/d/nab.svg)](https://crates.io/crates/nab)
[![docs.rs](https://img.shields.io/docsrs/nab)](https://docs.rs/nab)
[![Rust](https://img.shields.io/badge/Rust-1.95+-orange.svg?logo=rust)](https://www.rust-lang.org)
[![License: MIT + PolyForm NC](https://img.shields.io/badge/License-MIT%20%2B%20PolyForm%20NC-yellow.svg)](LICENSE.md)
[![MCP Protocol](https://img.shields.io/badge/MCP-2025--11--25-blueviolet.svg)](https://modelcontextprotocol.io)
[![nab MCP server](https://glama.ai/mcp/servers/MikkoParkkola/nab/badges/score.svg)](https://glama.ai/mcp/servers/MikkoParkkola/nab)
[![Install in VS Code](https://img.shields.io/badge/VS_Code-Install_MCP-0078d4?logo=visualstudiocode)](https://insiders.vscode.dev/redirect/mcp/install?name=nab&config=%7B%22command%22%3A%22nab-mcp%22%7D)
[![Install in Cursor](https://img.shields.io/badge/Cursor-Install_MCP-black?logo=cursor)](cursor://anysphere.cursor-deeplink/mcp/install?name=nab&config=%7B%22command%22%3A%22nab-mcp%22%7D)

Token-optimized web fetcher + multilingual ASR + URL watcher. MCP 2025-11-25 compliant. Rust. macOS arm64 first, cross-platform.

![demo](demo.gif)

nab is a single Rust binary that does three things very well: it **fetches** any URL as clean markdown (with your real browser cookies and anti-bot evasion), it **analyzes** any audio or video file with on-device multilingual ASR and speaker diarization, and it **watches** any URL for changes and pushes notifications when content moves. Everything runs locally. There are no API keys to set up by default. The output is shaped for LLM context windows.

## Quick start

**Tell your AI assistant** (recommended):

> Read https://github.com/MikkoParkkola/nab and install nab as my web fetching and audio analysis MCP server

Your agent will install the binary, wire itself up, and start fetching. Works in Claude Code, Cursor, Windsurf, and any AI with terminal access.

**Or install and try manually:**

```bash
brew install MikkoParkkola/tap/nab                            # install
nab fetch https://news.ycombinator.com                        # fetch as markdown
nab models fetch fluidaudio                                   # download ASR model
nab analyze interview.mp4 --diarize                           # transcribe + identify speakers
nab watch add https://status.openai.com --interval 5m         # subscribe to changes
```

## Features

| Command | What it does |
|---------|--------------|
| `nab fetch <url>` | Fetch any URL as clean markdown. HTTP/3, browser cookie injection (Brave / Chrome / Firefox / Safari / Edge / Dia), 1Password auto-login, fingerprint spoofing, fetch-time YARA-X redaction for prompt-injection/exfil signatures, 12 site providers. MCP fetch also supports query-focused extraction, readability, and token budgets. |
| `nab analyze <video\|audio>` | Transcribe and diarize. FluidAudio (Parakeet TDT v3) on Apple Neural Engine, 131x realtime on a 2-hour clip, word-level timestamps, 25 EU languages, optional Qwen3-ASR for zh/ja/ko/vi, optional active reading via MCP sampling. |
| `nab watch add <url>` | Monitor a URL and push notifications via subscribable MCP resources. RSS for the entire web. Conditional GETs, semantic diff, adaptive backoff. |
| `nab models fetch <name>` | Persistent install of inference model binaries. Supports `fluidaudio` (default on macOS Apple Silicon), `sherpa-onnx` (cross-platform Parakeet TDT, ~30× realtime CPU), and `whisper` (universal fallback, whisper-large-v3-turbo, 99 langs). |
| `nab-mcp` | MCP 2025-11-25 server. stdio + Streamable HTTP. 12 tools, 4 prompts, 2+N resources, structured logging, sampling, roots, elicitation. |
| `nab::content::ocr` | Apple Vision OCR engine. 15 languages. Apple Neural Engine accelerated. ~10-50 ms per image. macOS only. |

## Installation

### Homebrew (macOS, recommended)

```bash
brew tap MikkoParkkola/tap
brew install nab
```

### From crates.io

```bash
cargo install nab
```

Requires Rust 1.95 or newer.

### Pre-built binary

```bash
cargo binstall nab
```

Or download directly from [GitHub Releases](https://github.com/MikkoParkkola/nab/releases):

| Platform | Binary |
|----------|--------|
| macOS Apple Silicon | `nab-aarch64-apple-darwin` |
| macOS Intel | `nab-x86_64-apple-darwin` |
| Linux x86_64 | `nab-x86_64-unknown-linux-gnu` |
| Linux ARM64 | `nab-aarch64-unknown-linux-gnu` |
| Windows x64 | `nab-x86_64-pc-windows-msvc.exe` |

### From source

```bash
git clone https://github.com/MikkoParkkola/nab.git
cd nab
cargo install --path .
```

## MCP Configuration

Add to your MCP client config (Claude Desktop, Cursor, Windsurf, etc.):

```json
{
  "mcpServers": {
    "nab": {
      "command": "nab-mcp"
    }
  }
}
```

Or use the auto-installer:

```bash
nab mcp install                        # Claude Desktop (default)
nab mcp install --client claude-code   # Claude Code
nab mcp install --client cursor        # Cursor
nab mcp install --client windsurf      # Windsurf
nab mcp install --client codex         # OpenAI Codex CLI
nab mcp install --client vscode        # VS Code Copilot
nab mcp install --client zed           # Zed
nab mcp install --dry-run              # preview without writing
```

Also supported: `gemini`, `amazon-q`, `lm-studio`.

See [MCP integration](#mcp-integration) below for the full list of tools, capabilities, and HTTP transport.

## Claude Code plugin

This repository includes a local Claude Code plugin in [plugin/](plugin/README.md). It bundles nab MCP auto-registration with the Claude Elite `research`, `url-insight`, `wayback`, `ia`, and `oreilly` skills.

```bash
claude --plugin-dir ./plugin
```

The plugin exposes the `/nab` workflow shape for `fetch`, authenticated Brave-cookie fetches, archive retrieval, and multi-source research. It keeps nab's auth-aware path front and center: `nab fetch --cookies brave <url>` for existing browser sessions and `nab fetch --1password <url>` for 1Password/TOTP flows.

## Usage

### Fetch

```bash
# Basic fetch — auto-detects browser, returns markdown
nab fetch https://example.com

# Use cookies from a specific browser
nab fetch https://github.com/notifications --cookies brave

# 1Password auto-login (TOTP/MFA supported)
nab fetch https://internal.company.com --1password

# Google Workspace (Docs, Sheets, Slides) with comments
nab fetch --cookies brave "https://docs.google.com/document/d/DOCID/edit"

# Output JSON with confidence scores
nab fetch https://example.com --format json

# Batch fetch with parallelism
nab fetch --batch urls.txt --parallel 8
```

Common flags for `fetch`:

| Flag | Description |
|------|-------------|
| `--cookies <browser>` | `auto`, `brave`, `chrome`, `firefox`, `safari`, `edge`, `none` |
| `--1password` / `--op` | 1Password credential lookup + auto-login |
| `--proxy <url>` | HTTP or SOCKS5 proxy |
| `--format <fmt>` | `full` (default), `compact`, `json` |
| `--raw-html` | Skip markdown conversion |
| `--readability` | Force readability extraction for generic HTML pages |
| `--max-output-tokens <n>` | Apply an output token envelope; returned markdown uses 80% for headroom |
| `--remote-fallback` | Opt in to remote thin-content recovery via `r.jina.ai`; avoid for internal, authenticated, or sensitive URLs |
| `--diff` | Show what changed since the last fetch |
| `-X <method>` `-d <data>` | HTTP method + body |
| `-o <path>` | Write body to file |

MCP `fetch` additionally supports `focus`, `readability`, `max_tokens`, and `session` parameters for query-focused extraction, readability extraction, structure-aware token budgets, and persistent encrypted cookie sessions.

### Analyze

`nab analyze` transcribes audio and video files locally. The default backend on macOS arm64 is FluidAudio, which runs Parakeet TDT v3 on the Apple Neural Engine.

```bash
# Download the ASR model (~600 MB, one-time)
nab models fetch fluidaudio

# Transcribe a video
nab analyze interview.mp4

# Add speaker diarization (PyAnnote community-1)
nab analyze interview.mp4 --diarize

# Force a language hint (BCP-47)
nab analyze podcast.mp3 --language fi

# Word-level timestamps
nab analyze talk.mp4 --word-timestamps

# Active reading: nab uses MCP sampling to look up references mentioned in the audio
nab analyze interview.mp4 --active-reading

# Expose speaker embeddings for matching against hebb's voiceprint database
nab analyze interview.mp4 --diarize --include-embeddings

# Output JSON
nab analyze podcast.mp3 --format json
```

Real numbers from a 2 h 09 m English audio file (Karen Hao interview, MacBook Pro M-series):

| Metric | Value |
|--------|-------|
| Wall time | 59.6 s |
| Realtime factor | 131x |
| FluidAudio mean confidence | 97.18 % |
| Audio extraction (ffmpeg) | ~650x realtime |

| Backend | Platform | Languages | Diarization |
|---------|----------|-----------|-------------|
| `fluidaudio` (default on macOS arm64) | macOS arm64 | 25 EU languages, +zh/ja/ko/vi via Qwen3-ASR (opt-in) | PyAnnote community-1 |
| `sherpa-onnx` | Linux/x86, macOS, Windows | Parakeet ONNX, 25+ langs | sherpa-onnx pyannote-seg-3.0 |
| `whisper-rs` | Universal fallback | whisper-large-v3-turbo, 99 langs | none |

### Watch

`nab watch` turns any URL into a subscribable resource. MCP clients receive `notifications/resources/updated` when the content changes.

```bash
nab watch add https://news.ycombinator.com --interval 10m
nab watch add https://example.com/pricing --interval 1h --selector "table.pricing"
nab watch add https://api.openai.com/status --interval 5m --notify-on regression
nab watch list
nab watch logs <id>
nab watch remove <id>
```

Per-watch options:

| Flag | Default | Description |
|------|---------|-------------|
| `--interval <duration>` | 1h | Polling interval (`5m`, `1h`, `24h`) |
| `--selector <css>` | none | CSS selector to scope diff to one element |
| `--notify-on <kind>` | `any` | `any`, `regression`, `semantic` |
| `--diff <kind>` | `semantic` | `text`, `semantic`, `dom` |

The poller uses conditional GETs (`If-None-Match`, `If-Modified-Since`), so 304 responses cost effectively nothing. Watches with five consecutive failures auto-mute. Adaptive backoff applies on 429 and 503.

### Models

```bash
nab models list                           # show installed model versions
nab models fetch fluidaudio               # download FluidAudio binary + Parakeet weights
nab models update fluidaudio              # check for upstream updates
nab models verify fluidaudio              # checksum + smoke test
```

Both `whisper` and `sherpa-onnx` ship as cross-platform fallbacks alongside the macOS-default `fluidaudio` backend.

## MCP integration

`nab-mcp` is a native Rust MCP server. It runs over stdio (default) or Streamable HTTP. It is fully compliant with MCP protocol version `2025-11-25`.

### Quick setup (recommended)

```bash
nab mcp install                        # Claude Desktop (default)
nab mcp install --client claude-code   # Claude Code
nab mcp install --client cursor        # Cursor
nab mcp install --client windsurf      # Windsurf
nab mcp install --client codex         # OpenAI Codex CLI
nab mcp install --client vscode        # VS Code Copilot
nab mcp install --client zed           # Zed
nab mcp install --dry-run              # preview what would change
```

Also supported: `gemini`, `amazon-q`, `lm-studio`. This auto-detects the `nab-mcp` binary path, backs up your existing config, and adds the `nab` entry. Restart your client after installing.

### Manual setup

Add to your MCP client configuration (`~/.config/claude/mcp.json` or equivalent):

```json
{
  "mcpServers": {
    "nab": {
      "command": "nab-mcp"
    }
  }
}
```

### HTTP transport

```bash
nab mcp serve --http 127.0.0.1:8765
# or directly:
nab-mcp --http 127.0.0.1:8765
```

Bind to localhost by default. Origin checks and `MCP-Protocol-Version` header validation are enforced per spec.

### MCP capabilities

| Capability | Status |
|-----------|--------|
| Tools | 12 tools with structured output schemas, annotations, validation errors |
| Prompts | 4 prompts (`fetch-and-extract`, `multi-page-research`, `authenticated-fetch`, `match-speakers-with-hebb`) |
| Resources | 2 static + N dynamic watch resources, all subscribable |
| Logging | `notifications/message` with RFC 5424 levels |
| Sampling | nab calls back to the host LLM for active reading, focus extraction, form auto-fill |
| Roots | `roots/list` queried for workspace-scoped saves |
| Elicitation | Form mode + URL mode for OAuth/SSO |
| Argument completion | `completion/complete` for tool args |
| Server icons | Light + dark SVG |
| Transports | stdio + Streamable HTTP (resumable, session-scoped) |

The 12 MCP tools:

| Tool | Description |
|------|-------------|
| `fetch` | Fetch URL → markdown, with cookies, focus, token budget, session |
| `fetch_batch` | Parallel multi-URL fetch with task-augmented async execution |
| `submit` | Submit a form with CSRF + smart field extraction |
| `login` | 1Password auto-login with TOTP support |
| `auth_lookup` | Look up 1Password credentials for a URL |
| `fingerprint` | Generate browser fingerprint profiles |
| `validate` | Run the validation test suite |
| `benchmark` | Time URL fetches with stats |
| `analyze` | Transcribe and diarize audio/video |
| `watch_create` | Create a URL watch and subscribe |
| `watch_list` / `watch_remove` | Manage watches |

## Site providers

nab detects URLs for 12 platforms and uses APIs or stable structured page data instead of broad HTML scraping.

| Provider | URL pattern | Method |
|----------|-------------|--------|
| Twitter / X | `x.com/*/status/*` | FxTwitter API |
| Reddit | `reddit.com/r/*/comments/*` | JSON API |
| Hacker News | `news.ycombinator.com/item?id=*` | Firebase API |
| GitHub | `github.com/*/*/issues/*`, `*/pull/*` | REST API |
| Google Workspace | Docs, Sheets, Slides | Export API + OOXML |
| YouTube | `youtube.com/watch?v=*`, `youtu.be/*` | oEmbed |
| Wikipedia | `*.wikipedia.org/wiki/*` | REST API |
| StackOverflow | `stackoverflow.com/questions/*` | API |
| Mastodon | `*/users/*/statuses/*` | ActivityPub |
| LinkedIn | `linkedin.com/posts/*` | oEmbed |
| Instagram | `instagram.com/p/*`, `*/reel/*` | oEmbed |
| Substack | `*.substack.com/p/*`, `substack.com/*/p/*` | Article DOM (`.available-content`) |

If no provider matches, nab falls back to standard HTML fetch + markdown conversion.

## Architecture

nab is built around a small set of orthogonal subsystems: `cmd/` (CLI), `bin/mcp_server/` (MCP server), `content/` (HTML / PDF / OCR pipeline), `analyze/` (ASR + diarization + vision), `watch/` (URL monitoring + subscriptions), `auth/` (cookies + 1Password + WebAuthn), `site/` (per-site providers), and the shared `AcceleratedClient` (HTTP/3 + connection pool + fingerprint store).

See:

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — full module map and data flow
- [docs/sovereign-stack.md](docs/sovereign-stack.md) — how nab composes with hebb to form a local-first multimodal stack
- [docs/getting-started.md](docs/getting-started.md) — new user onboarding

### Design notes

The `docs/design/` directory tracks recent design proposals:

- [analyze-v2.md](docs/design/analyze-v2.md) — multilingual ASR + diarization + vision pipeline
- [url-watch-resources.md](docs/design/url-watch-resources.md) — URL watch as MCP subscribable resources
- [active-reading.md](docs/design/active-reading.md) — active reading via MCP sampling
- [mcp-spec-closure.md](docs/design/mcp-spec-closure.md) — closing the last MCP 2025-11-25 spec gaps

## Companion tools

nab is half of a sovereign multimodal stack. The other half is [hebb](https://github.com/MikkoParkkola/hebb), a neuroscience-inspired memory MCP server. Composition examples:

- `nab analyze --diarize --include-embeddings` → `hebb voice_match` → speakers labeled with names
- `nab fetch URL` → `hebb kv_set` → personal sovereign web memory
- `nab watch add URL` → `hebb kv_set` (on update) → time-series of changes to any web page

See [docs/sovereign-stack.md](docs/sovereign-stack.md) for the full composition story.

## Configuration

nab requires no configuration files. It uses smart defaults: auto-detected browser cookies, randomized fingerprints, and markdown output.

Persistent state lives in `~/.nab/`:

| Path | Purpose |
|------|---------|
| `~/.nab/snapshots/` | Content snapshots for `--diff` change detection |
| `~/.nab/sessions/` | AES-256-GCM encrypted named-session jars (non-Windows) |
| `~/.nab/session-key` | Locally generated master key for session encryption (non-Windows) |
| `~/.nab/fingerprint_versions.json` | Cached browser versions (auto-updates every 14 days) |
| `~/.local/share/nab/watches/` | URL watch state |
| `~/.local/share/nab/models/` | Installed inference model binaries |

Optional plugin configuration at `~/.config/nab/plugins.toml`. See [docs/getting-started.md](docs/getting-started.md) for plugin examples.

### Environment variables

| Variable | Purpose |
|----------|---------|
| `HTTPS_PROXY` / `https_proxy` | HTTPS proxy URL |
| `HTTP_PROXY` / `http_proxy` | HTTP proxy URL |
| `ALL_PROXY` / `all_proxy` | Proxy for all protocols |
| `RUST_LOG` | Logging level (e.g., `nab=debug`) |
| `PUSHOVER_USER` / `PUSHOVER_TOKEN` | Pushover notifications for MFA |
| `TELEGRAM_BOT_TOKEN` / `TELEGRAM_CHAT_ID` | Telegram notifications for MFA |

## Library usage

```rust
use nab::AcceleratedClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = AcceleratedClient::new()?;
    let html = client.fetch_text("https://example.com").await?;
    println!("Fetched {} bytes", html.len());
    Ok(())
}
```

## Requirements

- **Rust 1.95+** for building from source
- **ffmpeg** for `analyze` and `stream` commands: `brew install ffmpeg`
- **1Password CLI** (optional, for credential integration): see [1Password docs](https://developer.1password.com/docs/cli/get-started/)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, code style guidelines, testing instructions, and pull request process.

## Responsible use

This tool includes browser cookie extraction and fingerprint spoofing capabilities. They are intended for legitimate use cases — accessing your own authenticated content, automated testing, sites where you have authorization. Use responsibly.

## Troubleshooting

**MCP server not connecting?** Run `nab-mcp` directly in your terminal to see errors. Verify the binary exists with `which nab-mcp`. If installed via `cargo install nab`, both `nab` and `nab-mcp` should be on your `$PATH`.

**Cookie extraction failing?** Grant Full Disk Access to your terminal in **System Settings > Privacy & Security > Full Disk Access** (macOS). Browser cookies are stored in protected directories. Use `--cookies brave` to target a specific browser.

**ASR model not found?** Run `nab models fetch fluidaudio` to download the model (~542 MB). The model directory is `~/.nab/models/`. Use `nab models list` to see what's installed.

**Fetch returning HTML instead of markdown?** Some sites block automated access. Try `nab fetch URL --cookies brave` to use your browser session, or `nab fetch URL --1password` for sites that need login.

**YARA-X guard redacted a fetch?** `nab fetch` and MCP `fetch` scan returned bodies by default before saving or returning content. `NAB_YARA_ACTION=refuse` blocks instead of redacting. `NAB_YARA_BYPASS=1` is an audited emergency opt-out.

**"too many open files" on watch?** Increase your ulimit: `ulimit -n 4096`. The default macOS limit (256) is too low for many concurrent watches.

## Ecosystem

nab is part of a suite of MCP tools:

| Tool | Description |
|------|-------------|
| [mcp-gateway](https://github.com/MikkoParkkola/mcp-gateway) | Universal MCP gateway — compact 12-15 tool surface replaces 100+ registrations |
| [trvl](https://github.com/MikkoParkkola/trvl) | AI travel agent — 36 MCP tools for flights, hotels, ground transport |
| **[nab](https://github.com/MikkoParkkola/nab)** | **Web content extraction — fetch any URL with cookies + anti-bot bypass** |
| [axterminator](https://github.com/MikkoParkkola/axterminator) | macOS GUI automation — 34 MCP tools via Accessibility API |

## License

nab is **dual-licensed** as of v0.9.0:

| Scope | License | File |
|-------|---------|------|
| Core fetch / analyze / watch / MCP server / public web fetching | MIT | [LICENSE](LICENSE) |
| Designated Enterprise Edition modules (authenticated reach + anti-bot) | PolyForm Noncommercial 1.0.0 | [LICENSE-EE.md](LICENSE-EE.md) |

EE-designated paths (every file carries `// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0`):

- `src/auth/` — 1Password, WebAuthn, and browser-cookie injection (premium authenticated reach)
- `src/fingerprint/` — browser fingerprint spoofing (anti-bot evasion)
- `src/waf/` — WAF challenge handling
- `src/site/` — per-site provider integrations (proprietary domain knowledge)
- `src/security/` — Secure Ingestion guard for stripping machine-targeted HTML directives and hidden metadata
- `crates/nab-yara-engine/` — fetch-time YARA-X signature guard for prompt injection, exfiltration, secrets, and obfuscation

**What this means in practice**:
- Free for noncommercial use, modification, redistribution.
- Commercial use of EE modules requires a separate commercial license — contact `mikko.parkkola@iki.fi`.
- All releases prior to v0.9.0 remain entirely MIT and stay MIT forever.
