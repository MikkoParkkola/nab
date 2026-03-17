# WASM Provider Marketplace Design Document

## Overview

A sandboxed WASM plugin system for nab that enables community-contributed, site-specific content extractors. WASM providers receive fetched HTML bytes and URL metadata, run in a zero-trust sandbox (no network, no filesystem), and return structured markdown content. This replaces the need for Rust code changes and recompilation when adding new site extractors.

## Design Summary (Meta)

```yaml
design_type: "new_feature"
risk_level: "medium"
complexity_level: "high"
complexity_rationale: >
  (1) ACs require sandboxed code execution, cross-language ABI, hot-reload,
  and a distribution registry -- each individually complex.
  (2) Security sandbox must resist adversarial providers; WASM host-guest
  boundary adds FFI/ABI constraints; compatibility across provider authoring
  languages (Rust, Go, AssemblyScript, C) requires a stable interface.
main_constraints:
  - "Zero-trust: providers must not access network, filesystem, or host memory"
  - "Cross-platform: macOS, Linux, Windows (nab's supported targets)"
  - "Minimal binary size impact (nab is currently ~2 MB stripped)"
  - "Hot-reload without restarting nab"
  - "Backward compatibility with existing binary/CSS plugin system"
biggest_risks:
  - "WASM runtime adds 5-15 MB to binary size"
  - "Cold-start compilation latency on first provider load"
  - "ABI stability across provider SDK versions"
  - "Supply-chain risk from community-contributed .wasm files"
unknowns:
  - "Actual performance overhead of WASM sandbox vs native Rust extractors"
  - "Community adoption rate and provider quality"
  - "Registry hosting costs and governance model"
```

## Background and Context

### Prerequisite ADRs

- ADR-0001-wasm-runtime-selection (to be created alongside this design)
- No existing ADRs in this repository; this is the first formal design document for nab's plugin architecture evolution.

### Agreement Checklist

#### Scope
- [x] New WASM provider type in `plugin/` module
- [x] WASM runtime integration (wasmtime)
- [x] Provider manifest format and validation
- [x] Local provider directory with hot-reload
- [x] Provider ABI (host-guest function interface)
- [x] CLI commands: `nab provider install/list/remove/test`
- [x] Provider SDK crate (`nab-provider-sdk`) for authoring providers in Rust
- [x] Remote registry protocol (Phase 3)

#### Non-Scope (Explicitly not changing)
- [x] Existing binary plugin system (unchanged, backward compatible)
- [x] Existing CSS extractor plugin system (unchanged)
- [x] Existing rule-based TOML providers (unchanged)
- [x] Existing hardcoded Rust providers (unchanged)
- [x] HTTP client internals
- [x] Content pipeline (ContentRouter, HtmlHandler, etc.)
- [x] MCP server protocol

#### Constraints
- [x] Parallel operation: Yes -- WASM providers must be safely callable from async contexts
- [x] Backward compatibility: Required -- all existing plugin types continue to work
- [x] Performance measurement: Required -- WASM overhead must be < 50ms per extraction on typical pages
- [x] Feature-gated: WASM support behind `wasm-providers` cargo feature (not in default features initially)

#### Applicable Standards
- [x] `clippy::pedantic` lint level `[explicit]` - Source: `Cargo.toml [lints.clippy]`
- [x] `async_trait` for provider interfaces `[explicit]` - Source: `site/mod.rs` SiteProvider trait
- [x] Feature-gating for optional heavy dependencies `[explicit]` - Source: `Cargo.toml` (pdf, browser, impersonate features)
- [x] Plugin config in `~/.config/nab/plugins.toml` `[explicit]` - Source: `plugin/config.rs`
- [x] Provider loading order: rules > hardcoded > plugins `[explicit]` - Source: `site/mod.rs` SiteRouter::new()
- [x] Error handling via `anyhow::Result` with `thiserror` for public types `[implicit]` - Evidence: all modules use `anyhow::Result`
- [x] `tracing` for structured logging `[implicit]` - Evidence: `site/mod.rs`, `plugin/config.rs`
- [x] Tests co-located in `#[cfg(test)] mod tests` blocks `[implicit]` - Evidence: all source files

### Problem to Solve

Adding a site-specific extractor to nab currently requires:
1. Writing Rust code in `src/site/` or a TOML rule in `src/site/rules/defaults/`
2. Recompiling the binary
3. Distributing a new release

This creates a bottleneck: only the nab maintainer can add new extractors. Community members can write binary plugins, but those execute arbitrary native code with full system access -- a security risk. CSS extractor plugins are safe but limited to CSS selector-based extraction, which cannot handle JavaScript-rendered content, API-backed sites, or complex HTML transformation.

### Current Challenges

1. **Trust boundary**: Binary plugins run as native processes with full OS access
2. **Distribution friction**: No standard way to discover, install, or update plugins
3. **Language lock-in**: Binary plugins must handle their own HTTP, JSON, and error handling
4. **No hot-reload**: Plugin changes require restarting nab
5. **No testing framework**: No way to validate a plugin works correctly before distribution

### Requirements

#### Functional Requirements

- FR1: WASM providers receive HTML bytes + URL metadata and return structured content
- FR2: Providers execute in a sandboxed runtime with no network/filesystem access
- FR3: Providers are loaded from a local directory (`~/.config/nab/providers/`)
- FR4: Providers can be installed from a remote registry via CLI
- FR5: Providers declare domain patterns in a manifest file
- FR6: Hot-reload: new/updated providers take effect without restarting nab
- FR7: Provider SDK enables authoring providers in Rust (compilable to wasm32-wasi)
- FR8: WASM providers integrate into the existing SiteRouter dispatch chain

#### Non-Functional Requirements

- **Performance**: WASM extraction overhead < 50ms on a 100KB HTML page (p95)
- **Binary size**: WASM runtime adds < 15 MB to nab binary (feature-gated, opt-in)
- **Memory**: WASM instance memory capped at 64 MB per provider execution
- **Startup**: Provider compilation cached; cold start < 500ms, warm start < 5ms
- **Security**: WASI capabilities limited to: clock_time, random, args, environ (read-only)

## Acceptance Criteria (AC) - EARS Format

### FR1: Provider Extraction

- [ ] **AC-1.1**: **When** nab fetches a URL matching a WASM provider's domain pattern, the system shall execute the WASM provider instead of generic HTML extraction and return the provider's markdown output
- [ ] **AC-1.2**: **When** a WASM provider returns an error, the system shall fall through to the next matching provider or generic extraction
- [ ] **AC-1.3**: The provider shall receive: HTML bytes, URL string, HTTP status code, and response headers as input

### FR2: Sandbox Security

- [ ] **AC-2.1**: **If** a WASM provider attempts to access the filesystem, **then** the system shall trap (abort) the provider execution
- [ ] **AC-2.2**: **If** a WASM provider attempts to open a network socket, **then** the system shall trap the provider execution
- [ ] **AC-2.3**: **If** a WASM provider exceeds 64 MB memory or 30 seconds execution time, **then** the system shall terminate the provider and return an error
- [ ] **AC-2.4**: **While** a WASM provider is executing, the system shall not expose any host memory outside the explicitly shared input buffers

### FR3: Local Provider Directory

- [ ] **AC-3.1**: The system shall scan `~/.config/nab/providers/` for `*.wasm` files with companion `manifest.toml` at startup
- [ ] **AC-3.2**: **If** a manifest is missing or invalid, **then** the system shall log a warning and skip that provider
- [ ] **AC-3.3**: **When** a provider directory contains multiple providers matching the same domain, the system shall use the first match by alphabetical manifest name

### FR5: Provider Manifest

- [ ] **AC-5.1**: The manifest shall declare: name, version, author, domain patterns (regex), and minimum SDK version
- [ ] **AC-5.2**: **If** a manifest declares an SDK version newer than the host supports, **then** the system shall skip the provider with a warning

### FR6: Hot-Reload

- [ ] **AC-6.1**: **When** a new `.wasm` file is added to the providers directory while nab-mcp is running, the system shall detect and load it within 30 seconds
- [ ] **AC-6.2**: **When** an existing `.wasm` file is updated (mtime change), the system shall recompile and reload it within 30 seconds

### FR7: Provider SDK

- [ ] **AC-7.1**: A Rust crate `nab-provider-sdk` shall compile to `wasm32-wasip1` target and expose the provider trait
- [ ] **AC-7.2**: **When** a developer implements the SDK trait and compiles to `.wasm`, the resulting file shall be directly loadable by nab

### FR8: SiteRouter Integration

- [ ] **AC-8.1**: WASM providers shall be checked in the SiteRouter dispatch chain after CSS extractor plugins and before generic HTML fallback
- [ ] **AC-8.2**: The provider loading order shall be: rules > hardcoded > CSS plugins > WASM providers

## Existing Codebase Analysis

### Implementation Path Mapping

| Type | Path | Description |
|------|------|-------------|
| Existing | `src/plugin/mod.rs` | Plugin module root -- exports binary and CSS plugin types |
| Existing | `src/plugin/config.rs` | TOML config parser for `~/.config/nab/plugins.toml` |
| Existing | `src/plugin/runner.rs` | Binary plugin subprocess runner implementing `SiteProvider` |
| Existing | `src/site/mod.rs` | SiteRouter with provider dispatch chain |
| Existing | `src/site/css_extractor.rs` | CSS selector-based SiteProvider |
| New | `src/plugin/wasm_runtime.rs` | WASM runtime wrapper (wasmtime engine + linker setup) |
| New | `src/plugin/wasm_provider.rs` | WasmProvider implementing SiteProvider trait |
| New | `src/plugin/wasm_manifest.rs` | Provider manifest parsing and validation |
| New | `src/plugin/wasm_registry.rs` | Remote registry client (Phase 3) |
| New | `src/cmd/provider.rs` | CLI subcommands: install, list, remove, test |
| New | `nab-provider-sdk/` | Separate crate for provider authoring (workspace member) |

### Integration Points (Include even for new implementations)

- **SiteRouter::new()**: Append WASM providers after CSS plugins in dispatch chain
- **plugin/config.rs**: Extend `LoadedPlugins` with WASM provider list
- **Cargo.toml**: New `wasm-providers` feature flag with `wasmtime` dependency
- **main.rs / Commands enum**: New `Provider` subcommand
- **MCP server tools/fetch.rs**: WASM providers available through same SiteRouter path (no changes needed)

### Code Inspection Evidence

| File/Function | Relevance |
|---------------|-----------|
| `src/site/mod.rs:SiteProvider` trait (lines 102-126) | Integration point -- WASM providers implement this trait |
| `src/site/mod.rs:SiteRouter::new()` (lines 148-174) | Integration point -- provider registration order |
| `src/site/mod.rs:try_extract_with_html()` (lines 205-233) | Pattern reference -- async dispatch with fallthrough on error |
| `src/plugin/runner.rs:PluginRunner` (lines 42-163) | Similar functionality -- binary plugin as SiteProvider, same pattern for WASM |
| `src/plugin/config.rs:load_all_plugins()` (lines 160-208) | Integration point -- extend to load WASM manifests |
| `src/plugin/config.rs:PluginType` enum (lines 105-111) | Integration point -- add `Wasm` variant |
| `src/site/css_extractor.rs:CssExtractorProvider` (lines 96-100) | Pattern reference -- compiled regex + config stored at init time |
| `src/cmd/fetch.rs:cmd_fetch()` (lines 45-71) | Integration point -- SiteRouter used here, WASM providers transparently included |
| `Cargo.toml` features (lines 179-193) | Pattern reference -- feature-gating convention |

### Similar Functionality Search

**Binary plugin system** (`plugin/runner.rs`): Executes external processes as SiteProviders. WASM providers serve the same purpose (custom extraction) but with sandboxing, no subprocess overhead, and cross-language support. The binary plugin system remains for backward compatibility; WASM does not replace it.

**CSS extractor plugins** (`site/css_extractor.rs`): In-process extractors defined via config. WASM providers are more powerful (arbitrary logic) but heavier. CSS extractors remain the lightweight option.

**Decision**: New implementation -- WASM providers are a distinct plugin type alongside binary and CSS, sharing the `SiteProvider` trait interface.

## Design

### Change Impact Map

```yaml
Change Target: plugin/ module + SiteRouter dispatch
Direct Impact:
  - src/plugin/mod.rs (new wasm module exports)
  - src/plugin/config.rs (PluginType::Wasm variant, WasmManifest loading)
  - src/site/mod.rs (SiteRouter::new() appends WASM providers)
  - Cargo.toml (wasmtime dependency, wasm-providers feature)
  - src/cmd/mod.rs (Provider subcommand registration)
  - src/main.rs (Commands::Provider variant)
Indirect Impact:
  - MCP fetch tool (uses SiteRouter -- gains WASM providers automatically)
  - Binary size (wasmtime adds ~10-15 MB when feature enabled)
  - Startup time (WASM provider scanning adds ~50ms)
No Ripple Effect:
  - HTTP clients (AcceleratedClient, Http3Client, ImpersonateClient)
  - Content pipeline (ContentRouter, HtmlHandler, readability, quality)
  - Auth stack (cookies, 1Password, OTP)
  - Fingerprinting module
  - Existing rule-based providers
  - Existing hardcoded providers (hackernews, github, linkedin, google)
  - Session management
  - Streaming module
```

### Architecture Overview

```
                           SiteRouter Dispatch Chain
                           ========================

  URL arrives
      |
      v
  1. Rule-based providers (TOML)     <- existing, unchanged
      |  (no match)
      v
  2. Hardcoded Rust providers         <- existing, unchanged
      |  (no match)
      v
  3. CSS extractor plugins            <- existing, unchanged
      |  (no match)
      v
  4. WASM providers [NEW]             <- sandboxed execution
      |  (no match)
      v
  5. Generic HTML extraction          <- existing fallback


                    WASM Provider Execution
                    =======================

  +-----------+     +------------------+     +------------------+
  | SiteRouter| --> | WasmProvider     | --> | wasmtime Engine  |
  | .matches()|     | .extract()       |     |                  |
  +-----------+     +------------------+     | +──────────────+ |
                          |                  | | WASM Module  | |
                    Input buffer:            | |              | |
                    - HTML bytes             | | extract()    | |
                    - URL string             | | fn called    | |
                    - HTTP status            | +──────────────+ |
                    - Headers                |                  |
                          |                  | Sandbox:         |
                    Output buffer:           | - No filesystem  |
                    - Markdown string        | - No network     |
                    - Metadata JSON          | - 64 MB mem cap  |
                          |                  | - 30s timeout    |
                          v                  +------------------+
                    SiteContent { markdown, metadata }
```

### Data Flow

```
URL fetch request
    |
    v
SiteRouter::try_extract_with_html(url, client, cookies, html_bytes)
    |
    +--> for each provider in [rules, hardcoded, css, WASM]:
    |        if provider.matches(url):
    |            provider.extract(url, client, cookies, html_bytes)
    |                |
    |                +-- [For WasmProvider]:
    |                |   1. Serialize input: WasmInput { url, html, status, headers }
    |                |   2. Allocate input buffer in WASM linear memory
    |                |   3. Call WASM export: extract(input_ptr, input_len) -> (out_ptr, out_len)
    |                |   4. Read output buffer from WASM linear memory
    |                |   5. Deserialize: WasmOutput { markdown, title, author, date }
    |                |   6. Map to SiteContent
    |                |
    |                +--> return Ok(SiteContent) or Err(...)
    |
    +--> None (no provider matched -> generic ContentRouter)
```

### Integration Point Map

```yaml
Integration Point 1:
  Existing Component: SiteRouter::new() in src/site/mod.rs
  Integration Method: Append WASM providers after CSS plugins (call append_wasm_providers)
  Impact Level: Medium (new providers added to dispatch chain)
  Required Test Coverage: Verify WASM providers loaded, dispatch order correct

Integration Point 2:
  Existing Component: PluginType enum in src/plugin/config.rs
  Integration Method: Add Wasm variant to PluginType
  Impact Level: Low (additive enum variant, existing variants unchanged)
  Required Test Coverage: Parse TOML with type = "wasm", verify backward compat

Integration Point 3:
  Existing Component: Cargo.toml feature flags
  Integration Method: New "wasm-providers" feature with wasmtime dependency
  Impact Level: Low (opt-in feature, not in default)
  Required Test Coverage: Build with and without feature flag

Integration Point 4:
  Existing Component: Commands enum in src/main.rs
  Integration Method: Add Provider subcommand
  Impact Level: Low (additive CLI command)
  Required Test Coverage: CLI help text, subcommand parsing
```

### Main Components

#### Component 1: WasmRuntime (`plugin/wasm_runtime.rs`)

- **Responsibility**: Initialize and manage the wasmtime Engine, compile WASM modules, cache compiled artifacts
- **Interface**:
  ```rust
  pub struct WasmRuntime {
      engine: wasmtime::Engine,
      cache_dir: PathBuf,
  }

  impl WasmRuntime {
      pub fn new() -> Result<Self>;
      pub fn load_module(&self, wasm_bytes: &[u8]) -> Result<WasmModule>;
      pub fn execute(
          &self,
          module: &WasmModule,
          input: &WasmInput,
      ) -> Result<WasmOutput>;
  }
  ```
- **Dependencies**: `wasmtime` crate, filesystem (for compilation cache only)

#### Component 2: WasmProvider (`plugin/wasm_provider.rs`)

- **Responsibility**: Implement `SiteProvider` trait for a loaded WASM module
- **Interface**:
  ```rust
  pub struct WasmProvider {
      manifest: WasmManifest,
      module: WasmModule,
      runtime: Arc<WasmRuntime>,
      patterns: Vec<Regex>,
  }

  #[async_trait]
  impl SiteProvider for WasmProvider {
      fn name(&self) -> &'static str;
      fn matches(&self, url: &str) -> bool;
      async fn extract(
          &self,
          url: &str,
          client: &AcceleratedClient,
          cookies: Option<&str>,
          prefetched_html: Option<&[u8]>,
      ) -> Result<SiteContent>;
  }
  ```
- **Dependencies**: WasmRuntime, WasmManifest, regex

#### Component 3: WasmManifest (`plugin/wasm_manifest.rs`)

- **Responsibility**: Parse and validate provider manifest files
- **Interface**:
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  pub struct WasmManifest {
      pub name: String,
      pub version: String,
      pub author: Option<String>,
      pub description: Option<String>,
      pub patterns: Vec<String>,
      pub sdk_version: String,
      pub capabilities: Vec<String>,  // reserved for future use
  }

  impl WasmManifest {
      pub fn from_file(path: &Path) -> Result<Self>;
      pub fn validate(&self) -> Result<()>;
  }
  ```
- **Dependencies**: `serde`, `toml`

#### Component 4: Provider SDK (`nab-provider-sdk` crate)

- **Responsibility**: Provide ergonomic Rust API for authoring WASM providers
- **Interface**:
  ```rust
  // nab-provider-sdk/src/lib.rs

  /// Input provided by the nab host to the WASM provider.
  pub struct ProviderInput {
      pub url: String,
      pub html: Vec<u8>,
      pub status_code: u16,
      pub headers: Vec<(String, String)>,
  }

  /// Output returned by the WASM provider to the nab host.
  pub struct ProviderOutput {
      pub markdown: String,
      pub title: Option<String>,
      pub author: Option<String>,
      pub published: Option<String>,
  }

  /// Trait that provider authors implement.
  pub trait Provider {
      fn extract(&self, input: ProviderInput) -> Result<ProviderOutput, String>;
  }

  /// Macro that generates the WASM ABI glue code.
  /// Usage: `nab_provider!(MyProvider);`
  #[macro_export]
  macro_rules! nab_provider {
      ($provider:ty) => { /* generates extern "C" fn nab_extract(...) */ };
  }
  ```
- **Dependencies**: None (no_std compatible, WASM-safe)

### Data Representation Decision

| Criterion | Assessment | Reason |
|-----------|-----------|--------|
| Semantic Fit | No | Existing `SiteContent`/`SiteMetadata` are host-side; WASM needs its own serializable ABI types |
| Responsibility Fit | Partial | Same domain (extraction output) but different bounded context (sandbox boundary) |
| Lifecycle Fit | No | WASM types exist only during serialization/deserialization across the host-guest boundary |
| Boundary/Interop Cost | High | Must serialize/deserialize across WASM linear memory boundary |

**Decision**: New structures (`WasmInput`, `WasmOutput`) for the ABI boundary, with conversion functions to/from existing `SiteContent`/`SiteMetadata`. The ABI types are intentionally minimal and stable; the host-side types can evolve independently.

### Contract Definitions

#### WASM ABI Contract (Host <-> Guest)

The WASM module must export exactly two functions:

```
// Required WASM exports
extern "C" fn nab_extract(input_ptr: i32, input_len: i32) -> i64;
//   Returns: high 32 bits = output_ptr, low 32 bits = output_len
//   On error: returns 0 (null pointer, zero length)

extern "C" fn nab_alloc(size: i32) -> i32;
//   Allocates `size` bytes in WASM linear memory, returns pointer
//   Used by host to write input data into guest memory

extern "C" fn nab_free(ptr: i32, size: i32);
//   Frees a previously allocated buffer (called by host after reading output)
```

#### Serialization Format

Input and output are MessagePack-encoded (compact binary, no schema needed):

```rust
// Input (host -> guest), MessagePack-encoded
struct WasmInput {
    url: String,         // Full URL
    html: Vec<u8>,       // Raw HTML bytes
    status: u16,         // HTTP status code
    headers: Vec<(String, String)>,  // Response headers
}

// Output (guest -> host), MessagePack-encoded
struct WasmOutput {
    markdown: String,              // Extracted markdown content
    title: Option<String>,         // Page title
    author: Option<String>,        // Author name
    published: Option<String>,     // Publication date
    extra: HashMap<String, String>,  // Reserved for future metadata
}
```

**Why MessagePack over JSON**: 30-50% smaller, 2-5x faster encode/decode, binary-safe (HTML bytes without escaping). The `rmp-serde` crate is well-maintained and adds minimal dependency weight.

### Data Contract

#### WasmProvider

```yaml
Input:
  Type: WasmInput (MessagePack-encoded bytes)
  Preconditions:
    - url is a valid, non-empty URL string
    - html is the raw response body (may be empty for non-HTML)
    - status is a valid HTTP status code (100-599)
    - headers is a list of key-value pairs
  Validation: URL and status validated before passing to WASM

Output:
  Type: WasmOutput (MessagePack-encoded bytes)
  Guarantees:
    - markdown is valid UTF-8
    - All optional fields may be None/null
    - extra map is always present (may be empty)
  On Error: Returns Err, SiteRouter falls through to next provider

Invariants:
  - Provider cannot modify host state
  - Provider cannot access host memory outside allocated buffers
  - Execution terminates within 30s wall-clock time
```

### Integration Boundary Contracts

```yaml
Boundary: SiteRouter -> WasmProvider
  Input: URL string, AcceleratedClient ref, cookies Option<&str>, prefetched_html Option<&[u8]>
  Output: Result<SiteContent> (sync from caller perspective, internally may use spawn_blocking)
  On Error: Log warning via tracing, return None to SiteRouter (falls through)

Boundary: WasmProvider -> WasmRuntime
  Input: WasmInput struct (serialized to MessagePack bytes)
  Output: Result<WasmOutput> (synchronous, runs in spawn_blocking)
  On Error: Propagate as anyhow::Error with provider name context

Boundary: Host -> WASM Module (ABI)
  Input: Pointer + length to MessagePack bytes in WASM linear memory
  Output: Pointer + length packed as i64, or 0 on error
  On Error: Guest returns 0; host returns Err("provider returned empty output")
```

### Field Propagation Map

| Field | Boundary | Status | Detail |
|-------|----------|--------|--------|
| `url` | Host -> WasmInput | preserved | Passed as-is to provider |
| `html` | Host -> WasmInput | preserved | Raw bytes from `prefetched_html` or fetched by host |
| `status_code` | Host -> WasmInput | preserved | HTTP response status |
| `headers` | Host -> WasmInput | transformed | Filtered to exclude security-sensitive headers (Cookie, Authorization) |
| `markdown` | WasmOutput -> SiteContent | preserved | Provider's markdown output |
| `title` | WasmOutput -> SiteMetadata.title | preserved | Optional, may be None |
| `author` | WasmOutput -> SiteMetadata.author | preserved | Optional, may be None |
| `published` | WasmOutput -> SiteMetadata.published | preserved | Optional, may be None |
| `platform` | WasmOutput -> SiteMetadata | transformed | Set to `"wasm:{manifest.name}"` by host |
| `canonical_url` | WasmOutput -> SiteMetadata | transformed | Set to input URL by host |
| `cookies` | Host -> WasmProvider | dropped | Never passed to WASM guest (security) |

### Error Handling

| Error Type | Source | Handling |
|------------|--------|----------|
| WASM compilation failure | wasmtime | Log error, skip provider, continue with next |
| WASM trap (memory OOB, unreachable) | wasmtime | Catch trap, log with provider name, return Err |
| Execution timeout (>30s) | wasmtime epoch interrupt | Terminate instance, log timeout, return Err |
| Memory limit exceeded (>64 MB) | wasmtime memory limiter | Trap, log, return Err |
| Invalid MessagePack output | rmp-serde | Log deserialization error with first 200 bytes, return Err |
| Manifest parse failure | toml | Log warning, skip provider at load time |
| Provider directory not found | filesystem | Create directory on first `nab provider install`, no error at startup |

### Logging and Monitoring

```rust
// Provider loading
tracing::debug!("Loaded WASM provider: {} v{} ({} patterns)", name, version, patterns.len());
tracing::warn!("WASM provider '{}' manifest invalid: {}", name, error);

// Execution
tracing::debug!("WASM provider '{}' matched URL: {}", name, url);
tracing::debug!("WASM provider '{}' completed in {}ms", name, elapsed_ms);
tracing::warn!("WASM provider '{}' failed for {}: {}", name, url, error);
tracing::warn!("WASM provider '{}' exceeded timeout (30s) for {}", name, url);
```

## Security Model

### Threat Model

| Threat | Mitigation |
|--------|-----------|
| Malicious provider reads filesystem | WASI: no `fd_*` capabilities granted |
| Malicious provider opens network sockets | WASI: no `sock_*` capabilities granted |
| Malicious provider reads host memory | WASM linear memory isolation (hardware-enforced) |
| Malicious provider exhausts CPU | wasmtime epoch-based interruption (30s limit) |
| Malicious provider exhausts memory | wasmtime memory limiter (64 MB cap) |
| Supply-chain attack via registry | SHA-256 content hash in manifest, signed by author (Phase 3) |
| Provider exfiltrates data via output | Output is markdown text -- no side channels beyond returned content |
| Denial of service via many providers | Cap at 100 loaded WASM providers |

### WASI Capabilities (Allowlist)

```rust
// Only these WASI capabilities are granted:
let wasi_ctx = WasiCtxBuilder::new()
    .inherit_stdout()   // For debug logging only (captured, not displayed)
    .build();
// Explicitly NOT granted:
// - filesystem access (no fd_open, fd_read, fd_write to real files)
// - network access (no sock_open, sock_connect)
// - environment variables (no environ_get)
// - command-line args (no args_get)
// - random (not needed -- providers are deterministic extractors)
```

### Cookie/Credential Isolation

Cookies and authentication credentials are **never** passed to WASM providers. The `extract()` method receives `cookies: Option<&str>` from SiteRouter, but WasmProvider drops this parameter before crossing the WASM boundary. The `headers` field in WasmInput has `Cookie`, `Authorization`, `Set-Cookie`, and `Proxy-Authorization` headers stripped.

## Implementation Plan

### Implementation Approach

**Selected Approach**: Vertical Slice (Feature-driven)

**Selection Reason**: Each phase delivers independently usable functionality. Phase 1 enables local WASM providers -- immediately useful for power users. Phase 2 adds the SDK -- enables community authoring. Phase 3 adds the registry -- enables discovery and sharing. Each phase can be shipped as a separate release. The feature-flag boundary (`wasm-providers`) means the feature is additive and does not affect users who don't opt in.

### Technical Dependencies and Implementation Order

#### Phase 1: Core WASM Runtime + Local Providers (~1 week)

1. **wasmtime integration** (`plugin/wasm_runtime.rs`)
   - Technical Reason: Foundation for all WASM execution
   - Dependent Elements: WasmProvider, compilation cache
   - Verification: L2 -- unit tests with a minimal test WASM module

2. **WasmManifest** (`plugin/wasm_manifest.rs`)
   - Technical Reason: Needed before providers can be loaded
   - Dependent Elements: WasmProvider pattern matching
   - Verification: L2 -- TOML parsing tests

3. **WasmProvider** (`plugin/wasm_provider.rs`)
   - Technical Reason: Core SiteProvider implementation
   - Prerequisites: WasmRuntime, WasmManifest
   - Verification: L1 -- end-to-end test: load test WASM, extract from sample HTML

4. **SiteRouter integration** (`site/mod.rs`)
   - Technical Reason: Makes WASM providers available in the fetch pipeline
   - Prerequisites: WasmProvider
   - Verification: L1 -- `nab fetch` with a WASM provider active

5. **Feature flag** (`Cargo.toml`)
   - Technical Reason: Keeps wasmtime optional
   - Dependent Elements: All WASM code behind `#[cfg(feature = "wasm-providers")]`
   - Verification: L3 -- builds with and without feature

**Phase 1 AC coverage**: AC-1.1, AC-1.2, AC-1.3, AC-2.1, AC-2.2, AC-2.3, AC-2.4, AC-3.1, AC-3.2, AC-3.3, AC-5.1, AC-5.2, AC-8.1, AC-8.2

#### Phase 2: Provider SDK + CLI Commands (~1 week)

1. **nab-provider-sdk crate** (`nab-provider-sdk/`)
   - Technical Reason: Enables provider authoring
   - Dependent Elements: ABI contract must match Phase 1 host
   - Verification: L1 -- compile example provider to .wasm, load in nab

2. **Example providers** (`examples/providers/`)
   - Technical Reason: Reference implementations and integration tests
   - Prerequisites: SDK crate
   - Verification: L1 -- each example extracts from its target site

3. **CLI commands** (`cmd/provider.rs`)
   - `nab provider list`: Show installed providers
   - `nab provider test <manifest-dir>`: Test a provider against sample HTML
   - `nab provider install <path>`: Install from local .wasm + manifest
   - Technical Reason: Developer workflow tooling
   - Prerequisites: WasmRuntime, WasmManifest
   - Verification: L1 -- CLI commands work as documented

**Phase 2 AC coverage**: AC-7.1, AC-7.2

#### Phase 3: Hot-Reload + Remote Registry (~1 week)

1. **Filesystem watcher** (hot-reload)
   - Technical Reason: Enables provider updates without restart
   - Prerequisites: WasmRuntime (recompilation), SiteRouter (re-registration)
   - Verification: L1 -- add .wasm while nab-mcp running, verify it loads

2. **Registry protocol** (`plugin/wasm_registry.rs`)
   - HTTP API: `GET /v1/providers?domain=example.com` returns manifest + download URL
   - `nab provider install medium-extractor` fetches from registry
   - Technical Reason: Enables community distribution
   - Prerequisites: CLI commands, WasmManifest (integrity verification)
   - Verification: L1 -- install from registry, verify extraction works

3. **Provider signing** (optional)
   - SHA-256 content hash in registry manifest
   - Verify hash after download
   - Technical Reason: Supply-chain security
   - Verification: L2 -- tampered .wasm rejected

**Phase 3 AC coverage**: AC-6.1, AC-6.2

### Integration Points

**Integration Point 1: SiteRouter dispatch chain**
- Components: `site/mod.rs` SiteRouter -> `plugin/wasm_provider.rs` WasmProvider
- Verification: Unit test with mock WASM provider in dispatch chain; integration test with real compiled WASM

**Integration Point 2: Plugin config loading**
- Components: `plugin/config.rs` -> `plugin/wasm_manifest.rs`
- Verification: Test loading plugins.toml with all three plugin types (binary + CSS + WASM)

**Integration Point 3: CLI commands**
- Components: `main.rs` Commands -> `cmd/provider.rs`
- Verification: CLI integration tests with `assert_cmd`

### Migration Strategy

No migration needed. WASM providers are additive:

1. Existing binary plugins continue to work (unchanged code path)
2. Existing CSS plugins continue to work (unchanged code path)
3. WASM providers are behind a feature flag (`wasm-providers`)
4. Users opt in by: `cargo install nab --features wasm-providers`
5. Provider loading order places WASM after all existing types

## ADR-0001: WASM Runtime Selection

### Status

Proposed

### Context

nab needs a WASM runtime to execute sandboxed provider plugins. The runtime must support WASI (for minimal system interaction), be embeddable in a Rust application, and provide strong sandboxing guarantees.

### Options

#### Option A: wasmtime

- **Overview**: Bytecode Alliance's production WASM runtime, used by Fastly, Shopify, Fermyon
- **Pros**:
  - Industry standard for server-side WASM (most battle-tested)
  - Cranelift JIT/AOT compiler -- fast execution after compilation
  - First-class Rust API (`wasmtime` crate)
  - Epoch-based interruption (clean timeout support)
  - Memory limiter API (configurable per-instance caps)
  - Pre-compiled module caching (near-instant warm starts)
  - WASI-p1 and WASI-p2 (component model) support
  - Active development by Mozilla/Fastly/Intel engineers
- **Cons**:
  - Large dependency (~10-15 MB binary impact with Cranelift)
  - Cold compilation latency (~200-500ms for first load of a module)
  - Complex API surface (Store, Instance, Linker, Engine hierarchy)
- **Effort**: 5 days

#### Option B: wasmer

- **Overview**: Wasmer Inc's WASM runtime, used by Wasmer Edge, Spacedrive
- **Pros**:
  - Multiple compiler backends (Cranelift, LLVM, Singlepass)
  - Singlepass backend: faster compilation, smaller binary
  - wasmer.io package registry already exists
  - Good Rust API
- **Cons**:
  - Less battle-tested than wasmtime for embedded use
  - WASI support lags behind wasmtime (WASI-p2 incomplete)
  - Corporate governance (Wasmer Inc) vs. Bytecode Alliance consortium
  - Epoch interruption not natively supported (must use fuel metering)
  - Recent API instability (breaking changes between 3.x and 4.x)
- **Effort**: 6 days

#### Option C: wasm3 (via `wasm3` crate)

- **Overview**: Interpreter-only WASM runtime, extremely lightweight
- **Pros**:
  - Tiny binary impact (~200 KB)
  - No compilation step (interpreter starts instantly)
  - Minimal memory overhead
  - Simple API
- **Cons**:
  - 10-100x slower execution than JIT runtimes
  - No WASI support in the Rust bindings
  - Project is largely dormant (last meaningful commit 2023)
  - No memory limiter or timeout mechanism
  - Would need custom sandboxing layer
- **Effort**: 8 days (extra for sandbox, WASI shim)

### Comparison

| Evaluation Axis | wasmtime | wasmer | wasm3 |
|-----------------|----------|--------|-------|
| Execution speed | Fast (JIT) | Fast (JIT/Singlepass) | Slow (interpreter) |
| Binary size impact | ~12 MB | ~8 MB (Singlepass) | ~200 KB |
| WASI support | Excellent (p1+p2) | Good (p1, partial p2) | None (Rust bindings) |
| Sandbox guarantees | Excellent | Good | Poor (manual) |
| Timeout support | Epoch interrupts | Fuel metering | None |
| Memory limits | Built-in API | Built-in API | None |
| Module caching | AOT compilation cache | AOT compilation cache | N/A (interpreter) |
| Ecosystem maturity | Highest | Medium | Low (dormant) |
| API stability | Stable (1.x semver) | Unstable (4.x breaking) | N/A |
| Community/maintenance | Bytecode Alliance (active) | Wasmer Inc (active) | Dormant |

### Decision

**wasmtime** selected.

**Rationale**: wasmtime provides the strongest combination of security guarantees (epoch interrupts, memory limiters, hardware-enforced isolation), ecosystem maturity (Bytecode Alliance, used by Fastly/Shopify), and Rust-first API design. The binary size cost (~12 MB) is acceptable because WASM support is feature-gated -- users who don't need it pay nothing. The cold compilation latency is mitigated by AOT module caching (compiled modules are cached to `~/.cache/nab/wasm/`). wasmer's API instability and wasm3's lack of WASI/sandbox support make them inferior choices for a security-critical plugin runtime.

**Kill criteria**: If wasmtime's binary size impact exceeds 20 MB after stripping, re-evaluate the Singlepass-only wasmer option.

## Test Strategy

### Unit Tests

- WasmManifest parsing: valid, invalid, missing fields, unknown fields
- WasmRuntime: module compilation, memory limits, timeout enforcement
- WasmProvider: pattern matching, SiteContent construction from WasmOutput
- ABI serialization: MessagePack round-trip for WasmInput/WasmOutput
- Security: verify WASI capabilities are restricted (no filesystem, no network)

### Integration Tests

- Load a real compiled WASM provider, extract from sample HTML
- SiteRouter with mixed provider types (rule + hardcoded + CSS + WASM)
- CLI commands: `nab provider list`, `nab provider test`
- Feature-flag gating: compile without `wasm-providers`, verify no wasmtime dependency

### E2E Tests

- `nab fetch https://example-covered-by-wasm-provider.com` produces expected markdown
- WASM provider failure falls through to generic extraction
- Hot-reload: add provider while nab-mcp is running (Phase 3)

### Performance Tests

- Benchmark WASM extraction vs. equivalent Rust extraction on 100KB HTML
- Measure cold vs. warm module load time
- Measure memory usage with 10 simultaneously loaded WASM providers

## Security Considerations

1. **WASM linear memory isolation**: wasmtime enforces hardware-level memory isolation. A WASM module cannot read or write outside its own linear memory. This is a fundamental property of the WASM specification, not a software check.

2. **No ambient authority**: WASI capabilities are an explicit allowlist. By not granting filesystem or network capabilities, providers physically cannot access these resources.

3. **Resource exhaustion**: Epoch-based interruption prevents infinite loops. Memory limiters prevent allocation bombs. Both are enforced by the runtime, not by the provider.

4. **Credential isolation**: Cookies and auth headers are stripped before crossing the WASM boundary. Even if a provider could exfiltrate data, it has no credentials to steal.

5. **Supply-chain**: Phase 3 adds SHA-256 content hashing. Future: Ed25519 signature verification for registry-distributed providers.

## Future Extensibility

1. **WASM Component Model (WASI-p2)**: wasmtime already supports the component model. Future SDK versions can use WIT (WebAssembly Interface Types) instead of raw ABI functions, providing type-safe host-guest communication.

2. **Streaming extraction**: Providers could receive HTML in chunks (for large pages) via a streaming API. Would require extending the ABI with `begin_extract()`, `feed_chunk()`, `finish()`.

3. **Provider composition**: A meta-provider that chains multiple WASM providers (e.g., "clean HTML" -> "extract article" -> "format markdown").

4. **Capability extensions**: Grant specific, auditable capabilities (e.g., "may call host HTTP client for one specific API endpoint") via WASI-p2 capability handles.

5. **Provider marketplace UI**: Web interface for browsing, rating, and installing providers. Integrated with `nab provider install` CLI.

## Alternative Solutions

### Alternative 1: Extend Binary Plugin Protocol

- **Overview**: Keep the existing binary plugin system but add a standardized SDK and sandboxing via seccomp/pledge
- **Advantages**: No new runtime dependency, simpler implementation
- **Disadvantages**: Platform-specific sandboxing (seccomp on Linux only), cannot sandbox on macOS without entitlements, binary distribution per-platform
- **Reason for Rejection**: WASM provides cross-platform sandboxing and write-once-run-anywhere distribution

### Alternative 2: Lua/JavaScript Scripting Engine

- **Overview**: Embed a Lua (rlua/mlua) or JavaScript (rquickjs, already in nab) engine for providers
- **Advantages**: Smaller runtime (~1 MB), rquickjs already in dependencies, faster cold start
- **Disadvantages**: Single language lock-in, weaker sandboxing (JS engine sandbox escapes are more common than WASM), performance (interpreted), no standard ABI for cross-language authoring
- **Reason for Rejection**: rquickjs is already used for SPA extraction but its sandbox model is weaker than WASM. Provider authors should be able to use any WASM-targeting language.

### Alternative 3: Dynamic Library Plugins (.so/.dylib)

- **Overview**: Load native shared libraries at runtime via `libloading`
- **Advantages**: Zero overhead, full Rust API, fast
- **Disadvantages**: No sandboxing (native code = full access), platform-specific binaries, unsafe FFI, ABI fragility
- **Reason for Rejection**: Fundamentally incompatible with zero-trust security requirement

## Risks and Mitigation

| Risk | Impact | Probability | Mitigation |
|------|--------|-------------|------------|
| wasmtime adds >15 MB binary size | Medium | Medium | Feature-gate; measure after integration; fallback to wasmer Singlepass |
| Cold compilation latency >1s | Low | Low | AOT module caching in `~/.cache/nab/wasm/`; warm start <5ms |
| ABI breaking changes between SDK versions | High | Medium | Version field in manifest; host checks compatibility before loading |
| Low community adoption | Medium | Medium | Ship 3-5 example providers for popular sites; good SDK documentation |
| WASM performance inadequate for complex extraction | Medium | Low | Benchmark during Phase 1; JIT compilation produces near-native speed |
| Registry becomes a supply-chain attack vector | High | Low | Content hashing (Phase 3); future: Ed25519 signing |

## Provider Manifest Format

```toml
# ~/.config/nab/providers/medium-extractor/manifest.toml

[provider]
name = "medium-extractor"
version = "1.0.0"
author = "Community Author <author@example.com>"
description = "Extracts Medium.com articles with clean formatting"
sdk_version = "0.1.0"
wasm_file = "medium_extractor.wasm"  # relative to manifest directory

[provider.patterns]
domains = [
    r"medium\.com/.*",
    r".*\.medium\.com/.*",
    r"towardsdatascience\.com/.*",
]

# Reserved for future capability declarations
[provider.capabilities]
# (empty -- no special capabilities needed)
```

## Provider Directory Layout

```
~/.config/nab/providers/
    medium-extractor/
        manifest.toml
        medium_extractor.wasm
    dev-to-extractor/
        manifest.toml
        dev_to_extractor.wasm

~/.cache/nab/wasm/
    medium_extractor.cwasm    # Pre-compiled (AOT) cached module
    dev_to_extractor.cwasm
```

## Example Provider (Rust SDK)

```rust
// examples/providers/medium-extractor/src/lib.rs

use nab_provider_sdk::{Provider, ProviderInput, ProviderOutput, nab_provider};

struct MediumExtractor;

impl Provider for MediumExtractor {
    fn extract(&self, input: ProviderInput) -> Result<ProviderOutput, String> {
        let html = String::from_utf8(input.html)
            .map_err(|e| format!("invalid UTF-8: {e}"))?;

        // Extract article content using simple string parsing
        // (providers can use any WASM-compatible HTML parsing library)
        let title = extract_between(&html, "<h1", "</h1>");
        let article = extract_between(&html, "<article", "</article>");

        let markdown = html_to_markdown(&article.unwrap_or_default());

        Ok(ProviderOutput {
            markdown,
            title,
            author: extract_meta(&html, "author"),
            published: extract_meta(&html, "datePublished"),
        })
    }
}

nab_provider!(MediumExtractor);
```

## Interface Change Impact Analysis

| Existing Operation | New Operation | Conversion Required | Adapter Required | Compatibility Method |
|-------------------|---------------|-------------------|------------------|---------------------|
| `SiteRouter::new()` | `SiteRouter::new()` | None | Not Required | Additive: WASM providers appended to existing chain |
| `SiteProvider::extract()` | `SiteProvider::extract()` | None | Not Required | WasmProvider implements same trait |
| `PluginType::Binary/Css` | `PluginType::Binary/Css/Wasm` | None | Not Required | Additive enum variant |
| `load_all_plugins()` | `load_all_plugins()` | Minor | Not Required | Returns `LoadedPlugins` with new `wasm` field |
| `plugins.toml` format | `plugins.toml` format | None | Not Required | Backward compatible (new `type = "wasm"` alongside existing types) |
| CLI `Commands` enum | CLI `Commands` enum | None | Not Required | Additive `Provider` variant |

## References

- [wasmtime documentation](https://docs.wasmtime.dev/) - Bytecode Alliance WASM runtime
- [WASI specification](https://wasi.dev/) - WebAssembly System Interface
- [wasmtime Rust API](https://docs.rs/wasmtime/) - Rust embedding API
- [rmp-serde](https://docs.rs/rmp-serde/) - MessagePack serialization for Rust
- [Extism](https://extism.org/) - WASM plugin framework (design reference, not used)
- [Zed extension system](https://zed.dev/docs/extensions) - Production WASM plugin system using wasmtime (design reference)
- [Fermyon Spin](https://www.fermyon.com/spin) - WASM microservice platform using wasmtime (design reference)
- [GitHub issue #19](https://github.com/MikkoParkkola/nab/issues/19) - Original feature request

## Update History

| Date | Version | Changes | Author |
|------|---------|---------|--------|
| 2026-03-17 | 1.0 | Initial version | Claude (design) + Mikko (requirements) |
