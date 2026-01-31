//! Multimodal video analysis pipeline
//!
//! Performs synchronized audio+video analysis:
//! - Frame extraction (keyframes via ffmpeg scene detection)
//! - Audio extraction and transcription (Whisper)
//! - Speaker diarization (pyannote)
//! - Visual analysis (local models or Claude Vision API)
//! - Multimodal fusion with timestamp alignment

pub mod diarize;
pub mod extract;
pub mod fusion;
pub mod report;
pub mod transcribe;
pub mod vision;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub use diarize::{Diarizer, SpeakerSegment};
pub use extract::{AudioExtractor, ExtractedFrame, FrameExtractor};
pub use fusion::{FusedSegment, FusionEngine};
pub use report::{AnalysisReport, ReportFormat};
pub use transcribe::{Transcriber, TranscriptSegment, WordTiming};
pub use vision::{VisionAnalyzer, VisionBackend, VisualAnalysis};

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

/// Main analysis pipeline
pub struct AnalysisPipeline {
    config: PipelineConfig,
    frame_extractor: FrameExtractor,
    audio_extractor: AudioExtractor,
    transcriber: Transcriber,
    diarizer: Diarizer,
    vision: VisionAnalyzer,
    fusion: FusionEngine,
}

impl AnalysisPipeline {
    /// Create new pipeline with default configuration
    pub fn new() -> Result<Self> {
        Self::with_config(PipelineConfig::default())
    }

    /// Create pipeline with custom configuration
    pub fn with_config(config: PipelineConfig) -> Result<Self> {
        // Ensure work directory exists
        std::fs::create_dir_all(&config.work_dir)?;

        Ok(Self {
            frame_extractor: FrameExtractor::new(config.scene_threshold, config.max_frames),
            audio_extractor: AudioExtractor::new(),
            transcriber: Transcriber::new(&config.whisper_model, config.dgx_host.clone())?,
            diarizer: Diarizer::new(config.dgx_host.clone())?,
            vision: VisionAnalyzer::new(config.vision_backend.clone(), config.dgx_host.clone())?,
            fusion: FusionEngine::new(),
            config,
        })
    }

    /// Run full analysis pipeline on a video file
    pub async fn analyze(&self, video_path: impl AsRef<Path>) -> Result<AnalysisOutput> {
        let video_path = video_path.as_ref();
        tracing::info!("Starting analysis of: {}", video_path.display());

        // 1. Extract frames and audio in parallel
        let work_dir = &self.config.work_dir;
        let frames_dir = work_dir.join("frames");
        let audio_path = work_dir.join("audio.wav");

        std::fs::create_dir_all(&frames_dir)?;

        // Run extraction
        let (frames, metadata) = self
            .frame_extractor
            .extract(video_path, &frames_dir)
            .await?;
        self.audio_extractor
            .extract(video_path, &audio_path)
            .await?;

        tracing::info!("Extracted {} keyframes", frames.len());

        // 2. Transcribe audio
        let transcript = self.transcriber.transcribe(&audio_path).await?;
        tracing::info!("Transcribed {} segments", transcript.len());

        // 3. Speaker diarization (if enabled)
        let speakers = if self.config.enable_diarization {
            Some(self.diarizer.diarize(&audio_path).await?)
        } else {
            None
        };

        // 4. Visual analysis of keyframes
        let visual_analyses = self.vision.analyze_frames(&frames).await?;
        tracing::info!("Analyzed {} frames visually", visual_analyses.len());

        // 5. Fuse all modalities
        let segments =
            self.fusion
                .fuse(&transcript, speakers.as_deref(), &frames, &visual_analyses)?;

        Ok(AnalysisOutput {
            segments,
            metadata: Some(metadata),
        })
    }

    /// Run analysis with only audio (faster, no vision)
    pub async fn analyze_audio_only(&self, video_path: impl AsRef<Path>) -> Result<AnalysisOutput> {
        let video_path = video_path.as_ref();
        let audio_path = self.config.work_dir.join("audio.wav");

        // Extract audio
        self.audio_extractor
            .extract(video_path, &audio_path)
            .await?;

        // Transcribe
        let transcript = self.transcriber.transcribe(&audio_path).await?;

        // Diarize
        let speakers = if self.config.enable_diarization {
            Some(self.diarizer.diarize(&audio_path).await?)
        } else {
            None
        };

        // Convert to segments without visual
        let segments = transcript
            .iter()
            .map(|t| {
                let speaker = speakers.as_ref().and_then(|s| {
                    s.iter()
                        .find(|sp| sp.start <= t.start && sp.end >= t.end)
                        .map(|sp| sp.speaker.clone())
                });

                AnalysisSegment {
                    start: t.start,
                    end: t.end,
                    speaker,
                    transcript: Some(t.text.clone()),
                    emotion: None,
                    visual: None,
                    flags: vec![],
                }
            })
            .collect();

        Ok(AnalysisOutput {
            segments,
            metadata: None,
        })
    }
}

impl Default for AnalysisPipeline {
    fn default() -> Self {
        Self::new().expect("Failed to create default pipeline")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = PipelineConfig::default();
        assert_eq!(config.scene_threshold, 0.3);
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
