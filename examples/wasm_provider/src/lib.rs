//! Example nab WASM provider — generic article extractor.
//!
//! This crate demonstrates how to write a nab WASM provider using the
//! **WIT Component Model** ABI.  The guest implements the
//! `nab:provider/extractor` interface declared in `wit/provider.wit`; the host
//! calls `extract(url, html)` and receives a typed `Article` record or an
//! error string.
//!
//! Compare with the legacy raw-ABI approach: no `extern "C"`, no manual
//! pointer arithmetic, no NUL-terminated JSON — just ordinary Rust.
//!
//! # Building
//!
//! Prerequisites:
//!
//! ```sh
//! # WASI P2 target (for the wasm32-wasip2 Component)
//! rustup target add wasm32-wasip2
//!
//! # wasm-tools CLI (converts a WASI P2 module to a self-contained Component)
//! cargo install wasm-tools
//! ```
//!
//! Build steps:
//!
//! ```sh
//! # 1. Compile to a WASI P2 module
//! cargo build --target wasm32-wasip2 --release \
//!     --manifest-path examples/wasm_provider/Cargo.toml
//!
//! # 2. The .wasm output is already a Component when targeting wasm32-wasip2
//! #    with wit-bindgen — no adapter step required.
//! # Output:
//! #   examples/wasm_provider/target/wasm32-wasip2/release/nab_wasm_example.wasm
//! ```
//!
//! # Installing
//!
//! ```sh
//! cp examples/wasm_provider/target/wasm32-wasip2/release/nab_wasm_example.wasm \
//!    /tmp/my-article-extractor.wasm
//!
//! nab provider install /tmp/my-article-extractor.wasm
//! nab provider list
//! ```

// Generate the `exports::nab::provider::extractor::Guest` trait, the
// `Article` type, and the `export!` macro from the WIT file at the given path
// (relative to the workspace root — resolved at compile time by wit-bindgen).
wit_bindgen::generate!({
    path: "../../wit/provider.wit",
    world: "provider",
});

use exports::nab::provider::extractor::Article;

// ─────────────────────────────────────────────────────────────────────────────
// Guest implementation
// ─────────────────────────────────────────────────────────────────────────────

/// The concrete type that implements the `nab:provider/extractor` interface.
struct GenericArticleExtractor;

impl exports::nab::provider::extractor::Guest for GenericArticleExtractor {
    /// Extract article content from raw HTML.
    ///
    /// Returns `Ok(Article)` with whatever fields could be found, or
    /// `Err(reason)` if the page contains nothing useful.
    fn extract(url: String, html: String) -> Result<Article, String> {
        let parsed = parse_article(&html, &url);

        let content = parsed
            .content
            .ok_or_else(|| "no article content found".to_string())?;

        Ok(Article {
            title: parsed.title,
            content,
            author: parsed.author,
            date: None,
        })
    }
}

// Register the implementation with the Component Model runtime.
export!(GenericArticleExtractor);

// ─────────────────────────────────────────────────────────────────────────────
// Internal HTML parsing (minimal, dependency-free)
// ─────────────────────────────────────────────────────────────────────────────

struct ParsedArticle {
    title: Option<String>,
    content: Option<String>,
    author: Option<String>,
}

fn parse_article(html: &str, url: &str) -> ParsedArticle {
    let _ = url; // used by callers for context; unused in this simple extractor
    ParsedArticle {
        title: extract_tag_content(html, "title")
            .or_else(|| extract_og_meta(html, "og:title")),
        content: extract_article_text(html),
        author: extract_og_meta(html, "article:author"),
    }
}

/// Extract the text content of the first `<tag>…</tag>` occurrence.
fn extract_tag_content(html: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");

    let start = html.find(open.as_str())?;
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
    let needle = format!("property=\"{property}\"");
    let pos = html.find(needle.as_str())?;
    let after = &html[pos..];
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
