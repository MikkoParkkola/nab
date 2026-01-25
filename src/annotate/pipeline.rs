//! Full annotation pipeline: transcribe -> analyze -> composite
//!
//! Orchestrates the complete workflow from raw video to annotated output.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::fs;
use tokio::io::AsyncWrite;
use tokio::process::Command;
use tracing::{debug, info};

use super::compositor::{Compositor, CompositorConfig};
use super::overlay::{AnalysisOverlay, OverlayPosition, SpeakerLabelOverlay};
use super::subtitle::{AssGenerator, SubtitleEntry, SubtitleGenerator};

/// Configuration for Whisper transcription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    /// Whisper model size (tiny, base, small, medium, large)
    pub model: String,
    /// Language code (e.g., "en", "fi", "auto" for detection)
    pub language: String,
    /// Enable word-level timestamps
    pub word_timestamps: bool,
    /// Enable speaker diarization (requires pyannote)
    pub diarization: bool,
    /// Path to whisper executable (or "whisper" for PATH lookup)
    pub whisper_path: String,
    /// Additional whisper arguments
    pub extra_args: Vec<String>,
    /// Output format from whisper (json for parsing)
    pub output_format: String,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            model: "base".to_string(),
            language: "auto".to_string(),
            word_timestamps: true,
            diarization: false,
            whisper_path: "whisper".to_string(),
            extra_args: Vec::new(),
            output_format: "json".to_string(),
        }
    }
}

impl TranscriptionConfig {
    /// Create config for fast transcription (lower quality)
    #[must_use]
    pub fn fast() -> Self {
        Self {
            model: "tiny".to_string(),
            word_timestamps: false,
            ..Default::default()
        }
    }

    /// Create config for high quality transcription
    #[must_use]
    pub fn high_quality() -> Self {
        Self {
            model: "large".to_string(),
            word_timestamps: true,
            diarization: true,
            ..Default::default()
        }
    }

    /// Set model size
    #[must_use]
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    /// Set language
    #[must_use]
    pub fn with_language(mut self, language: &str) -> Self {
        self.language = language.to_string();
        self
    }

    /// Enable diarization
    #[must_use]
    pub fn with_diarization(mut self) -> Self {
        self.diarization = true;
        self
    }
}

/// Configuration for analysis overlays
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct AnalysisConfig {
    /// Enable emotion/sentiment analysis
    pub emotion_analysis: bool,
    /// Enable behavioral analysis
    pub behavioral_analysis: bool,
    /// Custom analysis script path
    pub analysis_script: Option<String>,
    /// Analysis model or API endpoint
    pub analysis_model: Option<String>,
}


impl AnalysisConfig {
    /// Enable all analysis features
    #[must_use]
    pub fn full() -> Self {
        Self {
            emotion_analysis: true,
            behavioral_analysis: true,
            ..Default::default()
        }
    }
}

/// Full pipeline configuration
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Transcription settings
    pub transcription: TranscriptionConfig,
    /// Analysis settings
    pub analysis: AnalysisConfig,
    /// Compositor settings
    pub compositor: CompositorConfig,
    /// Temporary directory for intermediate files
    pub temp_dir: PathBuf,
    /// Include speaker labels in output
    pub speaker_labels: bool,
    /// Include analysis overlay in output
    pub analysis_overlay: bool,
    /// Include main subtitles in output
    pub subtitles: bool,
    /// Speaker label position
    pub speaker_position: OverlayPosition,
    /// Analysis overlay position
    pub analysis_position: OverlayPosition,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            transcription: TranscriptionConfig::default(),
            analysis: AnalysisConfig::default(),
            compositor: CompositorConfig::default(),
            temp_dir: std::env::temp_dir().join("microfetch_annotate"),
            speaker_labels: true,
            analysis_overlay: false,
            subtitles: true,
            speaker_position: OverlayPosition::TopLeft,
            analysis_position: OverlayPosition::TopRight,
        }
    }
}

impl PipelineConfig {
    /// Create config for streaming output
    #[must_use]
    pub fn streaming() -> Self {
        Self {
            compositor: CompositorConfig::streaming(),
            ..Default::default()
        }
    }

    /// Create config for high-quality file output
    #[must_use]
    pub fn high_quality() -> Self {
        Self {
            transcription: TranscriptionConfig::high_quality(),
            analysis: AnalysisConfig::full(),
            compositor: CompositorConfig::high_quality(),
            analysis_overlay: true,
            ..Default::default()
        }
    }

    /// Enable speaker labels
    #[must_use]
    pub fn with_speaker_labels(mut self, enabled: bool) -> Self {
        self.speaker_labels = enabled;
        self
    }

    /// Enable analysis overlay
    #[must_use]
    pub fn with_analysis(mut self, enabled: bool) -> Self {
        self.analysis_overlay = enabled;
        if enabled {
            self.analysis = AnalysisConfig::full();
        }
        self
    }
}

/// Result of pipeline processing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineResult {
    /// Transcribed text (full)
    pub transcript: String,
    /// Subtitle entries generated
    pub subtitle_count: usize,
    /// Detected language
    pub detected_language: Option<String>,
    /// Speaker segments (if diarization enabled)
    pub speakers: Vec<String>,
    /// Analysis results (if analysis enabled)
    pub analysis_results: HashMap<String, String>,
    /// Output file path (if file output)
    pub output_path: Option<PathBuf>,
    /// Processing time in seconds
    pub processing_time_secs: f64,
}

/// Whisper transcription output format (JSON)
#[derive(Debug, Clone, Deserialize)]
struct WhisperOutput {
    text: String,
    segments: Vec<WhisperSegment>,
    language: String,
}

#[derive(Debug, Clone, Deserialize)]
struct WhisperSegment {
    id: u32,
    start: f64,
    end: f64,
    text: String,
    #[serde(default)]
    words: Vec<WhisperWord>,
    #[serde(default)]
    speaker: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct WhisperWord {
    word: String,
    start: f64,
    end: f64,
    #[serde(default)]
    probability: f64,
}

/// Full annotation pipeline
pub struct AnnotationPipeline {
    config: PipelineConfig,
    compositor: Compositor,
}

impl AnnotationPipeline {
    /// Create a new annotation pipeline
    pub fn new(config: PipelineConfig) -> Result<Self> {
        // Ensure temp directory exists
        std::fs::create_dir_all(&config.temp_dir)?;

        let compositor = Compositor::with_config(config.compositor.clone());

        Ok(Self { config, compositor })
    }

    /// Create pipeline with default config
    pub fn default_pipeline() -> Result<Self> {
        Self::new(PipelineConfig::default())
    }

    /// Check if all required tools are available
    pub async fn check_dependencies(&self) -> Result<Vec<(String, bool)>> {
        let mut results = Vec::new();

        // Check ffmpeg
        let ffmpeg_ok = self.compositor.check_available().await;
        results.push(("ffmpeg".to_string(), ffmpeg_ok));

        // Check whisper
        let whisper_ok = Command::new(&self.config.transcription.whisper_path)
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        results.push(("whisper".to_string(), whisper_ok));

        Ok(results)
    }

    /// Extract audio from video for transcription
    async fn extract_audio(&self, input: &Path) -> Result<PathBuf> {
        let audio_path = self
            .config
            .temp_dir
            .join(format!("{}.wav", uuid::Uuid::new_v4()));

        let status = Command::new(&self.config.compositor.ffmpeg_path)
            .args([
                "-i",
                &input.to_string_lossy(),
                "-vn",
                "-acodec",
                "pcm_s16le",
                "-ar",
                "16000",
                "-ac",
                "1",
                "-y",
                &audio_path.to_string_lossy(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;

        if !status.success() {
            return Err(anyhow!("Failed to extract audio from video"));
        }

        Ok(audio_path)
    }

    /// Run Whisper transcription on audio file
    async fn transcribe_audio(&self, audio_path: &Path) -> Result<WhisperOutput> {
        let output_dir = self.config.temp_dir.clone();

        let mut args = vec![
            audio_path.to_string_lossy().to_string(),
            "--model".to_string(),
            self.config.transcription.model.clone(),
            "--output_format".to_string(),
            "json".to_string(),
            "--output_dir".to_string(),
            output_dir.to_string_lossy().to_string(),
        ];

        if self.config.transcription.language != "auto" {
            args.push("--language".to_string());
            args.push(self.config.transcription.language.clone());
        }

        if self.config.transcription.word_timestamps {
            args.push("--word_timestamps".to_string());
            args.push("True".to_string());
        }

        args.extend(self.config.transcription.extra_args.clone());

        debug!("Running whisper with args: {:?}", args);

        let output = Command::new(&self.config.transcription.whisper_path)
            .args(&args)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Whisper transcription failed: {stderr}"));
        }

        // Find the output JSON file
        let stem = audio_path.file_stem().unwrap().to_string_lossy();
        let json_path = output_dir.join(format!("{stem}.json"));

        if !json_path.exists() {
            return Err(anyhow!("Whisper output file not found: {json_path:?}"));
        }

        let json_content = fs::read_to_string(&json_path).await?;
        let whisper_output: WhisperOutput = serde_json::from_str(&json_content)?;

        // Cleanup
        let _ = fs::remove_file(&json_path).await;

        Ok(whisper_output)
    }

    /// Convert Whisper output to subtitle entries
    fn whisper_to_subtitles(&self, whisper: &WhisperOutput) -> Vec<SubtitleEntry> {
        whisper
            .segments
            .iter()
            .map(|seg| {
                let start_ms = (seg.start * 1000.0) as u64;
                let end_ms = (seg.end * 1000.0) as u64;
                let text = seg.text.trim().to_string();

                let mut entry = SubtitleEntry::new(start_ms, end_ms, text);

                if let Some(ref speaker) = seg.speaker {
                    entry = entry.with_speaker(speaker.clone());
                }

                entry
            })
            .collect()
    }

    /// Extract speaker segments from Whisper output
    fn extract_speaker_segments(&self, whisper: &WhisperOutput) -> Vec<(u64, u64, String)> {
        whisper
            .segments
            .iter()
            .filter_map(|seg| {
                seg.speaker.as_ref().map(|speaker| {
                    let start_ms = (seg.start * 1000.0) as u64;
                    let end_ms = (seg.end * 1000.0) as u64;
                    (start_ms, end_ms, speaker.clone())
                })
            })
            .collect()
    }

    /// Process a video file and output to another file
    pub async fn process_file(&self, input: &str, output: &str) -> Result<PipelineResult> {
        let start_time = std::time::Instant::now();
        let input_path = Path::new(input);
        let output_path = Path::new(output);

        info!("Starting annotation pipeline for {:?}", input_path);

        // Step 1: Extract audio
        info!("Extracting audio...");
        let audio_path = self.extract_audio(input_path).await?;

        // Step 2: Transcribe
        info!("Transcribing with Whisper ({})...", self.config.transcription.model);
        let whisper_output = self.transcribe_audio(&audio_path).await?;

        // Cleanup audio file
        let _ = fs::remove_file(&audio_path).await;

        // Step 3: Generate subtitles
        let subtitles = self.whisper_to_subtitles(&whisper_output);
        info!("Generated {} subtitle entries", subtitles.len());

        // Step 4: Generate overlay tracks
        let mut overlay_tracks = Vec::new();

        // Speaker labels
        if self.config.speaker_labels {
            let speaker_segments = self.extract_speaker_segments(&whisper_output);
            if !speaker_segments.is_empty() {
                let speaker_overlay =
                    SpeakerLabelOverlay::new().with_position(self.config.speaker_position);
                overlay_tracks.push(speaker_overlay.generate(&speaker_segments));
                info!("Generated speaker labels for {} segments", speaker_segments.len());
            }
        }

        // Analysis overlay (placeholder - would integrate with actual analysis)
        if self.config.analysis_overlay && self.config.analysis.emotion_analysis {
            // This is a placeholder - in a real implementation, this would
            // call an emotion detection model/API
            let _analysis_overlay = AnalysisOverlay::new().with_position(self.config.analysis_position);
            // Would generate real analysis data here
            info!("Analysis overlay enabled (placeholder)");
        }

        // Step 5: Generate combined ASS file
        let ass_path = self
            .config
            .temp_dir
            .join(format!("{}.ass", uuid::Uuid::new_v4()));

        if self.config.subtitles {
            self.compositor
                .generate_combined_ass(&subtitles, &overlay_tracks, &ass_path)
                .await?;
            info!("Generated ASS subtitle file");
        }

        // Step 6: Composite video
        info!("Compositing video with overlays...");
        self.compositor
            .composite_to_file(
                input,
                output_path,
                if self.config.subtitles {
                    Some(&ass_path)
                } else {
                    None
                },
                &overlay_tracks,
            )
            .await?;

        // Cleanup ASS file
        let _ = fs::remove_file(&ass_path).await;

        let elapsed = start_time.elapsed().as_secs_f64();
        info!("Pipeline completed in {:.2}s", elapsed);

        let speakers: Vec<String> = whisper_output
            .segments
            .iter()
            .filter_map(|s| s.speaker.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        Ok(PipelineResult {
            transcript: whisper_output.text,
            subtitle_count: subtitles.len(),
            detected_language: Some(whisper_output.language),
            speakers,
            analysis_results: HashMap::new(),
            output_path: Some(output_path.to_path_buf()),
            processing_time_secs: elapsed,
        })
    }

    /// Process a video and stream output
    pub async fn process_to_stream<W: AsyncWrite + Unpin + Send>(
        &self,
        input: &str,
        output: &mut W,
    ) -> Result<PipelineResult> {
        let start_time = std::time::Instant::now();
        let input_path = Path::new(input);

        info!("Starting streaming annotation pipeline for {:?}", input_path);

        // Same process as file, but stream at the end
        let audio_path = self.extract_audio(input_path).await?;
        let whisper_output = self.transcribe_audio(&audio_path).await?;
        let _ = fs::remove_file(&audio_path).await;

        let subtitles = self.whisper_to_subtitles(&whisper_output);

        let mut overlay_tracks = Vec::new();
        if self.config.speaker_labels {
            let speaker_segments = self.extract_speaker_segments(&whisper_output);
            if !speaker_segments.is_empty() {
                let speaker_overlay =
                    SpeakerLabelOverlay::new().with_position(self.config.speaker_position);
                overlay_tracks.push(speaker_overlay.generate(&speaker_segments));
            }
        }

        let ass_path = self
            .config
            .temp_dir
            .join(format!("{}.ass", uuid::Uuid::new_v4()));

        if self.config.subtitles {
            self.compositor
                .generate_combined_ass(&subtitles, &overlay_tracks, &ass_path)
                .await?;
        }

        // Stream output
        self.compositor
            .composite_to_stream(
                input,
                if self.config.subtitles {
                    Some(&ass_path)
                } else {
                    None
                },
                &overlay_tracks,
                output,
            )
            .await?;

        let _ = fs::remove_file(&ass_path).await;

        let elapsed = start_time.elapsed().as_secs_f64();

        let speakers: Vec<String> = whisper_output
            .segments
            .iter()
            .filter_map(|s| s.speaker.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        Ok(PipelineResult {
            transcript: whisper_output.text,
            subtitle_count: subtitles.len(),
            detected_language: Some(whisper_output.language),
            speakers,
            analysis_results: HashMap::new(),
            output_path: None,
            processing_time_secs: elapsed,
        })
    }

    /// Generate subtitles only (no video compositing)
    pub async fn generate_subtitles_only(
        &self,
        input: &str,
        output_path: &Path,
    ) -> Result<PipelineResult> {
        let start_time = std::time::Instant::now();
        let input_path = Path::new(input);

        info!("Generating subtitles for {:?}", input_path);

        let audio_path = self.extract_audio(input_path).await?;
        let whisper_output = self.transcribe_audio(&audio_path).await?;
        let _ = fs::remove_file(&audio_path).await;

        let subtitles = self.whisper_to_subtitles(&whisper_output);

        // Generate ASS file
        let generator = AssGenerator::new();
        generator.write_to_file(&subtitles, output_path).await?;

        let elapsed = start_time.elapsed().as_secs_f64();

        let speakers: Vec<String> = whisper_output
            .segments
            .iter()
            .filter_map(|s| s.speaker.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        Ok(PipelineResult {
            transcript: whisper_output.text,
            subtitle_count: subtitles.len(),
            detected_language: Some(whisper_output.language),
            speakers,
            analysis_results: HashMap::new(),
            output_path: Some(output_path.to_path_buf()),
            processing_time_secs: elapsed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transcription_config_fast() {
        let config = TranscriptionConfig::fast();
        assert_eq!(config.model, "tiny");
        assert!(!config.word_timestamps);
    }

    #[test]
    fn test_transcription_config_high_quality() {
        let config = TranscriptionConfig::high_quality();
        assert_eq!(config.model, "large");
        assert!(config.word_timestamps);
        assert!(config.diarization);
    }

    #[test]
    fn test_pipeline_config_streaming() {
        use crate::annotate::compositor::CompositorOutput;
        let config = PipelineConfig::streaming();
        assert_eq!(
            config.compositor.output_format,
            CompositorOutput::MpegTs
        );
    }

    #[test]
    fn test_pipeline_config_high_quality() {
        let config = PipelineConfig::high_quality();
        assert_eq!(config.transcription.model, "large");
        assert!(config.analysis_overlay);
        assert!(config.analysis.emotion_analysis);
    }
}
