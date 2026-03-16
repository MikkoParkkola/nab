//! Audio transcription via Whisper and vLLM/OpenAI-compatible ASR backends
//!
//! Supports three backends:
//! - Local Whisper via Python subprocess
//! - Remote Whisper on DGX Spark (SSH + GPU)
//! - vLLM HTTP API (Qwen3-ASR, OpenAI-compatible `/v1/audio/transcriptions`)

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

use super::{AnalysisError, Result};

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

/// Whisper transcription engine
pub struct Transcriber {
    model: String,
    dgx_host: Option<String>,
}

impl Transcriber {
    pub fn new(model: &str, dgx_host: Option<String>) -> Result<Self> {
        Ok(Self {
            model: model.to_string(),
            dgx_host,
        })
    }

    /// Transcribe audio file with word-level timestamps
    pub async fn transcribe(&self, audio_path: &Path) -> Result<Vec<TranscriptSegment>> {
        if let Some(host) = &self.dgx_host {
            self.transcribe_remote(audio_path, host).await
        } else {
            self.transcribe_local(audio_path).await
        }
    }

    /// Local transcription using Python whisper
    async fn transcribe_local(&self, audio_path: &Path) -> Result<Vec<TranscriptSegment>> {
        // Create Python script for Whisper transcription
        let script = format!(
            r#"
import json
import sys
import whisper

model = whisper.load_model("{model}")
result = model.transcribe(
    "{audio_path}",
    word_timestamps=True,
    verbose=False
)

segments = []
for seg in result["segments"]:
    segment = {{
        "start": seg["start"],
        "end": seg["end"],
        "text": seg["text"].strip(),
        "language": result.get("language"),
    }}

    if "words" in seg:
        segment["words"] = [
            {{
                "word": w["word"].strip(),
                "start": w["start"],
                "end": w["end"],
                "confidence": w.get("probability")
            }}
            for w in seg["words"]
        ]

    segments.append(segment)

print(json.dumps(segments))
"#,
            model = self.model,
            audio_path = audio_path.display()
        );

        let output = Command::new("python3")
            .args(["-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AnalysisError::Whisper(format!("Whisper failed: {stderr}")));
        }

        let segments: Vec<TranscriptSegment> = serde_json::from_slice(&output.stdout)?;
        Ok(segments)
    }

    /// Remote transcription on DGX Spark
    async fn transcribe_remote(
        &self,
        audio_path: &Path,
        host: &str,
    ) -> Result<Vec<TranscriptSegment>> {
        // Copy audio to DGX
        let remote_path = format!("/tmp/nab_audio_{}.wav", std::process::id());

        let audio_str = audio_path.to_str().ok_or_else(|| {
            AnalysisError::Whisper("audio path contains non-UTF8 bytes".to_string())
        })?;

        let scp_status = Command::new("scp")
            .args([audio_str, &format!("{host}:{remote_path}")])
            .status()
            .await?;

        if !scp_status.success() {
            return Err(AnalysisError::Whisper(
                "Failed to copy audio to DGX".to_string(),
            ));
        }

        // Run Whisper on DGX with GPU acceleration
        let script = format!(
            r#"
import json
import whisper

# Use large-v3 on DGX for best quality
model = whisper.load_model("{model}", device="cuda")
result = model.transcribe(
    "{remote_path}",
    word_timestamps=True,
    fp16=True,  # Use FP16 for speed on Blackwell
    verbose=False
)

segments = []
for seg in result["segments"]:
    segment = {{
        "start": seg["start"],
        "end": seg["end"],
        "text": seg["text"].strip(),
        "language": result.get("language"),
    }}

    if "words" in seg:
        segment["words"] = [
            {{
                "word": w["word"].strip(),
                "start": w["start"],
                "end": w["end"],
                "confidence": w.get("probability")
            }}
            for w in seg["words"]
        ]

    segments.append(segment)

print(json.dumps(segments))
"#,
            model = if self.model == "base" {
                "large-v3"
            } else {
                &self.model
            },
            remote_path = remote_path
        );

        let output = Command::new("ssh")
            .args([host, "python3", "-c", &format!("'{script}'")])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        // Clean up remote file
        let _ = Command::new("ssh")
            .args([host, "rm", "-f", &remote_path])
            .status()
            .await;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AnalysisError::Whisper(format!(
                "Remote Whisper failed: {stderr}"
            )));
        }

        let segments: Vec<TranscriptSegment> = serde_json::from_slice(&output.stdout)?;
        Ok(segments)
    }

    /// Transcribe with language hint
    pub async fn transcribe_with_language(
        &self,
        audio_path: &Path,
        language: &str,
    ) -> Result<Vec<TranscriptSegment>> {
        let script = format!(
            r#"
import json
import whisper

model = whisper.load_model("{model}")
result = model.transcribe(
    "{audio_path}",
    language="{language}",
    word_timestamps=True,
    verbose=False
)

segments = []
for seg in result["segments"]:
    segment = {{
        "start": seg["start"],
        "end": seg["end"],
        "text": seg["text"].strip(),
        "language": "{language}",
    }}

    if "words" in seg:
        segment["words"] = [
            {{
                "word": w["word"].strip(),
                "start": w["start"],
                "end": w["end"],
                "confidence": w.get("probability")
            }}
            for w in seg["words"]
        ]

    segments.append(segment)

print(json.dumps(segments))
"#,
            model = self.model,
            audio_path = audio_path.display(),
            language = language
        );

        let output = Command::new("python3")
            .args(["-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AnalysisError::Whisper(format!("Whisper failed: {stderr}")));
        }

        let segments: Vec<TranscriptSegment> = serde_json::from_slice(&output.stdout)?;
        Ok(segments)
    }
}

// ─── vLLM / OpenAI-compatible ASR backend ─────────────────────────────────────

/// Default Qwen3-ASR model served by vLLM.
pub const DEFAULT_VLLM_MODEL: &str = "Qwen/Qwen3-ASR-1.7B";

/// Default vLLM base URL (local deployment).
pub const DEFAULT_VLLM_BASE_URL: &str = "http://localhost:8000";

/// Transcription backend selector.
///
/// Chooses between the local Whisper subprocess, remote DGX Whisper, and the
/// vLLM HTTP API.  The vLLM variant targets any server that exposes the
/// OpenAI-compatible `/v1/audio/transcriptions` endpoint — including
/// Qwen3-ASR, faster-whisper-server, and OpenAI itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptionBackend {
    /// Local Python whisper subprocess.
    Whisper,
    /// Remote execution on DGX Spark via SSH.
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
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }

    /// Create a transcriber with [`DEFAULT_VLLM_BASE_URL`] and
    /// [`DEFAULT_VLLM_MODEL`].
    pub fn default_local() -> Self {
        Self::new(DEFAULT_VLLM_BASE_URL, DEFAULT_VLLM_MODEL)
    }

    /// Full URL of the transcription endpoint.
    pub fn transcription_url(&self) -> String {
        format!("{}/v1/audio/transcriptions", self.base_url)
    }

    /// Model identifier used in requests.
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
        let text = self.post_audio_with_language(&client, audio_path, language).await?;
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
    async fn post_audio(
        &self,
        client: &reqwest::Client,
        audio_path: &Path,
    ) -> Result<String> {
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
            .map(ApiErrorEnvelope::into_message)
            .unwrap_or_else(|_| body.clone());

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

    // ── Existing Whisper serialisation tests ────────────────────────────────

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
        let json = r#"{"text": "Bonjour.", "language": "fr", "duration": 1.2, "task": "transcribe"}"#;
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
}
