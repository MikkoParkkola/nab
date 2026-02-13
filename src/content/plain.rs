//! Plain text passthrough handler.
//!
//! Handles `text/plain`, `application/json`, and any other text-like
//! content types by passing them through without transformation.

use anyhow::Result;

use super::{ContentHandler, ConversionResult};

/// Passes text content through without transformation.
///
/// Used for `text/plain`, `application/json`, `text/csv`, and as the
/// ultimate fallback for unknown content types.
pub struct PlainHandler;

impl ContentHandler for PlainHandler {
    fn supported_types(&self) -> &[&str] {
        &[
            "text/plain",
            "application/json",
            "text/csv",
            "text/xml",
            "application/xml",
        ]
    }

    fn to_markdown(&self, bytes: &[u8], content_type: &str) -> Result<ConversionResult> {
        let start = std::time::Instant::now();
        let text = String::from_utf8_lossy(bytes).to_string();

        Ok(ConversionResult {
            markdown: text,
            page_count: None,
            content_type: content_type.to_string(),
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_plain_text_through() {
        let handler = PlainHandler;
        let text = b"Hello, world!";
        let result = handler.to_markdown(text, "text/plain").unwrap();
        assert_eq!(result.markdown, "Hello, world!");
        assert!(result.page_count.is_none());
    }

    #[test]
    fn passes_json_through() {
        let handler = PlainHandler;
        let json = br#"{"key": "value", "num": 42}"#;
        let result = handler.to_markdown(json, "application/json").unwrap();
        assert!(result.markdown.contains(r#""key""#));
        assert!(result.markdown.contains("42"));
    }

    #[test]
    fn handles_empty_input() {
        let handler = PlainHandler;
        let result = handler.to_markdown(b"", "text/plain").unwrap();
        assert_eq!(result.markdown, "");
    }

    #[test]
    fn handles_non_utf8() {
        let handler = PlainHandler;
        let bytes: &[u8] = &[0xff, 0xfe, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
        let result = handler.to_markdown(bytes, "text/plain").unwrap();
        assert!(result.markdown.contains("Hello"));
    }
}
