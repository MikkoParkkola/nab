//! CSS-selector-based site content extractor.
//!
//! Implements [`SiteProvider`] using configurable CSS selectors so users can
//! extend nab with custom extractors without writing Rust code.  The extractor
//! is fed **already-fetched** HTML via the `prefetched_html` parameter; it
//! never issues a second HTTP request for the same URL.
//!
//! Content extracted by the CSS selector is passed through the full
//! [`ContentRouter`] pipeline (readability, SPA extraction, etc.) rather than
//! raw `html2md`, which ensures consistent, high-quality markdown output.
//!
//! # Configuration
//!
//! ```toml
//! [[plugins]]
//! name    = "internal-wiki"
//! type    = "css"
//! patterns = ["wiki\\.internal\\.corp/.*"]
//!
//! [plugins.content]
//! selector = "div.wiki-content"
//! remove   = ["div.sidebar", "nav.breadcrumbs"]
//!
//! [plugins.metadata]
//! title     = "h1.page-title"
//! author    = ".author-name"
//! published = "time.published"
//! ```
//!
//! # Example
//!
//! ```rust
//! use nab::site::css_extractor::{CssExtractorConfig, CssExtractorProvider};
//! use nab::site::SiteProvider;
//!
//! let config = CssExtractorConfig {
//!     name:             "blog".to_string(),
//!     url_pattern:      r"myblog\.com".to_string(),
//!     content_selector: "article.post".to_string(),
//!     title_selector:   Some("h1.title".to_string()),
//!     author_selector:  None,
//!     date_selector:    None,
//!     remove_selectors: vec![".sidebar".to_string()],
//! };
//!
//! let provider = CssExtractorProvider::new(config).unwrap();
//! assert!(provider.matches("https://myblog.com/post/123"));
//! assert!(!provider.matches("https://other.com/post/123"));
//! ```

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};

use super::{SiteContent, SiteMetadata, SiteProvider};
use crate::content::ContentRouter;
use crate::http_client::AcceleratedClient;

// ─────────────────────────────────────────────────────────────────────────────
// Public config type
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for a CSS-selector-based site extractor.
///
/// Instances are typically loaded from `~/.config/nab/plugins.toml` via
/// [`crate::plugin::config::load_plugins`] but can also be constructed
/// programmatically.
#[derive(Debug, Clone)]
pub struct CssExtractorConfig {
    /// Human-readable name (used in logging and `SiteMetadata::platform`).
    pub name: String,
    /// Regex pattern matched against the full URL.
    pub url_pattern: String,
    /// CSS selector for the main content container.
    pub content_selector: String,
    /// Optional CSS selector for the page title element.
    pub title_selector: Option<String>,
    /// Optional CSS selector for the author element.
    pub author_selector: Option<String>,
    /// Optional CSS selector for the publication-date element.
    pub date_selector: Option<String>,
    /// CSS selectors for elements to *remove* before extraction (ads, nav, etc.).
    pub remove_selectors: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider
// ─────────────────────────────────────────────────────────────────────────────

/// [`SiteProvider`] that extracts content using CSS selectors.
///
/// Created via [`CssExtractorProvider::new`].  The compiled regex and
/// `Selector` objects are stored so matching and extraction are both cheap at
/// call time.
pub struct CssExtractorProvider {
    config: CssExtractorConfig,
    url_regex: Regex,
    content_sel: Selector,
    title_sel: Option<Selector>,
    author_sel: Option<Selector>,
    date_sel: Option<Selector>,
    remove_sels: Vec<Selector>,
    /// Leaked name for the `&'static str` required by `SiteProvider::name`.
    static_name: &'static str,
}

impl CssExtractorProvider {
    /// Build a provider from a [`CssExtractorConfig`].
    ///
    /// Compiles the URL regex and all CSS selectors eagerly so that any
    /// invalid patterns are surfaced at startup rather than per-request.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `url_pattern` is not a valid regex
    /// - `content_selector` is not a valid CSS selector
    /// - Any selector in `remove_selectors` is not a valid CSS selector
    pub fn new(config: CssExtractorConfig) -> Result<Self> {
        let url_regex = Regex::new(&config.url_pattern).with_context(|| {
            format!(
                "invalid URL pattern '{}' in CSS extractor '{}'",
                config.url_pattern, config.name
            )
        })?;

        let content_sel = compile_selector(&config.content_selector, &config.name, "content")?;

        let title_sel = config
            .title_selector
            .as_deref()
            .map(|s| compile_selector(s, &config.name, "title"))
            .transpose()?;

        let author_sel = config
            .author_selector
            .as_deref()
            .map(|s| compile_selector(s, &config.name, "author"))
            .transpose()?;

        let date_sel = config
            .date_selector
            .as_deref()
            .map(|s| compile_selector(s, &config.name, "published"))
            .transpose()?;

        let remove_sels = config
            .remove_selectors
            .iter()
            .enumerate()
            .map(|(i, s)| compile_selector(s, &config.name, &format!("remove[{i}]")))
            .collect::<Result<Vec<_>>>()?;

        // Leak the name once: providers are created at startup and live for
        // the process lifetime, so the allocation is effectively static.
        let static_name: &'static str = Box::leak(config.name.clone().into_boxed_str());

        Ok(Self {
            config,
            url_regex,
            content_sel,
            title_sel,
            author_sel,
            date_sel,
            remove_sels,
            static_name,
        })
    }
}

#[async_trait]
impl SiteProvider for CssExtractorProvider {
    fn name(&self) -> &'static str {
        self.static_name
    }

    fn matches(&self, url: &str) -> bool {
        self.url_regex.is_match(url)
    }

    /// Extract content from `prefetched_html`.
    ///
    /// When `prefetched_html` is `None` the provider cannot proceed without
    /// re-fetching the URL, which CSS extractors intentionally avoid.
    /// In that case an error is returned — callers must provide HTML bytes.
    async fn extract(
        &self,
        url: &str,
        _client: &AcceleratedClient,
        _cookies: Option<&str>,
        prefetched_html: Option<&[u8]>,
    ) -> Result<SiteContent> {
        let html_bytes = prefetched_html.with_context(|| {
            format!(
                "CSS extractor '{}' requires pre-fetched HTML but none was provided for {url}",
                self.config.name
            )
        })?;

        let html_str = std::str::from_utf8(html_bytes)
            .with_context(|| format!("HTML body for {url} is not valid UTF-8"))?;

        let document = Html::parse_document(html_str);

        let title = extract_text_opt(&document, self.title_sel.as_ref());
        let author = extract_text_opt(&document, self.author_sel.as_ref());
        let published = extract_text_opt(&document, self.date_sel.as_ref());

        let content_html = build_content_html(&document, &self.content_sel, &self.remove_sels, url);

        if content_html.is_empty() {
            // AC: selector matched zero elements — return empty content, not an error.
            return Ok(SiteContent {
                markdown: String::new(),
                metadata: build_metadata(&self.config.name, url, title, author, published),
            });
        }

        let router = ContentRouter::new();
        let result = router
            .convert_with_url(content_html.as_bytes(), "text/html", Some(url))
            .with_context(|| {
                format!(
                    "ContentRouter failed for CSS extractor '{}' on {url}",
                    self.config.name
                )
            })?;

        Ok(SiteContent {
            markdown: result.markdown,
            metadata: build_metadata(&self.config.name, url, title, author, published),
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Private helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Compile a CSS selector, producing an `anyhow::Error` with context that
/// identifies which field and extractor produced the bad selector.
fn compile_selector(selector: &str, extractor_name: &str, field: &str) -> Result<Selector> {
    Selector::parse(selector).map_err(|e| {
        anyhow::anyhow!(
            "invalid CSS selector '{selector}' for field '{field}' \
             in extractor '{extractor_name}': {e}"
        )
    })
}

/// Extract the trimmed text content of the first element matching `selector`.
/// Returns `None` if the selector is absent or no element matches.
fn extract_text_opt(document: &Html, selector: Option<&Selector>) -> Option<String> {
    let sel = selector?;
    let el = document.select(sel).next()?;
    let text: String = el.text().collect::<Vec<_>>().join(" ");
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Serialise a single `ElementRef` to its outer HTML string, skipping any
/// sub-elements (and their descendants) that match `remove_sels`.
///
/// Because `scraper` does not support in-place DOM mutation, we walk the
/// element tree manually and emit HTML for each node that should be kept.
fn serialise_element_filtered(element: ElementRef<'_>, remove_sels: &[Selector]) -> String {
    if remove_sels.is_empty() {
        return element.html();
    }

    let mut out = String::new();
    serialise_node_recursive(element, remove_sels, &mut out);
    out
}

/// Recursive DFS serialisation that skips nodes under removed selectors.
fn serialise_node_recursive(element: ElementRef<'_>, remove_sels: &[Selector], out: &mut String) {
    // Skip if this element itself is removed.
    if remove_sels.iter().any(|s| s.matches(&element)) {
        return;
    }

    // Opening tag
    out.push('<');
    out.push_str(element.value().name());
    for (attr, val) in element.value().attrs() {
        use std::fmt::Write as _;
        let _ = write!(out, " {attr}=\"{val}\"");
    }
    out.push('>');

    // Children (elements + text nodes)
    for child in element.children() {
        match child.value() {
            scraper::node::Node::Text(t) => out.push_str(t),
            scraper::node::Node::Element(_) => {
                if let Some(child_el) = ElementRef::wrap(child) {
                    serialise_node_recursive(child_el, remove_sels, out);
                }
            }
            _ => {}
        }
    }

    // Closing tag
    out.push_str("</");
    out.push_str(element.value().name());
    out.push('>');
}

/// Build an HTML string from all elements matched by `content_sel` in
/// `document`, with elements matching `remove_sels` stripped out.
///
/// Wraps the fragments in a minimal `<html><body>…</body></html>` so
/// `ContentRouter` sees a valid document rather than a bare fragment.
fn build_content_html(
    document: &Html,
    content_sel: &Selector,
    remove_sels: &[Selector],
    url: &str,
) -> String {
    let mut fragments: Vec<String> = document
        .select(content_sel)
        .map(|el| serialise_element_filtered(el, remove_sels))
        .collect();

    // Drop fragments that became empty after removal.
    fragments.retain(|f| !f.trim().is_empty());

    if fragments.is_empty() {
        return String::new();
    }

    // Wrap in a minimal document so the content router can parse it properly.
    format!(
        "<!DOCTYPE html><html><head></head><body>\
         <!-- extracted by nab CSS extractor from {url} -->\
         {}\
         </body></html>",
        fragments.join("\n")
    )
}

/// Build [`SiteMetadata`] from extracted values.
fn build_metadata(
    name: &str,
    url: &str,
    title: Option<String>,
    author: Option<String>,
    published: Option<String>,
) -> SiteMetadata {
    SiteMetadata {
        author,
        title,
        published,
        platform: format!("css:{name}"),
        canonical_url: url.to_string(),
        media_urls: Vec::new(),
        engagement: None,
    }
}

/// Validate that a CSS extractor config is usable at construction time.
/// Called during plugin loading so bad configs are skipped with a warning.
pub fn validate_config(config: &CssExtractorConfig) -> Result<()> {
    if config.name.is_empty() {
        bail!("CSS extractor name must not be empty");
    }
    if config.content_selector.is_empty() {
        bail!("CSS extractor '{}' has no content.selector", config.name);
    }
    Selector::parse(&config.content_selector).map_err(|e| {
        anyhow::anyhow!(
            "CSS extractor '{}' has invalid content.selector '{}': {e}",
            config.name,
            config.content_selector
        )
    })?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;

    static H1_SELECTOR: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("h1").expect("static h1 selector"));
    static H2_SELECTOR: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("h2").expect("static h2 selector"));
    static P_SELECTOR: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("p").expect("static p selector"));
    static ARTICLE_SELECTOR: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("article").expect("static article selector"));
    static NAV_SELECTOR: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("nav").expect("static nav selector"));
    static ASIDE_SELECTOR: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("aside").expect("static aside selector"));
    static MAIN_SELECTOR: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("main").expect("static main selector"));

    // ── helpers ──────────────────────────────────────────────────────────────

    fn simple_config() -> CssExtractorConfig {
        CssExtractorConfig {
            name: "test-blog".to_string(),
            url_pattern: r"myblog\.com".to_string(),
            content_selector: "article".to_string(),
            title_selector: Some("h1.title".to_string()),
            author_selector: Some(".author".to_string()),
            date_selector: Some("time".to_string()),
            remove_selectors: vec![".sidebar".to_string(), "nav".to_string()],
        }
    }

    fn make_provider(config: CssExtractorConfig) -> CssExtractorProvider {
        CssExtractorProvider::new(config).expect("valid config should build provider")
    }

    // ── URL matching ──────────────────────────────────────────────────────────

    #[test]
    fn matches_url_that_satisfies_pattern() {
        let p = make_provider(simple_config());
        assert!(p.matches("https://myblog.com/post/hello-world"));
    }

    #[test]
    fn does_not_match_url_outside_pattern() {
        let p = make_provider(simple_config());
        assert!(!p.matches("https://other.com/post/hello-world"));
    }

    #[test]
    fn matches_case_sensitive_by_default() {
        // regex crate is case-sensitive unless (?i) is used in pattern.
        let p = make_provider(simple_config());
        assert!(!p.matches("https://MYBLOG.COM/post"));
    }

    #[test]
    fn case_insensitive_flag_in_pattern_works() {
        let config = CssExtractorConfig {
            url_pattern: r"(?i)myblog\.com".to_string(),
            ..simple_config()
        };
        let p = make_provider(config);
        assert!(p.matches("https://MYBLOG.COM/post"));
        assert!(p.matches("https://myblog.com/post"));
    }

    #[test]
    fn provider_name_matches_config_name() {
        let p = make_provider(simple_config());
        assert_eq!(p.name(), "test-blog");
    }

    // ── constructor validation ────────────────────────────────────────────────

    #[test]
    fn rejects_invalid_url_pattern() {
        let config = CssExtractorConfig {
            url_pattern: r"[invalid".to_string(),
            ..simple_config()
        };
        assert!(CssExtractorProvider::new(config).is_err());
    }

    #[test]
    fn rejects_invalid_content_selector() {
        let config = CssExtractorConfig {
            content_selector: "::invalid-pseudo".to_string(),
            ..simple_config()
        };
        assert!(CssExtractorProvider::new(config).is_err());
    }

    #[test]
    fn rejects_invalid_remove_selector() {
        let config = CssExtractorConfig {
            remove_selectors: vec!["::bad".to_string()],
            ..simple_config()
        };
        assert!(CssExtractorProvider::new(config).is_err());
    }

    #[test]
    fn rejects_invalid_title_selector() {
        let config = CssExtractorConfig {
            title_selector: Some("::bad".to_string()),
            ..simple_config()
        };
        assert!(CssExtractorProvider::new(config).is_err());
    }

    #[test]
    fn accepts_config_with_no_optional_selectors() {
        let config = CssExtractorConfig {
            title_selector: None,
            author_selector: None,
            date_selector: None,
            remove_selectors: vec![],
            ..simple_config()
        };
        assert!(CssExtractorProvider::new(config).is_ok());
    }

    // ── extract_text_opt ──────────────────────────────────────────────────────

    #[test]
    fn extract_text_returns_none_when_selector_absent() {
        let doc = Html::parse_document("<html><body><p>Hello</p></body></html>");
        assert!(extract_text_opt(&doc, None).is_none());
    }

    #[test]
    fn extract_text_returns_text_of_first_match() {
        let doc = Html::parse_document("<html><body><h1>  Title Here  </h1></body></html>");
        assert_eq!(
            extract_text_opt(&doc, Some(&H1_SELECTOR)),
            Some("Title Here".to_string())
        );
    }

    #[test]
    fn extract_text_returns_none_when_no_element_matches() {
        let doc = Html::parse_document("<html><body><h1>Only H1</h1></body></html>");
        assert!(extract_text_opt(&doc, Some(&H2_SELECTOR)).is_none());
    }

    #[test]
    fn extract_text_joins_inner_text_nodes() {
        let doc = Html::parse_document("<p>Hello <strong>world</strong></p>");
        let text = extract_text_opt(&doc, Some(&P_SELECTOR)).unwrap();
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
    }

    // ── serialise_element_filtered ────────────────────────────────────────────

    #[test]
    fn serialise_keeps_all_content_when_no_remove_sels() {
        let html = "<html><body><article><p>Keep</p><nav>Nav</nav></article></body></html>";
        let doc = Html::parse_document(html);
        let el = doc.select(&ARTICLE_SELECTOR).next().unwrap();
        let out = serialise_element_filtered(el, &[]);
        assert!(out.contains("Keep"));
        assert!(out.contains("Nav"));
    }

    #[test]
    fn serialise_strips_removed_element() {
        let html = "<html><body><article><p>Keep</p><nav>Remove</nav></article></body></html>";
        let doc = Html::parse_document(html);
        let el = doc.select(&ARTICLE_SELECTOR).next().unwrap();
        let out = serialise_element_filtered(el, std::slice::from_ref(&*NAV_SELECTOR));
        assert!(out.contains("Keep"));
        assert!(!out.contains("Remove"));
    }

    #[test]
    fn serialise_strips_nested_children_of_removed() {
        let html = "<html><body><article>\
            <aside><a>Link1</a><a>Link2</a></aside>\
            <p>Body</p>\
            </article></body></html>";
        let doc = Html::parse_document(html);
        let el = doc.select(&ARTICLE_SELECTOR).next().unwrap();
        let out = serialise_element_filtered(el, std::slice::from_ref(&*ASIDE_SELECTOR));
        assert!(out.contains("Body"));
        assert!(!out.contains("Link1"));
        assert!(!out.contains("Link2"));
    }

    // ── build_content_html ────────────────────────────────────────────────────

    #[test]
    fn content_html_returns_empty_when_selector_misses() {
        let html = "<html><body><p>No article here</p></body></html>";
        let doc = Html::parse_document(html);
        let result = build_content_html(&doc, &ARTICLE_SELECTOR, &[], "https://example.com");
        assert!(result.is_empty());
    }

    #[test]
    fn content_html_captures_matched_element() {
        let html = "<html><body><article><p>Article text</p></article></body></html>";
        let doc = Html::parse_document(html);
        let result = build_content_html(&doc, &ARTICLE_SELECTOR, &[], "https://example.com");
        assert!(result.contains("Article text"));
        // Should be wrapped in a valid HTML document
        assert!(result.contains("<html>"));
        assert!(result.contains("<body>"));
    }

    #[test]
    fn content_html_joins_multiple_matches() {
        let html = "<html><body>\
            <article><p>First</p></article>\
            <article><p>Second</p></article>\
            </body></html>";
        let doc = Html::parse_document(html);
        let result = build_content_html(&doc, &ARTICLE_SELECTOR, &[], "https://example.com");
        assert!(result.contains("First"));
        assert!(result.contains("Second"));
    }

    #[test]
    fn content_html_wraps_in_document_for_content_router() {
        let html = "<html><body><main><h1>Hello</h1><p>World</p></main></body></html>";
        let doc = Html::parse_document(html);
        let result = build_content_html(&doc, &MAIN_SELECTOR, &[], "https://example.com");
        assert!(result.starts_with("<!DOCTYPE html>"));
    }

    // ── validate_config ───────────────────────────────────────────────────────

    #[test]
    fn validate_config_rejects_empty_name() {
        let config = CssExtractorConfig {
            name: String::new(),
            ..simple_config()
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_config_rejects_empty_selector() {
        let config = CssExtractorConfig {
            content_selector: String::new(),
            ..simple_config()
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_config_rejects_invalid_selector() {
        let config = CssExtractorConfig {
            content_selector: "::bad-selector".to_string(),
            ..simple_config()
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_config_accepts_valid_config() {
        assert!(validate_config(&simple_config()).is_ok());
    }

    // ── metadata building ─────────────────────────────────────────────────────

    #[test]
    fn metadata_platform_includes_extractor_name() {
        let meta = build_metadata("my-blog", "https://example.com", None, None, None);
        assert_eq!(meta.platform, "css:my-blog");
    }

    #[test]
    fn metadata_canonical_url_is_set() {
        let meta = build_metadata("x", "https://example.com/page", None, None, None);
        assert_eq!(meta.canonical_url, "https://example.com/page");
    }

    #[test]
    fn metadata_optional_fields_propagated() {
        let meta = build_metadata(
            "x",
            "https://example.com",
            Some("My Title".to_string()),
            Some("Alice".to_string()),
            Some("2026-01-01".to_string()),
        );
        assert_eq!(meta.title.as_deref(), Some("My Title"));
        assert_eq!(meta.author.as_deref(), Some("Alice"));
        assert_eq!(meta.published.as_deref(), Some("2026-01-01"));
    }

    // ── extract (async) ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn extract_returns_error_when_no_prefetched_html() {
        let client = AcceleratedClient::new().expect("client");
        let provider = make_provider(simple_config());
        let result = provider
            .extract("https://myblog.com/post/1", &client, None, None)
            .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("pre-fetched HTML"));
    }

    #[tokio::test]
    async fn extract_returns_empty_markdown_when_selector_misses() {
        let client = AcceleratedClient::new().expect("client");
        let html = b"<html><body><p>No article here</p></body></html>";
        let provider = make_provider(simple_config());
        let result = provider
            .extract("https://myblog.com/post/1", &client, None, Some(html))
            .await
            .expect("should succeed even with no match");
        assert!(result.markdown.is_empty());
    }

    #[tokio::test]
    async fn extract_converts_article_html_to_markdown() {
        let client = AcceleratedClient::new().expect("client");
        let html = b"<html><body>\
            <h1 class=\"title\">Hello World</h1>\
            <article><h2>Section</h2><p>Body text here.</p></article>\
            </body></html>";
        let provider = make_provider(simple_config());
        let result = provider
            .extract("https://myblog.com/post/1", &client, None, Some(html))
            .await
            .expect("extract should succeed");
        assert!(!result.markdown.is_empty());
        assert!(result.markdown.contains("Body text here"));
        assert_eq!(result.metadata.platform, "css:test-blog");
    }

    #[tokio::test]
    async fn extract_populates_title_from_selector() {
        let client = AcceleratedClient::new().expect("client");
        let html = b"<html><body>\
            <h1 class=\"title\">My Post Title</h1>\
            <article><p>Content.</p></article>\
            </body></html>";
        let provider = make_provider(simple_config());
        let result = provider
            .extract("https://myblog.com/post/1", &client, None, Some(html))
            .await
            .expect("extract should succeed");
        assert_eq!(result.metadata.title.as_deref(), Some("My Post Title"));
    }

    #[tokio::test]
    async fn extract_removes_sidebar_from_content() {
        let client = AcceleratedClient::new().expect("client");
        let html = b"<html><body>\
            <article>\
              <p>Main content</p>\
              <div class=\"sidebar\">Sidebar ads</div>\
            </article>\
            </body></html>";
        let provider = make_provider(simple_config());
        let result = provider
            .extract("https://myblog.com/post/1", &client, None, Some(html))
            .await
            .expect("extract should succeed");
        assert!(result.markdown.contains("Main content"));
        assert!(!result.markdown.contains("Sidebar ads"));
    }

    #[tokio::test]
    async fn extract_rejects_non_utf8_html() {
        let client = AcceleratedClient::new().expect("client");
        // Invalid UTF-8 sequence
        let html: &[u8] = &[0xFF, 0xFE, 0x00];
        let provider = make_provider(simple_config());
        let result = provider
            .extract("https://myblog.com/post/1", &client, None, Some(html))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn extract_canonical_url_matches_input_url() {
        let client = AcceleratedClient::new().expect("client");
        let html = b"<html><body><article><p>Text.</p></article></body></html>";
        let provider = make_provider(simple_config());
        let result = provider
            .extract("https://myblog.com/post/42", &client, None, Some(html))
            .await
            .expect("extract should succeed");
        assert_eq!(result.metadata.canonical_url, "https://myblog.com/post/42");
    }
}
