//! Apple Vision OCR engine.
//!
//! Uses the `VNRecognizeTextRequest` API, accessed either via:
//! 1. A direct objc2 binding (when the `VNImageRequestHandler::initWithData:options:`
//!    path is available), or
//! 2. A subprocess call to a bundled Swift helper (Phase 3 wiring).
//!
//! For Phase 1 the engine is implemented via direct objc2 bindings. The
//! Vision framework is always available on macOS 13+.
//!
//! # Supported languages (`VNRecognizeTextRequestRevision3`)
//!
//! `en`, `fr`, `it`, `de`, `es`, `pt`, `zh-Hans`, `zh-Hant`, `ja`, `ko`,
//! `ru`, `uk`, `th`, `vi`, `ar`.
//!
//! Finnish (`fi`) and Swedish (`sv`) are not supported by Apple Vision OCR.
//! Use `Tesseract` or `PaddleOCR` (Phase 3) for these languages.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString, NSUInteger};
use objc2_vision::{VNImageRequestHandler, VNRecognizeTextRequest, VNRequestTextRecognitionLevel};

use async_trait::async_trait;

use super::{OcrEngine, OcrError, OcrRegion, OcrResult};

// ─── Language constants ───────────────────────────────────────────────────────

/// Languages supported by `VNRecognizeTextRequestRevision3`.
///
/// Excludes fi and sv — Vision's OCR does not support these.
/// (Parakeet ASR supports them but OCR is a separate pipeline.)
const SUPPORTED_LANGUAGES: &[&str] = &[
    "en", "fr", "it", "de", "es", "pt", "zh-Hans", "zh-Hant", "ja", "ko", "ru", "uk", "th", "vi",
    "ar",
];

// ─── Engine struct ────────────────────────────────────────────────────────────

/// OCR engine backed by Apple's Vision framework (`VNRecognizeTextRequest`).
///
/// All state is stateless — `VNImageRequestHandler` is created per-call so
/// the engine can be freely shared across async tasks via `Arc<dyn OcrEngine>`.
///
/// Requires macOS 13+ (`VNRecognizeTextRequestRevision3`).
pub struct AppleVisionEngine;

impl AppleVisionEngine {
    /// Create a new `AppleVisionEngine`.
    ///
    /// Infallible — Vision is always present on macOS 13+. Availability is
    /// checked lazily at call time via [`OcrEngine::is_available`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for AppleVisionEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ─── OcrEngine implementation ─────────────────────────────────────────────────

#[async_trait]
impl OcrEngine for AppleVisionEngine {
    fn name(&self) -> &'static str {
        "apple_vision"
    }

    fn supported_languages(&self) -> &'static [&'static str] {
        SUPPORTED_LANGUAGES
    }

    fn is_available(&self) -> bool {
        // Vision framework is always available on macOS 13+ (nab's minimum).
        true
    }

    async fn ocr_image(&self, image_bytes: &[u8]) -> Result<OcrResult, OcrError> {
        let bytes = image_bytes.to_vec();
        // Spawn on a blocking thread — Vision OCR is synchronous and CPU-bound.
        tokio::task::spawn_blocking(move || run_vision_ocr(&bytes))
            .await
            .map_err(|e| OcrError::Framework(format!("spawn_blocking join error: {e}")))?
    }
}

// ─── Core OCR logic (blocking) ────────────────────────────────────────────────

/// Execute `VNRecognizeTextRequest` synchronously on the calling thread.
///
/// # Safety invariants
///
/// - `VNImageRequestHandler` must not be shared across threads; it is
///   created and destroyed within this function.
/// - All `Retained<T>` values are ARC-managed and safe to construct/drop on
///   any thread.
/// - The `unsafe` pointer casts performed to reinterpret `NSArray<T>` as
///   `NSArray<U>` are sound because `NSArray` is covariant in its element
///   type in Objective-C and the cast stays within the same memory layout.
fn run_vision_ocr(image_bytes: &[u8]) -> Result<OcrResult, OcrError> {
    // SAFETY: NSData::from_vec does not alias or mutate after construction.
    let ns_data: Retained<NSData> = NSData::from_vec(image_bytes.to_vec());

    // SAFETY: NSDictionary::new returns an empty, valid dictionary.
    let options: Retained<NSDictionary<objc2_vision::VNImageOption, AnyObject>> =
        NSDictionary::new();

    // Build the request before the handler so any config errors surface early.
    let request = build_text_request();

    let handler = VNImageRequestHandler::initWithData_options(
        VNImageRequestHandler::alloc(),
        &ns_data,
        &options,
    );

    // Build a single-element NSArray<VNRecognizeTextRequest> for performRequests:error:.
    // SAFETY: The array is created from a well-formed Retained<VNRecognizeTextRequest>.
    let req_array: Retained<NSArray<VNRecognizeTextRequest>> =
        NSArray::from_retained_slice(std::slice::from_ref(&request));

    // SAFETY: VNRequest is the superclass of VNRecognizeTextRequest; the
    // reinterpret is safe because NSArray is covariant and the underlying
    // Objective-C type system allows this upcast.
    let vn_req_array: &NSArray<objc2_vision::VNRequest> = unsafe {
        &*std::ptr::from_ref::<NSArray<VNRecognizeTextRequest>>(req_array.as_ref())
            .cast::<NSArray<objc2_vision::VNRequest>>()
    };

    if let Err(err) = handler.performRequests_error(vn_req_array) {
        let msg = err.localizedDescription().to_string();
        return Err(OcrError::Framework(msg));
    }

    Ok(extract_results(&request))
}

/// Build a `VNRecognizeTextRequest` configured for high-accuracy recognition.
fn build_text_request() -> Retained<VNRecognizeTextRequest> {
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setUsesLanguageCorrection(true);
    request.setAutomaticallyDetectsLanguage(true);
    request
}

/// Extract an [`OcrResult`] from a completed `VNRecognizeTextRequest`.
fn extract_results(request: &VNRecognizeTextRequest) -> OcrResult {
    let observations = request.results();

    let observations = match observations {
        Some(obs) if !obs.is_empty() => obs,
        _ => {
            return OcrResult {
                text: String::new(),
                language: None,
                confidence: 0.0,
                regions: vec![],
            };
        }
    };

    let mut regions = Vec::with_capacity(observations.len());
    let mut total_confidence = 0.0_f32;

    for obs in &observations {
        // topCandidates(1) returns the best recognition for this observation.
        let candidates = obs.topCandidates(1 as NSUInteger);
        let Some(candidate) = candidates.iter().next() else {
            continue;
        };

        // `candidate.string()` returns `Retained<NSString>`; convert explicitly.
        let ns_text: Retained<NSString> = candidate.string();
        let text = ns_text.to_string();
        let confidence = candidate.confidence();
        // SAFETY: boundingBox() returns a CGRect value by value — no lifetime hazards.
        let bbox = unsafe { obs.boundingBox() };

        // Vision uses bottom-left origin; convert to top-left for consistency.
        #[allow(clippy::cast_possible_truncation)]
        let region = OcrRegion {
            text,
            bounding_box: [
                bbox.origin.x as f32,
                1.0 - (bbox.origin.y as f32) - (bbox.size.height as f32),
                bbox.size.width as f32,
                bbox.size.height as f32,
            ],
            confidence,
        };

        total_confidence += confidence;
        regions.push(region);
    }

    #[allow(clippy::cast_precision_loss)]
    let avg_confidence = if regions.is_empty() {
        0.0
    } else {
        total_confidence / regions.len() as f32
    };

    let full_text = regions
        .iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    OcrResult {
        text: full_text,
        language: None, // Vision does not expose per-call language detection result
        confidence: avg_confidence,
        regions,
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `AppleVisionEngine::name()` returns "apple_vision".
    #[test]
    fn name_is_apple_vision() {
        let engine = AppleVisionEngine::new();
        assert_eq!(engine.name(), "apple_vision");
    }

    /// `is_available()` returns true on macOS (Vision always present).
    #[test]
    fn is_available_on_macos() {
        let engine = AppleVisionEngine::new();
        assert!(engine.is_available());
    }

    /// `supported_languages()` includes key languages and excludes fi/sv.
    #[test]
    fn supported_languages_correct_set() {
        let engine = AppleVisionEngine::new();
        let langs = engine.supported_languages();

        // Must include these key languages
        for required in &["en", "ja", "zh-Hans", "ko", "ar", "ru"] {
            assert!(langs.contains(required), "missing language: {required}");
        }
        // Must NOT include — Vision OCR does not support these
        assert!(!langs.contains(&"fi"), "fi must not be in list");
        assert!(!langs.contains(&"sv"), "sv must not be in list");
    }

    /// `OcrRegion` bounding box is a normalized `[f32; 4]`.
    #[test]
    fn ocr_region_bounding_box_is_four_floats() {
        // GIVEN a region with known bounding box
        let region = OcrRegion {
            text: "test".to_string(),
            bounding_box: [0.1, 0.2, 0.5, 0.3],
            confidence: 0.9,
        };
        // THEN all four values are accessible
        assert_eq!(region.bounding_box.len(), 4);
        assert!((region.bounding_box[0] - 0.1).abs() < f32::EPSILON);
        assert!((region.bounding_box[2] - 0.5).abs() < f32::EPSILON);
    }
}
