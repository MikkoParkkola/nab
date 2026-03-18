//! WASM-backed site content extractor.
//!
//! Implements [`SiteProvider`] by executing a compiled WASM module inside a
//! sandboxed [`wasmtime`] runtime.  The guest module receives raw HTML and the
//! request URL, then returns a JSON-encoded [`WasmArticle`] on success.
//!
//! # Security model
//!
//! The sandbox provides the following guarantees:
//!
//! - **No filesystem access** — WASI is not configured; the guest has no
//!   `wasi_snapshot_preview1` or `wasi_preview2` imports.
//! - **No network access** — the host exposes zero import functions that could
//!   make outbound calls.
//! - **Bounded CPU** — fuel metering limits execution to [`FUEL_LIMIT`] Wasm
//!   instructions; exhaustion returns an error rather than hanging.
//! - **Bounded memory** — [`StoreLimitsBuilder`] caps linear memory growth at
//!   [`MEMORY_LIMIT_BYTES`] per instantiation.
//!
//! # Guest ABI
//!
//! The WASM guest must export three symbols:
//!
//! ```text
//! // Exported linear memory.
//! (memory (export "memory") 1)
//!
//! // Allocate `len` bytes; return a pointer to the region.
//! fn alloc(len: i32) -> i32
//!
//! // Extract content.
//! // html_ptr / html_len: UTF-8 HTML bytes placed into guest memory by the host.
//! // url_ptr  / url_len : UTF-8 URL bytes placed into guest memory by the host.
//! // Returns a pointer to a NUL-terminated, JSON-encoded WasmArticle, or 0
//! // on failure.
//! fn extract(html_ptr: i32, html_len: i32, url_ptr: i32, url_len: i32) -> i32
//! ```
//!
//! # Example guest (Rust, targeting `wasm32-unknown-unknown`)
//!
//! ```rust,ignore
//! #[no_mangle]
//! pub extern "C" fn alloc(len: i32) -> i32 {
//!     let mut buf = Vec::<u8>::with_capacity(len as usize);
//!     let ptr = buf.as_ptr() as i32;
//!     std::mem::forget(buf);
//!     ptr
//! }
//!
//! #[no_mangle]
//! pub extern "C" fn extract(
//!     html_ptr: i32, html_len: i32,
//!     _url_ptr: i32, _url_len: i32,
//! ) -> i32 {
//!     // ... parse HTML, return JSON ...
//!     0
//! }
//! ```

use std::path::Path;

use anyhow::{Result, bail};
use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use wasmtime::{Engine, Linker, Module, Store, StoreLimitsBuilder};

use super::{SiteContent, SiteMetadata, SiteProvider};
use crate::http_client::AcceleratedClient;

// ─────────────────────────────────────────────────────────────────────────────
// Safety constants
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum Wasm instructions the guest may execute per extraction call.
///
/// At ~10⁹ instructions/second on modern hardware this caps a single
/// extraction at roughly 100ms of CPU time before fuel runs out.
const FUEL_LIMIT: u64 = 100_000_000;

/// Maximum linear-memory growth the guest may request per instantiation.
const MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

// ─────────────────────────────────────────────────────────────────────────────
// Guest / host data types
// ─────────────────────────────────────────────────────────────────────────────

/// JSON structure the WASM guest returns via its `extract` export.
///
/// All fields are optional so that a guest can return partial results.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct WasmArticle {
    /// Extracted title.
    pub title: Option<String>,
    /// Main article content as plain text or Markdown.
    pub content: Option<String>,
    /// Author name.
    pub author: Option<String>,
    /// Publication date (ISO 8601 or human-readable).
    pub date: Option<String>,
    /// Canonical URL (defaults to the request URL if omitted).
    pub canonical_url: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider type
// ─────────────────────────────────────────────────────────────────────────────

/// Store data: holds the memory limiter so we can pass it by ref to `store.limiter`.
struct StoreData {
    limiter: wasmtime::StoreLimits,
}

/// A sandboxed [`SiteProvider`] backed by a compiled WASM module.
///
/// Created via [`WasmProvider::from_file`] or [`WasmProvider::from_bytes`].
/// The [`wasmtime::Module`] is compiled once at construction; each extraction
/// call instantiates a fresh module to guarantee isolation between requests.
pub struct WasmProvider {
    /// Provider name leaked for the `&'static str` required by [`SiteProvider`].
    static_name: &'static str,
    /// Compiled WASM module (expensive to compile; reused across calls).
    module: Module,
    /// Shared engine (config is immutable after construction).
    engine: Engine,
    /// Compiled URL-matching regex (alternation of all `url_patterns`).
    url_regex: Regex,
}

impl WasmProvider {
    /// Compile a WASM module from `wasm_path` and build a provider.
    ///
    /// # Errors
    ///
    /// Returns an error if `wasm_path` cannot be read, the WASM bytes fail to
    /// compile, or `url_pattern` is not a valid regex.
    pub fn from_file(name: &str, wasm_path: &Path, url_pattern: &str) -> Result<Self> {
        let engine = build_engine()?;
        let wasm_bytes = std::fs::read(wasm_path)
            .map_err(|e| anyhow::anyhow!("cannot read WASM file {}: {e}", wasm_path.display()))?;
        Self::compile(name, &engine, &wasm_bytes, url_pattern)
    }

    /// Compile a WASM module from raw bytes.  Primarily useful for tests.
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes fail to compile or `url_pattern` is invalid.
    pub fn from_bytes(name: &str, wasm_bytes: &[u8], url_pattern: &str) -> Result<Self> {
        let engine = build_engine()?;
        Self::compile(name, &engine, wasm_bytes, url_pattern)
    }

    /// Shared constructor — compiles the module and validates the URL pattern.
    fn compile(name: &str, engine: &Engine, wasm_bytes: &[u8], url_pattern: &str) -> Result<Self> {
        let module = Module::new(engine, wasm_bytes)
            .map_err(|e| anyhow::anyhow!("failed to compile WASM module for '{name}': {e}"))?;
        let url_regex = Regex::new(url_pattern).map_err(|e| {
            anyhow::anyhow!("invalid URL pattern '{url_pattern}' for WASM provider '{name}': {e}")
        })?;
        let static_name: &'static str = Box::leak(name.to_owned().into_boxed_str());
        Ok(Self {
            static_name,
            module,
            engine: engine.clone(),
            url_regex,
        })
    }

    /// Run the WASM guest's `extract` function with the given inputs.
    ///
    /// Instantiates a fresh module for isolation, writes inputs into linear
    /// memory via the guest's `alloc` export, calls `extract`, and reads the
    /// NUL-terminated JSON result back from linear memory.
    ///
    /// # Clippy: cast lints
    ///
    /// `usize → i32` and `i32 → usize` casts are required by the Wasm32 ABI —
    /// all pointers and lengths are `i32` in the guest.  Wasm linear memory is
    /// bounded to 4 GiB so these casts are safe in practice; inputs larger than
    /// `i32::MAX` would have been rejected by the memory limiter first.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    fn run_guest(&self, html: &[u8], url: &str) -> Result<WasmArticle> {
        let mut store = build_store(&self.engine)?;
        let linker: Linker<StoreData> = Linker::new(&self.engine);

        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| anyhow::anyhow!("failed to instantiate WASM module: {e}"))?;

        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "alloc")
            .map_err(|e| anyhow::anyhow!("WASM guest must export 'alloc(len: i32) -> i32': {e}"))?;

        let extract_fn = instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "extract")
            .map_err(|e| anyhow::anyhow!(
                "WASM guest must export 'extract(html_ptr, html_len, url_ptr, url_len) -> i32': {e}"
            ))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| anyhow::anyhow!("WASM guest must export a 'memory'"))?;

        // Write HTML into guest memory via alloc
        let html_ptr = alloc
            .call(&mut store, html.len() as i32)
            .map_err(|e| anyhow::anyhow!("guest alloc() for HTML failed: {e}"))?;
        write_guest_memory(&memory, &mut store, html_ptr as usize, html)?;

        // Write URL into guest memory via alloc
        let url_bytes = url.as_bytes();
        let url_ptr = alloc
            .call(&mut store, url_bytes.len() as i32)
            .map_err(|e| anyhow::anyhow!("guest alloc() for URL failed: {e}"))?;
        write_guest_memory(&memory, &mut store, url_ptr as usize, url_bytes)?;

        // Call the guest's extract function
        let result_ptr = extract_fn
            .call(
                &mut store,
                (html_ptr, html.len() as i32, url_ptr, url_bytes.len() as i32),
            )
            .map_err(|e| anyhow::anyhow!("WASM extract() call failed: {e}"))?;

        if result_ptr == 0 {
            bail!("WASM guest returned null pointer — extraction failed");
        }

        let json_bytes = read_guest_cstring(&memory, &mut store, result_ptr as usize)?;
        let article: WasmArticle = serde_json::from_slice(&json_bytes).map_err(|e| {
            anyhow::anyhow!(
                "guest returned invalid JSON ({e}): {:?}",
                &json_bytes[..json_bytes.len().min(200)]
            )
        })?;

        Ok(article)
    }
}

#[async_trait]
impl SiteProvider for WasmProvider {
    fn name(&self) -> &'static str {
        self.static_name
    }

    fn matches(&self, url: &str) -> bool {
        self.url_regex.is_match(url)
    }

    /// Extract content by running the WASM guest with the provided HTML.
    ///
    /// When `prefetched_html` is `None` the provider cannot proceed — callers
    /// must supply the HTML bytes (same contract as `CssExtractorProvider`).
    async fn extract(
        &self,
        url: &str,
        _client: &AcceleratedClient,
        _cookies: Option<&str>,
        prefetched_html: Option<&[u8]>,
    ) -> Result<SiteContent> {
        let html = prefetched_html.ok_or_else(|| {
            anyhow::anyhow!(
                "WASM provider '{}' requires pre-fetched HTML but none was provided for {url}",
                self.static_name
            )
        })?;

        let article = self.run_guest(html, url).map_err(|e| {
            anyhow::anyhow!("WASM provider '{}' failed for {url}: {e}", self.static_name)
        })?;

        let markdown = article.content.unwrap_or_default();

        let metadata = SiteMetadata {
            title: article.title,
            author: article.author,
            published: article.date,
            platform: format!("wasm:{}", self.static_name),
            canonical_url: article.canonical_url.unwrap_or_else(|| url.to_string()),
            media_urls: Vec::new(),
            engagement: None,
        };

        Ok(SiteContent { markdown, metadata })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Engine / store construction
// ─────────────────────────────────────────────────────────────────────────────

/// Build a [`wasmtime::Engine`] with fuel metering enabled.
fn build_engine() -> Result<Engine> {
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    config.max_wasm_stack(512 * 1024); // 512 KiB Wasm stack
    Engine::new(&config).map_err(|e| anyhow::anyhow!("failed to create wasmtime Engine: {e}"))
}

/// Build a [`Store`] with fuel and memory limits applied.
fn build_store(engine: &Engine) -> Result<Store<StoreData>> {
    let limits = StoreLimitsBuilder::new()
        .memory_size(MEMORY_LIMIT_BYTES)
        .build();
    let data = StoreData { limiter: limits };
    let mut store = Store::new(engine, data);
    store
        .set_fuel(FUEL_LIMIT)
        .map_err(|e| anyhow::anyhow!("failed to set fuel limit: {e}"))?;
    store.limiter(|data| &mut data.limiter);
    Ok(store)
}

// ─────────────────────────────────────────────────────────────────────────────
// Memory helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Write `bytes` into guest linear memory starting at `offset`.
fn write_guest_memory(
    memory: &wasmtime::Memory,
    store: &mut Store<StoreData>,
    offset: usize,
    bytes: &[u8],
) -> Result<()> {
    memory.write(store, offset, bytes).map_err(|e| {
        anyhow::anyhow!(
            "cannot write {}-byte slice to guest at offset {offset}: {e}",
            bytes.len()
        )
    })
}

/// Read a NUL-terminated byte string from guest linear memory at `offset`.
fn read_guest_cstring(
    memory: &wasmtime::Memory,
    store: &mut Store<StoreData>,
    offset: usize,
) -> Result<Vec<u8>> {
    let mem_slice = memory.data(store);
    let available = mem_slice
        .get(offset..)
        .ok_or_else(|| anyhow::anyhow!("guest result pointer {offset} is out of memory bounds"))?;

    let nul_pos = available.iter().position(|&b| b == 0).ok_or_else(|| {
        anyhow::anyhow!("no NUL terminator found in guest result starting at {offset}")
    })?;

    Ok(available[..nul_pos].to_vec())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── WAT test fixtures ─────────────────────────────────────────────────────

    /// A minimal WAT guest that writes a hardcoded JSON result at offset 256.
    fn minimal_wasm_bytes() -> Vec<u8> {
        let json = br#"{"title":"Test Title","content":"Hello World","author":"Alice","date":"2026-01-01"}"#;
        assert!(json.len() < 200, "JSON too long for test WAT module");

        let mut stores = String::new();
        for (i, &b) in json.iter().enumerate() {
            stores.push_str(&format!(
                "i32.const {}\ni32.const {}\ni32.store8\n",
                256 + i,
                b
            ));
        }
        // NUL terminator
        stores.push_str(&format!(
            "i32.const {}\ni32.const 0\ni32.store8\n",
            256 + json.len()
        ));

        let wat = format!(
            r#"(module
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32)
    i32.const 0)
  (func (export "extract") (param i32 i32 i32 i32) (result i32)
    {stores}
    i32.const 256)
)"#
        );

        wat::parse_str(&wat).expect("valid WAT")
    }

    /// A WAT guest whose `extract` returns 0 (failure).
    fn failing_wasm_bytes() -> Vec<u8> {
        wat::parse_str(
            r#"(module
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) i32.const 0)
  (func (export "extract") (param i32 i32 i32 i32) (result i32) i32.const 0)
)"#,
        )
        .expect("valid WAT")
    }

    /// A WAT guest that loops forever (burns all fuel).
    fn infinite_loop_wasm_bytes() -> Vec<u8> {
        wat::parse_str(
            r#"(module
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) i32.const 0)
  (func (export "extract") (param i32 i32 i32 i32) (result i32)
    (loop $loop (br $loop))
    i32.const 0)
)"#,
        )
        .expect("valid WAT")
    }

    // ── WasmProvider construction ─────────────────────────────────────────────

    #[test]
    fn from_bytes_builds_provider_with_valid_input() {
        // GIVEN: valid WASM bytes and a URL pattern
        let wasm = minimal_wasm_bytes();
        // WHEN
        let provider = WasmProvider::from_bytes("test", &wasm, r"example\.com");
        // THEN
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "test");
    }

    #[test]
    fn from_bytes_rejects_invalid_url_pattern() {
        // GIVEN: valid WASM but invalid regex
        let wasm = minimal_wasm_bytes();
        // WHEN / THEN
        assert!(WasmProvider::from_bytes("test", &wasm, r"[invalid").is_err());
    }

    #[test]
    fn from_bytes_rejects_invalid_wasm() {
        // GIVEN: garbage bytes
        assert!(WasmProvider::from_bytes("test", b"not wasm at all", r"example\.com").is_err());
    }

    // ── matches ───────────────────────────────────────────────────────────────

    #[test]
    fn matches_url_satisfying_pattern() {
        // GIVEN
        let p = WasmProvider::from_bytes("t", &minimal_wasm_bytes(), r"example\.com").unwrap();
        // WHEN / THEN
        assert!(p.matches("https://example.com/article/1"));
    }

    #[test]
    fn does_not_match_url_outside_pattern() {
        // GIVEN
        let p = WasmProvider::from_bytes("t", &minimal_wasm_bytes(), r"example\.com").unwrap();
        // WHEN / THEN
        assert!(!p.matches("https://other.com/article/1"));
    }

    // ── run_guest ─────────────────────────────────────────────────────────────

    #[test]
    fn run_guest_returns_article_from_valid_module() {
        // GIVEN
        let p = WasmProvider::from_bytes("t", &minimal_wasm_bytes(), r"example\.com").unwrap();
        // WHEN
        let article = p
            .run_guest(b"<html></html>", "https://example.com")
            .unwrap();
        // THEN
        assert_eq!(article.title.as_deref(), Some("Test Title"));
        assert_eq!(article.content.as_deref(), Some("Hello World"));
        assert_eq!(article.author.as_deref(), Some("Alice"));
    }

    #[test]
    fn run_guest_returns_error_when_extract_returns_null() {
        // GIVEN
        let p = WasmProvider::from_bytes("t", &failing_wasm_bytes(), r"example\.com").unwrap();
        // WHEN
        let result = p.run_guest(b"<html></html>", "https://example.com");
        // THEN: null pointer error
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("null pointer"));
    }

    #[test]
    fn run_guest_returns_error_when_fuel_exhausted() {
        // GIVEN: a module that loops forever (burns all fuel)
        let p =
            WasmProvider::from_bytes("t", &infinite_loop_wasm_bytes(), r"example\.com").unwrap();
        // WHEN
        let result = p.run_guest(b"<html></html>", "https://example.com");
        // THEN: fuel-exhaustion error (sandbox enforced)
        assert!(result.is_err());
    }

    // ── extract (async) ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn extract_returns_error_without_prefetched_html() {
        // GIVEN
        let p = WasmProvider::from_bytes("t", &minimal_wasm_bytes(), r"example\.com").unwrap();
        let client = AcceleratedClient::new().expect("client");
        // WHEN
        let result = p
            .extract("https://example.com/article", &client, None, None)
            .await;
        // THEN
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("pre-fetched HTML"));
    }

    #[tokio::test]
    async fn extract_produces_site_content_from_wasm_guest() {
        // GIVEN
        let p = WasmProvider::from_bytes("t", &minimal_wasm_bytes(), r"example\.com").unwrap();
        let client = AcceleratedClient::new().expect("client");
        let html = b"<html><body><article>Hello</article></body></html>";
        // WHEN
        let result = p
            .extract("https://example.com/article", &client, None, Some(html))
            .await
            .expect("extract should succeed");
        // THEN
        assert_eq!(result.markdown, "Hello World");
        assert_eq!(result.metadata.title.as_deref(), Some("Test Title"));
        assert_eq!(result.metadata.author.as_deref(), Some("Alice"));
        assert_eq!(result.metadata.platform, "wasm:t");
    }

    #[tokio::test]
    async fn extract_uses_request_url_as_canonical_when_guest_omits_it() {
        // GIVEN: our minimal module returns no canonical_url
        let p = WasmProvider::from_bytes("t", &minimal_wasm_bytes(), r"example\.com").unwrap();
        let client = AcceleratedClient::new().expect("client");
        // WHEN
        let result = p
            .extract(
                "https://example.com/page",
                &client,
                None,
                Some(b"<html></html>"),
            )
            .await
            .expect("extract should succeed");
        // THEN: falls back to the request URL
        assert_eq!(result.metadata.canonical_url, "https://example.com/page");
    }

    // ── WasmArticle serde ─────────────────────────────────────────────────────

    #[test]
    fn wasm_article_deserialises_all_fields() {
        // GIVEN
        let json = r#"{"title":"T","content":"C","author":"A","date":"D","canonical_url":"U"}"#;
        // WHEN
        let a: WasmArticle = serde_json::from_str(json).unwrap();
        // THEN
        assert_eq!(a.title.as_deref(), Some("T"));
        assert_eq!(a.content.as_deref(), Some("C"));
        assert_eq!(a.author.as_deref(), Some("A"));
        assert_eq!(a.date.as_deref(), Some("D"));
        assert_eq!(a.canonical_url.as_deref(), Some("U"));
    }

    #[test]
    fn wasm_article_all_fields_optional() {
        // GIVEN: empty JSON object
        let a: WasmArticle = serde_json::from_str("{}").unwrap();
        // THEN
        assert!(a.title.is_none());
        assert!(a.content.is_none());
    }

    // ── engine / store ────────────────────────────────────────────────────────

    #[test]
    fn build_engine_succeeds() {
        assert!(build_engine().is_ok());
    }

    #[test]
    fn build_store_has_fuel_and_limiter() {
        // GIVEN / WHEN: building a store should not panic
        let engine = build_engine().unwrap();
        let store = build_store(&engine);
        assert!(store.is_ok());
    }
}
