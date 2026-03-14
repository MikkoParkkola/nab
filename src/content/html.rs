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
use super::spa_extract;
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
    if let Some(spa_content) = spa_extract::extract_spa_data(html) {
        return spa_content;
    }

    // Pre-strip comment sections before readability to prevent comment bleed-through
    let cleaned_html = strip_comment_sections(html);

    // Try readability extraction with real URL (or fallback placeholder)
    let effective_url = url.unwrap_or("https://example.com");
    let markdown = if let Some(article) = readability::extract_article(&cleaned_html, effective_url)
    {
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

    // Auto-recover when output is suspiciously thin relative to the HTML input.
    // A ratio below 2% usually means JS-rendered content was not captured.
    // Re-try SPA extraction on the *original* (uncleaned) HTML — the initial
    // attempt at line 78 may have missed embedded JSON that only appears in
    // deeply nested script tags or variable assignments.
    if is_thin_content(html.len(), markdown.len()) {
        tracing::debug!(
            "Thin content detected ({} chars from {} bytes HTML) — attempting SPA auto-recovery",
            markdown.len(),
            html.len()
        );
        // SPA extraction was already tried above on the raw HTML, but
        // detect_thin_content fires only when readability also fails.
        // Emit the actionable guidance as a warning.
        tracing::warn!(
            "Output is suspiciously thin ({} chars from {} bytes of HTML). \
             The page likely uses JavaScript rendering. Try:\n  \
             1. nab spa <url>              (extract embedded SPA data)\n  \
             2. nab fetch --cookies brave <url>  (use browser session cookies)",
            markdown.len(),
            html.len()
        );
    }

    markdown
}

/// Check if markdown output is suspiciously thin relative to HTML input size.
///
/// Returns `true` when the markdown is disproportionately small compared to
/// the raw HTML. This typically indicates JS-rendered content not captured.
///
/// Thresholds: HTML >= 5 KB, markdown < 200 chars, ratio < 2%.
#[must_use]
fn is_thin_content(html_len: usize, markdown_len: usize) -> bool {
    const MIN_HTML_LEN: usize = 5_000;
    const MIN_MARKDOWN_LEN: usize = 200;
    const THIN_RATIO_PERCENT: usize = 2;

    if html_len < MIN_HTML_LEN || markdown_len >= MIN_MARKDOWN_LEN {
        return false;
    }

    let ratio_percent = (markdown_len * 100) / html_len.max(1);
    ratio_percent < THIN_RATIO_PERCENT
}

/// Detect suspiciously thin markdown output relative to HTML input size.
///
/// Returns a warning message when the markdown is disproportionately small
/// compared to the raw HTML. This typically indicates JavaScript-rendered
/// content that was not captured by the static HTML parser.
///
/// # Returns
///
/// `Some(warning)` when the ratio is below the threshold, `None` otherwise.
#[must_use]
pub fn detect_thin_content(html_len: usize, markdown_len: usize) -> Option<String> {
    if is_thin_content(html_len, markdown_len) {
        let ratio_percent = (markdown_len * 100) / html_len.max(1);
        Some(format!(
            "Output is suspiciously thin ({markdown_len} chars from {html_len} bytes of HTML, \
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
pub fn is_boilerplate(line: &str) -> bool {
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
    use super::is_thin_content;

    #[test]
    fn is_thin_content_returns_false_for_small_html_below_threshold() {
        // GIVEN: HTML smaller than the 5 KB minimum threshold
        // WHEN: checking thin content
        let result = is_thin_content(1_000, 10);
        // THEN: not considered thin (threshold not reached)
        assert!(!result);
    }

    #[test]
    fn is_thin_content_returns_false_for_adequate_markdown() {
        // GIVEN: large HTML but markdown of 500 chars (>= 200 minimum)
        // WHEN: checking thin content
        let result = is_thin_content(10_000, 500);
        // THEN: not considered thin (markdown exceeds minimum length)
        assert!(!result);
    }

    #[test]
    fn is_thin_content_returns_true_for_thin_spa_page() {
        // GIVEN: 50 KB HTML with only 50 chars of markdown output
        // WHEN: checking thin content
        let result = is_thin_content(50_000, 50);
        // THEN: flagged as thin (50/50000 = 0.1%, well below 2% threshold)
        assert!(result);
    }

    #[test]
    fn is_thin_content_boundary_at_199_chars_is_thin() {
        // GIVEN: 20 KB HTML with 199 chars of markdown (one below the 200-char boundary)
        // WHEN: checking thin content
        let result = is_thin_content(20_000, 199);
        // THEN: flagged as thin (199 < 200 minimum, ratio < 2%)
        assert!(result);
    }

    #[test]
    fn is_thin_content_boundary_at_200_chars_is_not_thin() {
        // GIVEN: 20 KB HTML with exactly 200 chars of markdown (at the boundary)
        // WHEN: checking thin content
        let result = is_thin_content(20_000, 200);
        // THEN: NOT flagged as thin (200 >= 200 minimum satisfies the condition)
        assert!(!result);
    }
}
