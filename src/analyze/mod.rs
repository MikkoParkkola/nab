//! Multimodal video analysis pipeline
//!
//! Performs synchronized audio+video analysis:
//! - Frame extraction (keyframes via ffmpeg scene detection)
//! - Audio extraction and transcription (Whisper / FluidAudio / sherpa-onnx)
//! - Speaker diarization (pyannote / FluidAudio VBx)
//! - Visual analysis (local models or Claude Vision API)
//! - Multimodal fusion with timestamp alignment
//!
//! # Backend selection
//!
//! Use [`default_backend`] to obtain the best available [`AsrBackend`] for the
//! current platform. [`FluidAudioBackend`] is returned unconditionally; it calls
//! the standalone `fluidaudiocli` binary via subprocess. When that binary is not
//! installed, [`AsrBackend::is_available`] returns `false`.

pub mod active_reading;
pub mod asr_backend;
pub mod diarize;
pub mod extract;
pub mod fluidaudio_backend;
pub mod fusion;
pub mod report;
pub mod transcribe;
pub mod vision;

#[cfg(feature = "analyze-sherpa")]
pub mod sherpa_onnx_backend;
#[cfg(feature = "analyze-whisper")]
pub mod whisper_rs_backend;

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── New trait-based API (Phase 1+) ────────────────────────────────────────────
pub use active_reading::{
    ActiveReader, ActiveReadingConfig, ActiveReadingError, ActiveReadingMetadata,
    ActiveReadingOutput, LlmSampler, LookupResult, Reference, ReferenceKind, UrlFetcher,
};
pub use asr_backend::{
    AsrBackend, SpeakerSegment as AsrSpeakerSegment, TranscribeOptions,
    TranscriptSegment as AsrTranscriptSegment, TranscriptionResult, WordTiming as AsrWordTiming,
};
pub use fluidaudio_backend::FluidAudioBackend;

// ── Legacy API ────────────────────────────────────────────────────────────────
pub use diarize::{Diarizer, SpeakerSegment};
pub use extract::{AudioExtractor, ExtractedFrame, FrameExtractor};
pub use fusion::{FusedSegment, FusionEngine};
pub use report::{AnalysisReport, ReportFormat};
pub use transcribe::{TranscriptSegment, TranscriptionBackend, VllmTranscriber, WordTiming};
pub use vision::{VisionAnalyzer, VisionBackend, VisualAnalysis};

// ─── Backend factory ──────────────────────────────────────────────────────────

/// Return the best available [`AsrBackend`] for the current platform.
///
/// Selection order (first available wins):
///
/// 1. **`FluidAudio`** (macOS aarch64 only) — ~150× `RTFx` on Apple Neural Engine.
/// 2. **`SherpaOnnx`** (all platforms, `analyze-sherpa` feature) — ~30× `RTFx` `CPU`,
///    requires model files at `~/.cache/nab/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3/`.
/// 3. **`WhisperRs`** (all platforms, `analyze-whisper` feature) — ~3–15× `RTFx`,
///    99-language universal fallback, requires `~/.cache/nab/models/whisper-large-v3-turbo-q5_0.bin`.
/// 4. **`Stub`** — returns an error on transcription. Indicates no backend is installed.
///
/// Install backends with:
/// - `nab models fetch fluidaudio` (macOS Apple Silicon)
/// - `nab models fetch sherpa-onnx` (all platforms, `--features analyze-sherpa`)
/// - `nab models fetch whisper` (all platforms, `--features analyze-whisper`)
#[must_use]
pub fn default_backend() -> Arc<dyn AsrBackend> {
    // 1. FluidAudio — macOS Apple Silicon only.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let fluid = FluidAudioBackend::new()
            .unwrap_or_else(|_| FluidAudioBackend::with_binary(PathBuf::from("fluidaudiocli")));
        if fluid.is_available() {
            return Arc::new(fluid);
        }
    }

    // 2. Sherpa-ONNX — cross-platform, opt-in via `analyze-sherpa` feature.
    #[cfg(feature = "analyze-sherpa")]
    {
        let sherpa = sherpa_onnx_backend::SherpaOnnxBackend::new();
        if sherpa.is_available() {
            return Arc::new(sherpa);
        }
    }

    // 3. Whisper-rs — universal fallback, opt-in via `analyze-whisper` feature.
    #[cfg(feature = "analyze-whisper")]
    {
        let whisper = whisper_rs_backend::WhisperRsBackend::new();
        if whisper.is_available() {
            return Arc::new(whisper);
        }
    }

    // 4. Stub — all backends unavailable (no models downloaded / features disabled).
    // Return a FluidAudio instance that reports is_available=false; callers that
    // check is_available() before transcribing will get a clear MissingDependency
    // error at use time rather than a panic here.
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        Arc::new(FluidAudioBackend::with_binary(PathBuf::from(
            "fluidaudiocli",
        )))
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Arc::new(FluidAudioBackend::with_binary(PathBuf::from(
            "fluidaudiocli",
        )))
    }
}

/// Analysis pipeline errors
#[derive(Error, Debug)]
pub enum AnalysisError {
    #[error("FFmpeg error: {0}")]
    Ffmpeg(String),

    #[error("Whisper error: {0}")]
    Whisper(String),

    #[error("Diarization error: {0}")]
    Diarization(String),

    #[error("Vision analysis error: {0}")]
    Vision(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Missing dependency: {0}")]
    MissingDependency(String),

    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("Format error: {0}")]
    Format(#[from] std::fmt::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Transcription API error: {0}")]
    TranscriptionApi(String),
}

pub type Result<T> = std::result::Result<T, AnalysisError>;

/// Primary emotion detected in a segment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmotionAnalysis {
    pub primary: String,
    pub confidence: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary: Option<String>,
}

/// Visual context from frame analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualContext {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaze: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objects: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
}

/// Analysis segment with all modalities fused
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisSegment {
    pub start: f64,
    pub end: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emotion: Option<EmotionAnalysis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visual: Option<VisualContext>,
    #[serde(default)]
    pub flags: Vec<String>,
}

/// Analysis output containing all segments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisOutput {
    pub segments: Vec<AnalysisSegment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<VideoMetadata>,
}

/// Video metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoMetadata {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_channels: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_sample_rate: Option<u32>,
}

/// Pipeline configuration
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Minimum scene change threshold (0.0-1.0)
    pub scene_threshold: f32,
    /// Maximum frames to extract
    pub max_frames: usize,
    /// Whisper model size (tiny, base, small, medium, large)
    pub whisper_model: String,
    /// Enable speaker diarization
    pub enable_diarization: bool,
    /// Vision backend preference
    pub vision_backend: VisionBackend,
    /// Output directory for intermediate files
    pub work_dir: PathBuf,
    /// DGX Spark host for GPU offload
    pub dgx_host: Option<String>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            scene_threshold: 0.3,
            max_frames: 100,
            whisper_model: "base".to_string(),
            enable_diarization: true,
            vision_backend: VisionBackend::Local,
            work_dir: std::env::temp_dir().join("nab_analyze"),
            dgx_host: None,
        }
    }
}

// AnalysisPipeline was removed in chore(analyze): it held a `Transcriber` field
// (deprecated 6fa7164). Compose FrameExtractor + AudioExtractor + AsrBackend +
// Diarizer + VisionAnalyzer + FusionEngine directly.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = PipelineConfig::default();
        assert!((config.scene_threshold - 0.3).abs() < f32::EPSILON);
        assert_eq!(config.whisper_model, "base");
        assert!(config.enable_diarization);
    }

    #[test]
    fn test_segment_serialization() {
        let segment = AnalysisSegment {
            start: 0.0,
            end: 5.2,
            speaker: Some("Speaker_1".to_string()),
            transcript: Some("Hello, welcome to the show".to_string()),
            emotion: Some(EmotionAnalysis {
                primary: "happy".to_string(),
                confidence: 0.85,
                secondary: None,
            }),
            visual: Some(VisualContext {
                action: "waving".to_string(),
                gaze: Some("camera".to_string()),
                objects: None,
                scene: None,
            }),
            flags: vec![],
        };

        let json = serde_json::to_string_pretty(&segment).unwrap();
        assert!(json.contains("Speaker_1"));
        assert!(json.contains("waving"));
    }
}
