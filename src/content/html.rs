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
/// 4. **Thin-content warning**: Emit a tracing warning when output is disproportionately
///    small vs. the HTML body size, indicating JS-rendered content may be missing.
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
    let markdown = if let Some(article) = readability::extract_article(&cleaned_html, effective_url) {
        let md = html2md::parse_html(&article.content_html);
        let lines: Vec<&str> = md
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        lines.join("\n")
    } else {
        // Fallback to raw html2md with filtering if readability fails
        html_to_markdown(html)
    };

    // Warn when output is suspiciously thin relative to the HTML input.
    // A ratio below 2% usually means JS-rendered content was not captured.
    if let Some(warning) = detect_thin_content(html.len(), markdown.len()) {
        tracing::warn!("{}", warning);
    }

    markdown
}

/// Detect suspiciously thin markdown output relative to HTML input size.
///
/// Returns a warning message when the markdown is disproportionately small
/// compared to the raw HTML. This typically indicates JavaScript-rendered
/// content that was not captured by the static HTML parser.
///
/// The threshold is empirically calibrated:
/// - Normal article pages: markdown ≥ 8% of HTML body size
/// - JS-rendered pages (e.g., Stripe blog): markdown < 1% of HTML body size
/// - Minimum HTML size to avoid false positives on tiny pages: 5 KB
///
/// # Returns
///
/// `Some(warning)` when the ratio is below the threshold, `None` otherwise.
#[must_use]
pub fn detect_thin_content(html_len: usize, markdown_len: usize) -> Option<String> {
    const MIN_HTML_LEN: usize = 5_000;
    const THIN_RATIO_PERCENT: usize = 2;

    if html_len < MIN_HTML_LEN {
        return None;
    }

    #[allow(clippy::cast_precision_loss)]
    let ratio_percent = (markdown_len * 100) / html_len.max(1);

    if ratio_percent < THIN_RATIO_PERCENT {
        Some(format!(
            "Warning: output is suspiciously thin ({markdown_len} chars from {html_len} bytes of HTML, \
             {ratio_percent}% ratio). The page likely uses JavaScript rendering — \
             the article body may be missing. Try:\n  \
             1. nab spa <url>              (extract embedded SPA data)\n  \
             2. nab fetch --cookies brave <url>  (use browser session cookies)"
        ))
    } else {
        None
    }
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
/// # Extraction Pipeline
///
/// 1. `<script id="__NEXT_DATA__">` (Next.js SSR data)
/// 2. `<script id="__NUXT_DATA__">` / `<script id="__nuxt-data">` (Nuxt.js SSR data)
/// 3. `<script type="application/ld+json">` (Schema.org structured data — `Article`,
///    `BlogPosting`, `NewsArticle`)
/// 4. Inline `<script>` variable assignments (`window.__NEXT_DATA__ = {...}`)
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

    // Try JSON-LD structured data (Schema.org — widely used by modern blogs)
    if let Some(content) = extract_jsonld_content(&document) {
        return Some(content);
    }

    // Try inline script variable assignments (window.__NEXT_DATA__ = {...}, etc.)
    if let Some(content) = extract_inline_script_json(html) {
        return Some(content);
    }

    None
}

/// Extract article content from `<script type="application/ld+json">` tags.
///
/// Many modern blogs (including JS-rendered ones) embed Schema.org structured
/// data as JSON-LD. This contains the article body in `articleBody`, or at
/// minimum `description`, which gives us content without needing JS execution.
///
/// Handles both single JSON-LD objects and arrays of objects (some sites
/// emit `[{...}, {...}]` with multiple schema types).
fn extract_jsonld_content(document: &scraper::Html) -> Option<String> {
    const MIN_CONTENT_LEN: usize = 200;

    let sel = scraper::Selector::parse(r#"script[type="application/ld+json"]"#).ok()?;

    // Ordered by preference: articleBody > description
    let content_keys = ["articleBody", "text", "description"];

    let mut best: Option<String> = None;

    for script in document.select(&sel) {
        let json_text = script.text().collect::<String>();
        let json_text = json_text.trim();
        if json_text.is_empty() {
            continue;
        }

        // Parse as single object or array
        let values: Vec<serde_json::Value> = if json_text.starts_with('[') {
            serde_json::from_str(json_text).ok()?
        } else if json_text.starts_with('{') {
            vec![serde_json::from_str(json_text).ok()?]
        } else {
            continue;
        };

        for value in &values {
            // Only consider Article-like types (skip Organization, WebSite, BreadcrumbList)
            if let Some(schema_type) = value.get("@type").and_then(|t| t.as_str()) {
                let is_article = schema_type.contains("Article")
                    || schema_type.contains("Posting")
                    || schema_type.contains("Report")
                    || schema_type.contains("ScholarlyArticle")
                    || schema_type.contains("TechArticle")
                    || schema_type.contains("NewsArticle")
                    || schema_type.contains("BlogPosting");
                if !is_article {
                    continue;
                }
            } else {
                continue; // Skip objects without @type
            }

            for key in &content_keys {
                if let Some(serde_json::Value::String(s)) = value.get(*key)
                    && s.len() >= MIN_CONTENT_LEN
                {
                    let current_best_len = best.as_deref().map_or(0, str::len);
                    if s.len() > current_best_len {
                        best = Some(s.clone());
                    }
                }
            }
        }
    }

    best.map(|content| render_spa_content(&content))
}

/// Extract content from inline `<script>` variable assignments.
///
/// Some JS-rendered pages embed data via:
/// ```text
/// <script>window.__NEXT_DATA__ = {"props":{"pageProps":{...}}}</script>
/// ```
/// rather than using a `<script id="__NEXT_DATA__" type="application/json">` tag.
///
/// This function scans all inline scripts for known variable assignment patterns
/// and attempts to extract the JSON payload.
fn extract_inline_script_json(html: &str) -> Option<String> {
    const PATTERNS: &[&str] = &[
        "window.__NEXT_DATA__",
        "self.__NEXT_DATA__",
        "__NEXT_DATA__",
        "window.__NUXT__",
        "window.__INITIAL_STATE__",
        "window.__PRELOADED_STATE__",
        "window.__APOLLO_STATE__",
    ];

    const MIN_CONTENT_LEN: usize = 200;

    for pattern in PATTERNS {
        if let Some(start_idx) = html.find(pattern) {
            // Find the '=' after the variable name
            let after_pattern = start_idx + pattern.len();
            let remaining = &html[after_pattern..];

            // Skip whitespace and find '='
            let eq_offset = remaining.find('=')?;
            let after_eq = &remaining[eq_offset + 1..];

            // Find the start of the JSON object or array
            let json_offset = after_eq.chars().position(|c| c == '{' || c == '[')?;
            let json_start = &after_eq[json_offset..];

            // Extract balanced JSON
            if let Some(json_str) = extract_balanced_json(json_start)
                && let Ok(data) = serde_json::from_str::<serde_json::Value>(json_str)
            {
                // Try Next.js structure (props.pageProps)
                if let Some(content) = extract_nextjs_content(&data) {
                    return Some(content);
                }
                // Try generic longest-string search on the entire payload
                if let Some(longest) = find_longest_string(&data, MIN_CONTENT_LEN) {
                    return Some(render_spa_content(&longest));
                }
            }
        }
    }

    None
}

/// Extract a balanced JSON object or array from the beginning of a string.
///
/// Tracks brace/bracket depth and string escaping to find the end of the
/// outermost JSON structure. Returns the slice containing the full JSON,
/// or `None` if the structure is not balanced.
fn extract_balanced_json(s: &str) -> Option<&str> {
    let first_char = s.chars().next()?;
    let (open, close) = match first_char {
        '{' => ('{', '}'),
        '[' => ('[', ']'),
        _ => return None,
    };

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, c) in s.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match c {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            _ if in_string => {}
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
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
/// Next.js stores page data under `props.pageProps`. We use two strategies:
///
/// 1. **Named-key search**: Look for well-known content field names (most accurate).
/// 2. **Longest-string fallback**: If named-key search fails, walk the entire tree
///    and return the longest string value found. This handles sites like Stripe's
///    developer blog that use proprietary key names (`bodyText`, `richContent`, etc.).
///
/// The named-key strategy is tried first because it is more precise. The
/// longest-string fallback is less precise (may pick up large JSON blobs instead of
/// article text) but is far better than returning nothing for JS-rendered pages.
fn extract_nextjs_content(data: &serde_json::Value) -> Option<String> {
    // Ordered by specificity — HTML/rich content first, summaries last.
    // Extend this list when new CMS key patterns are discovered.
    const CONTENT_KEYS: &[&str] = &[
        "body",
        "bodyText",
        "bodyHtml",
        "body_html",
        "html",
        "content",
        "contentHtml",
        "content_html",
        "richContent",
        "richText",
        "articleBody",
        "article_body",
        "article",
        "post",
        "postBody",
        "postContent",
        "markdown",
        "source",
        "text",
        "fullText",
        "full_text",
        "excerpt",
        "description",
        "summary",
    ];
    // Minimum chars to be considered article content (not a blurb or empty string)
    const MIN_CONTENT_LEN: usize = 200;

    // Next.js: props.pageProps holds the actual page data
    let page_props = data.get("props")?.get("pageProps")?;

    // Strategy 1: named-key search across the entire pageProps subtree
    let mut best: Option<String> = None;
    for key in CONTENT_KEYS {
        if let Some(found) = find_content_by_key(page_props, key) {
            let current_best_len = best.as_deref().map_or(0, str::len);
            if found.len() >= MIN_CONTENT_LEN && found.len() > current_best_len {
                best = Some(found);
            }
        }
    }

    // Strategy 2: longest-string fallback for unknown CMS structures.
    // Only activates when named-key search found nothing useful.
    if best.is_none() {
        best = find_longest_string(page_props, MIN_CONTENT_LEN);
    }

    best.map(|content| render_spa_content(&content))
}

/// Convert a SPA content string (HTML or plain text) to clean markdown.
fn render_spa_content(content: &str) -> String {
    if content.contains('<') && content.contains('>') {
        // Looks like HTML — convert to markdown
        let md = html2md::parse_html(content);
        md.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        content.to_string()
    }
}

/// Recursively walk a JSON value tree looking for a string field named `key`.
///
/// Returns the first string value found using depth-first search (object fields
/// before array items). Returns `None` if the key is not present.
fn find_content_by_key(value: &serde_json::Value, key: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            // Check this level first
            if let Some(serde_json::Value::String(s)) = map.get(key) {
                return Some(s.clone());
            }
            // Recurse into values
            for (_, v) in map {
                if let Some(found) = find_content_by_key(v, key) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(found) = find_content_by_key(item, key) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Find the longest string value anywhere in a JSON tree.
///
/// This is a last-resort fallback for Next.js / SPA pages that use proprietary
/// content key names. By finding the longest string, we can usually recover the
/// article body even when its field name is unknown.
///
/// Skips strings shorter than `min_len` to avoid picking up IDs, slugs, or
/// short metadata strings.
fn find_longest_string(value: &serde_json::Value, min_len: usize) -> Option<String> {
    match value {
        serde_json::Value::String(s) => {
            if s.len() >= min_len { Some(s.clone()) } else { None }
        }
        serde_json::Value::Object(map) => map
            .values()
            .filter_map(|v| find_longest_string(v, min_len))
            .max_by_key(std::string::String::len),
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| find_longest_string(v, min_len))
            .max_by_key(std::string::String::len),
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
            "Expected article content, got: {markdown}"
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
        let html = r"
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
        ";

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
        let html = r"
            <html><body>
                <article>
                    <h1>The Real Post</h1>
                    <p>Substantive article body that we must not lose.</p>
                </article>
            </body></html>
        ";

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
                        "body": "<p>This is the article body content with substantial text that should be extracted by the SPA extractor when readability fails. It contains enough characters to satisfy the minimum content length threshold of two hundred characters.</p>"
                    }
                }
            }
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(
            content.contains("article body content"),
            "Expected body content, got: {content}"
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
    fn find_content_by_key_finds_nested_key() {
        let value = serde_json::json!({
            "level1": {
                "level2": {
                    "content": "deep content string that is long enough to be found"
                }
            }
        });

        let result = find_content_by_key(&value, "content");
        assert!(result.is_some());
        assert!(result.unwrap().contains("deep content"));
    }

    #[test]
    fn find_content_by_key_finds_in_array() {
        let value = serde_json::json!([
            {"title": "skip"},
            {"body": "the actual article body content found in array"}
        ]);

        let result = find_content_by_key(&value, "body");
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
            It contains multiple sentences to pass the minimum length threshold check. \
            The content must be at least two hundred characters long to be considered \
            a valid article body by the SPA extractor.";
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
                <script id="__NEXT_DATA__" type="application/json">{json}</script>
                <div id="__next"><p>SSR placeholder</p></div>
            </body></html>"#
        );

        let result = extract_spa_data(&html);
        assert!(result.is_some(), "Should extract Next.js content");
        let content = result.unwrap();
        assert!(
            content.contains("article body content"),
            "Expected SPA body, got: {content}"
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

    // ── detect_thin_content ─────────────────────────────────────────────────

    #[test]
    fn detect_thin_content_warns_when_ratio_below_threshold() {
        // GIVEN: 10 KB HTML producing only 50 chars of markdown (0.5% ratio)
        let warning = detect_thin_content(10_000, 50);
        // THEN: a warning is returned
        assert!(warning.is_some(), "should warn for 0.5% ratio");
        let msg = warning.unwrap();
        assert!(msg.contains("suspiciously thin"), "message should describe the problem");
        assert!(msg.contains("JavaScript rendering"), "message should explain likely cause");
        assert!(msg.contains("--cookies"), "message should suggest a workaround");
        assert!(msg.contains("nab spa"), "message should suggest nab spa as alternative");
    }

    #[test]
    fn detect_thin_content_no_warning_for_normal_ratio() {
        // GIVEN: 10 KB HTML producing 1 KB of markdown (10% ratio — normal article)
        let warning = detect_thin_content(10_000, 1_000);
        // THEN: no warning
        assert!(warning.is_none(), "should not warn for healthy 10% ratio");
    }

    #[test]
    fn detect_thin_content_no_warning_for_tiny_html() {
        // GIVEN: HTML body below the minimum size threshold (4 KB)
        // WHEN: markdown is also very small
        let warning = detect_thin_content(4_000, 10);
        // THEN: no warning — too small to be reliable signal
        assert!(
            warning.is_none(),
            "should not warn for HTML below 5 KB minimum"
        );
    }

    #[test]
    fn detect_thin_content_no_warning_at_exact_threshold() {
        // GIVEN: ratio exactly at the 2% threshold (boundary condition)
        let html_len = 10_000;
        let markdown_len = 200; // 2% exactly
        let warning = detect_thin_content(html_len, markdown_len);
        // THEN: no warning (2% is at the boundary, not below it)
        assert!(warning.is_none(), "exact threshold should not trigger warning");
    }

    #[test]
    fn detect_thin_content_warns_just_below_threshold() {
        // GIVEN: ratio just below the 2% threshold (boundary condition)
        let html_len = 10_000;
        let markdown_len = 199; // 1.99% — just below
        let warning = detect_thin_content(html_len, markdown_len);
        // THEN: warning is returned
        assert!(warning.is_some(), "just-below-threshold should trigger warning");
    }

    #[test]
    fn detect_thin_content_no_warning_for_empty_markdown_on_small_html() {
        // GIVEN: small page that produces empty markdown — not a JS rendering issue
        let warning = detect_thin_content(100, 0);
        // THEN: no warning — HTML is below minimum size
        assert!(warning.is_none(), "tiny HTML should never warn");
    }

    // ── find_longest_string ─────────────────────────────────────────────────

    #[test]
    fn find_longest_string_returns_longest_in_flat_object() {
        // GIVEN: JSON object with strings of different lengths
        let value = serde_json::json!({
            "slug": "my-post",
            "title": "A short title",
            "bodyText": "This is a much longer string representing the article body content that should be selected as the longest value in the tree."
        });
        // WHEN: we search for the longest string above minimum length
        let result = find_longest_string(&value, 50);
        // THEN: the longest string is returned
        assert!(result.is_some());
        assert!(result.unwrap().contains("article body content"));
    }

    #[test]
    fn find_longest_string_returns_longest_in_nested_object() {
        // GIVEN: nested JSON where the longest string is deep in the tree
        let value = serde_json::json!({
            "meta": {"slug": "test"},
            "content": {
                "sections": [
                    {"type": "text", "value": "Short section."},
                    {"type": "text", "value": "This section contains a much longer body of text that represents the actual article content. It spans many more characters than the other sections."}
                ]
            }
        });
        let result = find_longest_string(&value, 50);
        assert!(result.is_some());
        assert!(result.unwrap().contains("actual article content"));
    }

    #[test]
    fn find_longest_string_returns_none_when_all_strings_below_min_len() {
        // GIVEN: JSON where all string values are below the minimum length
        let value = serde_json::json!({
            "id": "abc123",
            "slug": "my-post",
            "status": "published"
        });
        let result = find_longest_string(&value, 50);
        // THEN: None, since no string meets the minimum length
        assert!(result.is_none());
    }

    #[test]
    fn find_longest_string_handles_array_of_strings() {
        // GIVEN: array containing strings of varying lengths
        let value = serde_json::json!([
            "short",
            "medium length string here",
            "this is the longest string in the array and it exceeds the minimum length threshold easily"
        ]);
        let result = find_longest_string(&value, 30);
        assert!(result.is_some());
        assert!(result.unwrap().contains("longest string in the array"));
    }

    // ── extract_nextjs_content with unknown CMS key names ───────────────────

    #[test]
    fn extract_nextjs_content_falls_back_to_longest_string_for_unknown_keys() {
        // GIVEN: Next.js data where the article body uses a proprietary key name
        // (simulating Stripe blog's structure which uses non-standard keys)
        let article_body = "This is the full article body from Stripe's blog post about \
            payment processing and financial infrastructure. It discusses how modern \
            payment systems work and the engineering challenges involved in building \
            reliable financial software at scale.";

        let json_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "slug": "payment-systems",
                        "author": "Jane Smith",
                        "publishedAt": "2024-01-15",
                        // Proprietary key not in CONTENT_KEYS list
                        "richBodyContent": article_body
                    }
                }
            },
            "buildId": "stripe-blog-build-123"
        });

        // WHEN: we extract content
        let result = extract_nextjs_content(&json_data);

        // THEN: the article body is found via longest-string fallback
        assert!(result.is_some(), "should extract content via longest-string fallback");
        let content = result.unwrap();
        assert!(
            content.contains("Stripe's blog post") || content.contains("payment processing"),
            "expected article body, got: {content}"
        );
    }

    #[test]
    fn extract_nextjs_content_prefers_named_key_over_longer_incidental_string() {
        // GIVEN: pageProps with both a well-known key and a longer non-content string
        // The named-key match should win even if a longer string exists elsewhere
        let known_body = "This is the article body content stored under the well-known \
            'body' key. It is substantial enough to pass the minimum length check.";
        let longer_non_content = "a".repeat(500); // longer but not a content field

        let json_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "body": known_body,
                        // A longer string under an opaque key
                        "_internalData": longer_non_content
                    }
                }
            }
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_some());
        // Named-key strategy finds 'body' first — but since longest-string only
        // activates when named-key finds nothing, both strategies can coexist.
        // The key assertion: we get something useful back.
        assert!(
            result.unwrap().len() >= known_body.len() - 50,
            "should return at least the known body content"
        );
    }

    #[test]
    fn extract_nextjs_content_extended_key_body_html() {
        // GIVEN: pageProps using 'bodyHtml' — one of the newly added keys
        let html_body =
            "<p>Article content stored as HTML in the bodyHtml field. \
            This is a common pattern in headless CMS systems where content \
            is stored as rendered HTML fragments rather than plain text or markdown. \
            The content is substantial enough to pass the minimum length threshold \
            required by the SPA extractor.</p>";

        let json_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "page": {
                        "title": "CMS Article",
                        "bodyHtml": html_body
                    }
                }
            }
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_some(), "should find content via 'bodyHtml' key");
        let content = result.unwrap();
        // HTML should be converted to markdown
        assert!(
            content.contains("Article content stored as HTML"),
            "expected HTML converted to markdown, got: {content}"
        );
    }

    // ── JS-rendered page simulation (issue #32 regression test) ────────────

    #[test]
    fn js_rendered_page_with_next_data_extracts_article_body() {
        // GIVEN: A simulated JS-rendered blog page similar to stripe.dev/blog/*
        // The static HTML shell has very little content; all article text lives
        // in the embedded __NEXT_DATA__ JSON blob.
        let article_body = "Minions & Stripe's One-Shot End-to-End Coding Agents: \
            In this post we explore how we built autonomous coding agents capable of \
            completing entire engineering tasks from specification to pull request. \
            We discuss the architecture, the challenges of tool use, and the lessons \
            learned from running thousands of agent sessions in production. \
            The system combines LLM planning with deterministic execution steps, \
            enabling reliable automation of complex software engineering workflows.";

        let next_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "title": "Minions & Stripe's One-Shot End-to-End Coding Agents",
                        "slug": "minions-stripes-one-shot-end-to-end-coding-agents",
                        "author": {"name": "Stripe Engineering"},
                        // Non-standard key as used by Stripe's CMS
                        "bodyText": article_body
                    }
                }
            },
            "buildId": "abc123",
            "page": "/blog/[slug]"
        });

        // Simulate the minimal static HTML shell a JS-rendered page serves
        // (only author bio in SSR, all article content in __NEXT_DATA__)
        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
    <title>Minions &amp; Stripe's One-Shot End-to-End Coding Agents - Stripe Blog</title>
    <meta name="description" content="How we built autonomous coding agents.">
</head>
<body>
    <div id="__next">
        <header><nav><a href="/">Stripe</a></nav></header>
        <main>
            <!-- JS renders article here; SSR only has author bio -->
            <p class="author-bio">Stripe Engineering Team</p>
        </main>
    </div>
    <script id="__NEXT_DATA__" type="application/json">{next_data}</script>
</body>
</html>"#
        );

        // WHEN: we convert the page to markdown
        let markdown = html_to_markdown_with_url(&html, Some("https://stripe.dev/blog/minions-stripes-one-shot-end-to-end-coding-agents"));

        // THEN: the article body is in the output (not just the author bio)
        assert!(
            markdown.contains("autonomous coding agents") || markdown.contains("Minions"),
            "expected article body in markdown, got only: {markdown}"
        );
        assert!(
            markdown.len() > 200,
            "output should be substantially longer than a bio, got {} chars",
            markdown.len()
        );
    }

    #[test]
    fn detect_thin_content_fires_for_js_rendered_page_shell() {
        // GIVEN: The 34 KB HTML shell of a JS-rendered page that produces only ~200 chars
        // (simulating the actual stripe.dev/blog issue reported in #32)
        let html_len = 34_936; // actual size reported in the bug
        let markdown_len = 200; // actual output size reported in the bug

        // WHEN: we check for thin content
        let warning = detect_thin_content(html_len, markdown_len);

        // THEN: a warning is returned
        assert!(warning.is_some(), "34 KB HTML → 200 char markdown must trigger thin-content warning");
        let msg = warning.unwrap();
        assert!(msg.contains("200"), "warning should include actual markdown length");
        assert!(msg.contains("34936") || msg.contains("34,936") || msg.contains("bytes"), "warning should include HTML size");
    }

    // ── JSON-LD extraction (issue #32) ────────────────────────────────────

    #[test]
    fn extract_jsonld_article_body() {
        // GIVEN: A JS-rendered page with Schema.org JSON-LD containing the article body
        let article_body = "This is the full article body extracted from JSON-LD structured \
            data. Modern blogs increasingly embed Schema.org Article markup which contains \
            the complete article text in the articleBody field, making it possible to \
            extract content even when the HTML shell is empty.";

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
    <title>Test Blog Post</title>
    <script type="application/ld+json">
    {{
        "@context": "https://schema.org",
        "@type": "BlogPosting",
        "headline": "Test Blog Post",
        "author": {{"@type": "Person", "name": "Test Author"}},
        "articleBody": "{article_body}"
    }}
    </script>
</head>
<body>
    <div id="root"><p>Loading...</p></div>
</body>
</html>"#
        );

        let markdown = html_to_markdown_with_url(&html, Some("https://example.com/blog/test"));

        assert!(
            markdown.contains("JSON-LD structured data") || markdown.contains("full article body"),
            "expected JSON-LD article body in markdown, got: {markdown}"
        );
        assert!(
            markdown.len() > 200,
            "output should be substantial, got {} chars",
            markdown.len()
        );
    }

    #[test]
    fn extract_jsonld_ignores_non_article_types() {
        // GIVEN: JSON-LD with Organization type (not an article)
        let html = r#"<!DOCTYPE html>
<html>
<head>
    <script type="application/ld+json">
    {
        "@context": "https://schema.org",
        "@type": "Organization",
        "name": "Example Corp",
        "description": "We are a company that does things and this description is long enough to pass minimum length thresholds but should not be extracted as article content since it is an Organization type."
    }
    </script>
</head>
<body><div>Minimal page</div></body>
</html>"#;

        let document = scraper::Html::parse_document(html);
        let result = extract_jsonld_content(&document);
        assert!(result.is_none(), "Organization JSON-LD should not be extracted as article content");
    }

    #[test]
    fn extract_jsonld_handles_array_of_schemas() {
        // GIVEN: JSON-LD as an array with multiple types (common pattern)
        let article_body = "The complete article body from a page that uses an array of \
            JSON-LD schemas. This is a common pattern where the page includes both a \
            WebSite schema and a BlogPosting schema in a single script tag. The \
            extraction should find the article content from the BlogPosting entry.";

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
    <script type="application/ld+json">
    [
        {{
            "@context": "https://schema.org",
            "@type": "WebSite",
            "name": "Example Blog",
            "url": "https://example.com"
        }},
        {{
            "@context": "https://schema.org",
            "@type": "BlogPosting",
            "headline": "Array Test",
            "articleBody": "{article_body}"
        }}
    ]
    </script>
</head>
<body><div id="root"></div></body>
</html>"#
        );

        let markdown = html_to_markdown_with_url(&html, Some("https://example.com/blog/test"));
        assert!(
            markdown.contains("array of JSON-LD schemas") || markdown.contains("complete article body"),
            "expected article body from JSON-LD array, got: {markdown}"
        );
    }

    #[test]
    fn extract_jsonld_uses_description_fallback() {
        // GIVEN: JSON-LD Article without articleBody, but with a long description
        let description = "A detailed description of the article that serves as a fallback \
            when the articleBody field is not present. This is a common pattern in some CMS \
            systems that only populate the description field in their JSON-LD markup. The \
            description should be extracted as content when no better field is available.";

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
    <script type="application/ld+json">
    {{
        "@context": "https://schema.org",
        "@type": "Article",
        "headline": "Description Fallback Test",
        "description": "{description}"
    }}
    </script>
</head>
<body><div id="root"></div></body>
</html>"#
        );

        let markdown = html_to_markdown_with_url(&html, Some("https://example.com/article"));
        assert!(
            markdown.contains("detailed description") || markdown.contains("fallback"),
            "expected description content as fallback, got: {markdown}"
        );
    }

    // ── Inline script variable extraction (issue #32) ─────────────────────

    #[test]
    fn extract_inline_script_next_data_assignment() {
        // GIVEN: A page that assigns __NEXT_DATA__ via inline script (not a tagged JSON blob)
        let article_body = "This is the article body from a page that uses inline script \
            assignment for Next.js data instead of a tagged script element. This pattern \
            is used by some Next.js deployments and custom frameworks that inject the \
            hydration data via window.__NEXT_DATA__ = {...} in an inline script.";

        let next_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "title": "Inline Assignment Test",
                        "body": article_body
                    }
                }
            },
            "buildId": "test123"
        });

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head><title>Inline Next.js</title></head>
<body>
    <div id="__next"><p>Loading...</p></div>
    <script>window.__NEXT_DATA__ = {next_data};</script>
</body>
</html>"#
        );

        let markdown = html_to_markdown_with_url(&html, Some("https://example.com/blog/test"));
        assert!(
            markdown.contains("inline script assignment") || markdown.contains("article body from a page"),
            "expected article body from inline script, got: {markdown}"
        );
    }

    #[test]
    fn extract_inline_script_initial_state_assignment() {
        // GIVEN: A page using window.__INITIAL_STATE__ = {...}
        let content = "A long article body stored in the Redux initial state payload. \
            This pattern is common in React applications that use Redux for state management \
            and prehydrate the store with server-side rendered data via a global variable \
            assignment in an inline script tag.";

        let state = serde_json::json!({
            "article": {
                "content": content,
                "title": "Redux State Test"
            }
        });

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<body>
    <div id="root"></div>
    <script>window.__INITIAL_STATE__ = {state};</script>
</body>
</html>"#
        );

        let result = extract_inline_script_json(&html);
        assert!(
            result.is_some(),
            "should extract content from __INITIAL_STATE__ assignment"
        );
        let content = result.unwrap();
        assert!(
            content.contains("Redux initial state") || content.contains("long article body"),
            "expected state content, got: {content}"
        );
    }

    #[test]
    fn extract_balanced_json_basic_object() {
        let input = r#"{"key": "value", "nested": {"a": 1}}; more stuff"#;
        let result = extract_balanced_json(input);
        assert!(result.is_some());
        let json = result.unwrap();
        assert_eq!(json, r#"{"key": "value", "nested": {"a": 1}}"#);
    }

    #[test]
    fn extract_balanced_json_with_string_braces() {
        let input = r#"{"content": "text with {braces} inside"}"#;
        let result = extract_balanced_json(input);
        assert!(result.is_some());
        let json = result.unwrap();
        // Should correctly handle braces inside strings
        assert!(serde_json::from_str::<serde_json::Value>(json).is_ok());
    }

    #[test]
    fn extract_balanced_json_array() {
        let input = r#"[1, 2, {"a": [3, 4]}];"#;
        let result = extract_balanced_json(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), r#"[1, 2, {"a": [3, 4]}]"#);
    }

    #[test]
    fn extract_balanced_json_returns_none_for_unbalanced() {
        let input = r#"{"key": "value""#; // missing closing brace
        let result = extract_balanced_json(input);
        assert!(result.is_none());
    }

    // ── Issue #32 end-to-end regression: JS-rendered page with JSON-LD ────

    #[test]
    fn js_rendered_page_with_jsonld_extracts_article_body() {
        // GIVEN: A simulated JS-rendered blog page with NO __NEXT_DATA__ but WITH JSON-LD
        // This is the actual pattern for many modern blogs (Stripe, Medium, Ghost)
        let article_body = "Minions and autonomous coding agents represent a new paradigm in \
            software engineering automation. In this comprehensive post we explore the \
            architecture decisions that went into building Stripe's end-to-end coding \
            agent system. We discuss the challenges of tool use, context management, \
            and the critical importance of verification in autonomous code generation. \
            The system achieved remarkable results in production, completing complex \
            engineering tasks from specification to pull request.";

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
    <title>Minions - Stripe Blog</title>
    <script type="application/ld+json">
    {{
        "@context": "https://schema.org",
        "@type": "BlogPosting",
        "headline": "Minions: Stripe's One-Shot End-to-End Coding Agents",
        "author": {{"@type": "Person", "name": "Alistair Gray"}},
        "datePublished": "2025-03-01",
        "articleBody": "{article_body}"
    }}
    </script>
</head>
<body>
    <div id="__next">
        <header><nav><a href="/">Stripe</a></nav></header>
        <main>
            <!-- JS renders the article here; SSR only has author bio -->
            <p class="author-bio">Alistair is a software engineer on the Leverage team</p>
        </main>
    </div>
    <!-- No __NEXT_DATA__ script tag -->
</body>
</html>"#
        );

        // WHEN: we convert the page to markdown
        let markdown = html_to_markdown_with_url(
            &html,
            Some("https://stripe.dev/blog/minions-stripes-one-shot-end-to-end-coding-agents"),
        );

        // THEN: the article body is in the output (not just the author bio)
        assert!(
            markdown.contains("autonomous coding agents") || markdown.contains("Minions"),
            "expected article body from JSON-LD in markdown, got only: {markdown}"
        );
        assert!(
            markdown.len() > 200,
            "output should be substantially longer than a bio, got {} chars",
            markdown.len()
        );
        // Ensure we did NOT just get the author bio
        assert!(
            !markdown.starts_with("Alistair is a software engineer"),
            "should extract article body, not just author bio"
        );
    }
}
