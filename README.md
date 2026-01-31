# MicroFetch

[![Rust](https://img.shields.io/badge/rust-1.93+-blue.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Ultra-minimal browser engine with HTTP/3, JS support, cookie auth, passkeys, and anti-fingerprinting. Optimized for LLM token efficiency.

**Smart Defaults**: Auto-detects browser cookies, outputs markdown, zero configuration needed.

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
microfetch fetch https://example.com

# Disable cookies
microfetch fetch https://example.com --cookies none

# Force specific browser
microfetch fetch https://example.com --cookies brave

# Raw HTML output (disable markdown)
microfetch fetch https://example.com --raw-html

# With 1Password credentials
microfetch fetch https://example.com --1password
```

### Extract Data from SPAs (React, Next.js, Vue, Nuxt)
```bash
# Auto-extracts embedded JSON (__NEXT_DATA__, __NUXT__, window state)
# 80% success rate, auto-cookies, 5s wait, fetch logging
microfetch spa https://nextjs-app.com

# Extract specific JSON path
microfetch spa https://nextjs-app.com --extract "props.pageProps.data"

# Structure summary
microfetch spa https://nextjs-app.com --summary
```

### Streaming (HLS/DASH)
```bash
# Stream to player
microfetch stream generic https://example.com/master.m3u8 vlc

# Stream to file with duration limit
microfetch stream generic https://example.com/master.m3u8 file --duration 60
```

### Video/Audio Analysis
```bash
# Transcribe and analyze media
microfetch analyze video.mp4

# Add subtitle annotations
microfetch annotate video.mp4
```

### Benchmark
```bash
microfetch bench "https://example.com,https://httpbin.org/get" -i 10
```

### Generate Browser Fingerprints
```bash
microfetch fingerprint -c 5
```

### Test 1Password Integration
```bash
microfetch auth https://github.com
```

### Token-Optimized Output (LLM-friendly)
```bash
# Markdown output (default, 25× token savings)
microfetch fetch https://example.com

# Compact format: STATUS SIZE TIME
microfetch fetch https://api.example.com --format compact
# 200 1234B 45ms

# JSON format for parsing
microfetch fetch https://api.example.com --format json

# Save full body to file (bypasses truncation)
microfetch fetch https://example.com --output body.html

# Raw HTML (disable markdown conversion)
microfetch fetch https://example.com --raw-html
```

### Custom Headers & Session Warmup
```bash
# Add custom headers (API access)
microfetch fetch https://api.example.com \
  --add-header "Accept: application/json" \
  --add-header "X-Custom: value"

# Auto-add Referer header
microfetch fetch https://api.example.com --auto-referer

# Warmup session first (for APIs requiring prior page load)
microfetch fetch https://api.example.com/data \
  --cookies brave \
  --warmup-url https://example.com/dashboard
```

### Get OTP Codes
```bash
microfetch otp github.com
```

### Validate All Features
```bash
microfetch validate
```

## Library Usage

```rust
use microfetch::AcceleratedClient;

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

## Responsible Use

This tool includes browser cookie extraction and fingerprint spoofing capabilities. These features are intended for legitimate use cases such as accessing your own authenticated content and automated testing. Use responsibly and only on sites where you have authorization.

## License

MIT License - see [LICENSE](LICENSE) for details.

## Credits

Created by [Mikko Parkkola](https://github.com/MikkoParkkola)
