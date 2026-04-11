//! Sherpa-ONNX ASR backend for cross-platform transcription.
//!
//! Runs Parakeet TDT v3 ONNX models via the sherpa-onnx C++ runtime with
//! official Rust bindings. Works on:
//! - Linux `x86_64` (CPU + CUDA)
//! - Linux arm64
//! - Windows
//! - macOS Intel
//! - macOS Apple Silicon (as a fallback when `FluidAudio` is unavailable)
//!
//! ## Model installation
//!
//! Models live at `~/.cache/nab/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3/`.
//! Use `nab models fetch sherpa-onnx` to download automatically.
//!
//! ## Limitations vs `FluidAudio`
//!
//! - Slower: ~30× realtime on CPU vs `FluidAudio`'s ~150× on Apple Neural Engine
//! - No offline diarization bundled (use pyannote ONNX separately, Phase 4)
//! - No Qwen3-ASR opt-in (Phase 4)

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig};

use super::asr_backend::{AsrBackend, TranscribeOptions, TranscriptSegment, TranscriptionResult};
use super::{AnalysisError, Result};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Languages natively supported by Parakeet TDT v3 ONNX (25 EU + RU/UK).
const SHERPA_LANGUAGES: &[&str] = &[
    "en", "fi", "sv", "no", "da", "de", "fr", "es", "it", "pt", "nl", "pl", "cs", "ro", "hu", "bg",
    "el", "hr", "sk", "sl", "lt", "lv", "et", "mt", "ru", "uk",
];

/// Required model file names in the model directory.
const REQUIRED_FILES: &[&str] = &["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"];

// ─── Backend ──────────────────────────────────────────────────────────────────

/// ASR backend powered by the sherpa-onnx C++ runtime via official Rust bindings.
///
/// The recognizer is lazily initialized on first use to avoid paying the ONNX
/// model load cost (~300 ms) at program startup.
pub struct SherpaOnnxBackend {
    model_dir: PathBuf,
    /// `None` until first `transcribe()` call.
    recognizer: Mutex<Option<OfflineRecognizer>>,
}

impl SherpaOnnxBackend {
    /// Construct a backend pointing at the default model directory.
    ///
    /// Does not load the model — call [`AsrBackend::is_available`] to confirm
    /// the model weights exist, then call [`AsrBackend::transcribe`].
    pub fn new() -> Self {
        Self::with_model_dir(default_model_dir())
    }

    /// Construct a backend with an explicit model directory (useful for tests).
    pub fn with_model_dir(model_dir: PathBuf) -> Self {
        Self {
            model_dir,
            recognizer: Mutex::new(None),
        }
    }

    /// Path to the model directory.
    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    /// Initialize and cache the recognizer on first use.
    fn ensure_recognizer(&self) -> Result<()> {
        let mut guard = self
            .recognizer
            .lock()
            .map_err(|_| AnalysisError::Whisper("sherpa-onnx recognizer mutex poisoned".into()))?;

        if guard.is_some() {
            return Ok(());
        }

        let recognizer = build_recognizer(&self.model_dir)?;
        *guard = Some(recognizer);
        Ok(())
    }

    /// Run recognition on pre-loaded PCM samples and return the raw transcript text.
    fn recognize_samples(&self, samples: &[f32], sample_rate: i32) -> Result<String> {
        let guard = self
            .recognizer
            .lock()
            .map_err(|_| AnalysisError::Whisper("sherpa-onnx recognizer mutex poisoned".into()))?;

        let recognizer = guard
            .as_ref()
            .expect("recognizer must be initialized before recognize_samples");

        let stream = recognizer.create_stream();
        stream.accept_waveform(sample_rate, samples);
        recognizer.decode(&stream);

        let result = stream.get_result().ok_or_else(|| {
            AnalysisError::Whisper("sherpa-onnx: get_result returned None".into())
        })?;

        Ok(result.text)
    }
}

impl Default for SherpaOnnxBackend {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: The sherpa-onnx C++ runtime is thread-safe for decode operations.
// `OfflineRecognizer` holds a raw C pointer which is not `Send` by default,
// but the runtime documentation guarantees concurrent decode safety, and we
// further guard all access through a `Mutex`.
unsafe impl Send for SherpaOnnxBackend {}
unsafe impl Sync for SherpaOnnxBackend {}

// ─── AsrBackend implementation ─────────────────────────────────────────────────

#[async_trait]
impl AsrBackend for SherpaOnnxBackend {
    fn name(&self) -> &'static str {
        "sherpa-onnx"
    }

    fn supported_languages(&self) -> &'static [&'static str] {
        SHERPA_LANGUAGES
    }

    /// Returns `true` when all four model files exist in the model directory.
    fn is_available(&self) -> bool {
        REQUIRED_FILES
            .iter()
            .all(|f| self.model_dir.join(f).exists())
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

        if !self.is_available() {
            return Err(AnalysisError::MissingDependency(format!(
                "sherpa-onnx model files not found at {}. \
                 Run `nab models fetch sherpa-onnx` to download.",
                self.model_dir.display()
            )));
        }

        self.ensure_recognizer()?;

        let audio_path_owned = audio_path.to_path_buf();
        let max_duration = opts.max_duration_seconds;
        let language = opts.language.clone();
        let word_timestamps = opts.word_timestamps;

        // Decode audio on a blocking thread — WAV decoding is CPU-bound.
        let (samples, sample_rate, audio_duration) = tokio::task::spawn_blocking(move || {
            load_audio_samples(&audio_path_owned, max_duration)
        })
        .await
        .map_err(|e| AnalysisError::Whisper(format!("audio decode task panicked: {e}")))??;

        tracing::debug!(
            backend = "sherpa-onnx",
            audio_duration,
            num_samples = samples.len(),
            sample_rate,
            "starting recognition"
        );

        let wall_start = Instant::now();
        let raw_text = self.recognize_samples(&samples, sample_rate)?;
        let processing_time_seconds = wall_start.elapsed().as_secs_f64();

        let rtfx = if processing_time_seconds > 0.0 {
            audio_duration / processing_time_seconds
        } else {
            0.0
        };

        let detected_language = language.unwrap_or_else(|| "en".to_string());
        let segments = text_to_segments(
            &raw_text,
            audio_duration,
            &detected_language,
            word_timestamps,
        );

        tracing::info!(
            backend = "sherpa-onnx",
            model = "parakeet-tdt-0.6b-v3",
            duration_seconds = audio_duration,
            rtfx,
            segments = segments.len(),
            "transcription complete"
        );

        Ok(TranscriptionResult {
            segments,
            language: detected_language,
            duration_seconds: audio_duration,
            model: "parakeet-tdt-0.6b-v3".to_string(),
            backend: "sherpa-onnx".to_string(),
            rtfx,
            processing_time_seconds,
            speakers: None,
            footnotes: None,
            active_reading: None,
        })
    }
}

// ─── Model construction ───────────────────────────────────────────────────────

fn default_model_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("nab/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3")
}

/// Build a sherpa-onnx `OfflineRecognizer` for Parakeet TDT v3 (transducer).
fn build_recognizer(model_dir: &Path) -> Result<OfflineRecognizer> {
    let encoder = model_dir.join("encoder.onnx");
    let decoder = model_dir.join("decoder.onnx");
    let joiner = model_dir.join("joiner.onnx");
    let tokens = model_dir.join("tokens.txt");

    for f in [&encoder, &decoder, &joiner, &tokens] {
        if !f.exists() {
            return Err(AnalysisError::MissingDependency(format!(
                "sherpa-onnx model file missing: {}. \
                 Run `nab models fetch sherpa-onnx`.",
                f.display()
            )));
        }
    }

    let mut config = OfflineRecognizerConfig::default();

    config.model_config.transducer = OfflineTransducerModelConfig {
        encoder: Some(encoder.to_string_lossy().into_owned()),
        decoder: Some(decoder.to_string_lossy().into_owned()),
        joiner: Some(joiner.to_string_lossy().into_owned()),
    };
    config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
    config.model_config.num_threads = num_cpus();
    config.model_config.model_type = Some("nemo_transducer".into());

    OfflineRecognizer::create(&config).ok_or_else(|| {
        AnalysisError::Whisper(
            "sherpa-onnx: OfflineRecognizer::create returned None — \
             check model files and sherpa-onnx library installation"
                .into(),
        )
    })
}

/// Return a reasonable thread count for ONNX inference (half of logical CPUs, min 1).
fn num_cpus() -> i32 {
    let n = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(2);
    ((n / 2).max(1)) as i32
}

// ─── Audio loading ────────────────────────────────────────────────────────────

/// Read a WAV file into f32 PCM samples at the file's native sample rate.
///
/// Sherpa-onnx handles resampling internally when given the correct sample rate.
/// Truncates to `max_duration_seconds` when set.
fn load_audio_samples(
    audio_path: &Path,
    max_duration: Option<u32>,
) -> Result<(Vec<f32>, i32, f64)> {
    let mut reader = hound::WavReader::open(audio_path).map_err(|e| {
        AnalysisError::Ffmpeg(format!(
            "failed to open WAV '{}': {e}",
            audio_path.display()
        ))
    })?;

    let spec = reader.spec();
    let sample_rate = spec.sample_rate;
    let channels = spec.channels as usize;

    let max_samples = max_duration.map(|d| d as usize * sample_rate as usize * channels);

    let raw_samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .take(max_samples.unwrap_or(usize::MAX))
            .collect::<std::result::Result<Vec<f32>, _>>()
            .map_err(|e| AnalysisError::Ffmpeg(format!("WAV read error: {e}")))?,
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .take(max_samples.unwrap_or(usize::MAX))
            .map(|s| s.map(|v| v as f32 / i32::MAX as f32))
            .collect::<std::result::Result<Vec<f32>, _>>()
            .map_err(|e| AnalysisError::Ffmpeg(format!("WAV read error: {e}")))?,
    };

    // Mix down to mono by averaging channels.
    let mono: Vec<f32> = if channels == 1 {
        raw_samples
    } else {
        raw_samples
            .chunks_exact(channels)
            .map(|chunk| chunk.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    let audio_duration = mono.len() as f64 / sample_rate as f64;
    Ok((mono, sample_rate as i32, audio_duration))
}

// ─── Text segmentation ────────────────────────────────────────────────────────

/// Convert a flat transcription string into [`TranscriptSegment`]s.
///
/// Sherpa-onnx offline recognition returns a single text string without
/// per-token timestamps. We split on sentence boundaries (`.!?`) and distribute
/// time evenly across segments proportional to character count.
fn text_to_segments(
    text: &str,
    total_duration: f64,
    language: &str,
    _word_timestamps: bool,
) -> Vec<TranscriptSegment> {
    let sentences = split_sentences(text);
    if sentences.is_empty() {
        return vec![];
    }

    let total_chars: usize = sentences.iter().map(String::len).sum();
    let total_chars = total_chars.max(1);

    let mut time_cursor = 0.0_f64;
    sentences
        .into_iter()
        .map(|sentence| {
            let fraction = sentence.len() as f64 / total_chars as f64;
            let seg_duration = total_duration * fraction;
            let start = time_cursor;
            let end = start + seg_duration;
            time_cursor = end;

            TranscriptSegment {
                text: sentence,
                start,
                end,
                confidence: 0.9, // sherpa-onnx transducer offline doesn't report per-segment confidence
                language: Some(language.to_string()),
                speaker: None,
                words: None,
            }
        })
        .collect()
}

/// Split text on `.!?` sentence boundaries.
///
/// A boundary is detected when `.!?` is followed by optional whitespace and an
/// uppercase letter (same heuristic as `fluidaudio_backend`).
pub(crate) fn split_sentences(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec![];
    }

    let mut sentences = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        if matches!(bytes[i], b'.' | b'!' | b'?') {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() && bytes[j].is_ascii_uppercase() {
                let slice = text[start..=i].trim();
                if !slice.is_empty() {
                    sentences.push(slice.to_string());
                }
                start = j;
            }
        }
        i += 1;
    }

    let tail = text[start..].trim();
    if !tail.is_empty() {
        sentences.push(tail.to_string());
    }

    sentences
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `name()` returns the canonical backend identifier.
    #[test]
    fn name_returns_sherpa_onnx() {
        // GIVEN a backend
        let backend = SherpaOnnxBackend::with_model_dir(PathBuf::from("/nonexistent"));
        // WHEN we query the name
        // THEN it matches the canonical identifier
        assert_eq!(backend.name(), "sherpa-onnx");
    }

    /// `supported_languages()` returns all 26 expected languages.
    #[test]
    fn supported_languages_returns_expected_set() {
        // GIVEN a backend
        let backend = SherpaOnnxBackend::with_model_dir(PathBuf::from("/nonexistent"));
        // WHEN we query supported languages
        let langs = backend.supported_languages();
        // THEN all expected EU + RU/UK languages are present
        assert_eq!(langs.len(), 26);
        assert!(langs.contains(&"en"));
        assert!(langs.contains(&"fi"));
        assert!(langs.contains(&"ru"));
        assert!(langs.contains(&"uk"));
    }

    /// `is_available()` returns `false` when the model directory does not exist.
    #[test]
    fn is_available_false_when_model_dir_missing() {
        // GIVEN a backend pointing at a nonexistent directory
        let backend = SherpaOnnxBackend::with_model_dir(PathBuf::from("/no/such/dir/__test__"));
        // WHEN we check availability
        // THEN it is not available
        assert!(!backend.is_available());
    }

    /// Constructor does not panic with a non-existent model path.
    #[test]
    fn constructor_does_not_panic_with_nonexistent_path() {
        // GIVEN a nonexistent path
        // WHEN we construct the backend
        // THEN no panic
        let _backend = SherpaOnnxBackend::with_model_dir(PathBuf::from("/tmp/__no_such_model__"));
    }

    /// `split_sentences` correctly chunks at `.!?` followed by uppercase.
    #[test]
    fn split_sentences_chunks_at_sentence_boundaries() {
        // GIVEN a multi-sentence string
        let text = "Hello world. This is a test. Another sentence!";
        // WHEN split
        let sentences = split_sentences(text);
        // THEN three sentences
        assert_eq!(sentences.len(), 3);
        assert_eq!(sentences[0], "Hello world.");
        assert_eq!(sentences[1], "This is a test.");
        assert_eq!(sentences[2], "Another sentence!");
    }

    /// `split_sentences` handles a single sentence with no trailing punctuation.
    #[test]
    fn split_sentences_single_sentence_no_trailing_punct() {
        // GIVEN a single clause
        let text = "no punctuation here";
        // WHEN split
        let sentences = split_sentences(text);
        // THEN returned as-is
        assert_eq!(sentences, vec!["no punctuation here".to_string()]);
    }

    /// `split_sentences` returns empty vec for empty input.
    #[test]
    fn split_sentences_empty_input_returns_empty() {
        // GIVEN empty text
        // WHEN split
        let result = split_sentences("");
        // THEN empty
        assert!(result.is_empty());
    }

    /// `text_to_segments` distributes time proportionally to character count.
    #[test]
    fn text_to_segments_distributes_time_proportionally() {
        // GIVEN two sentences (roughly equal length)
        let text = "Hello world. Second here.";
        // WHEN converted with 10s total
        let segs = text_to_segments(text, 10.0, "en", false);
        // THEN two segments covering the full duration
        assert_eq!(segs.len(), 2);
        assert!((segs[0].start - 0.0).abs() < 1e-9);
        let total_end = segs.last().unwrap().end;
        assert!((total_end - 10.0).abs() < 1e-9);
    }

    /// `num_cpus` returns at least 1.
    #[test]
    fn num_cpus_at_least_one() {
        // GIVEN the current host
        // WHEN queried
        let n = num_cpus();
        // THEN positive
        assert!(n >= 1);
    }
}
