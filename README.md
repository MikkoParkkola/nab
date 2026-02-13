# nab

[![CI](https://github.com/MikkoParkkola/nab/actions/workflows/ci.yml/badge.svg)](https://github.com/MikkoParkkola/nab/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/nab.svg)](https://crates.io/crates/nab)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Fetch any URL as clean markdown. Auth-aware. LLM-optimized. Blazing fast.

## Quick Install

```bash
# Homebrew (macOS/Linux)
brew install MikkoParkkola/tap/nab

# From crates.io
cargo install nab

# Pre-built binary (cargo-binstall)
cargo binstall nab

# From source
git clone https://github.com/MikkoParkkola/nab.git
cd nab && cargo install --path .
```

## Demo

![nab demo](demo.gif)

25x fewer tokens. Same content. Zero configuration.

## Features

- **10 Site Providers** - Specialized extractors for Twitter/X, Reddit, Hacker News, GitHub, YouTube, Wikipedia, StackOverflow, Mastodon, LinkedIn, and Instagram. API-backed where possible for structured output.
- **HTML-to-Markdown** - Automatic conversion with boilerplate removal. 25x token savings vs raw HTML.
- **PDF Extraction** - PDF-to-markdown with heading and table detection (requires pdfium).
- **Browser Cookie Auth** - Auto-detects your default browser (Brave, Chrome, Firefox, Safari, Edge, Dia) and injects session cookies. Zero config.
- **1Password Integration** - Credential lookup, auto-login with CSRF handling, TOTP/MFA support.
- **HTTP/3 (QUIC)** - 0-RTT connection resumption, HTTP/2 multiplexing, TLS 1.3.
- **Anti-Fingerprinting** - Realistic Chrome/Firefox/Safari browser profiles to avoid bot detection.
- **Compression** - Brotli, Zstd, Gzip, Deflate decompression built in.
- **MCP Server** - `nab-mcp` binary for direct integration with Claude Code and other MCP clients.
- **Batch Fetching** - Parallel URL fetching with connection pooling.

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

## Site Providers

nab detects URLs for these platforms and uses their APIs or structured data instead of scraping HTML:

| Provider | URL Patterns | Method |
|----------|-------------|--------|
| Twitter/X | `x.com/*/status/*`, `twitter.com/*/status/*` | FxTwitter API |
| Reddit | `reddit.com/r/*/comments/*` | JSON API |
| Hacker News | `news.ycombinator.com/item?id=*` | Firebase API |
| GitHub | `github.com/*/*/issues/*`, `*/pull/*` | REST API |
| YouTube | `youtube.com/watch?v=*`, `youtu.be/*` | oEmbed |
| Wikipedia | `*.wikipedia.org/wiki/*` | REST API |
| StackOverflow | `stackoverflow.com/questions/*` | API |
| Mastodon | `*/users/*/statuses/*` | ActivityPub |
| LinkedIn | `linkedin.com/posts/*` | oEmbed |
| Instagram | `instagram.com/p/*`, `*/reel/*` | oEmbed |

If no provider matches, nab falls back to standard HTML fetch + markdown conversion.

## MCP Server

nab ships a native Rust MCP server (`nab-mcp`) for integration with Claude Code:

```json
{
  "mcpServers": {
    "nab": {
      "command": "nab-mcp"
    }
  }
}
```

Tools: `fetch`, `fetch_batch`, `submit`, `login`, `auth_lookup`, `fingerprint`, `validate`, `benchmark`.

## Usage

```bash
# Basic fetch (auto-cookies, markdown output)
nab fetch https://example.com

# Force specific browser cookies
nab fetch https://github.com/notifications --cookies brave

# With 1Password credentials
nab fetch https://internal.company.com --1password

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

## Comparison

| | nab | curl | Jina Reader | FireCrawl |
|---|---|---|---|---|
| **Output** | Clean markdown | Raw HTML | Markdown | Markdown |
| **Tokens (typical page)** | ~500 | ~12,500 | ~2,000 | ~2,000 |
| **Speed** | ~50ms | ~100ms | ~500ms | ~1-3s |
| **Auth** | Cookies + 1Password | Manual | API key | API key |
| **Site providers** | 10 built-in | None | None | None |
| **Cost** | Free (local) | Free (local) | Free tier / paid | Paid |
| **HTTP/3** | Yes | Build-dependent | N/A (cloud) | N/A (cloud) |

## Install Options

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

## Responsible Use

This tool includes browser cookie extraction and fingerprint spoofing capabilities. These features are intended for legitimate use cases such as accessing your own authenticated content and automated testing. Use responsibly and only on sites where you have authorization.

## License

MIT License - see [LICENSE](LICENSE) for details.
