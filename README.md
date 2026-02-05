# MicroFetch

[![Rust](https://img.shields.io/badge/rust-1.93+-blue.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Ultra-minimal browser engine with HTTP/3, JS support, cookie auth, passkeys, and anti-fingerprinting. Optimized for LLM token efficiency.

**Smart Defaults**: Auto-detects browser cookies, outputs markdown, zero configuration needed.

## ⚡ Quick Start

```bash
# Install
cargo install --path .

# Fetch a page (auto-cookies, markdown output)
nab fetch https://example.com
```

## Features

- **Zero Friction**: Auto-detects default browser (Dia, Brave, Chrome, Firefox, Safari, Edge) and uses cookies automatically
- **Token-Optimized**: Markdown output by default (25× savings vs HTML)
- **HTTP Acceleration**: HTTP/2 multiplexing, HTTP/3 (QUIC) with 0-RTT, TLS 1.3, Brotli/Zstd compression
- **Browser Fingerprinting**: Realistic Chrome/Firefox/Safari profiles to avoid detection
- **Authentication**:
  - Auto browser cookie extraction (default)
  - 1Password CLI integration
  - Apple Keychain password retrieval
  - Browser password storage (Chromium-based)
- **JavaScript**: QuickJS engine with minimal DOM (ES2020 support)
- **SPA Extraction**: 80% success rate across Next.js, React, Nuxt, Vue apps
- **Streaming**: HLS/DASH streaming with native and ffmpeg backends
- **Video/Audio Analysis**: Transcription, annotation, and subtitle generation
- **WebSocket**: Full WebSocket support with JSON-RPC convenience layer
- **Prefetching**: Early Hints (103) support, link hint extraction
- **Cross-Platform**: Works on macOS, Linux, and Windows. Cookie extraction has the broadest browser support on macOS.

## 📊 Performance

**Speed**: ~50ms typical response time with HTTP/3 and 0-RTT resumption

**Token Efficiency** (critical for LLM context):
| Tool | Output Size | Tokens | Use Case |
|------|-------------|--------|----------|
| `nab fetch` (markdown) | ~2KB | ~500 | LLM-optimized |
| `curl` (raw HTML) | ~50KB | ~12,500 | Traditional CLI |
| WebFetch (HTML→text) | ~50KB | ~12,500 | Claude built-in |

**25× token savings** vs raw HTML approaches. Preserves structure and links while removing noise.

**Benchmarks**:
```bash
nab bench "https://example.com,https://httpbin.org/get" -i 10
# Measures: median/p95/p99 latency, success rate, throughput
```

## Requirements

- **Rust 1.93+**
- **ffmpeg** (optional, for streaming/analyze/annotate commands): `brew install ffmpeg` / `apt install ffmpeg`
- **1Password CLI** (optional, for credential integration): [Install guide](https://developer.1password.com/docs/cli/get-started/)

## Installation

```bash
cargo install --path .
```

## Usage

### Fetch a URL
```bash
# Basic fetch (auto-detects browser cookies, outputs markdown)
nab fetch https://example.com

# Disable cookies
nab fetch https://example.com --cookies none

# Force specific browser
nab fetch https://example.com --cookies brave

# Raw HTML output (disable markdown)
nab fetch https://example.com --raw-html

# With 1Password credentials
nab fetch https://example.com --1password
```

## 🔐 Authentication Examples

Real-world patterns for accessing authenticated content:

### Browser Cookie Auth (Default)
```bash
# Auto-detects default browser (Dia, Brave, Chrome, Firefox, Safari, Edge)
nab fetch https://github.com/notifications
# Uses your active browser session automatically

# Force specific browser if auto-detection fails
nab fetch https://linkedin.com/feed --cookies brave
nab fetch https://twitter.com/home --cookies chrome
```

### 1Password Integration
```bash
# Fetch with 1Password credentials (prompts for item selection)
nab fetch https://internal.company.com --1password

# Get OTP code from 1Password or iMessage
nab otp github.com
# Returns: 123456

# Test credential retrieval
nab auth https://github.com
```

### Keychain Passwords
```bash
# Retrieves password from Apple Keychain (macOS)
# Automatically used when --1password flag is present
nab fetch https://example.com --1password
```

### Session Warmup (for APIs requiring prior page load)
```bash
# Load dashboard first to establish session, then fetch API
nab fetch https://api.example.com/data \
  --cookies brave \
  --warmup-url https://example.com/dashboard
```

**Cookie Troubleshooting**:
- Check browser selection: `nab fetch URL --cookies brave`
- Debug cookie detection: `RUST_LOG=debug nab fetch URL`
- Ensure browser is running or has recent session
- macOS has best browser support (Dia, Brave, Chrome, Firefox, Safari, Edge)

### Extract Data from SPAs (React, Next.js, Vue, Nuxt)
```bash
# Auto-extracts embedded JSON (__NEXT_DATA__, __NUXT__, window state)
# 80% success rate, auto-cookies, 5s wait, fetch logging
nab spa https://nextjs-app.com

# Extract specific JSON path
nab spa https://nextjs-app.com --extract "props.pageProps.data"

# Structure summary
nab spa https://nextjs-app.com --summary
```

### Streaming (HLS/DASH)
```bash
# Stream to player
nab stream generic https://example.com/master.m3u8 vlc

# Stream to file with duration limit
nab stream generic https://example.com/master.m3u8 file --duration 60
```

### Video/Audio Analysis
```bash
# Transcribe and analyze media
nab analyze video.mp4

# Add subtitle annotations
nab annotate video.mp4
```

### Benchmark
```bash
nab bench "https://example.com,https://httpbin.org/get" -i 10
```

### Generate Browser Fingerprints
```bash
nab fingerprint -c 5
```

### Test 1Password Integration
```bash
nab auth https://github.com
```

## 🚀 LLM Integration

nab is designed for AI workflows where token efficiency matters:

### Claude/LLM Context Example
```bash
# Traditional approach (❌ 12,500 tokens)
curl https://docs.anthropic.com/claude/docs | claude

# nab approach (✅ 500 tokens - 25× savings)
nab fetch https://docs.anthropic.com/claude/docs | claude
```

### Token Comparison
| Method | Tokens | Cost (Opus input) | Use Case |
|--------|--------|-------------------|----------|
| `nab fetch` (MD) | 500 | $0.0075 | LLM context |
| `curl` (HTML) | 12,500 | $0.1875 | Raw data |
| WebFetch tool | 12,500 | $0.1875 | Built-in fallback |

**Savings**: $0.18 per page = **$1,800/yr** at 10K pages

### Output Formats
```bash
# Markdown output (default, 25× token savings)
nab fetch https://example.com

# Compact format: STATUS SIZE TIME
nab fetch https://api.example.com --format compact
# 200 1234B 45ms

# JSON format for parsing
nab fetch https://api.example.com --format json

# Save full body to file (bypasses truncation)
nab fetch https://example.com --output body.html

# Raw HTML (disable markdown conversion)
nab fetch https://example.com --raw-html
```

### Custom Headers & Session Warmup
```bash
# Add custom headers (API access)
nab fetch https://api.example.com \
  --add-header "Accept: application/json" \
  --add-header "X-Custom: value"

# Auto-add Referer header
nab fetch https://api.example.com --auto-referer

# Warmup session first (for APIs requiring prior page load)
nab fetch https://api.example.com/data \
  --cookies brave \
  --warmup-url https://example.com/dashboard
```

### Get OTP Codes
```bash
nab otp github.com
```

### Validate All Features
```bash
nab validate
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

## HTTP/3 Support

HTTP/3 is enabled by default. To disable:

```bash
cargo build --no-default-features --features cli
```

## ❓ FAQ / Troubleshooting

### Why not curl or wget?
- **Token Efficiency**: curl outputs raw HTML (~12,500 tokens), nab outputs markdown (~500 tokens) - 25× savings for LLM context
- **Auth Integration**: curl requires manual cookie copying, nab auto-detects browser cookies
- **Modern Protocols**: curl lacks HTTP/3 support in many builds, nab has HTTP/3 + 0-RTT by default
- **Anti-Fingerprinting**: curl is easily detected, nab spoofs realistic browser fingerprints

### Cookie Detection Not Working?
1. Verify browser: `nab fetch URL --cookies brave` (try different browsers)
2. Check browser is running or has recent session
3. Enable debug logging: `RUST_LOG=debug nab fetch URL`
4. macOS has broadest support (Dia, Brave, Chrome, Firefox, Safari, Edge)
5. Use `--1password` as fallback for credential auth

### HTTP/3 Issues?
```bash
# Disable HTTP/3 if site has compatibility issues
cargo build --no-default-features --features cli
```

### Debug Output
```bash
# See detailed request/response info
RUST_LOG=debug nab fetch https://example.com

# Trace-level for maximum detail
RUST_LOG=trace nab fetch https://example.com
```

### Performance Tuning
```bash
# Benchmark to identify slow sites
nab bench "https://example.com" -i 10

# Use --raw-html to skip markdown conversion (faster, more tokens)
nab fetch https://example.com --raw-html
```

## Responsible Use

This tool includes browser cookie extraction and fingerprint spoofing capabilities. These features are intended for legitimate use cases such as accessing your own authenticated content and automated testing. Use responsibly and only on sites where you have authorization.

## License

MIT License - see [LICENSE](LICENSE) for details.

## Credits

Created by [Mikko Parkkola](https://github.com/MikkoParkkola)
