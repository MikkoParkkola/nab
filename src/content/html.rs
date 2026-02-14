//! HTML to Markdown conversion handler.
//!
//! Wraps `html2md` with post-processing to remove boilerplate (cookie notices,
//! navigation bars, privacy footers) and clean up excessive whitespace.

use anyhow::Result;

use super::{ContentHandler, ConversionResult};

/// Converts HTML responses to clean markdown.
pub struct HtmlHandler;

impl ContentHandler for HtmlHandler {
    fn supported_types(&self) -> &[&str] {
        &["text/html", "application/xhtml+xml"]
    }

    fn to_markdown(&self, bytes: &[u8], content_type: &str) -> Result<ConversionResult> {
        let start = std::time::Instant::now();
        let html = String::from_utf8_lossy(bytes);
        let markdown = html_to_markdown(&html);

        Ok(ConversionResult {
            markdown,
            page_count: None,
            content_type: content_type.to_string(),
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

/// Convert HTML to markdown with boilerplate filtering.
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
}
