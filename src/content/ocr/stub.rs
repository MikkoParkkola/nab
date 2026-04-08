//! Stub OCR engine for non-macOS platforms.
//!
//! Returns a human-readable "not available" message. Phase 3 will add
//! `PaddleOCR` and `Tesseract` backends that work cross-platform.

use async_trait::async_trait;

use super::{OcrEngine, OcrError, OcrResult};

// ─── Languages ────────────────────────────────────────────────────────────────

/// Stub supports no languages — callers should check `is_available()` first.
const STUB_LANGUAGES: [&str; 0] = [];

// ─── Stub engine ─────────────────────────────────────────────────────────────

/// Placeholder OCR engine returned on non-macOS platforms.
///
/// Always returns [`OcrError::NotAvailable`] from [`OcrEngine::ocr_image`].
/// Phase 3 will replace this with a real cross-platform implementation.
pub struct StubEngine;

#[async_trait]
impl OcrEngine for StubEngine {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn supported_languages(&self) -> &'static [&'static str] {
        &STUB_LANGUAGES
    }

    fn is_available(&self) -> bool {
        false
    }

    async fn ocr_image(&self, _image_bytes: &[u8]) -> Result<OcrResult, OcrError> {
        Err(OcrError::NotAvailable(
            "stub".to_string(),
            "OCR is not available on this platform. \
             Use `nab models fetch paddle-ocr` in Phase 3 for cross-platform OCR support."
                .to_string(),
        ))
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `StubEngine::name()` returns "stub".
    #[test]
    fn stub_name_is_stub() {
        assert_eq!(StubEngine.name(), "stub");
    }

    /// `StubEngine::is_available()` returns false.
    #[test]
    fn stub_is_not_available() {
        assert!(!StubEngine.is_available());
    }

    /// `StubEngine::supported_languages()` is empty.
    #[test]
    fn stub_supports_no_languages() {
        assert!(StubEngine.supported_languages().is_empty());
    }
}
