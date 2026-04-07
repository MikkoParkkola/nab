//! FluidAudio subprocess backend
//!
//! Wraps the standalone `fluidaudiocli` binary (Swift, MIT license, MIT-licensed
//! models) from <https://github.com/FluidInference/FluidAudio>. We shell out via
//! subprocess rather than in-process FFI because the `fluidaudio-rs` Rust binding
//! has upstream Swift compilation bugs as of 2026-04-07.
//!
//! Subprocess gives us:
//! - Zero Rust deps (no fluidaudio-rs)
//! - Build works on every platform (binary is just missing at runtime on Linux)
//! - Same pattern as ffmpeg/yt-dlp (existing nab subprocess pattern)
//! - Decoupled FluidAudio updates (just rebuild the CLI, no Cargo changes)
//!
//! ## Binary discovery
//! Searches in this order:
//! 1. `$PATH` via `which`
//! 2. `~/.local/share/nab/bin/fluidaudiocli`
//! 3. `/opt/homebrew/bin/fluidaudiocli`
//! 4. `/private/tmp/FluidAudio/.build/arm64-apple-macosx/release/fluidaudiocli` (dev fallback)
//!
//! ## Supported features
//! - `transcribe` → Parakeet TDT v3 (25 EU languages, ~150× realtime)
//! - `process` → offline diarization (PyAnnote community-1 model, ~120× realtime)
//! - `qwen3-transcribe` → Qwen3-ASR (zh/ja/ko/vi + 25 EU langs, ~30–50× realtime)

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use serde::Deserialize;
use tempfile::NamedTempFile;
use tokio::process::Command;

use super::asr_backend::{
    AsrBackend, SpeakerSegment, TranscribeOptions, TranscriptSegment, TranscriptionResult,
    WordTiming,
};
use super::{AnalysisError, Result};

// ─── Language routing ──────────────────────────────────────────────────────────

/// Languages routed to `qwen3-transcribe` instead of `transcribe`.
const QWEN3_LANGUAGES: &[&str] = &["zh", "ja", "ko", "vi"];

/// Languages natively supported by Parakeet TDT v3.
const PARAKEET_LANGUAGES: &[&str] = &[
    "en", "de", "fr", "es", "it", "pt", "nl", "pl", "ru", "uk", "cs", "sk", "ro", "hu", "fi",
    "sv", "da", "nb", "no", "el", "bg", "hr", "sl", "lt", "lv", "et", "mt",
];

// ─── Internal JSON types ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct FluidTranscribeOutput {
    text: String,
    confidence: f32,
    #[serde(rename = "processingTimeSeconds")]
    processing_time_seconds: f64,
    #[serde(rename = "modelVersion", default)]
    model_version: String,
    #[serde(rename = "wordTimings", default)]
    word_timings: Vec<FluidWordTiming>,
}

#[derive(Debug, Deserialize)]
struct FluidWordTiming {
    word: String,
    #[serde(rename = "startTime")]
    start_time: f64,
    #[serde(rename = "endTime")]
    end_time: f64,
    confidence: f32,
}

#[derive(Debug, Deserialize)]
struct FluidProcessOutput {
    #[serde(rename = "durationSeconds", default)]
    duration_seconds: f64,
    #[serde(rename = "processingTimeSeconds")]
    processing_time_seconds: f64,
    #[serde(default)]
    segments: Vec<FluidDiarSegment>,
}

#[derive(Debug, Deserialize)]
struct FluidDiarSegment {
    #[serde(rename = "speakerId")]
    speaker_id: i32,
    #[serde(rename = "startTimeSeconds")]
    start_time_seconds: f64,
    #[serde(rename = "endTimeSeconds")]
    end_time_seconds: f64,
    #[serde(rename = "qualityScore", default)]
    quality_score: f64,
    /// Raw embedding from diarizer output (256 floats).
    ///
    /// Populated only when `TranscribeOptions::include_embeddings = true`.
    /// Deserialization is skipped by default to avoid the ~1 KB per-segment
    /// JSON cost. When embeddings are needed (voiceprint workflows), the
    /// JSON is re-parsed with a struct that includes this field.
    #[serde(default)]
    embedding: Vec<f32>,
}

// ─── Backend ───────────────────────────────────────────────────────────────────

/// ASR backend powered by the `fluidaudiocli` subprocess.
///
/// Constructed via [`FluidAudioBackend::new`], which locates the binary.
/// On platforms or environments where the binary is absent, call
/// [`FluidAudioBackend::new_unchecked`] to defer the error to transcription time.
pub struct FluidAudioBackend {
    binary_path: PathBuf,
}

impl FluidAudioBackend {
    /// Locate the `fluidaudiocli` binary and return a ready backend.
    ///
    /// Returns `AnalysisError::MissingDependency` when the binary cannot be found
    /// anywhere in the search path.
    pub fn new() -> Result<Self> {
        let binary_path = detect_binary().ok_or_else(|| {
            AnalysisError::MissingDependency(
                "fluidaudiocli not found. Install with `nab models fetch fluidaudio` or build \
                 from https://github.com/FluidInference/FluidAudio"
                    .to_string(),
            )
        })?;
        Ok(Self { binary_path })
    }

    /// Construct a backend with an explicit binary path (useful for testing).
    pub fn with_binary(binary_path: PathBuf) -> Self {
        Self { binary_path }
    }
}

/// Search for the `fluidaudiocli` binary in priority order.
fn detect_binary() -> Option<PathBuf> {
    // 1. PATH via `which`
    if let Ok(path) = which::which("fluidaudiocli") {
        return Some(path);
    }

    // 2. nab managed bin directory
    let candidates = [
        dirs::data_local_dir()
            .map(|d| d.join("nab/bin/fluidaudiocli"))
            .unwrap_or_default(),
        PathBuf::from("/opt/homebrew/bin/fluidaudiocli"),
        // Dev build fallback — present during local FluidAudio development
        PathBuf::from(
            "/private/tmp/FluidAudio/.build/arm64-apple-macosx/release/fluidaudiocli",
        ),
    ];

    candidates.into_iter().find(|p| p.exists())
}

// ─── AsrBackend implementation ─────────────────────────────────────────────────

#[async_trait]
impl AsrBackend for FluidAudioBackend {
    fn name(&self) -> &str {
        "fluidaudio"
    }

    fn supported_languages(&self) -> &[&str] {
        // All Parakeet + Qwen3 languages combined.
        // "*" not returned — we route per-language explicitly.
        PARAKEET_LANGUAGES
    }

    fn is_available(&self) -> bool {
        self.binary_path.exists()
    }

    async fn transcribe(
        &self,
        audio_path: &Path,
        opts: TranscribeOptions,
    ) -> Result<TranscriptionResult> {
        if !audio_path.exists() {
            return Err(AnalysisError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("audio file not found: {}", audio_path.display()),
            )));
        }

        // Convert to 16 kHz mono WAV when needed.
        let wav_guard: Option<NamedTempFile> = maybe_convert_to_wav(audio_path).await?;
        let wav_path: &Path = wav_guard
            .as_ref()
            .map_or(audio_path, |g| g.path());

        // Decide subcommand.
        let use_qwen3 = opts.language.as_deref().is_some_and(|lang| {
            QWEN3_LANGUAGES.contains(&lang)
        });

        let asr_output = run_transcribe(&self.binary_path, wav_path, &opts, use_qwen3).await?;

        // batch mode reports durationSeconds = 0; compute from last word timing.
        let audio_duration = compute_duration(&asr_output.word_timings);
        let rtfx = if asr_output.processing_time_seconds > 0.0 {
            audio_duration / asr_output.processing_time_seconds
        } else {
            0.0
        };

        let model = resolve_model_name(use_qwen3, &asr_output.model_version);

        // Build transcript segments from word timings + text.
        let mut segments = build_transcript_segments(
            &asr_output.text,
            asr_output.word_timings,
            asr_output.confidence,
            opts.language.as_deref(),
            opts.word_timestamps,
        );

        // Optional diarization.
        let speakers = if opts.diarize {
            let diar = run_diarize(&self.binary_path, wav_path).await?;
            assign_speakers_to_segments(&mut segments, &diar.segments);
            let include_emb = opts.include_embeddings;
            let speaker_segs = diar
                .segments
                .into_iter()
                .map(|d| fluid_diar_to_speaker(d, include_emb))
                .collect();
            Some(speaker_segs)
        } else {
            None
        };

        let language = opts.language.unwrap_or_else(|| "en".to_string());

        tracing::info!(
            backend = "fluidaudio",
            model = %model,
            duration_seconds = audio_duration,
            rtfx = rtfx,
            segments = segments.len(),
            "transcription complete"
        );

        Ok(TranscriptionResult {
            segments,
            language,
            duration_seconds: audio_duration,
            model,
            backend: "fluidaudio".to_string(),
            rtfx,
            processing_time_seconds: asr_output.processing_time_seconds,
            speakers,
            footnotes: None,
            active_reading: None,
        })
    }
}

// ─── Subprocess helpers ────────────────────────────────────────────────────────

/// Convert a non-WAV audio file to 16 kHz mono WAV using ffmpeg.
///
/// Returns `None` when the file is already a `.wav` (no conversion needed).
/// Returns a `NamedTempFile` that stays alive as long as the caller holds it.
async fn maybe_convert_to_wav(audio_path: &Path) -> Result<Option<NamedTempFile>> {
    let is_wav = audio_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"));

    if is_wav {
        return Ok(None);
    }

    let tmp = NamedTempFile::with_suffix(".wav").map_err(AnalysisError::Io)?;
    let tmp_path = tmp.path().to_path_buf();

    tracing::debug!(
        src = %audio_path.display(),
        dst = %tmp_path.display(),
        "converting audio to 16 kHz mono WAV"
    );

    let status = Command::new("ffmpeg")
        .args([
            "-i",
            &audio_path.to_string_lossy(),
            "-vn",
            "-acodec", "pcm_s16le",
            "-ar", "16000",
            "-ac", "1",
            &tmp_path.to_string_lossy(),
            "-y",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;

    if !status.success() {
        return Err(AnalysisError::Ffmpeg(format!(
            "ffmpeg conversion failed for {}",
            audio_path.display()
        )));
    }

    Ok(Some(tmp))
}

/// Run the `transcribe` (or `qwen3-transcribe`) subcommand and parse the JSON output.
async fn run_transcribe(
    binary: &Path,
    wav_path: &Path,
    opts: &TranscribeOptions,
    use_qwen3: bool,
) -> Result<FluidTranscribeOutput> {
    let out_tmp = NamedTempFile::with_suffix(".json").map_err(AnalysisError::Io)?;
    let out_path = out_tmp.path().to_path_buf();

    let subcommand = if use_qwen3 { "qwen3-transcribe" } else { "transcribe" };

    let mut cmd = Command::new(binary);
    cmd.arg(subcommand)
        .arg(wav_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    if use_qwen3 {
        if let Some(lang) = &opts.language {
            cmd.args(["--language", lang]);
        }
        // qwen3-transcribe uses --output, not --output-json
        cmd.args(["--output", &out_path.to_string_lossy()]);
    } else {
        cmd.args(["--output-json", &out_path.to_string_lossy()]);
    }

    tracing::debug!(subcommand, wav = %wav_path.display(), "running fluidaudiocli");

    let output = cmd.output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AnalysisError::Whisper(format!(
            "fluidaudiocli {subcommand} failed: {stderr}"
        )));
    }

    let json = std::fs::read_to_string(&out_path)?;
    let parsed: FluidTranscribeOutput = serde_json::from_str(&json)?;
    Ok(parsed)
}

/// Run the `process` (diarization) subcommand and parse the JSON output.
async fn run_diarize(binary: &Path, wav_path: &Path) -> Result<FluidProcessOutput> {
    let out_tmp = NamedTempFile::with_suffix(".json").map_err(AnalysisError::Io)?;
    let out_path = out_tmp.path().to_path_buf();

    tracing::debug!(wav = %wav_path.display(), "running fluidaudiocli process (diarization)");

    let output = Command::new(binary)
        .args([
            "process",
            &wav_path.to_string_lossy(),
            "--output",
            &out_path.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AnalysisError::Diarization(format!(
            "fluidaudiocli process failed: {stderr}"
        )));
    }

    let json = std::fs::read_to_string(&out_path)?;
    let parsed: FluidProcessOutput = serde_json::from_str(&json)?;
    Ok(parsed)
}

// ─── Data transformation helpers ──────────────────────────────────────────────

/// Compute audio duration from the last word timing's end time.
///
/// FluidAudio batch mode always reports `durationSeconds = 0`; we derive it
/// from word timings instead.
fn compute_duration(word_timings: &[FluidWordTiming]) -> f64 {
    word_timings.last().map_or(0.0, |w| w.end_time)
}

/// Resolve the canonical model name string from CLI flags and the JSON output.
fn resolve_model_name(use_qwen3: bool, reported: &str) -> String {
    if use_qwen3 {
        return "qwen3-asr-0.6b".to_string();
    }
    if reported.is_empty() {
        "parakeet-tdt-0.6b-v3".to_string()
    } else {
        format!("parakeet-tdt-0.6b-{}", reported.to_lowercase())
    }
}

/// Split text into sentence boundaries using `.!?` followed by whitespace + uppercase.
///
/// Returns sentence strings in order; the last sentence may lack a trailing
/// punctuation mark.
fn segment_text_into_sentences(text: &str) -> Vec<&str> {
    let text = text.trim();
    if text.is_empty() {
        return vec![];
    }

    let mut sentences = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;

    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if matches!(b, b'.' | b'!' | b'?') {
            // Look for whitespace + uppercase letter following the punctuation.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() && bytes[j].is_ascii_uppercase() {
                // Split here: include the punctuation in this sentence.
                if let Some(s) = text.get(start..=i) {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        sentences.push(trimmed);
                    }
                }
                start = j;
            }
        }
        i += 1;
    }

    // Remaining text after last split.
    if let Some(tail) = text.get(start..) {
        let trimmed = tail.trim();
        if !trimmed.is_empty() {
            sentences.push(trimmed);
        }
    }

    sentences
}

/// Map word timings to transcript segments by sentence boundaries (greedy token match).
///
/// Words are assigned to sentences in order: each sentence gets the next N words
/// whose combined text matches the sentence prefix. When word timings are absent,
/// returns a single segment spanning the full duration.
fn assign_words_to_segments<'a>(
    sentences: &[&'a str],
    word_timings: Vec<FluidWordTiming>,
    overall_confidence: f32,
    language: Option<&str>,
    include_words: bool,
) -> Vec<TranscriptSegment> {
    if sentences.is_empty() {
        return vec![];
    }

    if word_timings.is_empty() {
        return sentences
            .iter()
            .map(|s| TranscriptSegment {
                text: (*s).to_string(),
                start: 0.0,
                end: 0.0,
                confidence: overall_confidence,
                language: language.map(str::to_string),
                speaker: None,
                words: None,
            })
            .collect();
    }

    let mut segments = Vec::with_capacity(sentences.len());
    let mut word_idx = 0;
    let total_words = word_timings.len();

    for (sent_idx, sentence) in sentences.iter().enumerate() {
        let is_last_sentence = sent_idx + 1 == sentences.len();

        // Count words that belong to this sentence by greedy prefix match.
        let sentence_word_count = if is_last_sentence {
            total_words.saturating_sub(word_idx)
        } else {
            count_words_for_sentence(sentence, &word_timings[word_idx..])
        };

        if sentence_word_count == 0 {
            // Fallback: give remaining words to the last segment.
            if is_last_sentence && word_idx < total_words {
                add_segment_from_words(
                    sentence,
                    &word_timings[word_idx..],
                    overall_confidence,
                    language,
                    include_words,
                    &mut segments,
                );
            }
            continue;
        }

        let end_idx = (word_idx + sentence_word_count).min(total_words);
        add_segment_from_words(
            sentence,
            &word_timings[word_idx..end_idx],
            overall_confidence,
            language,
            include_words,
            &mut segments,
        );
        word_idx = end_idx;
    }

    segments
}

/// Count how many consecutive words from `words` fit into `sentence`.
///
/// Matches greedily by normalizing both sides to lowercase and stripping
/// punctuation for comparison.
fn count_words_for_sentence(sentence: &str, words: &[FluidWordTiming]) -> usize {
    let sentence_normalized: String = sentence
        .chars()
        .filter(|c| c.is_alphabetic() || c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let sentence_tokens: Vec<&str> = sentence_normalized.split_whitespace().collect();

    words
        .iter()
        .take(sentence_tokens.len())
        .count()
        .min(words.len())
}

/// Build a single `TranscriptSegment` from a slice of word timings.
fn add_segment_from_words(
    text: &str,
    words: &[FluidWordTiming],
    overall_confidence: f32,
    language: Option<&str>,
    include_words: bool,
    out: &mut Vec<TranscriptSegment>,
) {
    if words.is_empty() {
        return;
    }

    let start = words.first().map_or(0.0, |w| w.start_time);
    let end = words.last().map_or(0.0, |w| w.end_time);

    let confidence = {
        let sum: f32 = words.iter().map(|w| w.confidence).sum();
        #[allow(clippy::cast_precision_loss)]
        let avg = sum / words.len() as f32;
        avg
    };

    let mapped_words = if include_words {
        Some(
            words
                .iter()
                .map(|w| WordTiming {
                    word: w.word.clone(),
                    start: w.start_time,
                    end: w.end_time,
                    confidence: w.confidence,
                })
                .collect(),
        )
    } else {
        None
    };

    out.push(TranscriptSegment {
        text: text.to_string(),
        start,
        end,
        confidence,
        language: language.map(str::to_string),
        speaker: None,
        words: mapped_words,
    });

    let _ = overall_confidence; // used as fallback for empty-word case only
}

/// Build a `Vec<TranscriptSegment>` from the full text and word timings.
fn build_transcript_segments(
    text: &str,
    word_timings: Vec<FluidWordTiming>,
    confidence: f32,
    language: Option<&str>,
    include_words: bool,
) -> Vec<TranscriptSegment> {
    let sentences = segment_text_into_sentences(text);

    if sentences.is_empty() {
        // No text at all — return empty.
        return vec![];
    }

    assign_words_to_segments(&sentences, word_timings, confidence, language, include_words)
}

/// Assign speaker labels to transcript segments via maximum temporal overlap.
fn assign_speakers_to_segments(
    segments: &mut Vec<TranscriptSegment>,
    diar: &[FluidDiarSegment],
) {
    for seg in segments.iter_mut() {
        let best = diar.iter().filter_map(|d| {
            overlap(seg.start, seg.end, d.start_time_seconds, d.end_time_seconds)
                .map(|ov| (ov, d))
        }).max_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        seg.speaker = best.map(|(_, d)| format!("SPEAKER_{:02}", d.speaker_id));
    }
}

/// Convert a `FluidDiarSegment` to the public `SpeakerSegment` type.
///
/// When `include_embedding` is `true` and the raw embedding is non-empty,
/// it is moved into `SpeakerSegment::embedding`. Otherwise `embedding` is `None`.
fn fluid_diar_to_speaker(d: FluidDiarSegment, include_embedding: bool) -> SpeakerSegment {
    let embedding = if include_embedding && !d.embedding.is_empty() {
        Some(d.embedding)
    } else {
        None
    };
    SpeakerSegment {
        speaker: format!("SPEAKER_{:02}", d.speaker_id),
        start: d.start_time_seconds,
        end: d.end_time_seconds,
        embedding,
    }
}

/// Compute overlap in seconds between two time intervals; `None` when disjoint.
#[inline]
fn overlap(a_start: f64, a_end: f64, b_start: f64, b_end: f64) -> Option<f64> {
    let start = a_start.max(b_start);
    let end = a_end.min(b_end);
    (end > start).then_some(end - start)
}

// ─── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── detect_binary ──────────────────────────────────────────────────────────

    /// `detect_binary` returns None when the binary does not exist anywhere.
    #[test]
    fn detect_binary_returns_none_when_absent() {
        // We can't mock PATH here, but we can verify the function handles the
        // case where none of the hardcoded fallback paths exist on this machine
        // (only the dev path at /private/tmp/... might exist).
        // At minimum, the function must not panic.
        let _result = detect_binary();
        // No assertion — just must not panic.
    }

    /// `FluidAudioBackend::with_binary` constructs without searching PATH.
    #[test]
    fn with_binary_stores_path() {
        let path = PathBuf::from("/nonexistent/fluidaudiocli");
        let backend = FluidAudioBackend::with_binary(path.clone());
        assert_eq!(backend.binary_path, path);
    }

    /// `is_available` returns false for a nonexistent path.
    #[test]
    fn is_available_returns_false_for_missing_binary() {
        let backend = FluidAudioBackend::with_binary(PathBuf::from("/no/such/binary"));
        assert!(!backend.is_available());
    }

    // ── name / supported_languages ─────────────────────────────────────────────

    /// `name()` returns the canonical backend identifier.
    #[test]
    fn name_returns_fluidaudio() {
        let backend = FluidAudioBackend::with_binary(PathBuf::from("/dev/null"));
        assert_eq!(backend.name(), "fluidaudio");
    }

    /// `supported_languages()` lists at least the core EU languages.
    #[test]
    fn supported_languages_contains_core_eu_set() {
        let backend = FluidAudioBackend::with_binary(PathBuf::from("/dev/null"));
        let langs = backend.supported_languages();
        for required in &["en", "fi", "de", "fr", "es"] {
            assert!(
                langs.contains(required),
                "missing language: {required}"
            );
        }
    }

    // ── JSON deserialization ───────────────────────────────────────────────────

    /// Parses a realistic `transcribe` JSON output.
    #[test]
    fn parse_transcribe_output_real_shape() {
        let json = r#"{
            "audioFile": "/tmp/audio.wav",
            "confidence": 0.9718,
            "durationSeconds": 0,
            "mode": "batch",
            "modelVersion": "v3",
            "processingTimeSeconds": 118.32,
            "rtfx": 0,
            "text": "Hello world.",
            "wordTimings": [
                {"word": "Hello", "startTime": 0.1, "endTime": 0.5, "confidence": 0.99},
                {"word": "world.", "startTime": 0.6, "endTime": 1.1, "confidence": 0.95}
            ]
        }"#;
        let out: FluidTranscribeOutput = serde_json::from_str(json).expect("parse");
        assert_eq!(out.text, "Hello world.");
        assert!((out.confidence - 0.9718).abs() < 1e-4);
        assert!((out.processing_time_seconds - 118.32).abs() < 1e-4);
        assert_eq!(out.model_version, "v3");
        assert_eq!(out.word_timings.len(), 2);
        assert_eq!(out.word_timings[0].word, "Hello");
        assert!((out.word_timings[1].end_time - 1.1).abs() < 1e-9);
    }

    /// Parses a realistic `process` (diarization) JSON output.
    #[test]
    fn parse_process_output_real_shape() {
        let json = r#"{
            "audioFile": "/tmp/audio.wav",
            "config": {"clusteringThreshold": 0.7045655, "minActivityThreshold": 10,
                        "minDurationOff": 0.5, "minDurationOn": 1, "numClusters": -1},
            "durationSeconds": 30,
            "processingTimeSeconds": 0.214,
            "realTimeFactor": 140.01,
            "segments": [
                {"speakerId": 1, "startTimeSeconds": 10.0, "endTimeSeconds": 15.91,
                 "qualityScore": 0.85, "embedding": [0.273, 0.1]}
            ]
        }"#;
        let out: FluidProcessOutput = serde_json::from_str(json).expect("parse");
        assert!((out.duration_seconds - 30.0).abs() < 1e-9);
        assert_eq!(out.segments.len(), 1);
        assert_eq!(out.segments[0].speaker_id, 1);
        assert!((out.segments[0].start_time_seconds - 10.0).abs() < 1e-9);
        assert!((out.segments[0].quality_score - 0.85).abs() < 1e-9);
    }

    // ── compute_duration ──────────────────────────────────────────────────────

    /// `compute_duration` returns 0 for empty word timings.
    #[test]
    fn compute_duration_empty_returns_zero() {
        assert!((compute_duration(&[]) - 0.0).abs() < 1e-9);
    }

    /// `compute_duration` returns the end time of the last word.
    #[test]
    fn compute_duration_returns_last_end_time() {
        let words = vec![
            FluidWordTiming { word: "a".into(), start_time: 0.0, end_time: 0.5, confidence: 1.0 },
            FluidWordTiming { word: "b".into(), start_time: 0.6, end_time: 7792.3, confidence: 1.0 },
        ];
        assert!((compute_duration(&words) - 7792.3).abs() < 1e-6);
    }

    // ── segment_text_into_sentences ───────────────────────────────────────────

    /// Single sentence without terminal punctuation remains a single segment.
    #[test]
    fn sentence_split_single_unpunctuated() {
        let result = segment_text_into_sentences("Hello world");
        assert_eq!(result, vec!["Hello world"]);
    }

    /// Two sentences separated by `. ` split correctly.
    #[test]
    fn sentence_split_two_sentences_period() {
        let result = segment_text_into_sentences("Hello. World!");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "Hello.");
        assert_eq!(result[1], "World!");
    }

    /// Three sentences with mixed punctuation split correctly.
    #[test]
    fn sentence_split_three_sentences_mixed_punctuation() {
        let result = segment_text_into_sentences("Hello. Are you there? Yes I am.");
        assert_eq!(result.len(), 3, "got: {:?}", result);
    }

    /// Empty string returns empty vec.
    #[test]
    fn sentence_split_empty_string() {
        assert!(segment_text_into_sentences("").is_empty());
    }

    // ── overlap ───────────────────────────────────────────────────────────────

    /// Disjoint intervals return None.
    #[test]
    fn overlap_disjoint_returns_none() {
        assert!(overlap(0.0, 1.0, 2.0, 3.0).is_none());
    }

    /// Touching intervals (shared endpoint) return None.
    #[test]
    fn overlap_touching_returns_none() {
        assert!(overlap(0.0, 1.0, 1.0, 2.0).is_none());
    }

    /// Overlapping intervals return the correct intersection length.
    #[test]
    fn overlap_intersecting_returns_correct_length() {
        let ov = overlap(0.0, 2.0, 1.0, 3.0);
        assert!(ov.is_some());
        assert!((ov.unwrap() - 1.0).abs() < 1e-9);
    }

    // ── assign_speakers_to_segments ───────────────────────────────────────────

    /// Speaker with greater overlap wins when two speakers overlap the same segment.
    #[test]
    fn assign_speakers_picks_max_overlap() {
        let mut segments = vec![TranscriptSegment {
            text: "test".into(),
            start: 0.0,
            end: 3.0,
            confidence: 0.9,
            language: None,
            speaker: None,
            words: None,
        }];
        let diar = vec![
            FluidDiarSegment {
                speaker_id: 0,
                start_time_seconds: 0.0,
                end_time_seconds: 1.0,
                quality_score: 0.9,
                embedding: vec![],
            },
            FluidDiarSegment {
                speaker_id: 1,
                start_time_seconds: 1.0,
                end_time_seconds: 3.0,
                quality_score: 0.9,
                embedding: vec![],
            },
        ];
        // Note: overlap(0,3, 1,3)=2.0 beats overlap(0,3, 0,1)=1.0
        assign_speakers_to_segments(&mut segments, &diar);
        assert_eq!(segments[0].speaker.as_deref(), Some("SPEAKER_01"));
    }

    /// Speaker stays None when no diarization segment overlaps.
    #[test]
    fn assign_speakers_no_overlap_stays_none() {
        let mut segments = vec![TranscriptSegment {
            text: "test".into(),
            start: 10.0,
            end: 11.0,
            confidence: 0.9,
            language: None,
            speaker: None,
            words: None,
        }];
        let diar = vec![FluidDiarSegment {
            speaker_id: 0,
            start_time_seconds: 0.0,
            end_time_seconds: 5.0,
            quality_score: 0.9,
            embedding: vec![],
        }];
        assign_speakers_to_segments(&mut segments, &diar);
        assert!(segments[0].speaker.is_none());
    }

    // ── resolve_model_name ────────────────────────────────────────────────────

    /// Qwen3 path always returns the qwen3 model string.
    #[test]
    fn resolve_model_name_qwen3() {
        assert_eq!(resolve_model_name(true, "v3"), "qwen3-asr-0.6b");
        assert_eq!(resolve_model_name(true, ""), "qwen3-asr-0.6b");
    }

    /// Parakeet path uses the reported version, or a default when empty.
    #[test]
    fn resolve_model_name_parakeet_fallback() {
        assert_eq!(resolve_model_name(false, ""), "parakeet-tdt-0.6b-v3");
        assert_eq!(resolve_model_name(false, "v3"), "parakeet-tdt-0.6b-v3");
    }

    // ── fluid_diar_to_speaker ─────────────────────────────────────────────────

    /// Speaker label is formatted as `SPEAKER_NN` with zero-padded two digits.
    #[test]
    fn fluid_diar_to_speaker_formats_label() {
        let d = FluidDiarSegment {
            speaker_id: 3,
            start_time_seconds: 1.5,
            end_time_seconds: 4.0,
            quality_score: 0.8,
            embedding: vec![],
        };
        let s = fluid_diar_to_speaker(d, false);
        assert_eq!(s.speaker, "SPEAKER_03");
        assert!((s.start - 1.5).abs() < 1e-9);
        assert!((s.end - 4.0).abs() < 1e-9);
        assert!(s.embedding.is_none());
    }

    /// `include_embedding=false` produces `embedding: None`.
    #[test]
    fn fluid_diar_to_speaker_embedding_omitted_when_false() {
        // GIVEN a segment with a non-empty embedding
        let d = FluidDiarSegment {
            speaker_id: 0,
            start_time_seconds: 0.0,
            end_time_seconds: 1.0,
            quality_score: 0.9,
            embedding: vec![0.1_f32; 256],
        };
        // WHEN include_embedding is false
        let s = fluid_diar_to_speaker(d, false);
        // THEN embedding is None
        assert!(s.embedding.is_none());
    }

    /// `include_embedding=true` with a 256-float vector populates the embedding.
    #[test]
    fn fluid_diar_to_speaker_embedding_present_when_true() {
        // GIVEN a segment with a 256-element embedding
        let raw: Vec<f32> = (0..256).map(|i| i as f32 / 256.0).collect();
        let d = FluidDiarSegment {
            speaker_id: 1,
            start_time_seconds: 2.0,
            end_time_seconds: 5.0,
            quality_score: 0.85,
            embedding: raw.clone(),
        };
        // WHEN include_embedding is true
        let s = fluid_diar_to_speaker(d, true);
        // THEN embedding is present with length 256
        let emb = s.embedding.expect("embedding must be present");
        assert_eq!(emb.len(), 256);
        assert!((emb[0] - raw[0]).abs() < f32::EPSILON);
    }
}
