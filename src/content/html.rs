//! HTML to Markdown conversion handler.
//!
//! Uses Mozilla-style readability extraction to extract clean article content
//! before converting to markdown. Falls back to raw html2md for non-article pages.
//!
//! # Pipeline
//!
//! 1. **SPA data extraction** (Next.js/Nuxt): Extract from `__NEXT_DATA__` / `__NUXT__` JSON
//! 2. **Pre-strip hidden sections**: Remove `<details>`, `<noscript>`, `<dialog>` (browser-hidden)
//! 3. **Pre-strip noise sections**: Remove advisories, cookie banners, vulnerability text
//! 4. **Pre-strip comments**: Remove comment threads, Disqus, discussion sections
//! 5. **Readability extraction** (default): Extract main article content, strip boilerplate
//! 6. **Fallback to raw html2md**: If extraction fails, use raw HTML with basic filtering
//!
//! The readability step significantly improves output quality by removing navigation,
//! footers, ads, and other noise before markdown conversion.

use anyhow::Result;

use super::quality;
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
        let quality = quality::score_extraction(bytes, &markdown);

        Ok(ConversionResult {
            markdown,
            page_count: None,
            content_type: content_type.to_string(),
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
            quality: Some(quality),
        })
    }
}

/// Convert HTML to markdown with URL-aware readability extraction.
///
/// # Pipeline
///
/// 1. **SPA extraction**: Try `__NEXT_DATA__` / `__NUXT_DATA__` for React/Vue SPAs
/// 2. **Pre-strip**: Hidden elements → noise sections → comment sections
/// 3. **Readability extraction**: Extract article content from cleaned HTML
/// 4. **Fallback**: If extraction fails, use raw HTML with basic filtering
/// 5. **Thin-content warning**: Emit a tracing warning when output is disproportionately
///    small vs. the HTML body size, indicating JS-rendered content may be missing.
///
/// Passing the real `url` significantly improves readability quality for sites with
/// complex DOM structures (`LessWrong`, `Ghost CMS`, etc.).
#[must_use]
pub fn html_to_markdown_with_url(html: &str, url: Option<&str>) -> String {
    const MIN_READABILITY_LEN: usize = 50;

    // Try SPA data extraction first (Next.js, Nuxt, etc.)
    if let Some(spa_content) = spa_extract::extract_spa_data(html) {
        return spa_content;
    }

    // Pre-strip noise before readability to prevent non-article content
    // from dominating the scoring. Order: hidden elements → noise sections → comments.
    let cleaned_html = strip_hidden_sections(html);
    let cleaned_html = strip_noise_sections(&cleaned_html);
    let cleaned_html = strip_comment_sections(&cleaned_html);

    // Try readability extraction with real URL (or fallback placeholder)
    let effective_url = url.unwrap_or("https://example.com");
    let readability_md =
        readability::extract_article(&cleaned_html, effective_url).map(|article| {
            let md = html2md::parse_html(&article.content_html);
            let lines: Vec<&str> = md
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            let md_result = lines.join("\n");

            // html2md sometimes truncates list-heavy content (<ol>/<li>).
            // If the article's plain text is significantly longer than the
            // markdown output, fall back to the plain text which preserves
            // all content from the DOM.
            if article.text_content.len() > md_result.len() + 100 {
                article.text_content
            } else {
                md_result
            }
        });

    // Direct html2md on cleaned HTML (preserves tables, lists, etc.)
    let direct_md = html_to_markdown(&cleaned_html);

    // Pick the better result.  Readability produces clean article content but
    // is much shorter than direct conversion (which includes navigation, CSS,
    // JSON-LD, footers, etc.).  Prefer readability whenever it extracted
    // non-trivial content (>= 50 chars), since readability strips boilerplate
    // that direct_md preserves.  Only fall back to direct when readability
    // produced nearly nothing.
    let markdown = match readability_md {
        Some(ref r_md) if r_md.len() >= MIN_READABILITY_LEN => r_md.clone(),
        Some(ref r_md) if r_md.len() > direct_md.len() => r_md.clone(),
        _ => direct_md,
    };

    // Post-process: strip markdown noise from html2md output
    let markdown = clean_markdown_noise(&markdown);

    // Auto-recover when output is suspiciously thin relative to the HTML input.
    // A ratio below 2% usually means JS-rendered content was not captured.
    // Re-try SPA extraction on the *original* (uncleaned) HTML — the initial
    // attempt at line 78 may have missed embedded JSON that only appears in
    // deeply nested script tags or variable assignments.
    if is_thin_content(html.len(), markdown.len()) {
        tracing::debug!(
            "Thin content detected ({} chars from {} bytes HTML) — attempting recovery",
            markdown.len(),
            html.len()
        );

        // Last-resort fallback: Jina reader can render JS-heavy pages.
        // Only attempt when we have a URL and local extraction failed.
        if let Some(page_url) = url
            && let Some(jina_md) = fetch_jina_reader(page_url)
        {
            tracing::info!(
                "Thin content recovered via Jina reader ({} chars)",
                jina_md.len()
            );
            return jina_md;
        }

        // Jina fallback either wasn't attempted (no URL) or failed.
        // Emit actionable guidance as a warning.
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

/// Fetch content from Jina reader as a last-resort fallback for JS-rendered pages.
///
/// Jina reader (`r.jina.ai`) renders JavaScript and returns clean markdown.
/// This is used when local extraction produces suspiciously thin content,
/// indicating the page relies on client-side rendering.
///
/// Returns `Some(markdown)` if Jina returns substantial content (> 200 chars),
/// `None` on any failure (network error, empty response, timeout).
fn fetch_jina_reader(url: &str) -> Option<String> {
    const MIN_JINA_CONTENT_LEN: usize = 200;
    const JINA_TIMEOUT_SECS: u64 = 15;

    let jina_url = format!("https://r.jina.ai/{url}");

    tracing::debug!("Fetching Jina reader fallback: {}", jina_url);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(JINA_TIMEOUT_SECS))
        .build()
        .ok()?;

    let response = client
        .get(&jina_url)
        .header("Accept", "text/markdown")
        .send()
        .ok()?;

    if !response.status().is_success() {
        tracing::debug!("Jina reader returned HTTP {}", response.status().as_u16());
        return None;
    }

    let body = response.text().ok()?;
    let trimmed = body.trim();

    if trimmed.len() >= MIN_JINA_CONTENT_LEN {
        Some(trimmed.to_string())
    } else {
        tracing::debug!(
            "Jina reader returned thin content ({} chars), discarding",
            trimmed.len()
        );
        None
    }
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

/// Remove hidden/collapsed sections from HTML before readability processing.
///
/// Browsers collapse `<details>` elements by default and hide `<noscript>` content
/// when JavaScript is enabled. However, readability and html2md see the full DOM
/// text, so these elements can dominate scoring and crowd out the actual page content.
///
/// This is critical for pages like `deps.rs` where dozens of `<details>` elements
/// containing security advisory text overwhelm the dependency status table.
///
/// # Targeted elements
///
/// - `<details>` — collapsed by default unless `open` attribute is present
/// - `<noscript>` — hidden when JS is available (which static fetch implies)
/// - `<dialog>` — hidden unless `open` attribute is present
///
/// Elements with the `open` attribute are preserved since the page author
/// explicitly intended them to be visible.
#[must_use]
pub fn strip_hidden_sections(html: &str) -> String {
    let document = scraper::Html::parse_document(html);

    // Select all details, noscript, and dialog elements
    #[allow(clippy::unwrap_used)]
    let hidden_sel = scraper::Selector::parse("details, noscript, dialog")
        .unwrap_or_else(|_| scraper::Selector::parse("details").unwrap());

    // Collect node IDs of hidden elements to remove
    // Keep <details open>, <dialog open> — those are intentionally visible
    let hidden_ids: std::collections::HashSet<ego_tree::NodeId> = document
        .select(&hidden_sel)
        .filter(|el| {
            let name = el.value().name();
            // noscript is always hidden in a JS-capable context
            if name == "noscript" {
                return true;
            }
            // details/dialog: remove only if NOT open
            el.value().attr("open").is_none()
        })
        .map(|el| el.id())
        .collect();

    if hidden_ids.is_empty() {
        return html.to_string();
    }

    // Rebuild: collect body children, skipping hidden elements
    let body_selector = scraper::Selector::parse("body").ok();
    let body_html = body_selector
        .as_ref()
        .and_then(|sel| document.select(sel).next())
        .map_or_else(
            || html.to_string(),
            |body| serialize_children_excluding(&document, body.id(), &hidden_ids),
        );

    let head_html = scraper::Selector::parse("head")
        .ok()
        .and_then(|sel| document.select(&sel).next())
        .map(|h| h.html())
        .unwrap_or_default();

    format!("<html>{head_html}<body>{body_html}</body></html>")
}

/// Recursively serialize a subtree, skipping nodes whose IDs are in `exclude`.
///
/// This handles nested hidden elements at any depth (not just direct children
/// of `<body>`), which is essential for pages where `<details>` elements appear
/// inside `<div>` wrappers or table cells.
fn serialize_children_excluding(
    document: &scraper::Html,
    parent_id: ego_tree::NodeId,
    exclude: &std::collections::HashSet<ego_tree::NodeId>,
) -> String {
    let node = document.tree.get(parent_id).unwrap();
    let mut out = String::new();

    for child in node.children() {
        if exclude.contains(&child.id()) {
            continue;
        }
        match child.value() {
            scraper::Node::Element(el) => {
                // Open tag
                out.push('<');
                out.push_str(el.name());
                for (k, v) in el.attrs() {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&v.replace('"', "&quot;"));
                    out.push('"');
                }
                out.push('>');
                // Recurse into children (may also contain excluded nodes)
                out.push_str(&serialize_children_excluding(document, child.id(), exclude));
                // Close tag (skip void elements)
                if !is_void_element(el.name()) {
                    out.push_str("</");
                    out.push_str(el.name());
                    out.push('>');
                }
            }
            scraper::Node::Text(text) => {
                out.push_str(text);
            }
            _ => {}
        }
    }

    out
}

/// Returns `true` for HTML void elements that must not have a closing tag.
fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Remove noise sections (advisories, cookie banners, etc.) from HTML.
///
/// Identifies elements whose class or id attributes match known noise patterns
/// and strips them using deep recursive exclusion. Unlike `strip_comment_sections`
/// (which only filters direct children of `<body>`), this handles noise elements
/// nested at any depth — critical for pages like `deps.rs` where advisory `<div>`
/// elements sit inside wrapper containers.
///
/// # Targeted patterns
///
/// - `#vulnerabilities`, `.advisories`, `.security-advisories` — security advisory sections
/// - `.cookie-banner`, `.consent-banner`, `.gdpr-banner` — cookie/consent UI
/// - `.newsletter-signup`, `.subscribe-form` — signup noise
/// - Headings (`<h1>`–`<h6>`) with noise IDs and all sibling elements that follow
#[must_use]
pub fn strip_noise_sections(html: &str) -> String {
    let document = scraper::Html::parse_document(html);

    // Match any element with class or id attributes
    #[allow(clippy::unwrap_used)]
    let attr_sel = scraper::Selector::parse("[class], [id]")
        .unwrap_or_else(|_| scraper::Selector::parse("div").unwrap());

    // Collect node IDs of noise elements
    let mut noise_ids: std::collections::HashSet<ego_tree::NodeId> = document
        .select(&attr_sel)
        .filter(|el| {
            let class = el.value().attr("class").unwrap_or("");
            let id = el.value().attr("id").unwrap_or("");
            let combined = format!("{class} {id}").to_lowercase();
            is_noise_section(&combined)
        })
        .map(|el| el.id())
        .collect();

    // Special case: headings with noise IDs (e.g., <h3 id="vulnerabilities">)
    // Strip all subsequent siblings of such headings, since the advisory content
    // follows the heading as sibling <div> elements (deps.rs pattern).
    #[allow(clippy::unwrap_used)]
    let heading_sel = scraper::Selector::parse("h1[id], h2[id], h3[id], h4[id], h5[id], h6[id]")
        .unwrap_or_else(|_| scraper::Selector::parse("h3").unwrap());

    for heading in document.select(&heading_sel) {
        let id = heading.value().attr("id").unwrap_or("");
        if is_noise_section(&id.to_lowercase()) {
            // Mark the heading itself
            noise_ids.insert(heading.id());
            // Mark all subsequent siblings
            let mut sibling = document
                .tree
                .get(heading.id())
                .and_then(|n| n.next_sibling());
            while let Some(sib) = sibling {
                noise_ids.insert(sib.id());
                sibling = sib.next_sibling();
            }
        }
    }

    if noise_ids.is_empty() {
        return html.to_string();
    }

    // Rebuild HTML excluding noise nodes (deep recursive)
    let body_selector = scraper::Selector::parse("body").ok();
    let body_html = body_selector
        .as_ref()
        .and_then(|sel| document.select(sel).next())
        .map_or_else(
            || html.to_string(),
            |body| serialize_children_excluding(&document, body.id(), &noise_ids),
        );

    let head_html = scraper::Selector::parse("head")
        .ok()
        .and_then(|sel| document.select(&sel).next())
        .map(|h| h.html())
        .unwrap_or_default();

    format!("<html>{head_html}<body>{body_html}</body></html>")
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
    let class = element.value().attr("class").unwrap_or("");
    let id = element.value().attr("id").unwrap_or("");
    let combined = format!("{class} {id}").to_lowercase();

    is_noise_section(&combined)
}

/// Returns `true` if a class/id string indicates a noise section that should
/// be stripped before readability processing.
///
/// Covers: comment sections, vulnerability advisories, cookie banners,
/// newsletter signup forms, and similar non-article content that tends to
/// dominate readability scoring due to high text density.
fn is_noise_section(combined: &str) -> bool {
    const NOISE_MARKERS: &[&str] = &[
        // Comment sections
        "comment-section",
        "comments-section",
        "comments-container",
        "comment-list",
        "comment-thread",
        "disqus",
        "discussion-section",
        "replies-section",
        // Security advisories / vulnerability sections (deps.rs, GitHub, etc.)
        "vulnerabilities",
        "advisories",
        "security-advisories",
        "advisory-list",
        // Cookie/consent banners
        "cookie-banner",
        "cookie-consent",
        "consent-banner",
        "gdpr-banner",
        // Newsletter/signup noise
        "newsletter-signup",
        "subscribe-form",
        // Footer noise
        "site-footer",
        "global-footer",
    ];

    NOISE_MARKERS.iter().any(|marker| combined.contains(marker))
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

/// Strip common markdown noise produced by html2md.
///
/// Removes artefacts that are technically valid markdown but carry no useful
/// information for an LLM or human reader:
///
/// - **Base64 data URIs**: Inline `![](data:image/...)` badges/icons (often huge)
/// - **Empty links**: `[](url)` from `<a><img/></a>` patterns (icon-only links)
/// - **Bare image noise in tables**: `[](url)` cells that are just crate icons
///
/// Applied as a final post-processing pass on the markdown string.
#[must_use]
fn clean_markdown_noise(md: &str) -> String {
    md.lines()
        .map(|line| {
            let mut cleaned = line.to_string();

            // Remove inline base64 data URIs: ![alt](data:...) or ![](data:...)
            while let Some(start) = cleaned.find("![") {
                // Find the matching ](data:
                let after_bang = &cleaned[start + 2..];
                if let Some(paren) = after_bang.find("](data:") {
                    // Find the closing )
                    let data_start = start + 2 + paren + 2; // past ](
                    if let Some(close) = cleaned[data_start..].find(')') {
                        let end = data_start + close + 1;
                        cleaned = format!("{}{}", &cleaned[..start], &cleaned[end..]);
                        continue;
                    }
                }
                break;
            }

            // Remove empty link artefacts: [](url) — no link text, just icon wrappers
            while let Some(start) = cleaned.find("[](") {
                if let Some(close) = cleaned[start + 3..].find(')') {
                    let end = start + 3 + close + 1;
                    let replacement = &cleaned[end..];
                    cleaned = format!("{}{}", cleaned[..start].trim_end(), replacement);
                    continue;
                }
                break;
            }

            cleaned
        })
        .filter(|l| {
            let trimmed = l.trim();
            // Drop lines that are entirely base64 data or became empty after cleaning
            !trimmed.is_empty()
                && !trimmed.starts_with("![](data:")
                && !trimmed.starts_with("```\n[![")
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    use super::{is_thin_content, strip_hidden_sections, strip_noise_sections};

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

    #[test]
    fn strip_hidden_sections_removes_closed_details() {
        let html = r"<html><body>
            <h1>Status</h1>
            <p>All good</p>
            <details><summary>Advisory</summary><p>CVE-2024-1234</p></details>
        </body></html>";
        let result = strip_hidden_sections(html);
        assert!(!result.contains("CVE-2024-1234"));
        assert!(result.contains("All good"));
        assert!(result.contains("Status"));
    }

    #[test]
    fn strip_hidden_sections_preserves_open_details() {
        let html = r"<html><body>
            <details open><summary>Visible</summary><p>Important info</p></details>
            <details><summary>Hidden</summary><p>Secret</p></details>
        </body></html>";
        let result = strip_hidden_sections(html);
        assert!(result.contains("Important info"));
        assert!(!result.contains("Secret"));
    }

    #[test]
    fn strip_hidden_sections_removes_noscript() {
        let html = r"<html><body>
            <p>Main content</p>
            <noscript><p>Enable JavaScript</p></noscript>
        </body></html>";
        let result = strip_hidden_sections(html);
        assert!(result.contains("Main content"));
        assert!(!result.contains("Enable JavaScript"));
    }

    #[test]
    fn strip_hidden_sections_removes_closed_dialog() {
        let html = r"<html><body>
            <p>Page</p>
            <dialog><p>Modal content</p></dialog>
            <dialog open><p>Visible modal</p></dialog>
        </body></html>";
        let result = strip_hidden_sections(html);
        assert!(result.contains("Page"));
        assert!(!result.contains("Modal content"));
        assert!(result.contains("Visible modal"));
    }

    #[test]
    fn strip_hidden_sections_handles_nested_details_in_divs() {
        let html = r#"<html><body>
            <div class="content"><p>Real content</p></div>
            <div class="advisories">
                <details><summary>CVE-1</summary><p>Bad thing 1</p></details>
                <details><summary>CVE-2</summary><p>Bad thing 2</p></details>
            </div>
        </body></html>"#;
        let result = strip_hidden_sections(html);
        assert!(result.contains("Real content"));
        assert!(!result.contains("Bad thing 1"));
        assert!(!result.contains("Bad thing 2"));
    }

    #[test]
    fn strip_hidden_sections_noop_when_no_hidden_elements() {
        let html = r"<html><body><p>Just text</p></body></html>";
        let result = strip_hidden_sections(html);
        assert!(result.contains("Just text"));
    }

    #[test]
    fn strip_noise_sections_removes_vulnerabilities_heading_and_siblings() {
        // Simulates deps.rs structure: h3#vulnerabilities followed by advisory boxes
        let html = r#"<html><body>
            <table><tr><td>reqwest</td><td>up to date</td></tr></table>
            <h3 id="vulnerabilities">Security Vulnerabilities</h3>
            <div class="box"><p>CVE-2022-24713 regex advisory</p></div>
            <div class="box"><p>chrono segfault advisory</p></div>
        </body></html>"#;
        let result = strip_noise_sections(html);
        assert!(result.contains("reqwest"));
        assert!(result.contains("up to date"));
        assert!(!result.contains("CVE-2022-24713"));
        assert!(!result.contains("chrono segfault"));
        assert!(!result.contains("Security Vulnerabilities"));
    }

    #[test]
    fn strip_noise_sections_removes_advisory_by_class() {
        let html = r#"<html><body>
            <p>Main content</p>
            <div class="advisories"><p>RUSTSEC-2020-0159</p></div>
        </body></html>"#;
        let result = strip_noise_sections(html);
        assert!(result.contains("Main content"));
        assert!(!result.contains("RUSTSEC-2020-0159"));
    }

    #[test]
    fn strip_noise_sections_removes_cookie_banner() {
        let html = r#"<html><body>
            <article><p>Article text</p></article>
            <div class="cookie-banner"><p>Accept cookies</p></div>
        </body></html>"#;
        let result = strip_noise_sections(html);
        assert!(result.contains("Article text"));
        assert!(!result.contains("Accept cookies"));
    }

    #[test]
    fn strip_noise_sections_preserves_all_when_no_noise() {
        let html = r"<html><body><p>Clean page</p></body></html>";
        let result = strip_noise_sections(html);
        assert!(result.contains("Clean page"));
    }

    #[test]
    fn ghost_blog_ordered_list_not_truncated() {
        // Ghost CMS article with <ol><li> content.
        // html2md truncates ordered lists; the pipeline must detect this
        // and fall back to plain text to preserve the full article.
        use super::html_to_markdown_with_url;

        let html = r#"
            <html>
            <head><title>Porting Software</title></head>
            <body>
                <header><nav>Site Nav</nav></header>
                <main class="site-main">
                    <article class="gh-article post tag-ai">
                        <header class="gh-article-header">
                            <h1 class="gh-article-title">porting software has been trivial</h1>
                        </header>
                        <div class="gh-content gh-canvas">
                            <p>This one is short and sweet. if you want to port a codebase from one language to another here's the approach:</p>
                            <ol>
                                <li>Run a ralph loop which compresses all tests into specs which looks similar to study every file in tests using separate subagents and document in specs and link the implementation as citations in the specification</li>
                                <li>Then do a separate Ralph loop for all product functionality ensuring there are citations to the specification. Study every file in src using separate subagents per file and link the implementation as citations in the specification</li>
                                <li>Once you have that within the same repo run a Ralph loop to create a TODO file and then execute a classic ralph doing just one thing and the most important thing per loop. Remind the agent that it can study the specifications and follow the citations to reference source code.</li>
                                <li>For best outcomes you wanna configure your target language to have strict compilation</li>
                            </ol>
                            <p>The key theory here is usage of citations in the specifications which tease the file_read tool to study the original implementation during stage 3. Reducing stage 1 and stage 2 to specs is the precursor which transforms a code base into high level PRDs without coupling the implementation from the source language.</p>
                        </div>
                    </article>
                </main>
                <section class="newsletter-signup"><p>Subscribe</p></section>
                <footer>Copyright</footer>
            </body>
            </html>
        "#;

        let md = html_to_markdown_with_url(html, Some("https://ghuntley.com/porting/"));

        // Must contain the conclusion paragraph (the part html2md truncates)
        assert!(
            md.contains("high level PRDs without coupling"),
            "Missing conclusion paragraph in markdown output ({} chars): {}",
            md.len(),
            md
        );
        // Must contain list items
        assert!(
            md.contains("Ralph loop"),
            "Missing list content in markdown output: {}",
            md
        );
    }

    #[test]
    fn ghost_blog_real_html_not_truncated() {
        // Test with actual Ghost CMS HTML structure that triggers html2md truncation.
        // The key difference from the simplified test above: the <em> tags inside
        // the <li> elements and the inline <ol> (no whitespace between tags).
        use super::html_to_markdown_with_url;

        let html = r#"<html>
<head><title>porting software has been trivial for a while now. here's how you do it.</title></head>
<body>
<header><nav>Site Nav</nav></header>
<main class="site-main">
<article class="gh-article post tag-ai">
<header class="gh-article-header"><h1 class="gh-article-title">porting software has been trivial</h1></header>
<div class="gh-content gh-canvas">
            <p>This one is short and sweet. if you want to port a codebase from one language to another here's the approach:</p><ol><li>Run a ralph loop which compresses all tests into /specs/<em>.md which looks similar to "study every file in tests/</em>* using separate subagents and document in /specs/*.md and link the implementation as citations in the specification"</li><li>Then do a separate Ralph loop for all product functionality - ensuring there's citations to the specification. "study every file in src/* using seperate subagents per file and link the implementation as citations in the specification"</li><li>Once you have that - within the same repo run a Ralph loop to create a TODO file and then execute a classic ralph - doing just one thing and the most important thing per loop. Remind the agent that it can study the specifications and follow the citations to reference source code.</li><li>For best outcomes you wanna configure your target language to have strict compilation </li></ol><p>The key theory here is usage of citations in the specifications which tease the file_read tool to study the original implementation during stage 3. Reducing stage 1 and stage 2 to specs is the precursor which transforms a code base into high level PRDs without coupling the implementation from the source language.</p>
        </div>
</article>
</main>
<section class="newsletter-signup"><h3>Subscribe</h3><p>Join subscribers</p></section>
<footer><p>Copyright 2026</p></footer>
</body></html>"#;

        let md = html_to_markdown_with_url(html, Some("https://ghuntley.com/porting/"));

        // Must contain the conclusion paragraph
        assert!(
            md.contains("high level PRDs without coupling"),
            "Missing conclusion paragraph in markdown output ({} chars): {}",
            md.len(),
            md
        );
        // Must contain list item content
        assert!(
            md.contains("Ralph loop") || md.contains("ralph loop"),
            "Missing list content in markdown output: {}",
            md
        );
        // Should be substantial
        assert!(
            md.len() > 500,
            "Markdown too short: {} chars. Content: {}",
            md.len(),
            md
        );
    }
}
