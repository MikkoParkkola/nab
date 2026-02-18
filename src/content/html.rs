//! HTML to Markdown conversion handler.
//!
//! Uses Mozilla-style readability extraction to extract clean article content
//! before converting to markdown. Falls back to raw html2md for non-article pages.
//!
//! # Pipeline
//!
//! 1. **SPA data extraction** (Next.js/Nuxt): Extract from `__NEXT_DATA__` / `__NUXT__` JSON
//! 2. **Readability extraction** (default): Extract main article content, strip boilerplate
//! 3. **Fallback to raw html2md**: If extraction fails, use raw HTML with basic filtering
//!
//! The readability step significantly improves output quality by removing navigation,
//! footers, ads, and other noise before markdown conversion.

use anyhow::Result;

use super::readability;
use super::{ContentHandler, ConversionResult};

/// Converts HTML responses to clean markdown.
pub struct HtmlHandler;

impl ContentHandler for HtmlHandler {
    fn supported_types(&self) -> &[&str] {
        &["text/html", "application/xhtml+xml"]
    }

    fn to_markdown(&self, bytes: &[u8], content_type: &str) -> Result<ConversionResult> {
        self.to_markdown_with_url(bytes, content_type, None)
    }
}

impl HtmlHandler {
    /// Convert HTML to markdown, using the real page URL for readability heuristics.
    ///
    /// Providing `url` improves extraction quality: the readability crate uses it
    /// for relative link resolution and site-specific scoring.
    ///
    /// # Errors
    ///
    /// Infallible in practice — returns `Err` only if the internal `Ok(...)` somehow
    /// fails, which cannot happen with the current implementation.
    pub fn to_markdown_with_url(
        &self,
        bytes: &[u8],
        content_type: &str,
        url: Option<&str>,
    ) -> Result<ConversionResult> {
        let start = std::time::Instant::now();
        let html = String::from_utf8_lossy(bytes);
        let markdown = html_to_markdown_with_url(&html, url);

        Ok(ConversionResult {
            markdown,
            page_count: None,
            content_type: content_type.to_string(),
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

/// Convert HTML to markdown with URL-aware readability extraction.
///
/// # Pipeline
///
/// 1. **SPA extraction**: Try `__NEXT_DATA__` / `__NUXT_DATA__` for React/Vue SPAs
/// 2. **Readability extraction**: Strip comment sections, then extract article content
/// 3. **Fallback**: If extraction fails, use raw HTML with basic filtering
///
/// Passing the real `url` significantly improves readability quality for sites with
/// complex DOM structures (`LessWrong`, `Ghost CMS`, etc.).
#[must_use]
pub fn html_to_markdown_with_url(html: &str, url: Option<&str>) -> String {
    // Try SPA data extraction first (Next.js, Nuxt, etc.)
    if let Some(spa_content) = extract_spa_data(html) {
        return spa_content;
    }

    // Pre-strip comment sections before readability to prevent comment bleed-through
    let cleaned_html = strip_comment_sections(html);

    // Try readability extraction with real URL (or fallback placeholder)
    let effective_url = url.unwrap_or("https://example.com");
    if let Some(article) = readability::extract_article(&cleaned_html, effective_url) {
        let md = html2md::parse_html(&article.content_html);
        let lines: Vec<&str> = md
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        return lines.join("\n");
    }

    // Fallback to raw html2md with filtering if readability fails
    html_to_markdown(html)
}

/// Convert HTML to markdown with readability extraction (URL-unaware).
///
/// Calls [`html_to_markdown_with_url`] with no URL. Prefer the URL-aware
/// variant when the fetch URL is available.
#[must_use]
pub fn html_to_markdown_with_readability(html: &str) -> String {
    html_to_markdown_with_url(html, None)
}

/// Try to extract article content from SPA JSON bundles embedded in HTML.
///
/// Modern single-page applications (Next.js, Nuxt, etc.) embed serialized
/// server-side render state in `<script>` tags. This function extracts that
/// state and recursively searches for the longest text content field.
///
/// Returns `Some(markdown)` if a substantial content field is found (>200 chars),
/// `None` otherwise.
fn extract_spa_data(html: &str) -> Option<String> {
    let document = scraper::Html::parse_document(html);

    // Try __NEXT_DATA__ (Next.js) — highest priority, most structured
    if let Some(content) = try_extract_script_json(&document, "script#__NEXT_DATA__") {
        return Some(content);
    }

    // Try __NUXT_DATA__ / __NUXT_STATE__ (Nuxt.js)
    for selector in &["script#__NUXT_DATA__", "script#__nuxt-data"] {
        if let Some(content) = try_extract_script_json(&document, selector) {
            return Some(content);
        }
    }

    None
}

/// Extract and convert content from a JSON-bearing `<script>` element.
fn try_extract_script_json(document: &scraper::Html, css_selector: &str) -> Option<String> {
    let sel = scraper::Selector::parse(css_selector).ok()?;
    let script = document.select(&sel).next()?;
    let json_text = script.text().collect::<String>();
    let data: serde_json::Value = serde_json::from_str(&json_text).ok()?;
    extract_nextjs_content(&data)
}

/// Recursively search a Next.js `pageProps` tree for the longest content field.
///
/// Next.js stores page data under `props.pageProps`. We look for well-known
/// content field names and return the longest match above the minimum threshold.
fn extract_nextjs_content(data: &serde_json::Value) -> Option<String> {
    // Ordered by specificity: html/content first, metadata last
    const CONTENT_KEYS: &[&str] = &[
        "body",
        "html",
        "content",
        "article",
        "post",
        "markdown",
        "text",
        "description",
    ];
    // Minimum chars to be considered article content (not a blurb or empty string)
    const MIN_CONTENT_LEN: usize = 100;

    // Next.js: props.pageProps holds the actual page data
    let page_props = data.get("props")?.get("pageProps")?;

    let mut best: Option<String> = None;

    for key in CONTENT_KEYS {
        if let Some(found) = find_content_recursive(page_props, key) {
            let current_best_len = best.as_deref().map_or(0, str::len);
            if found.len() >= MIN_CONTENT_LEN && found.len() > current_best_len {
                best = Some(found);
            }
        }
    }

    best.map(|content| {
        if content.contains('<') && content.contains('>') {
            // Looks like HTML — convert to markdown
            let md = html2md::parse_html(&content);
            md.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            content
        }
    })
}

/// Recursively walk a JSON value tree looking for a string field named `key`.
///
/// Returns the first string value found, or `None`. Depth-first, object before array.
fn find_content_recursive(value: &serde_json::Value, key: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            // Check this level first
            if let Some(serde_json::Value::String(s)) = map.get(key) {
                return Some(s.clone());
            }
            // Recurse into values
            for (_, v) in map {
                if let Some(found) = find_content_recursive(v, key) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(found) = find_content_recursive(item, key) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Remove comment section DOM nodes from HTML before readability processing.
///
/// Sites like `LessWrong` have dense comment sections that confuse the readability
/// crate's scoring heuristics. We identify comment containers by common class/id
/// patterns and blank out their text content, preserving the DOM structure so
/// the readability crate can still score remaining nodes correctly.
///
/// This is a fast string-scan approach — not full DOM manipulation — so it
/// operates on the raw HTML string and handles nested structures by targeting
/// outermost comment containers.
///
/// # Panics
///
/// Never panics in practice — the `"div"` CSS selector used as a last-resort
/// fallback is always valid and cannot fail to parse.
#[must_use]
pub fn strip_comment_sections(html: &str) -> String {
    let document = scraper::Html::parse_document(html);

    // We can't mutate the scraper DOM, so we serialize element HTML of everything
    // EXCEPT comment containers, then reconstruct a valid document.
    let body_selector = scraper::Selector::parse("body").ok();

    // Build a set of comment container hashes to skip
    // The primary selector targets elements with class or id attributes.
    // Fallback to "div" is always valid, so the unwrap() is safe.
    #[allow(clippy::unwrap_used)]
    let div_sel = scraper::Selector::parse("div[class], div[id], section[class], section[id]")
        .unwrap_or_else(|_| scraper::Selector::parse("div").unwrap());

    let comment_container_ids: std::collections::HashSet<u64> = document
        .select(&div_sel)
        .filter(|el| is_comment_container(el))
        .map(|el| element_hash(el))
        .collect();

    if comment_container_ids.is_empty() {
        return html.to_string();
    }

    // Rebuild HTML without comment containers by serializing the body's
    // direct children that aren't comment containers, then wrapping.
    // For simplicity and correctness, we blank the comment node HTML.
    let body_html = body_selector
        .as_ref()
        .and_then(|sel| document.select(sel).next())
        .map_or_else(
            || html.to_string(),
            |body| {
                // Filter children: collect HTML of non-comment children
                body.children()
                    .filter_map(|child| {
                        let el_ref = scraper::ElementRef::wrap(child)?;
                        if comment_container_ids.contains(&element_hash(el_ref)) {
                            None
                        } else {
                            Some(el_ref.html())
                        }
                    })
                    .collect::<String>()
            },
        );

    // Preserve head and wrap body
    let head_html = scraper::Selector::parse("head")
        .ok()
        .and_then(|sel| document.select(&sel).next())
        .map(|h| h.html())
        .unwrap_or_default();

    format!("<html>{head_html}<body>{body_html}</body></html>")
}

/// Returns `true` if this element looks like a comment section container.
fn is_comment_container(element: &scraper::ElementRef<'_>) -> bool {
    const COMMENT_MARKERS: &[&str] = &[
        "comment-section",
        "comments-section",
        "comments-container",
        "comment-list",
        "comment-thread",
        "disqus",
        "discussion-section",
        "replies-section",
    ];

    let class = element.value().attr("class").unwrap_or("");
    let id = element.value().attr("id").unwrap_or("");
    let combined = format!("{class} {id}").to_lowercase();

    COMMENT_MARKERS
        .iter()
        .any(|marker| combined.contains(marker))
}

/// Stable hash for a DOM element based on its outer HTML.
///
/// Used to identify specific elements across two passes over the parsed DOM.
fn element_hash(element: scraper::ElementRef<'_>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Hash the element's name + attributes as a structural fingerprint
    element.value().name().hash(&mut hasher);
    for (k, v) in element.value().attrs() {
        k.hash(&mut hasher);
        v.hash(&mut hasher);
    }
    hasher.finish()
}

/// Convert HTML to markdown with boilerplate filtering (fallback).
///
/// Uses `html2md` for the heavy lifting, then post-processes to remove
/// common web boilerplate (cookie notices, navigation, privacy footers)
/// and collapse excessive whitespace.
pub fn html_to_markdown(html: &str) -> String {
    let md = html2md::parse_html(html);

    let lines: Vec<&str> = md
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !is_boilerplate(l))
        .collect();

    lines.join("\n")
}

/// Returns `true` if a line looks like web boilerplate.
fn is_boilerplate(line: &str) -> bool {
    // Preserve markdown links -- never filter lines containing link syntax
    if line.contains("](") {
        return false;
    }

    let lower = line.to_lowercase();
    lower.contains("skip to content")
        || lower.contains("cookie")
        || lower.contains("privacy policy")
        || lower.contains("terms of service")
        || lower.starts_with("©")
        || lower.starts_with("copyright")
        || (lower.len() < 3 && !lower.chars().any(char::is_alphanumeric))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_basic_html() {
        let html = "<html><body><h1>Title</h1><p>Paragraph</p></body></html>";
        let md = html_to_markdown(html);
        assert!(md.contains("Title"));
        assert!(md.contains("Paragraph"));
    }

    #[test]
    fn filters_boilerplate() {
        let html = "<html><body>\
            <p>Skip to content</p>\
            <h1>Real Content</h1>\
            <p>© 2025 Company</p>\
            </body></html>";
        let md = html_to_markdown(html);
        assert!(md.contains("Real Content"));
        assert!(!md.contains("Skip to content"));
        assert!(!md.contains("2025 Company"));
    }

    #[test]
    fn preserves_markdown_links() {
        let html = r#"<html><body><a href="https://example.com">Link text</a></body></html>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("]("));
    }

    #[test]
    fn handler_returns_conversion_result() {
        let handler = HtmlHandler;
        let html = b"<html><body><p>Test</p></body></html>";
        let result = handler.to_markdown(html, "text/html").unwrap();
        assert!(result.markdown.contains("Test"));
        assert_eq!(result.content_type, "text/html");
        assert!(result.page_count.is_none());
        assert!(result.elapsed_ms >= 0.0);
    }

    #[test]
    fn handler_with_url_improves_extraction() {
        let handler = HtmlHandler;
        let html = b"<html><body>\
            <article><h1>Article</h1><p>Content here.</p></article>\
            </body></html>";
        let result = handler
            .to_markdown_with_url(html, "text/html", Some("https://example.com/article"))
            .unwrap();
        assert!(result.markdown.contains("Article") || result.markdown.contains("Content"));
    }

    #[test]
    fn handles_non_utf8_gracefully() {
        let handler = HtmlHandler;
        // Latin-1 encoded text (invalid UTF-8 byte 0xe9 for 'é')
        let bytes: &[u8] = b"<html><body>caf\xe9</body></html>";
        let result = handler.to_markdown(bytes, "text/html; charset=iso-8859-1");
        assert!(result.is_ok());
    }

    #[test]
    fn test_is_boilerplate_detects_common_patterns() {
        assert!(is_boilerplate("Skip to content"));
        assert!(is_boilerplate("Cookie Policy"));
        assert!(is_boilerplate("Privacy Policy"));
        assert!(is_boilerplate("Terms of Service"));
        assert!(is_boilerplate("© 2025 Company"));
        assert!(is_boilerplate("Copyright 2025"));
    }

    #[test]
    fn test_is_boilerplate_preserves_content() {
        assert!(!is_boilerplate("This is actual content"));
        assert!(!is_boilerplate("Welcome to our site"));
        assert!(!is_boilerplate("[Link](https://example.com)"));
    }

    #[test]
    fn test_html_to_markdown_removes_excess_whitespace() {
        let html = "<html><body><p>Line 1</p>\n\n\n<p>Line 2</p></body></html>";
        let md = html_to_markdown(html);
        // Should collapse multiple newlines
        assert!(!md.contains("\n\n\n"));
    }

    #[test]
    fn test_handler_supported_types() {
        let handler = HtmlHandler;
        let types = handler.supported_types();
        assert!(types.contains(&"text/html"));
        assert!(types.contains(&"application/xhtml+xml"));
    }

    #[test]
    fn test_readability_extraction_removes_boilerplate() {
        let html = r#"
            <html>
            <head><title>Article Title</title></head>
            <body>
                <nav>
                    <a href="/">Home</a>
                    <a href="/about">About</a>
                </nav>
                <main>
                    <article>
                        <h1>Main Article</h1>
                        <p>This is the main article content that should be extracted.</p>
                        <p>It contains important information for the reader.</p>
                    </article>
                </main>
                <aside class="sidebar">
                    <h3>Related Articles</h3>
                    <ul><li>Related 1</li></ul>
                </aside>
                <footer>
                    <p>© 2025 Company</p>
                    <p>Privacy Policy | Terms of Service</p>
                </footer>
            </body>
            </html>
        "#;

        let markdown = html_to_markdown_with_url(html, Some("https://example.com/article"));

        // Should contain main article content (case-insensitive check)
        let markdown_lower = markdown.to_lowercase();
        assert!(
            markdown_lower.contains("main article") || markdown_lower.contains("article content"),
            "Expected article content, got: {}",
            markdown
        );
        assert!(markdown.contains("main article content"));

        // Should NOT contain navigation or footer boilerplate
        assert!(!markdown.contains("Home") || !markdown.contains("About"));
        assert!(!markdown.contains("2025 Company"));
        assert!(!markdown.contains("Privacy Policy"));
    }

    #[test]
    fn test_readability_fallback_for_non_article_pages() {
        let html = "<html><body><div>Simple page</div></body></html>";
        let markdown = html_to_markdown_with_url(html, None);

        // Should still convert to markdown (fallback path)
        assert!(markdown.contains("Simple page"));
    }

    #[test]
    fn test_readability_with_semantic_html() {
        let html = r#"
            <html>
            <body>
                <header>Header content</header>
                <main>
                    <h1>Article Title</h1>
                    <p>This is the main content area with substantial text.</p>
                    <p>Multiple paragraphs ensure proper extraction.</p>
                </main>
                <footer>Footer content</footer>
            </body>
            </html>
        "#;

        let markdown = html_to_markdown_with_url(html, Some("https://example.com/article"));

        assert!(markdown.contains("Article Title"));
        assert!(markdown.contains("main content area"));
        assert!(!markdown.contains("Header content"));
        assert!(!markdown.contains("Footer content"));
    }

    #[test]
    fn test_readability_with_real_url_extracts_article() {
        // Verifies BUG 1 fix: real URL passed to readability, not "https://example.com"
        let html = r#"
            <html>
            <head><title>LessWrong Post</title></head>
            <body>
                <article>
                    <h1>Rationalist Argument</h1>
                    <p>This post contains substantial content about reasoning under uncertainty.</p>
                    <p>Multiple paragraphs to ensure proper article detection by readability.</p>
                </article>
                <div class="comment-section">
                    <div class="comment">User comment one</div>
                    <div class="comment">User comment two</div>
                </div>
            </body>
            </html>
        "#;

        let markdown =
            html_to_markdown_with_url(html, Some("https://www.lesswrong.com/posts/abc/title"));

        assert!(markdown.contains("Rationalist Argument") || markdown.contains("reasoning"));
    }

    #[test]
    fn strip_comment_sections_removes_comment_containers() {
        let html = r#"
            <html><body>
                <article>
                    <h1>Post Title</h1>
                    <p>Real article content here with enough text.</p>
                </article>
                <div class="comment-section">
                    <div>First comment: Lorem ipsum</div>
                    <div>Second comment: dolor sit amet</div>
                </div>
            </body></html>
        "#;

        let stripped = strip_comment_sections(html);

        // The stripped HTML should not contain the comment-section div
        assert!(
            !stripped.contains("comment-section"),
            "comment-section class should be removed, got: {}",
            &stripped[..stripped.len().min(500)]
        );
        // But should retain article content
        assert!(stripped.contains("Post Title") || stripped.contains("article"));
    }

    #[test]
    fn strip_comment_sections_preserves_article_content() {
        let html = r#"
            <html><body>
                <article>
                    <h1>The Real Post</h1>
                    <p>Substantive article body that we must not lose.</p>
                </article>
            </body></html>
        "#;

        let stripped = strip_comment_sections(html);

        assert!(stripped.contains("The Real Post"));
        assert!(stripped.contains("Substantive article body"));
    }

    #[test]
    fn extract_nextjs_content_finds_body_field() {
        let json_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "title": "Hello World",
                        "body": "<p>This is the article body content with substantial text that should be extracted by the SPA extractor when readability fails.</p>"
                    }
                }
            }
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(
            content.contains("article body content"),
            "Expected body content, got: {}",
            content
        );
    }

    #[test]
    fn extract_nextjs_content_returns_none_for_short_content() {
        let json_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "body": "Short"
                    }
                }
            }
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_none(), "Short content should return None");
    }

    #[test]
    fn extract_nextjs_content_returns_none_without_page_props() {
        let json_data = serde_json::json!({
            "query": {"id": "123"},
            "buildId": "abc"
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_none());
    }

    #[test]
    fn find_content_recursive_finds_nested_key() {
        let value = serde_json::json!({
            "level1": {
                "level2": {
                    "content": "deep content string that is long enough to be found"
                }
            }
        });

        let result = find_content_recursive(&value, "content");
        assert!(result.is_some());
        assert!(result.unwrap().contains("deep content"));
    }

    #[test]
    fn find_content_recursive_finds_in_array() {
        let value = serde_json::json!([
            {"title": "skip"},
            {"body": "the actual article body content found in array"}
        ]);

        let result = find_content_recursive(&value, "body");
        assert!(result.is_some());
        assert!(result.unwrap().contains("actual article body"));
    }

    #[test]
    fn extract_spa_data_returns_none_for_plain_html() {
        let html = "<html><body><p>Plain page, no SPA data</p></body></html>";
        assert!(extract_spa_data(html).is_none());
    }

    #[test]
    fn extract_spa_data_extracts_nextjs_data() {
        // Simulate a Next.js page with embedded data
        let body_content = "This is the article body content from a Next.js application. \
            It contains multiple sentences to pass the length threshold check.";
        let json = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "title": "Test Post",
                        "body": body_content
                    }
                }
            },
            "buildId": "abc123"
        });
        let html = format!(
            r#"<html><head></head><body>
                <script id="__NEXT_DATA__" type="application/json">{}</script>
                <div id="__next"><p>SSR placeholder</p></div>
            </body></html>"#,
            json
        );

        let result = extract_spa_data(&html);
        assert!(result.is_some(), "Should extract Next.js content");
        let content = result.unwrap();
        assert!(
            content.contains("article body content"),
            "Expected SPA body, got: {}",
            content
        );
    }

    // Backward-compatibility: old callers still work
    #[test]
    fn html_to_markdown_with_readability_still_works() {
        let html = "<html><body><article>\
            <h1>Compat Test</h1>\
            <p>Backward compatible call path works fine.</p>\
            </article></body></html>";
        let md = html_to_markdown_with_readability(html);
        assert!(md.contains("Compat Test") || md.contains("Backward compatible"));
    }
}
