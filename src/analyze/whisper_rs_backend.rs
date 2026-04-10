//! Whisper.cpp ASR backend via `whisper-rs` Rust bindings.
//!
//! Uses whisper-large-v3-turbo (GGUF Q5_0, ~590 MB) for universal language
//! coverage. This is the **fallback backend** for languages outside Parakeet
//! TDT v3's 26-language set (e.g. Arabic, Hindi, Turkish, Japanese, Chinese).
//!
//! ## Realtime factors (approximate)
//!
//! | Hardware               | RTFx  |
//! |------------------------|-------|
//! | macOS Metal (M-series) | ~6×   |
//! | Linux CPU x86_64       | ~3×   |
//! | Linux CUDA RTX 4090    | ~15×  |
//!
//! ## Model installation
//!
//! Model lives at `~/.cache/nab/models/whisper-large-v3-turbo-q5_0.bin`.
//! Use `nab models fetch whisper` to download automatically.
//!
//! ## Language support
//!
//! whisper-large-v3-turbo supports 99 languages; this backend returns `&["*"]`
//! to signal universal acceptance. The caller may pass a BCP-47 language hint
//! via `TranscribeOptions::language` for improved accuracy on short audio.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::asr_backend::{
    AsrBackend, TranscribeOptions, TranscriptSegment, TranscriptionResult, WordTiming,
};
use super::{AnalysisError, Result};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Minimum model file size (100 MB) used for sanity-check in `is_available`.
const MIN_MODEL_BYTES: u64 = 100 * 1024 * 1024;

// ─── Backend ──────────────────────────────────────────────────────────────────

/// ASR backend powered by whisper.cpp via `whisper-rs` Rust bindings.
///
/// The `WhisperContext` is lazily initialized on first use — loading the GGUF
/// model takes ~500 ms on cold start. Subsequent calls reuse the cached context.
pub struct WhisperRsBackend {
    model_path: PathBuf,
    /// `None` until first `transcribe()` call.
    ctx: Mutex<Option<WhisperContext>>,
}

impl WhisperRsBackend {
    /// Construct a backend pointing at the default model file.
    pub fn new() -> Self {
        Self::with_model_path(default_model_path())
    }

    /// Construct a backend with an explicit model file path (useful for tests).
    pub fn with_model_path(model_path: PathBuf) -> Self {
        Self {
            model_path,
            ctx: Mutex::new(None),
        }
    }

    /// Path to the GGUF model file.
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// Initialize and cache the `WhisperContext` on first use.
    fn ensure_context(&self) -> Result<()> {
        let mut guard = self
            .ctx
            .lock()
            .map_err(|_| AnalysisError::Whisper("whisper-rs context mutex poisoned".into()))?;

        if guard.is_some() {
            return Ok(());
        }

        let ctx = build_context(&self.model_path)?;
        *guard = Some(ctx);
        Ok(())
    }

    /// Run inference on f32 PCM samples at 16 kHz mono and return raw segments.
    ///
    /// `language_hint` must be a static or caller-owned `&str` slice. We accept
    /// `Option<&str>` here and delegate to a helper that satisfies the whisper-rs
    /// `FullParams<'a, 'b>` lifetime requirement by holding the lang string alongside
    /// the params struct.
    fn run_inference(
        &self,
        samples: &[f32],
        language_hint: Option<&str>,
        word_timestamps: bool,
    ) -> Result<Vec<RawSegment>> {
        let guard = self
            .ctx
            .lock()
            .map_err(|_| AnalysisError::Whisper("whisper-rs context mutex poisoned".into()))?;

        let ctx = guard
            .as_ref()
            .expect("context must be initialized before run_inference");

        // Build a FullParams with the language stored in an owned String so its
        // lifetime is tied to this stack frame — satisfying `FullParams<'a, '_>`.
        let lang_owned: Option<String> = language_hint.map(|s| s.to_string());

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_token_timestamps(word_timestamps);
        // set_language(&str) lifetime must outlive params; lang_owned lives until end of fn.
        params.set_language(lang_owned.as_deref());

        let mut state = ctx
            .create_state()
            .map_err(|e| AnalysisError::Whisper(format!("whisper-rs create_state: {e}")))?;

        state
            .full(params, samples)
            .map_err(|e| AnalysisError::Whisper(format!("whisper-rs inference failed: {e}")))?;

        let n_segments = state.full_n_segments();
        let mut raw = Vec::with_capacity(n_segments as usize);

        for i in 0..n_segments {
            let Some(seg) = state.get_segment(i) else {
                continue;
            };

            let text = seg
                .to_str_lossy()
                .map_err(|e| AnalysisError::Whisper(format!("segment text({i}): {e}")))?
                .trim()
                .to_string();

            // Timestamps are in centiseconds (10ms units) — convert to seconds.
            let start = seg.start_timestamp() as f64 * 0.01;
            let end = seg.end_timestamp() as f64 * 0.01;

            let words = if word_timestamps {
                Some(extract_word_timings(&seg)?)
            } else {
                None
            };

            raw.push(RawSegment {
                text,
                start,
                end,
                words,
            });
        }

        Ok(raw)
    }
}

impl Default for WhisperRsBackend {
    fn default() -> Self {
        Self::new()
    }
}

// ─── AsrBackend implementation ─────────────────────────────────────────────────

#[async_trait]
impl AsrBackend for WhisperRsBackend {
    fn name(&self) -> &'static str {
        "whisper-rs"
    }

    /// Returns `&["*"]` — whisper-large-v3-turbo supports 99 languages.
    fn supported_languages(&self) -> &'static [&'static str] {
        &["*"]
    }

    /// Returns `true` when the GGUF model file exists and is larger than 100 MB.
    fn is_available(&self) -> bool {
        self.model_path
            .metadata()
            .map(|m| m.len() >= MIN_MODEL_BYTES)
            .unwrap_or(false)
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
                "whisper model not found at {}. \
                 Run `nab models fetch whisper` to download.",
                self.model_path.display()
            )));
        }

        self.ensure_context()?;

        let audio_path_owned = audio_path.to_path_buf();
        let max_duration = opts.max_duration_seconds;
        let language_hint = opts.language.clone();
        let word_timestamps = opts.word_timestamps;

        // WAV decoding is CPU-bound — run on a blocking thread.
        let (samples, audio_duration) = tokio::task::spawn_blocking(move || {
            load_audio_samples_f32(&audio_path_owned, max_duration)
        })
        .await
        .map_err(|e| AnalysisError::Whisper(format!("audio decode task panicked: {e}")))??;

        tracing::debug!(
            backend = "whisper-rs",
            audio_duration,
            num_samples = samples.len(),
            "starting whisper inference"
        );

        let wall_start = Instant::now();
        let raw_segments =
            self.run_inference(&samples, language_hint.as_deref(), word_timestamps)?;
        let processing_time_seconds = wall_start.elapsed().as_secs_f64();

        let rtfx = if processing_time_seconds > 0.0 {
            audio_duration / processing_time_seconds
        } else {
            0.0
        };

        let detected_language = language_hint.unwrap_or_else(|| "en".to_string());
        let segments = raw_segments_to_transcript(raw_segments, &detected_language);

        tracing::info!(
            backend = "whisper-rs",
            model = "whisper-large-v3-turbo-q5_0",
            duration_seconds = audio_duration,
            rtfx,
            segments = segments.len(),
            "transcription complete"
        );

        Ok(TranscriptionResult {
            segments,
            language: detected_language,
            duration_seconds: audio_duration,
            model: "whisper-large-v3-turbo-q5_0".to_string(),
            backend: "whisper-rs".to_string(),
            rtfx,
            processing_time_seconds,
            speakers: None,
            footnotes: None,
            active_reading: None,
        })
    }
}

// ─── Model construction ───────────────────────────────────────────────────────

fn default_model_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("nab/models/whisper-large-v3-turbo-q5_0.bin")
}

fn build_context(model_path: &Path) -> Result<WhisperContext> {
    if !model_path.exists() {
        return Err(AnalysisError::MissingDependency(format!(
            "whisper model not found at {}. Run `nab models fetch whisper`.",
            model_path.display()
        )));
    }

    let params = WhisperContextParameters::default();
    WhisperContext::new_with_params(model_path, params).map_err(|e| {
        AnalysisError::Whisper(format!(
            "failed to load whisper model from '{}': {e}",
            model_path.display()
        ))
    })
}

// ─── Audio loading ────────────────────────────────────────────────────────────

/// Decode a WAV file to f32 PCM at 16 kHz mono (required by whisper-rs).
///
/// Resamples to 16 kHz when the source sample rate differs. Truncates to
/// `max_duration_seconds` when set.
fn load_audio_samples_f32(audio_path: &Path, max_duration: Option<u32>) -> Result<(Vec<f32>, f64)> {
    let mut reader = hound::WavReader::open(audio_path).map_err(|e| {
        AnalysisError::Ffmpeg(format!(
            "failed to open WAV '{}': {e}",
            audio_path.display()
        ))
    })?;

    let spec = reader.spec();
    let src_sample_rate = spec.sample_rate;
    let channels = spec.channels as usize;
    let target_sample_rate: u32 = 16_000;

    let max_src_samples = max_duration.map(|d| d as usize * src_sample_rate as usize * channels);

    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .take(max_src_samples.unwrap_or(usize::MAX))
            .collect::<std::result::Result<Vec<f32>, _>>()
            .map_err(|e| AnalysisError::Ffmpeg(format!("WAV read error: {e}")))?,
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .take(max_src_samples.unwrap_or(usize::MAX))
            .map(|s| s.map(|v| v as f32 / i32::MAX as f32))
            .collect::<std::result::Result<Vec<f32>, _>>()
            .map_err(|e| AnalysisError::Ffmpeg(format!("WAV read error: {e}")))?,
    };

    // Mix down to mono.
    let mono: Vec<f32> = if channels == 1 {
        raw
    } else {
        raw.chunks_exact(channels)
            .map(|chunk| chunk.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    // Resample to 16 kHz via linear interpolation (sufficient for ASR quality).
    let samples = if src_sample_rate == target_sample_rate {
        mono
    } else {
        resample_linear(&mono, src_sample_rate, target_sample_rate)
    };

    let audio_duration = samples.len() as f64 / target_sample_rate as f64;
    Ok((samples, audio_duration))
}

/// Linear interpolation resampler (sufficient for ASR quality, zero deps).
fn resample_linear(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src_rate == dst_rate {
        return input.to_vec();
    }
    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = (input.len() as f64 / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(0.0);
        out.push(a + frac * (b - a));
    }
    out
}

// ─── Result mapping ───────────────────────────────────────────────────────────

/// Internal intermediate from whisper-rs inference.
struct RawSegment {
    text: String,
    start: f64,
    end: f64,
    words: Option<Vec<WordTiming>>,
}

/// Extract word-level timings for a segment via the whisper-rs token API.
fn extract_word_timings(seg: &whisper_rs::WhisperSegment<'_>) -> Result<Vec<WordTiming>> {
    let n_tokens = seg.n_tokens();
    let mut words = Vec::with_capacity(n_tokens as usize);

    for tok_idx in 0..n_tokens {
        let Some(token) = seg.get_token(tok_idx) else {
            continue;
        };

        let text = token
            .to_str_lossy()
            .map_err(|e| AnalysisError::Whisper(format!("token text: {e}")))?;
        let word = text.trim().to_string();

        // Skip special tokens (e.g. [_BEG_], [_TT_N], whitespace-only).
        if word.is_empty() || word.starts_with('[') {
            continue;
        }

        let data = token.token_data();
        // t0/t1 are in centiseconds.
        let start = data.t0 as f64 * 0.01;
        let end = data.t1 as f64 * 0.01;
        let confidence = token.token_probability();

        words.push(WordTiming {
            word,
            start,
            end,
            confidence,
        });
    }
    Ok(words)
}

/// Convert [`RawSegment`]s to [`TranscriptSegment`]s, skipping empty segments.
fn raw_segments_to_transcript(raw: Vec<RawSegment>, language: &str) -> Vec<TranscriptSegment> {
    raw.into_iter()
        .filter(|s| !s.text.is_empty())
        .map(|s| {
            let confidence = s
                .words
                .as_ref()
                .and_then(|ws| avg_confidence(ws))
                .unwrap_or(0.9);
            TranscriptSegment {
                text: s.text,
                start: s.start,
                end: s.end,
                confidence,
                language: Some(language.to_string()),
                speaker: None,
                words: s.words,
            }
        })
        .collect()
}

fn avg_confidence(words: &[WordTiming]) -> Option<f32> {
    if words.is_empty() {
        return None;
    }
    let sum: f32 = words.iter().map(|w| w.confidence).sum();
    Some(sum / words.len() as f32)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `name()` returns the canonical backend identifier.
    #[test]
    fn name_returns_whisper_rs() {
        // GIVEN a backend
        let backend = WhisperRsBackend::with_model_path(PathBuf::from("/nonexistent.bin"));
        // WHEN we query the name
        // THEN the canonical identifier is returned
        assert_eq!(backend.name(), "whisper-rs");
    }

    /// `supported_languages()` returns `["*"]` for universal language support.
    #[test]
    fn supported_languages_returns_wildcard() {
        // GIVEN a backend
        let backend = WhisperRsBackend::with_model_path(PathBuf::from("/nonexistent.bin"));
        // WHEN queried
        let langs = backend.supported_languages();
        // THEN the wildcard is present
        assert_eq!(langs, &["*"]);
    }

    /// `is_available()` returns `false` when the model file does not exist.
    #[test]
    fn is_available_false_when_model_missing() {
        // GIVEN a backend pointing at a nonexistent file
        let backend = WhisperRsBackend::with_model_path(PathBuf::from("/no/such/model.bin"));
        // WHEN availability is checked
        // THEN it is not available
        assert!(!backend.is_available());
    }

    /// Constructor does not panic with a non-existent model path.
    #[test]
    fn constructor_does_not_panic_with_nonexistent_path() {
        // GIVEN a nonexistent path
        // WHEN we construct the backend
        // THEN no panic
        let _backend = WhisperRsBackend::with_model_path(PathBuf::from("/tmp/__no_model__.bin"));
    }

    /// `resample_linear` is a no-op when src == dst rate.
    #[test]
    fn resample_linear_noop_when_rates_equal() {
        // GIVEN samples at 16 kHz
        let input = vec![0.1_f32, 0.2, 0.3, 0.4];
        // WHEN resampled at same rate
        let output = resample_linear(&input, 16_000, 16_000);
        // THEN unchanged
        assert_eq!(output, input);
    }

    /// `resample_linear` produces expected output length for 44.1k → 16k.
    #[test]
    fn resample_linear_output_length_for_downsampling() {
        // GIVEN 44100 samples at 44.1 kHz (1 second)
        let input: Vec<f32> = (0..44100).map(|i| i as f32 / 44100.0).collect();
        // WHEN resampled to 16 kHz
        let output = resample_linear(&input, 44_100, 16_000);
        // THEN ~16000 samples (within rounding)
        let expected = (44100_f64 / (44100_f64 / 16000_f64)).ceil() as usize;
        assert!((output.len() as isize - expected as isize).abs() <= 2);
    }

    /// `avg_confidence` returns `None` for an empty slice.
    #[test]
    fn avg_confidence_empty_returns_none() {
        // GIVEN empty slice
        // WHEN
        let result = avg_confidence(&[]);
        // THEN
        assert!(result.is_none());
    }

    /// `avg_confidence` correctly averages word confidences.
    #[test]
    fn avg_confidence_averages_correctly() {
        // GIVEN words with known confidences
        let words = vec![
            WordTiming {
                word: "a".into(),
                start: 0.0,
                end: 0.1,
                confidence: 0.8,
            },
            WordTiming {
                word: "b".into(),
                start: 0.1,
                end: 0.2,
                confidence: 0.6,
            },
        ];
        // WHEN averaged
        let avg = avg_confidence(&words).unwrap();
        // THEN == 0.7
        assert!((avg - 0.7).abs() < 1e-6, "expected 0.7, got {avg}");
    }

    /// `raw_segments_to_transcript` skips empty text segments.
    #[test]
    fn raw_segments_to_transcript_skips_empty() {
        // GIVEN segments including one empty text
        let raw = vec![
            RawSegment {
                text: "Hello".into(),
                start: 0.0,
                end: 1.0,
                words: None,
            },
            RawSegment {
                text: "".into(),
                start: 1.0,
                end: 1.5,
                words: None,
            },
            RawSegment {
                text: "World".into(),
                start: 1.5,
                end: 2.0,
                words: None,
            },
        ];
        // WHEN converted
        let segs = raw_segments_to_transcript(raw, "en");
        // THEN only two non-empty segments
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "Hello");
        assert_eq!(segs[1].text, "World");
    }
}
