# Contributing to nab

Thank you for your interest in contributing to nab, an ultra-minimal browser engine with HTTP/3, JavaScript execution, and anti-fingerprinting capabilities.

## Prerequisites

- **Rust 1.95+**: `rustup update`
- **ffmpeg** (optional, for streaming/analysis): `brew install ffmpeg` / `apt install ffmpeg`
- **1Password CLI** (optional, for credential integration): [Install guide](https://developer.1password.com/docs/cli/get-started/)

## Building

```bash
# Clone the repository
git clone https://github.com/MikkoParkkola/nab.git
cd nab

# Build in release mode
cargo build --release

# Run tests
cargo test

# Run with debug logging
RUST_LOG=nab=debug cargo run -- fetch https://example.com
```

## Development Workflow

1. **Fork the repository** on GitHub
2. **Create a feature branch** from `main`:
   ```bash
   git checkout -b feature/your-feature-name
   ```
3. **Make your changes** following the code style guidelines below
4. **Test your changes**:
   ```bash
   cargo test
   cargo clippy --all-targets --all-features
   cargo fmt --check
   ```
5. **Commit with clear messages**:
   ```bash
   git commit -m "Add feature: clear description of what changed"
   ```
6. **Push to your fork** and **create a Pull Request**

## Code Style

- **Follow Rust conventions**: Use `cargo fmt` to format all code
- **Pass clippy lints**: Run `cargo clippy` with no warnings
- **Add tests**: New features should include tests
- **Document public APIs**: Add doc comments for public functions and types
- **Keep functions focused**: Maximum ~30 lines per function
- **Use tracing for logging**: Prefer `tracing::{debug, info, warn, error}` over `println!`

## Testing

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test
cargo test test_name

# Run with verbose logging
RUST_LOG=nab=debug cargo test -- --nocapture
```

### Test Categories

- **Unit tests**: In module files alongside code (`#[cfg(test)] mod tests`)
- **Integration tests**: In `tests/` directory
- **Real-world validation**: Use `nab validate` command

## Feature Flags

nab uses Cargo features for optional functionality:

- **`cli`** (default): Enables CLI binary with clap argument parsing
- **`http3`** (default): Enables HTTP/3 and QUIC support via quinn
- **`impersonate`** (default): TLS fingerprint impersonation via BoringSSL (Chrome/Safari/Firefox profiles)
- **`pdf`**: PDF-to-markdown conversion via pdfium (requires pdfium library)
- **`browser`**: Browser automation via Chrome DevTools Protocol (requires running Chrome)

To build without heavy defaults (faster compile):
```bash
cargo build --no-default-features --features cli
```

## Module Organization

Key modules:
- `http_client`: HTTP/2 client with connection pooling, compression, fingerprint headers
- `http3_client`: HTTP/3 (QUIC) with 0-RTT connection resumption
- `impersonate_client`: TLS fingerprint impersonation via BoringSSL (feature: `impersonate`)
- `js_engine`: QuickJS-based JavaScript execution (ES2020)
- `auth/`: 1Password, browser cookie extraction (AES-128-CBC decryption), OTP retrieval
- `fingerprint/`: Browser fingerprint generation, auto-update from real versions
- `content/`: HTML/plain/PDF conversion, readability extraction, quality scoring, query-focused extraction (`focus`), token budget (`budget`), diff tracking, link extraction
- `site/`: Site-specific extractors — `linkedin/` (7 files), `google/` (Workspace OOXML), `github`, `hackernews`, `reddit`, `css_extractor`, `rules/` (JSON rule configs)
- `plugin/`: CSS selector plugin system — custom extractors in `plugins.toml`
- `stream/`: HLS/DASH streaming — backends (native_hls, ffmpeg, streamlink), providers (NRK, SVT, DR, generic)
- `analyze/`: Video/audio transcription, speaker diarization, vision analysis, fusion
- `annotate/`: Subtitle generation, visual overlays, ffmpeg composition
- `cmd/`: CLI command implementations (18 commands)
- `bin/mcp_server/`: stdio MCP server — 8 tools, structured content, output schemas, elicitation
- `session`: Persistent named sessions with LRU eviction (32 slots)
- `login`: Automated form-based login with MFA detection
- `form`: HTML form detection and auto-fill
- `mfa`: MFA challenge detection (TOTP, SMS, Email, Push)
- `ssrf`: SSRF protection — blocks private IPs
- `rate_limit`: Per-domain rate limiting
- `prefetch`: Same-site link extraction with eTLD+1 filtering
- `error`: Typed error hierarchy (`NabError` with 9 variants)
- `arena`: Arena allocator for HTTP response buffering
- `browser`: Chrome DevTools Protocol automation (feature: `browser`)

## Pull Request Guidelines

- **Title**: Clear, concise description (e.g., "Add HTTP/3 connection pooling")
- **Description**: Explain what changed, why, and any relevant context
- **Tests**: Include tests for new functionality
- **Documentation**: Update README.md and relevant docs if needed
- **Breaking changes**: Clearly mark and explain in PR description
- **Keep PRs focused**: One feature or fix per PR

## Continuous integration

CI is **docs-aware**. A `changes` job classifies each PR: when it touches a
code path (`src/`, `crates/`, `Cargo.toml`, `Cargo.lock`, `build.rs`,
`tests/`, `benches/`, `.github/workflows/`, `.cargo/`, `rust-toolchain*`)
the full Rust matrix runs (format, clippy, tests, builds, package, audit).
Documentation-only PRs (`*.md`, `docs/**`, licenses) **skip** that matrix and
merge in seconds.

Two checks are required by branch protection and always report:

- **CI Gate** — aggregates the matrix; it passes when every upstream job
  finished as success/skipped, and fails on any failure/cancellation.
- **Secrets scan** — runs on every PR, including docs, since a doc can still
  leak a credential.

To force the full matrix for a new code path, extend the `code` filter in
`.github/workflows/ci.yml`.

## Performance Considerations

nab is optimized for speed and token efficiency:

- **HTTP acceleration**: Use HTTP/2 multiplexing, HTTP/3 0-RTT, compression
- **Token optimization**: Markdown output (25× smaller than HTML)
- **Connection reuse**: Leverage connection pooling
- **Profile realistic benchmarks**: Use `cargo bench` and `nab bench` commands

## Security

- **Cookie extraction**: Only for legitimate use cases (accessing your own content)
- **Fingerprint spoofing**: For anti-bot bypass, not malicious purposes
- **Credential handling**: Never log or expose credentials
- **Dependencies**: Keep dependencies up to date (`cargo update`)

## Questions or Issues?

- **Bug reports**: Open a GitHub issue with reproduction steps
- **Feature requests**: Open a GitHub issue describing the use case
- **Security issues**: Email directly rather than opening public issues

## License

By contributing, you agree your contributions will be licensed under the license that applies to the files you modify:

- MIT for core files.
- PolyForm Noncommercial 1.0.0 for Enterprise Edition files marked with `SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0`.

If a pull request adds a new Enterprise Edition file, include the PolyForm Noncommercial SPDX header. If it adds a new core file, use the MIT license boundary unless the maintainer explicitly marks the feature as Enterprise Edition.

Maintainers may designate new files as Enterprise Edition when the feature is primarily valuable for authenticated access, anti-bot handling, WAF challenge handling, browser-cookie workflows, site-specific commercial integrations, secure ingestion, hosted operations, multi-tenant service operation, or commercial platform integration. Existing MIT releases and core files that remain MIT are not retroactively relicensed.
