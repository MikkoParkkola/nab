//! Example nab WASM provider — generic article extractor.
//!
//! This crate demonstrates how to write a WASM provider for nab.  It receives
//! raw HTML bytes and a URL string, then returns a JSON-encoded article.
//!
//! # Building
//!
//! ```sh
//! rustup target add wasm32-unknown-unknown
//! cargo build --target wasm32-unknown-unknown --release \
//!     --manifest-path examples/wasm_provider/Cargo.toml
//!
//! # The compiled module is at:
//! # examples/wasm_provider/target/wasm32-unknown-unknown/release/nab_wasm_example.wasm
//! ```
//!
//! # Installing
//!
//! ```sh
//! # Copy the .wasm and the sidecar manifest:
//! cp target/wasm32-unknown-unknown/release/nab_wasm_example.wasm /tmp/generic-article.wasm
//! cp examples/wasm_provider/manifest.toml /tmp/generic-article.manifest.toml
//!
//! nab provider install /tmp/generic-article.wasm
//! nab provider list
//! ```
//!
//! # Guest ABI (implemented here)
//!
//! The host calls two exported functions:
//!
//! - `alloc(len: i32) -> i32`: allocate `len` bytes; return pointer.
//! - `extract(html_ptr, html_len, url_ptr, url_len) -> i32`: parse HTML,
//!   return pointer to NUL-terminated JSON, or 0 on failure.
//!
//! The host writes HTML and URL into the memory region returned by `alloc`.

// No standard library — keeps the .wasm binary small.
#![no_std]
extern crate alloc;

use alloc::{string::String, vec, vec::Vec};

// ─────────────────────────────────────────────────────────────────────────────
// Memory allocator (required for no_std + heap allocation)
// ─────────────────────────────────────────────────────────────────────────────

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

// ─────────────────────────────────────────────────────────────────────────────
// Guest ABI exports
// ─────────────────────────────────────────────────────────────────────────────

/// Allocate `len` bytes of heap memory and return a raw pointer.
///
/// The host uses this to obtain a region where it can write the HTML/URL input.
/// Memory is intentionally never freed — WASM instances are single-use.
#[no_mangle]
pub extern "C" fn alloc(len: i32) -> i32 {
    let mut buf: Vec<u8> = vec![0u8; len as usize];
    let ptr = buf.as_mut_ptr() as i32;
    core::mem::forget(buf);
    ptr
}

/// Extract article content from the HTML placed in guest memory by the host.
///
/// # Arguments
///
/// - `html_ptr` / `html_len`: raw HTML bytes written by the host via `alloc`.
/// - `url_ptr`  / `url_len`:  URL bytes written by the host via `alloc`.
///
/// # Returns
///
/// A pointer to a NUL-terminated JSON string conforming to `WasmArticle`, or
/// `0` if extraction fails.
#[no_mangle]
pub extern "C" fn extract(
    html_ptr: i32,
    html_len: i32,
    url_ptr: i32,
    url_len: i32,
) -> i32 {
    let html = unsafe { core::slice::from_raw_parts(html_ptr as *const u8, html_len as usize) };
    let url = unsafe { core::slice::from_raw_parts(url_ptr as *const u8, url_len as usize) };

    let url_str = core::str::from_utf8(url).unwrap_or("");
    let html_str = core::str::from_utf8(html).unwrap_or("");

    let article = parse_article(html_str, url_str);
    serialize_article(&article)
}

// ─────────────────────────────────────────────────────────────────────────────
// Minimal HTML parsing (no external parser — keeps binary tiny)
// ─────────────────────────────────────────────────────────────────────────────

struct Article {
    title: Option<String>,
    content: Option<String>,
    author: Option<String>,
    url: String,
}

fn parse_article(html: &str, url: &str) -> Article {
    Article {
        title: extract_tag_content(html, "title")
            .or_else(|| extract_og_meta(html, "og:title")),
        content: extract_article_text(html),
        author: extract_og_meta(html, "article:author"),
        url: url.to_string(),
    }
}

/// Extract the text content of the first `<tag>…</tag>` occurrence.
fn extract_tag_content(html: &str, tag: &str) -> Option<String> {
    let open = alloc::format!("<{tag}");
    let close = alloc::format!("</{tag}>");

    let start = html.find(open.as_str())?;
    // Skip past the closing `>` of the opening tag.
    let content_start = html[start..].find('>')? + start + 1;
    let content_end = html[content_start..].find(close.as_str())? + content_start;

    let raw = html[content_start..content_end].trim();
    if raw.is_empty() {
        None
    } else {
        Some(strip_tags(raw))
    }
}

/// Extract `<meta property="X" content="Y" />` → Y.
fn extract_og_meta(html: &str, property: &str) -> Option<String> {
    let needle = alloc::format!("property=\"{property}\"");
    let pos = html.find(needle.as_str())?;
    let after = &html[pos..];
    // Look for content="..." after the property attribute.
    let content_pos = after.find("content=\"")? + "content=\"".len();
    let content_end = after[content_pos..].find('"')? + content_pos;
    let value = after[content_pos..content_end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Extract visible text from inside `<article>` or `<main>` tags.
fn extract_article_text(html: &str) -> Option<String> {
    // Try <article> first, fall back to <main>.
    let content_html = extract_tag_content(html, "article")
        .or_else(|| extract_tag_content(html, "main"))?;
    let stripped = strip_tags(&content_html);
    let trimmed = stripped.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

/// Strip all HTML tags from a string, returning plain text.
fn strip_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

// ─────────────────────────────────────────────────────────────────────────────
// JSON serialisation (manual — avoids serde in no_std context)
// ─────────────────────────────────────────────────────────────────────────────

/// Serialise the article to a NUL-terminated JSON string in guest memory.
///
/// Returns a pointer to the NUL-terminated string, or `0` on failure.
fn serialize_article(article: &Article) -> i32 {
    let mut json = String::from("{");

    if let Some(ref t) = article.title {
        json.push_str(&json_field("title", t));
        json.push(',');
    }
    if let Some(ref c) = article.content {
        json.push_str(&json_field("content", c));
        json.push(',');
    }
    if let Some(ref a) = article.author {
        json.push_str(&json_field("author", a));
        json.push(',');
    }
    json.push_str(&json_field("canonical_url", &article.url));
    json.push('}');

    // NUL-terminate
    json.push('\0');

    let bytes = json.into_bytes();
    let ptr = bytes.as_ptr() as i32;
    core::mem::forget(bytes);
    ptr
}

/// Produce `"key":"value"` with JSON-escaped value.
fn json_field(key: &str, value: &str) -> String {
    alloc::format!("\"{}\":\"{}\"", key, json_escape(value))
}

/// Escape a string for embedding in JSON.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str(r#"\""#),
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            c => out.push(c),
        }
    }
    out
}
