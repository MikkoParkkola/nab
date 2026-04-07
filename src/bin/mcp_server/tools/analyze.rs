//! `analyze` MCP tool — transcribe audio/video with multilingual SOTA ASR.
//!
//! Delegates to [`nab::analyze::default_backend`] which selects:
//! - FluidAudio (Parakeet TDT v3, CoreML, 143× RTFx) on macOS Apple Silicon
//! - A stub returning `MissingDependency` on all other platforms
//!
//! For video files the audio track is first extracted with `ffmpeg` via
//! [`nab::analyze::AudioExtractor`] into a temporary WAV file, then passed
//! to the ASR backend.  Pure audio files (`.wav`, `.mp3`, `.flac`, `.m4a`,
//! `.aac`, `.ogg`) are passed directly without extraction.

use std::path::PathBuf;

use nab::analyze::{AudioExtractor, TranscribeOptions, default_backend};
use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

// ─── Tool definition ──────────────────────────────────────────────────────────

#[mcp_tool(
    name = "analyze",
    description = "Transcribe audio or video file with multilingual SOTA ASR.

Returns JSON with text, segments, word-level timestamps, and optional speaker
diarization.

Supported inputs:
- Audio: .wav, .mp3, .flac, .m4a, .aac, .ogg (passed directly to ASR)
- Video: .mp4, .mkv, .mov, .avi, .webm (audio extracted via ffmpeg first)

Language support (Parakeet TDT v3, macOS Apple Silicon):
- English, German, French, Spanish, Italian, Portuguese, Dutch, Polish, Russian,
  Ukrainian, Czech, Slovak, Romanian, Hungarian, Finnish, Swedish, Danish,
  Norwegian, Greek, Turkish, Arabic, Hebrew, Hindi, Japanese, Chinese

Backend:
- macOS Apple Silicon: FluidAudio (CoreML, Neural Engine, ~143× realtime)
- Other platforms: returns backend unavailability error

Returns: JSON-serialized TranscriptionResult with segments, language, RTFx,
processing time, and optional speaker diarization.",
    read_only_hint = true,
    open_world_hint = false
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyzeTool {
    /// Local file path to the audio or video to transcribe.
    ///
    /// Only local paths are supported in Phase 1. URL support (HTTP download
    /// before transcription) is planned for Phase 2.
    pub input: String,

    /// BCP-47 language hint, e.g. `"fi"`, `"en-US"`, `"zh"`.
    ///
    /// When omitted the backend performs automatic language detection.
    /// Providing a hint avoids the detection step and may improve accuracy for
    /// short clips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    /// Enable speaker diarization.
    ///
    /// When `true`, the FluidAudio VBx diarizer runs after transcription and
    /// assigns a speaker label (e.g. `"SPEAKER_00"`) to each segment.
    /// Adds ~20–50 ms of processing on typical recordings.
    #[serde(default)]
    pub diarize: bool,

    /// Backend override.
    ///
    /// Omit for automatic selection (recommended). Accepted values:
    /// `"fluidaudio"` (macOS arm64 only), `"sherpa-onnx"` (Phase 3),
    /// `"whisper-rs"` (Phase 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

impl AnalyzeTool {
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let input_path = PathBuf::from(&self.input);

        tracing::info!(
            input = %self.input,
            language = ?self.language,
            diarize = self.diarize,
            "analyze start"
        );

        // ── Validate input exists ──────────────────────────────────────────────
        if !input_path.exists() {
            return Err(CallToolError::from_message(format!(
                "File not found: {}",
                self.input
            )));
        }

        // ── Extract audio from video if needed ─────────────────────────────────
        let audio_path = extract_audio_if_needed(&input_path).await?;

        // ── Build transcription options ────────────────────────────────────────
        let opts = TranscribeOptions {
            language: self.language.clone(),
            word_timestamps: true,
            diarize: self.diarize,
            max_duration_seconds: None,
        };

        // ── Dispatch to backend ────────────────────────────────────────────────
        let backend = default_backend();
        tracing::info!(backend = %backend.name(), "using ASR backend");

        if !backend.is_available() {
            return Err(CallToolError::from_message(format!(
                "ASR backend '{}' is not available on this platform. \
                 Install fluidaudiocli with `nab models fetch fluidaudio` or build from \
                 https://github.com/FluidInference/FluidAudio",
                backend.name()
            )));
        }

        let result = backend
            .transcribe(&audio_path, opts)
            .await
            .map_err(|e| CallToolError::from_message(format!("transcription failed: {e}")))?;

        tracing::info!(
            segments = result.segments.len(),
            rtfx = result.rtfx,
            backend = %result.backend,
            "analyze complete"
        );

        // ── Clean up temp audio file ───────────────────────────────────────────
        if audio_path != input_path {
            let _ = tokio::fs::remove_file(&audio_path).await;
        }

        // ── Serialize and return ───────────────────────────────────────────────
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| CallToolError::from_message(format!("serialization failed: {e}")))?;

        let structured = serde_json::to_value(&result)
            .ok()
            .and_then(|v| v.as_object().cloned());

        let mut call_result = CallToolResult::text_content(vec![TextContent::from(json)]);
        call_result.structured_content = structured;
        Ok(call_result)
    }
}

// ─── Audio extraction helper ──────────────────────────────────────────────────

/// Audio file extensions that can be passed directly to the ASR backend.
const AUDIO_EXTENSIONS: &[&str] = &["wav", "mp3", "flac", "m4a", "aac", "ogg", "opus"];

/// Return `true` if the file extension indicates a pure audio file.
fn is_audio_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            let lower = ext.to_ascii_lowercase();
            AUDIO_EXTENSIONS.iter().any(|&a| a == lower)
        })
}

/// Extract audio to a temporary WAV file if the input is a video.
///
/// Returns the original path unchanged for pure audio files, or a new
/// temporary path `{tmpdir}/nab_analyze_{pid}.wav` for video inputs.
/// Callers are responsible for removing the temp file after use.
async fn extract_audio_if_needed(input: &std::path::Path) -> Result<PathBuf, CallToolError> {
    if is_audio_file(input) {
        return Ok(input.to_path_buf());
    }

    let tmp_path = std::env::temp_dir().join(format!(
        "nab_analyze_{}.wav",
        std::process::id()
    ));

    tracing::info!(
        video = %input.display(),
        output = %tmp_path.display(),
        "extracting audio from video"
    );

    AudioExtractor::new()
        .extract(input, &tmp_path)
        .await
        .map_err(|e| CallToolError::from_message(format!("audio extraction failed: {e}")))?;

    Ok(tmp_path)
}
