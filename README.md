# nab

[![CI](https://github.com/MikkoParkkola/nab/actions/workflows/ci.yml/badge.svg)](https://github.com/MikkoParkkola/nab/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/nab.svg)](https://crates.io/crates/nab)
[![Downloads](https://img.shields.io/crates/d/nab.svg)](https://crates.io/crates/nab)
[![docs.rs](https://img.shields.io/docsrs/nab)](https://docs.rs/nab)
[![Rust](https://img.shields.io/badge/Rust-1.93+-orange.svg?logo=rust)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![unsafe forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)](https://github.com/rust-secure-code/safety-dance)
[![dependency status](https://deps.rs/repo/github/MikkoParkkola/nab/status.svg)](https://deps.rs/repo/github/MikkoParkkola/nab)
[![Tests](https://img.shields.io/badge/tests-1252+-brightgreen.svg)](https://github.com/MikkoParkkola/nab/actions/workflows/ci.yml)
[![MCP Protocol](https://img.shields.io/badge/MCP-2025--11--25-blueviolet.svg)](https://modelcontextprotocol.io)
[![nab MCP server](https://glama.ai/mcp/servers/MikkoParkkola/nab/badges/score.svg)](https://glama.ai/mcp/servers/MikkoParkkola/nab)
[![MCP Servers](https://img.shields.io/badge/MCP_Tools-8-blue.svg)](https://glama.ai/mcp/servers/MikkoParkkola/nab)
[![Capabilities](https://img.shields.io/badge/Capabilities-tools%20%7C%20elicitation%20%7C%20structured_output-informational.svg)](https://modelcontextprotocol.io)

<a href="https://glama.ai/mcp/servers/MikkoParkkola/nab"><img width="380" height="200" src="https://glama.ai/mcp/servers/MikkoParkkola/nab/badge" alt="nab MCP server" /></a>

**Fetch any URL as clean markdown — with your browser cookies, anti-bot evasion, and 25x fewer tokens than raw HTML.**

![demo](demo.gif)

nab is a local, token-optimized HTTP client built for LLM pipelines. It converts web pages to clean markdown, injects your real browser session cookies for authenticated content, and spoofs browser fingerprints to bypass bot detection. No API keys. No cloud. Just fast, authenticated, LLM-ready output.

## Why nab?

| Feature | nab | Firecrawl | Crawl4AI | Playwright | Jina Reader | curl |
|---|---|---|---|---|---|---|
| **Clean markdown output** | Built-in (25x savings) | Markdown | Markdown | Raw HTML | Markdown | Raw HTML |
| **Browser cookie auth** | Auto-detect (6 browsers) | None | None | Requires login script | API key | Manual |
| **Anti-bot evasion** | Fingerprint spoofing | Cloud proxy | Stealth plugin | Stealth plugin | Cloud-side | None |
| **JS rendering** | QuickJS (1MB, local) | Cloud browser | Chromium (300MB+) | Chromium (300MB+) | Cloud-side | None |
| **Speed (typical page)** | ~50ms | ~1-3s | ~2-5s | ~2-5s | ~500ms | ~100ms |
| **Token output (typical)** | ~500 | ~1,500 | ~1,500 | ~12,500 | ~2,000 | ~12,500 |
| **Runs locally** | Yes (single binary) | Cloud API | Yes (Python + Chrome) | Yes (Node + Chrome) | Cloud API | Yes |
| **HTTP/3 (QUIC)** | Yes | No | No | No | N/A | Build-dependent |
| **Site-specific APIs** | 11 built-in providers | None | None | None | None | None |
| **1Password / Passkeys** | Native | None | None | None | None | None |
| **Cost** | Free (local) | $0.004/page | Free (local) | Free (local) | Free tier / paid | Free (local) |
| **Install size** | ~15MB binary | Cloud service | ~300MB+ | ~300MB+ | Cloud service | ~5MB |

## Quick Start

```bash
# Install (pick one)
brew install MikkoParkkola/tap/nab    # Homebrew
cargo install nab                      # From crates.io
cargo binstall nab                     # Pre-built binary
```

### Fetch a page as clean markdown

```bash
nab fetch https://example.com
```

### Access authenticated content with your browser cookies

```bash
# Auto-detects your default browser and injects session cookies
nab fetch https://github.com/notifications --cookies brave
```

No login flows. No API keys. nab reads your existing browser cookies (Brave, Chrome, Firefox, Safari, Edge, Dia) and uses them for the request. You stay logged in — nab just borrows the session.

### Bypass bot detection with fingerprint spoofing

```bash
# Realistic Chrome/Firefox/Safari profiles — not a headless browser signature
nab fetch https://protected-site.com
```

nab ships with anti-fingerprinting by default: realistic TLS fingerprints, browser-accurate headers, and randomized profiles. Sites see a normal browser, not a scraping tool.

## Features

- **11 Site Providers** — Specialized extractors for Twitter/X, Reddit, Hacker News, GitHub, Google Workspace, YouTube, Wikipedia, StackOverflow, Mastodon, LinkedIn, and Instagram. API-backed where possible for structured output.
- **Google Workspace Extraction** — Fetch Google Docs, Sheets, and Slides as clean markdown using browser cookies. Extracts comments and suggested edits from OOXML (docx/xlsx/pptx).
- **HTML-to-Markdown** — Automatic conversion with boilerplate removal. 25x token savings vs raw HTML.
- **PDF Extraction** — PDF-to-markdown with heading and table detection (requires pdfium).
- **Browser Cookie Auth** — Auto-detects your default browser (Brave, Chrome, Firefox, Safari, Edge, Dia) and injects session cookies. Zero config.
- **1Password Integration** — Credential lookup, auto-login with CSRF handling, TOTP/MFA support.
- **Passkey/WebAuthn** — Detects passkey flows, surfaces 1Password passkey metadata, and hands off final signing to the browser when needed.
- **HTTP/3 (QUIC)** — 0-RTT connection resumption, HTTP/2 multiplexing, TLS 1.3.
- **Anti-Fingerprinting** — Realistic Chrome/Firefox/Safari browser profiles to avoid bot detection.
- **JS Engine (QuickJS)** — Lightweight embedded JavaScript for pages that need it, without a full browser.
- **Compression** — Brotli, Zstd, Gzip, Deflate decompression built in.
- **Query-Focused Extraction** — BM25-lite scoring extracts only the sections relevant to your query. Send `focus="authentication"` and get back just the auth docs, not the entire page.
- **Token Budget** — Structure-aware truncation respects headings, code blocks, and tables. Never splits mid-block. Set `max_tokens=2000` to fit any context window.
- **Prefetch Link Graph** — Extract same-site links from fetched pages, scored by relevance to your focus query. eTLD+1 filtering via Mozilla's public suffix list.
- **Persistent Sessions** — Named sessions with automatic cookie persistence across requests. LRU eviction (32 slots), cookie seeding from browser jars.
- **CSS Extractor Plugins** — Define custom extractors in `plugins.toml` using CSS selectors — no Rust code required.
- **MCP Server** — `nab-mcp` binary for direct integration with Claude Code and other MCP clients.
- **Batch Fetching** — Parallel URL fetching with connection pooling.

## Site Providers

nab detects URLs for these platforms and uses their APIs or structured data instead of scraping HTML:

| Provider | URL Patterns | Method |
|----------|-------------|--------|
| Twitter/X | `x.com/*/status/*`, `twitter.com/*/status/*` | FxTwitter API |
| Reddit | `reddit.com/r/*/comments/*` | JSON API |
| Hacker News | `news.ycombinator.com/item?id=*` | Firebase API |
| GitHub | `github.com/*/*/issues/*`, `*/pull/*` | REST API |
| Google Workspace | `docs.google.com/document/d/*`, `*/spreadsheets/d/*`, `*/presentation/d/*` | Export API + OOXML |
| YouTube | `youtube.com/watch?v=*`, `youtu.be/*` | oEmbed |
| Wikipedia | `*.wikipedia.org/wiki/*` | REST API |
| StackOverflow | `stackoverflow.com/questions/*` | API |
| Mastodon | `*/users/*/statuses/*` | ActivityPub |
| LinkedIn | `linkedin.com/posts/*` | oEmbed |
| Instagram | `instagram.com/p/*`, `*/reel/*` | oEmbed |

If no provider matches, nab falls back to standard HTML fetch + markdown conversion.

## Usage

```bash
# Basic fetch (auto-cookies, markdown output)
nab fetch https://example.com

# Force specific browser cookies
nab fetch https://github.com/notifications --cookies brave

# With 1Password credentials
nab fetch https://internal.company.com --1password

# Google Docs (markdown with comments and suggested edits)
nab fetch --cookies brave "https://docs.google.com/document/d/DOCID/edit"

# Google Sheets (CSV rendered as markdown table)
nab fetch --cookies brave "https://docs.google.com/spreadsheets/d/SHEETID/edit"

# Google Slides (plain text with comments)
nab fetch --cookies brave "https://docs.google.com/presentation/d/SLIDEID/edit"

# Raw HTML output (skip markdown conversion)
nab fetch https://example.com --raw-html

# JSON output format
nab fetch https://api.example.com --format json

# Batch benchmark
nab bench "https://example.com,https://httpbin.org/get" -i 10

# Get OTP code from 1Password
nab otp github.com

# Generate browser fingerprint profiles
nab fingerprint -c 5
```

## CLI Reference

| Command | Description |
|---------|-------------|
| `nab fetch <url>` | Fetch a URL and convert to clean markdown |
| `nab spa <url>` | Extract data from JavaScript-heavy SPA pages |
| `nab submit <url>` | Submit a form with smart field extraction and CSRF handling |
| `nab login <url>` | Auto-login to a website using 1Password credentials |
| `nab stream <source> <id>` | Stream media from various providers (Yle, NRK, SVT, DR) |
| `nab analyze <video>` | Analyze video with transcription and vision pipeline |
| `nab annotate <video> <output>` | Add subtitles and overlays to video |
| `nab bench <urls>` | Benchmark fetching with timing statistics |
| `nab fingerprint` | Generate and display browser fingerprint profiles |
| `nab auth <url>` | Test 1Password credential lookup for a URL |
| `nab validate` | Run validation tests against real websites |
| `nab otp <domain>` | Get OTP code from 1Password |
| `nab cookies export <domain>` | Export browser cookies in Netscape format |

Common flags for `fetch`:

| Flag | Description |
|------|-------------|
| `--cookies <browser>` | Use cookies from browser: `auto`, `brave`, `chrome`, `firefox`, `safari`, `edge`, `none` |
| `--1password` / `--op` | Use 1Password credentials for this URL |
| `--proxy <url>` | HTTP or SOCKS5 proxy URL |
| `--format <fmt>` | Output format: `full` (default), `compact`, `json` |
| `--raw-html` | Output raw HTML instead of markdown |
| `--links` | Extract links only |
| `--diff` | Show what changed since the last fetch |
| `--no-spa` | Disable SPA data extraction |
| `--batch <file>` | Batch fetch URLs from file (one per line) |
| `--parallel <n>` | Max concurrent requests for batch mode (default: 5) |
| `-X <method>` | HTTP method: GET, POST, PUT, DELETE, PATCH |
| `-d <data>` | Request body data (for POST/PUT/PATCH) |
| `--add-header <h>` | Custom request header (repeatable) |
| `-o <path>` | Save body to file |
| `-v` | Enable verbose debug logging |

## PDF Extraction

nab converts PDF files to markdown with heading detection and table reconstruction. Requires [pdfium](https://pdfium.googlesource.com/pdfium/) (ships with Chromium, or install via Homebrew).

```bash
# Fetch a PDF and convert to markdown
nab fetch https://example.com/report.pdf

# Save PDF conversion to file
nab fetch https://arxiv.org/pdf/2301.00001 -o paper.md
```

The PDF pipeline extracts character positions via pdfium, reconstructs text lines, detects tables through column alignment, and renders clean markdown. Target performance is ~10ms/page. Maximum input size is 50 MB.

## Proxy Support

nab supports HTTP and SOCKS5 proxies via the `--proxy` flag or environment variables.

```bash
# Explicit proxy
nab fetch https://example.com --proxy socks5://127.0.0.1:1080
nab fetch https://example.com --proxy http://proxy.company.com:8080

# Environment variables (checked in this order)
export HTTPS_PROXY=http://proxy:8080
export HTTP_PROXY=http://proxy:8080
export ALL_PROXY=socks5://proxy:1080
```

The `--proxy` flag takes precedence over environment variables. Both uppercase and lowercase variants (`HTTPS_PROXY` / `https_proxy`) are recognized.

## Environment Variables

| Variable | Purpose |
|----------|---------|
| `HTTPS_PROXY` / `https_proxy` | HTTPS proxy URL |
| `HTTP_PROXY` / `http_proxy` | HTTP proxy URL |
| `ALL_PROXY` / `all_proxy` | Proxy for all protocols |
| `ANTHROPIC_API_KEY` | Claude API key for `analyze` command vision features |
| `RUST_LOG` | Logging level (e.g., `nab=debug`) |
| `PUSHOVER_USER` / `PUSHOVER_TOKEN` | Pushover notifications for MFA |
| `TELEGRAM_BOT_TOKEN` / `TELEGRAM_CHAT_ID` | Telegram notifications for MFA |

## Configuration

nab requires no configuration files. It uses smart defaults: auto-detected browser cookies, randomized fingerprints, and markdown output.

**Optional plugin configuration** at `~/.config/nab/plugins.toml`:

```toml
# Binary plugin — external process (original format)
[[plugins]]
name = "my-provider"
binary = "/usr/local/bin/nab-plugin-example"
patterns = ["example\\.com/.*"]

# CSS extractor — no external binary needed (new in v0.5)
[[plugins]]
name     = "internal-wiki"
type     = "css"
patterns = ["wiki\\.corp\\.com/.*"]

[plugins.content]
selector = "div.wiki-content"
remove   = ["nav", ".sidebar"]

[plugins.metadata]
title     = "h1.page-title"
author    = ".author-name"
published = "time.published"
```

Binary plugins receive a URL as JSON on stdin and return markdown on stdout. CSS extractors run in-process using configurable selectors — no code required.

**Persistent state** stored in `~/.nab/`:

| Path | Purpose |
|------|---------|
| `~/.nab/snapshots/` | Content snapshots for `--diff` change detection |
| `~/.nab/sessions/` | Saved login sessions |
| `~/.nab/fingerprint_versions.json` | Cached browser versions (auto-updates every 14 days) |

## MCP Server

nab ships a native Rust MCP server (`nab-mcp`) for integration with Claude Code and other MCP clients.

**Setup** -- add to your MCP client configuration:

```json
{
  "mcpServers": {
    "nab": {
      "command": "nab-mcp"
    }
  }
}
```

**Available tools:**

| Tool | Description | Key Parameters |
|------|-------------|------------|
| `fetch` | Fetch URL and convert to markdown | `url`, `cookies`, `focus`, `max_tokens`, `session` |
| `fetch_batch` | Fetch multiple URLs in parallel | `urls` (array) |
| `submit` | Submit a web form with CSRF extraction | `url`, `fields`, `cookies`, `session` |
| `login` | Auto-login via 1Password | `url`, `cookies`, `session` |
| `auth_lookup` | Look up 1Password credentials | `url` |
| `fingerprint` | Generate browser fingerprints | `count`, `browser` |
| `validate` | Run validation test suite | — |
| `benchmark` | Benchmark URL fetching | `urls`, `iterations` |

The MCP server uses MCP protocol **2025-11-25** (latest) over stdio and shares a single `AcceleratedClient` across all tool calls for connection pooling.

**Protocol features:**

- **Tool annotations** — read-only, destructive, and open-world hints on all 8 tools
- **Structured output** — `outputSchema` + `structured_content` on all 8 tools (machine-parseable JSON alongside human-readable text)
- **URL elicitation** — OAuth/SSO login sends the user to the auth URL in-browser (Google, GitHub, Microsoft, Apple, and 9 more)
- **Form elicitation** — interactive credential input and multi-select cookie source picker
- **Task-augmented execution** — `fetch_batch` can run asynchronously with progress notifications
- **Server icons** — globe SVG in light/dark themes

## Benchmarks

HTML-to-markdown conversion throughput (via `cargo bench`):

| Payload | Throughput |
|---------|-----------|
| 1 KB HTML | 2.8 MB/s |
| 10 KB HTML | 14.5 MB/s |
| 50 KB HTML | 22.3 MB/s |
| 200 KB HTML | 28.1 MB/s |

Arena allocator vs `Vec<String>` for response buffering:

| Benchmark | Arena (bumpalo) | Vec | Speedup |
|-----------|----------------|-----|---------|
| Realistic 10KB response | 4.2 us | 9.3 us | 2.2x |
| 1MB large response | 380 us | 890 us | 2.3x |
| 1000 small allocations | 12 us | 28 us | 2.3x |

Run benchmarks yourself: `cargo bench`

## Install

### Homebrew (macOS/Linux)

```bash
brew tap MikkoParkkola/tap
brew install nab
```

### From crates.io (requires Rust 1.93+)

```bash
cargo install nab
```

### Pre-built binary (cargo-binstall)

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
cd nab && cargo install --path .
```

## Library Usage

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

- **Rust 1.93+** (for building from source)
- **ffmpeg** (optional, for streaming/analyze commands): `brew install ffmpeg`
- **1Password CLI** (optional): [Install guide](https://developer.1password.com/docs/cli/get-started/)

## Architecture

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full internal architecture, module organization, data flow diagrams, and extension points.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, code style guidelines, testing instructions, and pull request process.

## Responsible Use

This tool includes browser cookie extraction and fingerprint spoofing capabilities. These features are intended for legitimate use cases such as accessing your own authenticated content and automated testing. Use responsibly and only on sites where you have authorization.

## License

MIT License - see [LICENSE](LICENSE) for details.
