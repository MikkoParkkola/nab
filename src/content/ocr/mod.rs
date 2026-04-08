//! OCR engine abstraction for local, on-device text extraction from images.
//!
//! The [`OcrEngine`] trait defines a platform-agnostic interface. Platform
//! implementations are feature-gated:
//!
//! | Platform | Implementation | Notes |
//! |----------|---------------|-------|
//! | macOS | [`AppleVisionEngine`] | `VNRecognizeTextRequest` revision 3, `CoreML` |
//! | Linux/Windows | [`StubEngine`] | Returns "not available" message |
//!
//! # Example
//!
//! ```rust
//! use nab::content::ocr::{OcrEngine, default_engine};
//!
//! let engine = default_engine();
//! println!("OCR engine: {}", engine.name());
//! println!("Available: {}", engine.is_available());
//! ```

use async_trait::async_trait;
use thiserror::Error;

#[cfg(target_os = "macos")]
pub mod apple_vision;
pub mod fetch_integration;
#[cfg(not(target_os = "macos"))]
pub mod stub;

// Re-export platform implementation for convenience
#[cfg(target_os = "macos")]
pub use apple_vision::AppleVisionEngine;
#[cfg(not(target_os = "macos"))]
pub use stub::StubEngine;

// ─── Error type ───────────────────────────────────────────────────────────────

/// Errors produced by OCR operations.
#[derive(Debug, Error)]
pub enum OcrError {
    /// The engine is not available on this platform or is not installed.
    #[error("OCR engine '{0}' is not available: {1}")]
    NotAvailable(String, String),

    /// The image bytes could not be decoded.
    #[error("failed to decode image: {0}")]
    ImageDecode(String),

    /// The underlying OCR framework returned an error.
    #[error("OCR framework error: {0}")]
    Framework(String),

    /// I/O error reading image data.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ─── Result types ─────────────────────────────────────────────────────────────

/// A single recognized text region within an image.
#[derive(Debug, Clone)]
pub struct OcrRegion {
    /// The text content of this region.
    pub text: String,
    /// Normalized bounding box `[x, y, width, height]` in the range `[0.0, 1.0]`.
    ///
    /// `(0, 0)` is the top-left corner of the image. Values are normalized to
    /// the image dimensions so they remain valid regardless of pixel size.
    pub bounding_box: [f32; 4],
    /// Confidence score in `[0.0, 1.0]`. Higher is more confident.
    pub confidence: f32,
}

/// The result of an OCR pass on a single image.
#[derive(Debug, Clone)]
pub struct OcrResult {
    /// Full text extracted from the image, regions joined by newlines.
    pub text: String,
    /// BCP-47 language code detected, if the engine supports detection.
    pub language: Option<String>,
    /// Aggregate confidence score across all recognized regions.
    pub confidence: f32,
    /// Individual text regions with bounding boxes and per-region confidence.
    pub regions: Vec<OcrRegion>,
}

// ─── Trait ────────────────────────────────────────────────────────────────────

/// Unified interface for on-device OCR engines.
///
/// Implementations must be `Send + Sync` so they can be held in an
/// `Arc<dyn OcrEngine>` and called from async contexts.
///
/// # Platform availability
///
/// Call [`OcrEngine::is_available`] before invoking
/// [`OcrEngine::ocr_image`]. On unsupported platforms the implementation
/// returns [`OcrError::NotAvailable`] from `ocr_image`.
#[async_trait]
pub trait OcrEngine: Send + Sync {
    /// Short identifier string for this engine (e.g. `"apple_vision"`).
    fn name(&self) -> &'static str;

    /// BCP-47 language codes this engine natively recognizes.
    ///
    /// Returns `&["*"]` when the engine supports any language via automatic
    /// detection (same convention as [`crate::analyze::asr_backend::AsrBackend`]).
    fn supported_languages(&self) -> &'static [&'static str];

    /// Returns `true` when the engine libraries and models are present at
    /// runtime. Does **not** perform a test recognition.
    fn is_available(&self) -> bool;

    /// Run OCR on raw image bytes and return the recognized text.
    ///
    /// `image_bytes` must be a valid PNG, JPEG, TIFF, or HEIC file.
    /// The image is decoded internally — no pre-processing is required.
    ///
    /// # Errors
    ///
    /// Returns [`OcrError::NotAvailable`] when the engine is absent,
    /// [`OcrError::ImageDecode`] when the bytes cannot be parsed, or
    /// [`OcrError::Framework`] when the underlying OCR call fails.
    async fn ocr_image(&self, image_bytes: &[u8]) -> Result<OcrResult, OcrError>;
}

// ─── Factory ──────────────────────────────────────────────────────────────────

/// Return the best available OCR engine for the current platform.
///
/// On macOS this returns an [`AppleVisionEngine`] backed by the Vision
/// framework. On Linux/Windows a [`StubEngine`] is returned that explains
/// the limitation.
///
/// # Example
///
/// ```rust
/// use nab::content::ocr::default_engine;
///
/// let engine = default_engine();
/// assert!(!engine.name().is_empty());
/// ```
pub fn default_engine() -> Box<dyn OcrEngine> {
    #[cfg(target_os = "macos")]
    {
        Box::new(AppleVisionEngine::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(StubEngine)
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `default_engine()` returns an engine with a non-empty name.
    #[test]
    fn default_engine_has_non_empty_name() {
        // GIVEN the platform's default engine
        let engine = default_engine();
        // THEN name is non-empty
        assert!(!engine.name().is_empty());
    }

    /// `default_engine()` returns an engine that lists supported languages.
    #[test]
    fn default_engine_lists_supported_languages() {
        // GIVEN the default engine
        let engine = default_engine();
        // THEN at least one language is listed
        assert!(!engine.supported_languages().is_empty());
    }

    /// On macOS, `default_engine()` returns an engine named "apple_vision".
    #[test]
    #[cfg(target_os = "macos")]
    fn default_engine_is_apple_vision_on_macos() {
        let engine = default_engine();
        assert_eq!(engine.name(), "apple_vision");
    }

    /// On non-macOS, `default_engine()` returns a stub engine.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn default_engine_is_stub_on_non_macos() {
        let engine = default_engine();
        assert_eq!(engine.name(), "stub");
    }

    /// `OcrResult` can be constructed and its fields are accessible.
    #[test]
    fn ocr_result_fields_are_accessible() {
        // GIVEN an OCR result with one region
        let result = OcrResult {
            text: "Hello World".to_string(),
            language: Some("en".to_string()),
            confidence: 0.95,
            regions: vec![OcrRegion {
                text: "Hello World".to_string(),
                bounding_box: [0.0, 0.0, 1.0, 1.0],
                confidence: 0.95,
            }],
        };
        // THEN all fields are accessible
        assert_eq!(result.text, "Hello World");
        assert_eq!(result.language.as_deref(), Some("en"));
        assert!((result.confidence - 0.95).abs() < f32::EPSILON);
        assert_eq!(result.regions.len(), 1);
        assert_eq!(result.regions[0].bounding_box, [0.0, 0.0, 1.0, 1.0]);
    }

    /// `OcrError::NotAvailable` formats correctly.
    #[test]
    fn ocr_error_not_available_formats() {
        let err = OcrError::NotAvailable("apple_vision".to_string(), "no macOS".to_string());
        let msg = err.to_string();
        assert!(msg.contains("apple_vision"), "msg: {msg}");
        assert!(msg.contains("no macOS"), "msg: {msg}");
    }

    /// `OcrError::ImageDecode` formats correctly.
    #[test]
    fn ocr_error_image_decode_formats() {
        let err = OcrError::ImageDecode("invalid PNG header".to_string());
        assert!(err.to_string().contains("invalid PNG"));
    }
}
