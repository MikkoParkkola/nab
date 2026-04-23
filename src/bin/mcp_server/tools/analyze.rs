//! `analyze` MCP tool — transcribe audio/video with multilingual SOTA ASR.
//!
//! Delegates to [`nab::analyze::default_backend`] which selects:
//! - `FluidAudio` (`Parakeet TDT v3`, `CoreML`, 143× `RTFx`) on macOS Apple Silicon
//! - A stub returning `MissingDependency` on all other platforms
//!
//! For video files the audio track is first extracted with `ffmpeg` via
//! [`nab::analyze::AudioExtractor`] into a temporary WAV file, then passed
//! to the ASR backend.  Pure audio files (`.wav`, `.mp3`, `.flac`, `.m4a`,
//! `.aac`, `.ogg`) are passed directly without extraction.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use nab::analyze::{AudioExtractor, TranscribeOptions, default_backend};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use crate::hebb_client::HebbClient;

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

Active reading (active_reading=true):
- Identifies papers, people, tools, and claims in the transcript via MCP sampling
- Fetches and summarises each reference
- Inlines numbered footnotes into the transcript segments
- Requires the MCP client to support sampling/createMessage

Returns: JSON-serialized TranscriptionResult with segments, language, RTFx,
processing time, optional speaker diarization, and optional footnotes.",
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
    ///
    /// Omit (or pass empty string) for automatic detection.
    #[serde(default)]
    pub language: String,

    /// Enable speaker diarization.
    ///
    /// When `true`, the `FluidAudio` `VBx` diarizer runs after transcription and
    /// assigns a speaker label (e.g. `"SPEAKER_00"`) to each segment.
    /// Adds ~20–50 ms of processing on typical recordings.
    #[serde(default)]
    pub diarize: bool,

    /// Backend override.
    ///
    /// Omit for automatic selection (recommended). Accepted values:
    /// `"fluidaudio"` (macOS arm64 only), `"sherpa-onnx"` (Phase 3),
    /// `"whisper-rs"` (Phase 3).
    ///
    /// Pass empty string for automatic selection (default).
    #[serde(default)]
    pub backend: String,

    /// Enable active reading — live reference lookup during transcription.
    ///
    /// When `true`, the host LLM is asked (via `sampling/createMessage`) to
    /// identify references in the transcript chunks — papers, people, tools, and
    /// claims — and each surviving reference is fetched and summarised as a
    /// numbered footnote inlined into the transcript.
    ///
    /// Requires the MCP client to advertise `sampling` capability. Falls back
    /// to passive transcription with a warning if sampling is unavailable.
    #[serde(default)]
    pub active_reading: bool,

    /// Include 256-dimensional speaker embeddings in diarization output.
    ///
    /// When `true` (and `diarize = true`), each entry in `speakers[]` gains an
    /// `embedding` field containing the raw diarizer embedding vector. Use this
    /// with the `match-speakers-with-hebb` prompt to resolve `SPEAKER_NN` labels
    /// to real names via the hebb voiceprint database.
    ///
    /// Omitted by default — embeddings add ~1 KB of JSON per speaker turn and
    /// are only needed for voiceprint workflows.
    #[serde(default)]
    pub include_embeddings: bool,
}

impl AnalyzeTool {
    /// Run the analyze tool.
    ///
    /// `runtime` is passed through to the active-reading sampler when
    /// `self.active_reading` is `true`. It is unused otherwise.
    pub async fn run(&self, runtime: &Arc<dyn McpServer>) -> Result<CallToolResult, CallToolError> {
        let input_path = PathBuf::from(&self.input);

        tracing::info!(
            input = %self.input,
            language = ?self.language,
            diarize = self.diarize,
            active_reading = self.active_reading,
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
            language: if self.language.is_empty() { None } else { Some(self.language.clone()) },
            word_timestamps: true,
            diarize: self.diarize,
            max_duration_seconds: None,
            include_embeddings: self.include_embeddings,
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

        let mut result = backend
            .transcribe(&audio_path, opts)
            .await
            .map_err(|e| CallToolError::from_message(format!("transcription failed: {e}")))?;

        tracing::info!(
            segments = result.segments.len(),
            rtfx = result.rtfx,
            backend = %result.backend,
            "analyze complete"
        );

        // ── hebb voice matching ────────────────────────────────────────────────
        if self.diarize
            && self.include_embeddings
            && let Some(ref mut speakers) = result.speakers
        {
            let speaker_map = match match_speakers_with_hebb(speakers).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("hebb voice match skipped: {e}");
                    HashMap::new()
                }
            };
            if !speaker_map.is_empty() {
                apply_speaker_names(&mut result.segments, &speaker_map);
            }
        }

        // ── Active reading pass ────────────────────────────────────────────────
        if self.active_reading {
            apply_active_reading(&mut result, runtime).await;
        }

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

// ─── Active reading helper ────────────────────────────────────────────────────

/// Run the active-reading pass on `result`.
///
/// Failures are logged as warnings; the transcript is returned unmodified on any
/// error so the caller always has a usable (passive) result.
async fn apply_active_reading(
    result: &mut nab::analyze::TranscriptionResult,
    runtime: &Arc<dyn McpServer>,
) {
    use crate::active_reading_mcp::{McpLlmSampler, NabUrlFetcher};
    use nab::analyze::{ActiveReader, ActiveReadingConfig};

    if !crate::sampling::is_supported(runtime) {
        tracing::warn!(
            "active reading requested but the MCP client does not support sampling; \
             falling back to passive transcription"
        );
        return;
    }

    let sampler = McpLlmSampler::new(runtime.clone());
    let client = match nab::AcceleratedClient::new() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::warn!("active reading: could not create HTTP client: {e}");
            return;
        }
    };
    let fetcher = NabUrlFetcher::new(client);

    let mut reader = ActiveReader::new(&sampler, &fetcher, ActiveReadingConfig::default());

    match reader.process(result).await {
        Ok(output) => {
            tracing::info!(
                footnotes = output.footnotes.len(),
                tokens_spent = output.metadata.tokens_spent,
                "active reading complete"
            );
            result.footnotes = Some(output.footnotes);
            result.active_reading = Some(output.metadata);
        }
        Err(e) => {
            tracing::warn!("active reading failed: {e}; returning passive transcript");
        }
    }
}

// ─── hebb voice-match helper ─────────────────────────────────────────────────

const VOICE_MATCH_THRESHOLD: f32 = 0.7;
const VOICE_MATCH_LIMIT: u32 = 3;

/// For each speaker segment that carries an embedding, query hebb's
/// `voice_match` tool.  Returns a map of `speaker_label → resolved_name`
/// for every speaker that matched at or above [`VOICE_MATCH_THRESHOLD`].
///
/// Silently returns an empty map when hebb is unavailable.
async fn match_speakers_with_hebb(
    speakers: &[nab::analyze::AsrSpeakerSegment],
) -> anyhow::Result<HashMap<String, String>> {
    if !HebbClient::is_available() {
        return Ok(HashMap::new());
    }

    let client_arc = HebbClient::global().await?;
    let mut map = HashMap::new();

    for seg in speakers {
        let embedding = match &seg.embedding {
            Some(e) if !e.is_empty() => e,
            _ => continue,
        };

        let mut client = client_arc.lock().await;
        match client
            .voice_match(embedding, VOICE_MATCH_THRESHOLD, VOICE_MATCH_LIMIT)
            .await
        {
            Ok(matches) => {
                if let Some(best) = matches
                    .into_iter()
                    .find(|m| m.similarity >= VOICE_MATCH_THRESHOLD)
                    && let Some(name) = best.name
                {
                    map.insert(seg.speaker.clone(), name);
                }
            }
            Err(e) => {
                tracing::warn!(speaker = %seg.speaker, "voice_match failed: {e}");
            }
        }
    }

    Ok(map)
}

/// Replace `SPEAKER_NN` labels in transcript segments with resolved names
/// from `speaker_map`.  Segments whose speaker label has no mapping are
/// left unchanged.
fn apply_speaker_names(
    segments: &mut [nab::analyze::AsrTranscriptSegment],
    speaker_map: &HashMap<String, String>,
) {
    for seg in segments {
        if let Some(label) = &seg.speaker
            && let Some(name) = speaker_map.get(label)
        {
            seg.speaker = Some(name.clone());
        }
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

    let tmp_path = std::env::temp_dir().join(format!("nab_analyze_{}.wav", std::process::id()));

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

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use nab::analyze::{AsrSpeakerSegment, AsrTranscriptSegment};

    use super::*;

    fn make_segment(speaker: Option<&str>) -> AsrTranscriptSegment {
        AsrTranscriptSegment {
            text: "hello".to_string(),
            start: 0.0,
            end: 1.0,
            confidence: 1.0,
            language: None,
            speaker: speaker.map(String::from),
            words: None,
        }
    }

    fn make_speaker(label: &str, embedding: Option<Vec<f32>>) -> AsrSpeakerSegment {
        AsrSpeakerSegment {
            speaker: label.to_string(),
            start: 0.0,
            end: 1.0,
            embedding,
        }
    }

    // ── apply_speaker_names ──────────────────────────────────────────────────

    /// Segments with a mapped speaker label get the resolved name applied.
    #[test]
    fn apply_speaker_names_replaces_matched_label() {
        // GIVEN a segment with SPEAKER_00 and a map resolving it to "Alice"
        let mut segments = vec![make_segment(Some("SPEAKER_00"))];
        let mut map = HashMap::new();
        map.insert("SPEAKER_00".to_string(), "Alice".to_string());
        // WHEN we apply the names
        apply_speaker_names(&mut segments, &map);
        // THEN the label is replaced
        assert_eq!(segments[0].speaker.as_deref(), Some("Alice"));
    }

    /// Segments whose label is not in the map are left unchanged.
    #[test]
    fn apply_speaker_names_leaves_unmatched_label_unchanged() {
        // GIVEN a segment with SPEAKER_01, a map with only SPEAKER_00
        let mut segments = vec![make_segment(Some("SPEAKER_01"))];
        let mut map = HashMap::new();
        map.insert("SPEAKER_00".to_string(), "Alice".to_string());
        // WHEN applied
        apply_speaker_names(&mut segments, &map);
        // THEN SPEAKER_01 is unchanged
        assert_eq!(segments[0].speaker.as_deref(), Some("SPEAKER_01"));
    }

    /// Segments with no speaker label are unaffected.
    #[test]
    fn apply_speaker_names_skips_segments_without_speaker() {
        // GIVEN a segment with no speaker label
        let mut segments = vec![make_segment(None)];
        let map: HashMap<String, String> = HashMap::new();
        // WHEN applied
        apply_speaker_names(&mut segments, &map);
        // THEN the speaker remains None
        assert!(segments[0].speaker.is_none());
    }

    // ── match_speakers_with_hebb (hebb unavailable) ──────────────────────────

    /// When hebb is unavailable, `match_speakers_with_hebb` returns an empty map
    /// without error so callers do not need to handle the unavailability case.
    #[tokio::test]
    async fn match_speakers_with_hebb_returns_empty_when_hebb_unavailable() {
        // GIVEN hebb-mcp is not installed (CI environment)
        // AND a speaker segment with an embedding
        let speakers = vec![make_speaker("SPEAKER_00", Some(vec![0.1; 256]))];
        // WHEN we check availability (cheap, no subprocess)
        let available = HebbClient::is_available();
        if available {
            // Skip: hebb is actually installed — cannot test the fallback path.
            return;
        }
        // THEN match returns Ok(empty map) — no crash, no error propagated
        let result = match_speakers_with_hebb(&speakers).await;
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert!(result.unwrap().is_empty());
    }
}
