//! Content-type-aware response conversion.
//!
//! Routes HTTP response bytes to the appropriate handler based on
//! the `Content-Type` header. Each handler implements [`ContentHandler`]
//! to convert raw bytes into markdown for LLM consumption.
//!
//! # Supported Content Types
//!
//! | Type | Handler | Feature Flag |
//! |------|---------|-------------|
//! | `text/html`, `application/xhtml+xml` | [`HtmlHandler`] | always |
//! | `application/pdf` | [`PdfHandler`] | `pdf` |
//! | `text/plain`, `application/json`, etc. | [`PlainHandler`] | always |
//!
//! # Example
//!
//! ```rust
//! use nab::content::{ContentRouter, ConversionResult};
//!
//! let router = ContentRouter::new();
//! let html = b"<html><body><h1>Hello</h1></body></html>";
//! let result = router.convert(html, "text/html").unwrap();
//! assert!(result.markdown.contains("Hello"));
//! ```

pub mod html;
#[cfg(feature = "pdf")]
pub mod pdf;
pub mod plain;
#[cfg(feature = "pdf")]
pub mod table;
#[cfg(feature = "pdf")]
pub mod types;

use anyhow::Result;

/// Metadata about a content conversion result.
#[derive(Debug, Clone)]
pub struct ConversionResult {
    /// The converted markdown content.
    pub markdown: String,
    /// Number of pages (for paginated formats like PDF).
    pub page_count: Option<usize>,
    /// Original content type.
    pub content_type: String,
    /// Conversion time in milliseconds.
    pub elapsed_ms: f64,
}

/// Converts response bytes into markdown.
///
/// Implementations are stateless and synchronous. The router runs them
/// inside `tokio::task::spawn_blocking` when needed (e.g., for PDF
/// extraction via pdfium FFI).
pub trait ContentHandler: Send + Sync {
    /// MIME types this handler supports (e.g., `["text/html"]`).
    fn supported_types(&self) -> &[&str];

    /// Convert raw response bytes to markdown.
    ///
    /// `content_type` is the full `Content-Type` header value (may include
    /// charset parameters like `; charset=utf-8`).
    fn to_markdown(&self, bytes: &[u8], content_type: &str) -> Result<ConversionResult>;
}

/// Routes response bytes to the appropriate [`ContentHandler`] based on
/// the `Content-Type` header.
///
/// Dispatch is O(n) over registered handlers. With 3-5 handlers this is
/// negligible (~nanoseconds). Falls back to [`PlainHandler`] for unknown types.
pub struct ContentRouter {
    handlers: Vec<Box<dyn ContentHandler>>,
}

impl ContentRouter {
    /// Create a router with all available handlers.
    ///
    /// PDF handler is included only when the `pdf` feature flag is enabled.
    pub fn new() -> Self {
        #[cfg(feature = "pdf")]
        let handlers: Vec<Box<dyn ContentHandler>> = vec![
            Box::new(pdf::PdfHandler::new()),
            Box::new(html::HtmlHandler),
            Box::new(plain::PlainHandler),
        ];

        #[cfg(not(feature = "pdf"))]
        let handlers: Vec<Box<dyn ContentHandler>> = vec![
            Box::new(html::HtmlHandler),
            Box::new(plain::PlainHandler),
        ];

        Self { handlers }
    }

    /// Find a handler for the given content type and convert the bytes.
    ///
    /// Falls back to HTML if the bytes look like HTML (common for responses
    /// with missing or incorrect `Content-Type`). Ultimate fallback is
    /// [`PlainHandler`].
    pub fn convert(&self, bytes: &[u8], content_type: &str) -> Result<ConversionResult> {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
            .to_lowercase();

        for handler in &self.handlers {
            if handler.supported_types().iter().any(|t| *t == mime) {
                return handler.to_markdown(bytes, content_type);
            }
        }

        // Fallback: if bytes look like HTML (common for missing Content-Type)
        if bytes.starts_with(b"<!") || bytes.starts_with(b"<html") || bytes.starts_with(b"<HTML")
        {
            return self
                .handlers
                .iter()
                .find(|h| h.supported_types().contains(&"text/html"))
                .expect("HtmlHandler always registered")
                .to_markdown(bytes, "text/html");
        }

        // Ultimate fallback: plain text passthrough
        plain::PlainHandler.to_markdown(bytes, content_type)
    }
}

impl Default for ContentRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_dispatches_html_to_html_handler() {
        let router = ContentRouter::new();
        let html = b"<html><body><h1>Title</h1><p>Body text</p></body></html>";
        let result = router.convert(html, "text/html").unwrap();
        assert!(result.markdown.contains("Title"));
        assert!(result.markdown.contains("Body text"));
        assert_eq!(result.content_type, "text/html");
        assert!(result.page_count.is_none());
    }

    #[test]
    fn router_dispatches_xhtml_to_html_handler() {
        let router = ContentRouter::new();
        let xhtml = b"<html><body><p>XHTML content</p></body></html>";
        let result = router.convert(xhtml, "application/xhtml+xml").unwrap();
        assert!(result.markdown.contains("XHTML content"));
    }

    #[test]
    fn router_dispatches_plain_text() {
        let router = ContentRouter::new();
        let text = b"Hello, plain world!";
        let result = router.convert(text, "text/plain").unwrap();
        assert_eq!(result.markdown, "Hello, plain world!");
    }

    #[test]
    fn router_dispatches_json() {
        let router = ContentRouter::new();
        let json = br#"{"key": "value"}"#;
        let result = router.convert(json, "application/json").unwrap();
        assert!(result.markdown.contains(r#""key""#));
    }

    #[test]
    fn router_handles_content_type_with_charset() {
        let router = ContentRouter::new();
        let html = b"<html><body>Charset test</body></html>";
        let result = router
            .convert(html, "text/html; charset=utf-8")
            .unwrap();
        assert!(result.markdown.contains("Charset test"));
    }

    #[test]
    fn router_falls_back_to_html_for_html_like_bytes() {
        let router = ContentRouter::new();
        let html = b"<!DOCTYPE html><html><body>Fallback</body></html>";
        let result = router
            .convert(html, "application/octet-stream")
            .unwrap();
        assert!(result.markdown.contains("Fallback"));
    }

    #[test]
    fn router_falls_back_to_plain_for_unknown() {
        let router = ContentRouter::new();
        let data = b"Some unknown binary-ish data";
        let result = router
            .convert(data, "application/octet-stream")
            .unwrap();
        assert!(result.markdown.contains("unknown binary"));
    }
}
