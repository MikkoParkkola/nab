//! `AsrBackend` trait — the single abstraction all ASR implementations satisfy.
//!
//! Platform-specific backends implement this trait:
//! - [`fluidaudio_backend::FluidAudioBackend`] — macOS Apple Silicon, Neural Engine
//! - `SherpaOnnxBackend` — cross-platform ONNX (Phase 3)
//! - `WhisperRsBackend` — universal GGML fallback (Phase 3)
//!
//! Consumers work against `Arc<dyn AsrBackend>` so the backend is swappable at
//! runtime (e.g., `--backend` CLI flag) without recompiling.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::Result;

// ─── Data model ───────────────────────────────────────────────────────────────

/// A single word with its timing and confidence from ASR output.
///
/// Maps 1-to-1 from `fluidaudio_rs::WordTiming` (and its ONNX / GGML
/// equivalents) so that the trait surface is backend-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WordTiming {
    /// The word text as returned by the ASR model.
    pub word: String,
    /// Start time in seconds from the start of the audio clip.
    pub start: f64,
    /// End time in seconds from the start of the audio clip.
    pub end: f64,
    /// Model-reported confidence in `[0.0, 1.0]`.
    pub confidence: f32,
}

/// One transcribed segment (sentence or phrase chunk).
///
/// Segments are the primary unit exposed to callers. Word-level detail is
/// optional — only populated when `TranscribeOptions::word_timestamps` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    /// Transcribed text for this segment.
    pub text: String,
    /// Segment start time (seconds).
    pub start: f64,
    /// Segment end time (seconds).
    pub end: f64,
    /// Average word confidence for this segment, in `[0.0, 1.0]`.
    pub confidence: f32,
    /// BCP-47 language tag (e.g. `"fi"`, `"en-US"`) when detected per-segment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Speaker label assigned after diarization (e.g. `"SPEAKER_00"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Word-level timings; present only when `word_timestamps = true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<WordTiming>>,
}

/// A speaker turn produced by the diarizer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerSegment {
    /// Speaker label, e.g. `"SPEAKER_00"`.
    pub speaker: String,
    /// Turn start time (seconds).
    pub start: f64,
    /// Turn end time (seconds).
    pub end: f64,
}

/// Full transcription result returned by [`AsrBackend::transcribe`].
///
/// The `segments` field carries the transcript; `speakers` is `Some` only when
/// diarization was requested via [`TranscribeOptions::diarize`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    /// Ordered transcript segments.
    pub segments: Vec<TranscriptSegment>,
    /// Dominant language detected (BCP-47).
    pub language: String,
    /// Duration of the audio processed, in seconds.
    pub duration_seconds: f64,
    /// Model identifier (e.g. `"parakeet-tdt-0.6b-v3"`).
    pub model: String,
    /// Backend identifier (e.g. `"fluidaudio"`, `"sherpa-onnx"`, `"whisper-rs"`).
    pub backend: String,
    /// Realtime factor — audio seconds processed per wall-clock second.
    pub rtfx: f64,
    /// Wall-clock processing time in seconds.
    pub processing_time_seconds: f64,
    /// Speaker turns; populated when `opts.diarize = true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speakers: Option<Vec<SpeakerSegment>>,
}

// ─── Options ──────────────────────────────────────────────────────────────────

/// Options forwarded to [`AsrBackend::transcribe`].
#[derive(Debug, Clone, Default)]
pub struct TranscribeOptions {
    /// BCP-47 language hint. `None` triggers auto-detection.
    pub language: Option<String>,
    /// When `true`, backends that support it will populate
    /// [`TranscriptSegment::words`].
    pub word_timestamps: bool,
    /// When `true`, also run the backend's diarizer and populate
    /// [`TranscriptionResult::speakers`] and [`TranscriptSegment::speaker`].
    pub diarize: bool,
    /// Hard cap on audio duration to process. Audio beyond this point is
    /// silently ignored. `None` = no limit.
    pub max_duration_seconds: Option<u32>,
}

// ─── Trait ────────────────────────────────────────────────────────────────────

/// Unified interface for all ASR backends.
///
/// Implementations are `Send + Sync` so they can be wrapped in `Arc<dyn
/// AsrBackend>` and shared across tasks without additional locking.
///
/// # Platform availability
///
/// Each backend declares its availability via [`AsrBackend::is_available`].
/// The caller should call this before using a backend obtained from
/// [`super::default_backend`] — the returned backend may still return an error
/// from [`AsrBackend::transcribe`] if, for example, model weights have not been
/// downloaded yet.
#[async_trait]
pub trait AsrBackend: Send + Sync {
    /// Short identifier string (e.g. `"fluidaudio"`, `"sherpa-onnx"`).
    fn name(&self) -> &str;

    /// BCP-47 language codes this backend natively supports.
    ///
    /// Return `&["*"]` to signal that the backend accepts any language (e.g.
    /// Whisper large-v3).
    fn supported_languages(&self) -> &[&str];

    /// Returns `true` if the backend libraries and binaries are present at
    /// runtime. Does **not** check whether model weights are downloaded.
    fn is_available(&self) -> bool;

    /// Transcribe the audio file at `audio_path`.
    ///
    /// `audio_path` must be a 16 kHz mono WAV file, or a supported audio
    /// format (MP3, FLAC, M4A). Non-WAV formats are accepted by FluidAudio and
    /// sherpa-onnx; callers should prefer WAV to avoid transcoding latency.
    async fn transcribe(
        &self,
        audio_path: &Path,
        opts: TranscribeOptions,
    ) -> Result<TranscriptionResult>;
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `WordTiming` round-trips through JSON without data loss.
    #[test]
    fn word_timing_json_roundtrip() {
        // GIVEN a word timing value
        let wt = WordTiming {
            word: "hello".to_string(),
            start: 1.23,
            end: 1.89,
            confidence: 0.97,
        };

        // WHEN serialized and deserialized
        let json = serde_json::to_string(&wt).expect("serialize");
        let decoded: WordTiming = serde_json::from_str(&json).expect("deserialize");

        // THEN all fields are preserved exactly
        assert_eq!(decoded, wt);
    }

    /// `TranscriptSegment` omits `None` optional fields in JSON.
    #[test]
    fn transcript_segment_omits_none_fields() {
        // GIVEN a minimal segment with no optional fields
        let seg = TranscriptSegment {
            text: "Hei maailma".to_string(),
            start: 0.0,
            end: 1.5,
            confidence: 0.94,
            language: None,
            speaker: None,
            words: None,
        };

        // WHEN serialized
        let json = serde_json::to_string(&seg).expect("serialize");

        // THEN optional None fields are absent
        assert!(!json.contains("language"));
        assert!(!json.contains("speaker"));
        assert!(!json.contains("words"));
    }

    /// `TranscriptSegment` includes populated optional fields.
    #[test]
    fn transcript_segment_includes_some_fields() {
        // GIVEN a segment with all optional fields populated
        let seg = TranscriptSegment {
            text: "Test".to_string(),
            start: 0.0,
            end: 1.0,
            confidence: 0.9,
            language: Some("fi".to_string()),
            speaker: Some("SPEAKER_00".to_string()),
            words: Some(vec![WordTiming {
                word: "Test".to_string(),
                start: 0.0,
                end: 1.0,
                confidence: 0.9,
            }]),
        };

        // WHEN serialized
        let json = serde_json::to_string(&seg).expect("serialize");

        // THEN all populated fields appear
        assert!(json.contains("\"language\""));
        assert!(json.contains("\"speaker\""));
        assert!(json.contains("\"words\""));
        assert!(json.contains("SPEAKER_00"));
        assert!(json.contains("\"fi\""));
    }

    /// `TranscriptionResult` omits `speakers` when `None`.
    #[test]
    fn transcription_result_omits_speakers_when_none() {
        // GIVEN a result without diarization
        let result = TranscriptionResult {
            segments: vec![],
            language: "en".to_string(),
            duration_seconds: 30.0,
            model: "parakeet-tdt-0.6b-v3".to_string(),
            backend: "fluidaudio".to_string(),
            rtfx: 143.0,
            processing_time_seconds: 0.21,
            speakers: None,
        };

        // WHEN serialized
        let json = serde_json::to_string(&result).expect("serialize");

        // THEN `speakers` is absent from the output
        assert!(!json.contains("speakers"));
    }

    /// `TranscribeOptions` defaults are all zero/false/None.
    #[test]
    fn transcribe_options_default_is_minimal() {
        // GIVEN the default options
        let opts = TranscribeOptions::default();

        // THEN nothing is opted in by default
        assert!(opts.language.is_none());
        assert!(!opts.word_timestamps);
        assert!(!opts.diarize);
        assert!(opts.max_duration_seconds.is_none());
    }
}
