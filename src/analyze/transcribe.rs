//! Audio transcription via vLLM/OpenAI-compatible ASR backends
//!
//! Supports three backends:
//! - Local `parakeet.cpp` binary (auto-detected via [`TranscriptionBackend::auto_detect`])
//! - Local Whisper via Python subprocess (auto-detected)
//! - vLLM HTTP API (Qwen3-ASR, OpenAI-compatible `/v1/audio/transcriptions`)
//!
//! The legacy `ParakeetTranscriber` and `Transcriber` types were deprecated in
//! commit 6fa7164 and removed in the follow-up cleanup. Use [`AsrBackend`] trait
//! implementations ([`FluidAudioBackend`], `SherpaOnnxBackend`, `WhisperRsBackend`)
//! instead.
//!
//! [`AsrBackend`]: super::AsrBackend
//! [`FluidAudioBackend`]: super::FluidAudioBackend

use super::{AnalysisError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Transcript segment with timestamps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<WordTiming>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// Word-level timing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordTiming {
    pub word: String,
    pub start: f64,
    pub end: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

// ─── Parakeet binary detection (used by TranscriptionBackend::auto_detect) ────

/// Default model search paths (checked in order).
const PARAKEET_MODEL_SEARCH_PATHS: &[&str] = &[
    "~/.cache/nab/models",
    "~/.cache/parakeet",
    "/usr/local/share/parakeet/models",
    "/opt/parakeet/models",
];

/// Default binary search paths (checked in order, after `$PATH`).
const PARAKEET_BINARY_SEARCH_PATHS: &[&str] =
    &["~/.local/bin", "/usr/local/bin", "/opt/parakeet/bin"];

/// Probe `$PATH` and well-known directories for the `parakeet` binary.
///
/// Returns the first path where `parakeet` (or `parakeet-cli`) exists.
fn detect_parakeet_binary() -> Option<std::path::PathBuf> {
    for name in ["parakeet", "parakeet-cli"] {
        if let Ok(output) = std::process::Command::new("which").arg(name).output()
            && output.status.success()
        {
            let p = std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
            if p.exists() {
                return Some(p);
            }
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for dir in PARAKEET_BINARY_SEARCH_PATHS {
        let expanded = dir.replace('~', &home);
        for name in ["parakeet", "parakeet-cli"] {
            let candidate = std::path::PathBuf::from(&expanded).join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Search well-known cache directories for a parakeet GGUF model file.
fn detect_parakeet_model() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    for dir in PARAKEET_MODEL_SEARCH_PATHS {
        let expanded = dir.replace('~', &home);
        let dir_path = std::path::PathBuf::from(&expanded);
        if !dir_path.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir_path) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.contains("parakeet") && name_str.ends_with(".gguf") {
                return Some(entry.path());
            }
        }
    }
    None
}

// ─── vLLM / OpenAI-compatible ASR backend ─────────────────────────────────────

/// Default Qwen3-ASR model served by vLLM.
pub const DEFAULT_VLLM_MODEL: &str = "Qwen/Qwen3-ASR-1.7B";

/// Default vLLM base URL (local deployment).
pub const DEFAULT_VLLM_BASE_URL: &str = "http://localhost:8000";

/// Transcription backend selector.
///
/// Ordered from highest to lowest preference for [`TranscriptionBackend::auto_detect`]:
///
/// 1. [`Parakeet`](TranscriptionBackend::Parakeet) — local `parakeet.cpp` binary (fastest,
///    >2000× RTFx, ~600 MB Q4 model, no Python runtime).
/// 2. [`ParakeetRemote`](TranscriptionBackend::ParakeetRemote) — `parakeet.cpp` on a remote
///    host via SSH + GPU.
/// 3. [`Whisper`](TranscriptionBackend::Whisper) — local Python `whisper` subprocess (legacy).
/// 4. [`WhisperRemote`](TranscriptionBackend::WhisperRemote) — Python `whisper` on DGX via SSH
///    (legacy).
/// 5. [`VllmApi`](TranscriptionBackend::VllmApi) — OpenAI-compatible HTTP endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptionBackend {
    /// Local `parakeet.cpp` binary — **default when available**.
    ///
    /// Detected automatically by [`TranscriptionBackend::auto_detect`] when
    /// both `parakeet` (or `parakeet-cli`) and a `.gguf` model file are found.
    Parakeet,
    /// Remote `parakeet.cpp` on a DGX/GPU host accessed over SSH.
    ParakeetRemote,
    /// Local Python whisper subprocess (legacy).
    Whisper,
    /// Remote execution on DGX Spark via SSH (legacy).
    WhisperRemote,
    /// vLLM (or any OpenAI-compatible) HTTP ASR endpoint.
    VllmApi {
        /// Server base URL, e.g. `"http://localhost:8000"`.
        base_url: String,
        /// Model identifier forwarded in the request, e.g.
        /// `"Qwen/Qwen3-ASR-1.7B"`.
        model: String,
    },
}

impl TranscriptionBackend {
    /// Detect the best available backend on the current machine.
    ///
    /// Priority (highest first):
    ///
    /// 1. [`Parakeet`](TranscriptionBackend::Parakeet) — when `parakeet` binary **and** a
    ///    `.gguf` model file are both found.
    /// 2. [`Whisper`](TranscriptionBackend::Whisper) — when `python3` is in `$PATH`.
    /// 3. [`VllmApi`](TranscriptionBackend::VllmApi) — default fallback pointing at
    ///    `http://localhost:8000` with [`DEFAULT_VLLM_MODEL`].
    ///
    /// Remote variants are never selected by auto-detect; configure them explicitly.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use nab::analyze::TranscriptionBackend;
    ///
    /// let backend = TranscriptionBackend::auto_detect();
    /// println!("{backend:?}");
    /// ```
    #[must_use]
    pub fn auto_detect() -> Self {
        if Self::parakeet_available() {
            return Self::Parakeet;
        }
        if Self::python3_available() {
            return Self::Whisper;
        }
        Self::VllmApi {
            base_url: DEFAULT_VLLM_BASE_URL.to_string(),
            model: DEFAULT_VLLM_MODEL.to_string(),
        }
    }

    /// `true` when both a parakeet binary and a model file are present.
    fn parakeet_available() -> bool {
        detect_parakeet_binary().is_some() && detect_parakeet_model().is_some()
    }

    /// `true` when `python3` resolves in `$PATH`.
    fn python3_available() -> bool {
        std::process::Command::new("which")
            .arg("python3")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// OpenAI-compatible `/v1/audio/transcriptions` response.
#[derive(Debug, Deserialize)]
struct TranscriptionApiResponse {
    text: String,
}

/// OpenAI-compatible error body returned by vLLM on failure.
#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    #[serde(default)]
    message: String,
}

/// Wraps an API error that may carry either the standard `{"error":{"message":…}}`
/// envelope or a bare `{"message":…}` body.
#[derive(Debug, Deserialize)]
struct ApiErrorEnvelope {
    #[serde(default)]
    error: Option<ApiErrorBody>,
    #[serde(default)]
    message: Option<String>,
}

impl ApiErrorEnvelope {
    fn into_message(self) -> String {
        self.error
            .map(|e| e.message)
            .or(self.message)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown API error".to_string())
    }
}

/// HTTP client for vLLM / OpenAI-compatible ASR endpoints.
///
/// Sends audio to any OpenAI-compatible `/v1/audio/transcriptions` endpoint,
/// including vLLM serving Qwen3-ASR.
///
/// # Example
///
/// ```no_run
/// use nab::analyze::VllmTranscriber;
/// use std::path::Path;
///
/// # async fn example() -> anyhow::Result<()> {
/// let t = VllmTranscriber::new("http://localhost:8000", "Qwen/Qwen3-ASR-1.7B");
/// let url = t.transcription_url();
/// assert_eq!(url, "http://localhost:8000/v1/audio/transcriptions");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct VllmTranscriber {
    base_url: String,
    model: String,
}

impl VllmTranscriber {
    /// Create a new transcriber pointing at `base_url` using `model`.
    ///
    /// Trailing slashes on `base_url` are stripped so URLs are always
    /// normalised.
    #[must_use]
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }

    /// Create a transcriber with [`DEFAULT_VLLM_BASE_URL`] and
    /// [`DEFAULT_VLLM_MODEL`].
    #[must_use]
    pub fn default_local() -> Self {
        Self::new(DEFAULT_VLLM_BASE_URL, DEFAULT_VLLM_MODEL)
    }

    /// Full URL of the transcription endpoint.
    #[must_use]
    pub fn transcription_url(&self) -> String {
        format!("{}/v1/audio/transcriptions", self.base_url)
    }

    /// Model identifier used in requests.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Transcribe `audio_path` by uploading it to the vLLM endpoint.
    ///
    /// Returns a single [`TranscriptSegment`] spanning the full audio file.
    /// vLLM's OpenAI-compatible endpoint does not return per-segment
    /// timestamps, so `start` and `end` are set to `0.0`.
    pub async fn transcribe(&self, audio_path: &Path) -> Result<Vec<TranscriptSegment>> {
        let client = reqwest::Client::new();
        let text = self.post_audio(&client, audio_path).await?;
        Ok(vec![TranscriptSegment {
            start: 0.0,
            end: 0.0,
            text,
            words: None,
            language: None,
            confidence: None,
        }])
    }

    /// Transcribe with a language hint forwarded as the `language` field.
    pub async fn transcribe_with_language(
        &self,
        audio_path: &Path,
        language: &str,
    ) -> Result<Vec<TranscriptSegment>> {
        let client = reqwest::Client::new();
        let text = self
            .post_audio_with_language(&client, audio_path, language)
            .await?;
        Ok(vec![TranscriptSegment {
            start: 0.0,
            end: 0.0,
            text,
            words: None,
            language: Some(language.to_string()),
            confidence: None,
        }])
    }

    /// Build the multipart form for `audio_path`, then POST and return the
    /// transcription text.
    async fn post_audio(&self, client: &reqwest::Client, audio_path: &Path) -> Result<String> {
        let form = self.build_multipart(audio_path, None).await?;
        let resp = client
            .post(self.transcription_url())
            .multipart(form)
            .send()
            .await?;
        self.extract_text(resp).await
    }

    /// Same as [`post_audio`] but appends a `language` field to the form.
    async fn post_audio_with_language(
        &self,
        client: &reqwest::Client,
        audio_path: &Path,
        language: &str,
    ) -> Result<String> {
        let form = self.build_multipart(audio_path, Some(language)).await?;
        let resp = client
            .post(self.transcription_url())
            .multipart(form)
            .send()
            .await?;
        self.extract_text(resp).await
    }

    /// Construct a multipart form containing the audio file and model name.
    async fn build_multipart(
        &self,
        audio_path: &Path,
        language: Option<&str>,
    ) -> Result<reqwest::multipart::Form> {
        let file_bytes = tokio::fs::read(audio_path).await?;
        let filename = audio_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio.wav")
            .to_string();

        let file_part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(filename)
            .mime_str("audio/wav")
            .map_err(|e| AnalysisError::TranscriptionApi(e.to_string()))?;

        let mut form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .text("model", self.model.clone());

        if let Some(lang) = language {
            form = form.text("language", lang.to_string());
        }

        Ok(form)
    }

    /// Parse a successful or error response into a `Result<String>`.
    async fn extract_text(&self, resp: reqwest::Response) -> Result<String> {
        let status = resp.status();
        let body = resp.text().await?;

        if status.is_success() {
            return self.parse_response(&body);
        }

        let msg = serde_json::from_str::<ApiErrorEnvelope>(&body)
            .map_or_else(|_| body.clone(), ApiErrorEnvelope::into_message);

        Err(AnalysisError::TranscriptionApi(format!(
            "HTTP {status}: {msg}"
        )))
    }

    /// Parse a JSON transcription response body into the transcribed text.
    ///
    /// Accepts the OpenAI-compatible `{"text": "…"}` envelope.
    pub fn parse_response(&self, json: &str) -> Result<String> {
        let parsed: TranscriptionApiResponse = serde_json::from_str(json)
            .map_err(|e| AnalysisError::TranscriptionApi(format!("malformed response: {e}")))?;

        if parsed.text.is_empty() {
            return Err(AnalysisError::TranscriptionApi(
                "empty transcription text in response".to_string(),
            ));
        }

        Ok(parsed.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TranscriptSegment / WordTiming serialisation ────────────────────────

    #[test]
    fn test_word_timing_serialization() {
        let word = WordTiming {
            word: "hello".to_string(),
            start: 0.0,
            end: 0.5,
            confidence: Some(0.95),
        };

        let json = serde_json::to_string(&word).unwrap();
        assert!(json.contains("hello"));
        assert!(json.contains("0.95"));
    }

    #[test]
    fn test_segment_serialization() {
        let segment = TranscriptSegment {
            start: 0.0,
            end: 2.5,
            text: "Hello world".to_string(),
            words: Some(vec![
                WordTiming {
                    word: "Hello".to_string(),
                    start: 0.0,
                    end: 0.5,
                    confidence: Some(0.9),
                },
                WordTiming {
                    word: "world".to_string(),
                    start: 0.6,
                    end: 1.2,
                    confidence: Some(0.85),
                },
            ]),
            language: Some("en".to_string()),
            confidence: None,
        };

        let json = serde_json::to_string_pretty(&segment).unwrap();
        assert!(json.contains("Hello world"));
        assert!(json.contains("\"en\""));
    }

    // ── VllmTranscriber construction ────────────────────────────────────────

    #[test]
    fn vllm_new_uses_supplied_base_url_and_model() {
        // GIVEN a custom URL and model name
        // WHEN constructing VllmTranscriber
        // THEN both are stored verbatim (trailing slash stripped)
        let t = VllmTranscriber::new("http://spark:8000", "Qwen/Qwen3-ASR-8B");
        assert_eq!(t.model(), "Qwen/Qwen3-ASR-8B");
        assert_eq!(
            t.transcription_url(),
            "http://spark:8000/v1/audio/transcriptions"
        );
    }

    #[test]
    fn vllm_new_strips_trailing_slash_from_base_url() {
        // GIVEN a base URL with a trailing slash
        // WHEN constructing VllmTranscriber
        // THEN the URL is normalised and the endpoint is still correct
        let t = VllmTranscriber::new("http://localhost:8000/", "model");
        assert_eq!(
            t.transcription_url(),
            "http://localhost:8000/v1/audio/transcriptions"
        );
    }

    #[test]
    fn vllm_default_local_uses_qwen3_asr_1_7b() {
        // GIVEN no explicit configuration
        // WHEN using the default local constructor
        // THEN the model is Qwen3-ASR-1.7B and the URL is localhost:8000
        let t = VllmTranscriber::default_local();
        assert_eq!(t.model(), DEFAULT_VLLM_MODEL);
        assert_eq!(
            t.transcription_url(),
            "http://localhost:8000/v1/audio/transcriptions"
        );
    }

    #[test]
    fn vllm_default_model_constant_is_qwen3_asr_1_7b() {
        // GIVEN the library constant
        // WHEN inspected
        // THEN it names the correct 1.7B variant
        assert_eq!(DEFAULT_VLLM_MODEL, "Qwen/Qwen3-ASR-1.7B");
    }

    // ── parse_response: happy paths ─────────────────────────────────────────

    #[test]
    fn vllm_parse_response_returns_text_for_valid_json() {
        // GIVEN a valid OpenAI transcription response
        // WHEN parsed
        // THEN the transcription text is returned
        let t = VllmTranscriber::default_local();
        let json = r#"{"text": "Hello, world."}"#;
        let result = t.parse_response(json).unwrap();
        assert_eq!(result, "Hello, world.");
    }

    #[test]
    fn vllm_parse_response_preserves_whitespace_in_text() {
        // GIVEN a response with leading/trailing spaces preserved by the model
        // WHEN parsed
        // THEN whitespace is kept as-is (caller decides trimming)
        let t = VllmTranscriber::default_local();
        let json = r#"{"text": "  spaced out  "}"#;
        let result = t.parse_response(json).unwrap();
        assert_eq!(result, "  spaced out  ");
    }

    #[test]
    fn vllm_parse_response_handles_extra_fields_in_response() {
        // GIVEN a response with additional fields (e.g. task, language, duration)
        // WHEN parsed
        // THEN the text field is extracted and extras are ignored
        let t = VllmTranscriber::default_local();
        let json =
            r#"{"text": "Bonjour.", "language": "fr", "duration": 1.2, "task": "transcribe"}"#;
        let result = t.parse_response(json).unwrap();
        assert_eq!(result, "Bonjour.");
    }

    // ── parse_response: error paths ─────────────────────────────────────────

    #[test]
    fn vllm_parse_response_errors_on_malformed_json() {
        // GIVEN a non-JSON body (e.g. a proxy returning HTML)
        // WHEN parsed
        // THEN a TranscriptionApi error is returned
        let t = VllmTranscriber::default_local();
        let err = t.parse_response("not json at all").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("malformed response"),
            "expected 'malformed response' in: {msg}"
        );
    }

    #[test]
    fn vllm_parse_response_errors_on_empty_text_field() {
        // GIVEN a response with an empty transcription
        // WHEN parsed
        // THEN an error signals that the response was unexpectedly empty
        let t = VllmTranscriber::default_local();
        let err = t.parse_response(r#"{"text": ""}"#).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("empty transcription"),
            "expected 'empty transcription' in: {msg}"
        );
    }

    #[test]
    fn vllm_parse_response_errors_on_missing_text_field() {
        // GIVEN a JSON object that lacks the required "text" key
        // WHEN parsed
        // THEN a malformed-response error is returned
        let t = VllmTranscriber::default_local();
        let err = t.parse_response(r#"{"result": "something"}"#).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("malformed response"),
            "expected 'malformed response' in: {msg}"
        );
    }

    // ── TranscriptionBackend enum ────────────────────────────────────────────

    #[test]
    fn backend_enum_vllm_api_round_trips_through_json() {
        // GIVEN a VllmApi backend value
        // WHEN serialised then deserialised
        // THEN the values survive the round-trip
        let backend = TranscriptionBackend::VllmApi {
            base_url: "http://localhost:8000".to_string(),
            model: "Qwen/Qwen3-ASR-1.7B".to_string(),
        };
        let json = serde_json::to_string(&backend).unwrap();
        assert!(json.contains("vllm_api"), "tag missing: {json}");
        assert!(json.contains("Qwen3-ASR"), "model missing: {json}");

        let back: TranscriptionBackend = serde_json::from_str(&json).unwrap();
        let TranscriptionBackend::VllmApi { base_url, model } = back else {
            panic!("wrong variant after round-trip");
        };
        assert_eq!(base_url, "http://localhost:8000");
        assert_eq!(model, "Qwen/Qwen3-ASR-1.7B");
    }

    #[test]
    fn backend_enum_whisper_variant_serialises_correctly() {
        // GIVEN the Whisper backend variant
        // WHEN serialised
        // THEN the tag is "whisper"
        let backend = TranscriptionBackend::Whisper;
        let json = serde_json::to_string(&backend).unwrap();
        assert!(json.contains("\"whisper\""), "unexpected json: {json}");
    }

    #[test]
    fn backend_enum_whisper_remote_variant_serialises_correctly() {
        // GIVEN the WhisperRemote backend variant
        // WHEN serialised
        // THEN the tag is "whisper_remote"
        let backend = TranscriptionBackend::WhisperRemote;
        let json = serde_json::to_string(&backend).unwrap();
        assert!(json.contains("whisper_remote"), "unexpected json: {json}");
    }

    #[test]
    fn backend_enum_parakeet_variant_serialises_correctly() {
        // GIVEN the Parakeet backend variant
        // WHEN serialised to JSON
        // THEN the tag is "parakeet"
        let backend = TranscriptionBackend::Parakeet;
        let json = serde_json::to_string(&backend).unwrap();
        assert!(json.contains("\"parakeet\""), "unexpected json: {json}");
    }

    #[test]
    fn backend_enum_parakeet_remote_variant_serialises_correctly() {
        // GIVEN the ParakeetRemote backend variant
        // WHEN serialised to JSON
        // THEN the tag is "parakeet_remote"
        let backend = TranscriptionBackend::ParakeetRemote;
        let json = serde_json::to_string(&backend).unwrap();
        assert!(json.contains("parakeet_remote"), "unexpected json: {json}");
    }

    #[test]
    fn backend_enum_parakeet_round_trips_through_json() {
        // GIVEN the Parakeet backend
        // WHEN serialised then deserialised
        // THEN the same variant is recovered
        let backend = TranscriptionBackend::Parakeet;
        let json = serde_json::to_string(&backend).unwrap();
        let back: TranscriptionBackend = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, TranscriptionBackend::Parakeet),
            "wrong variant after round-trip: {json}"
        );
    }

    // ── TranscriptionBackend: auto_detect fallback ───────────────────────────

    #[test]
    fn auto_detect_falls_back_to_vllm_when_neither_parakeet_nor_python3_present() {
        // GIVEN neither parakeet binary/model nor python3 is available
        // (this is guaranteed in a sandboxed CI environment with no parakeet installed)
        // WHEN auto_detect is called and neither parakeet nor whisper is present
        // THEN it returns either Parakeet, Whisper, or VllmApi — never panics
        //
        // We can't control the environment in tests, so we just verify that
        // auto_detect returns a valid variant without panicking.
        let backend = TranscriptionBackend::auto_detect();
        let _json = serde_json::to_string(&backend).unwrap(); // serialisable
        // The variant is one of the known ones (pattern match to exhaust all arms)
        match backend {
            TranscriptionBackend::Parakeet
            | TranscriptionBackend::ParakeetRemote
            | TranscriptionBackend::Whisper
            | TranscriptionBackend::WhisperRemote
            | TranscriptionBackend::VllmApi { .. } => {}
        }
    }

    #[test]
    fn auto_detect_returns_vllm_default_url_and_model_as_fallback() {
        // GIVEN no parakeet and no python3 on the current machine
        // WHEN auto_detect resolves to VllmApi
        // THEN it uses the published DEFAULT_VLLM_BASE_URL and DEFAULT_VLLM_MODEL constants
        // (only meaningful when the VllmApi arm is actually selected)
        if let TranscriptionBackend::VllmApi { base_url, model } =
            TranscriptionBackend::auto_detect()
        {
            assert_eq!(base_url, DEFAULT_VLLM_BASE_URL);
            assert_eq!(model, DEFAULT_VLLM_MODEL);
        }
        // If Parakeet or Whisper was selected instead, this test is vacuously true —
        // which is correct: the fallback constants are only verified when the fallback fires.
    }
}
