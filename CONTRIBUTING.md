# Contributing to nab

Thank you for your interest in contributing to nab, an ultra-minimal browser engine with HTTP/3, JavaScript execution, and anti-fingerprinting capabilities.

## Prerequisites

- **Rust 1.93+**: `rustup update`
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

To build without HTTP/3:
```bash
cargo build --no-default-features --features cli
```

## Module Organization

Key modules:
- `http_client`: HTTP/2 client with connection pooling
- `http3_client`: HTTP/3 (QUIC) implementation
- `js_engine`: QuickJS-based JavaScript execution
- `auth`: 1Password, cookie extraction, OTP retrieval
- `fingerprint`: Browser fingerprint generation and auto-updates
- `stream`: HLS/DASH streaming with multiple backends
- `analyze`: Video/audio transcription and vision analysis
- `annotate`: Subtitle generation and video overlay composition

## Pull Request Guidelines

- **Title**: Clear, concise description (e.g., "Add HTTP/3 connection pooling")
- **Description**: Explain what changed, why, and any relevant context
- **Tests**: Include tests for new functionality
- **Documentation**: Update README.md and relevant docs if needed
- **Breaking changes**: Clearly mark and explain in PR description
- **Keep PRs focused**: One feature or fix per PR

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

By contributing, you agree that your contributions will be licensed under the MIT License.
