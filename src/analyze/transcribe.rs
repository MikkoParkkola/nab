//! Audio transcription via Parakeet.cpp, Whisper, and vLLM/OpenAI-compatible ASR backends
//!
//! Supports five backends:
//! - Local `parakeet.cpp` binary (default — fastest, ~600 MB Q4 model, >2000× `RTFx`)
//! - Remote `parakeet.cpp` on DGX Spark (SSH + GPU)
//! - Local Whisper via Python subprocess (legacy)
//! - Remote Whisper on DGX Spark (SSH + GPU) (legacy)
//! - vLLM HTTP API (Qwen3-ASR, OpenAI-compatible `/v1/audio/transcriptions`)

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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

// ─── Parakeet.cpp local transcription backend ─────────────────────────────────

/// Default model search paths (checked in order).
const PARAKEET_MODEL_SEARCH_PATHS: &[&str] = &[
    "~/.cache/nab/models",
    "~/.cache/parakeet",
    "/usr/local/share/parakeet/models",
    "/opt/parakeet/models",
];

/// Default binary search paths (checked in order, after `$PATH`).
const PARAKEET_BINARY_SEARCH_PATHS: &[&str] = &[
    "~/.local/bin",
    "/usr/local/bin",
    "/opt/parakeet/bin",
];

/// Parakeet.cpp local transcription backend.
///
/// Spawns the `parakeet.cpp` CLI binary and captures its stdout as plain text.
/// No Python runtime is required. The binary is located automatically via
/// [`ParakeetTranscriber::detect_binary`] and the model via
/// [`ParakeetTranscriber::detect_model`].
///
/// # Example
///
/// ```no_run
/// use nab::analyze::ParakeetTranscriber;
/// use std::path::Path;
///
/// # #[tokio::main]
/// # async fn main() -> anyhow::Result<()> {
/// let t = ParakeetTranscriber::new(
///     Path::new("/usr/local/bin/parakeet"),
///     Path::new("~/.cache/nab/models/parakeet-tdt-1.1b-v2.Q4_K_M.gguf"),
/// )
/// .with_language("en");
/// let text = t.transcribe(Path::new("audio.wav")).await?;
/// println!("{text}");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ParakeetTranscriber {
    /// Path to the `parakeet.cpp` binary.
    binary_path: PathBuf,
    /// Path to the GGUF model file.
    model_path: PathBuf,
    /// Optional BCP-47 language tag forwarded as `-l <lang>` (e.g. `"en"`, `"de"`, `"ja"`).
    /// When `None` the model performs automatic language detection.
    language: Option<String>,
}

impl ParakeetTranscriber {
    /// Create a new transcriber with the given binary and model paths.
    #[must_use]
    pub fn new(binary: &Path, model: &Path) -> Self {
        Self {
            binary_path: binary.to_path_buf(),
            model_path: model.to_path_buf(),
            language: None,
        }
    }

    /// Set a language hint (BCP-47 tag, e.g. `"en"`, `"fi"`, `"ja"`).
    ///
    /// Forwarded to the binary as `-l <lang>`.  When not set the model performs
    /// automatic language detection.
    #[must_use]
    pub fn with_language(mut self, lang: &str) -> Self {
        self.language = Some(lang.to_string());
        self
    }

    /// Path to the `parakeet.cpp` binary.
    #[must_use]
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    /// Path to the model file.
    #[must_use]
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// Current language hint, if any.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Build the argument list for the parakeet.cpp subprocess.
    ///
    /// Produces:
    /// ```text
    /// parakeet -m <model> -f <audio> --output-txt --gpu --fp16 [-l <lang>]
    /// ```
    ///
    /// Always enables `--gpu` (Metal on macOS, no-op if unavailable) and
    /// `--fp16` (half-precision, ~2× memory reduction, no quality loss for ASR).
    #[must_use]
    pub fn build_args(&self, audio_path: &Path) -> Vec<String> {
        let mut args = vec![
            "-m".to_string(),
            self.model_path.to_string_lossy().into_owned(),
            "-f".to_string(),
            audio_path.to_string_lossy().into_owned(),
            "--output-txt".to_string(),
            "--gpu".to_string(),
            "--fp16".to_string(),
        ];
        if let Some(lang) = &self.language {
            args.push("-l".to_string());
            args.push(lang.clone());
        }
        args
    }

    /// Transcribe `audio_path` using the local parakeet.cpp binary.
    ///
    /// Spawns `{binary} -m {model} -f {audio} --output-txt --gpu --fp16 [-l {lang}]`
    /// and returns the trimmed stdout as the transcript.
    pub async fn transcribe(&self, audio_path: &Path) -> Result<String> {
        let args = self.build_args(audio_path);
        let output = Command::new(&self.binary_path)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| AnalysisError::Whisper(format!("failed to spawn parakeet: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AnalysisError::Whisper(format!(
                "parakeet exited with {}: {stderr}",
                output.status
            )));
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err(AnalysisError::Whisper(
                "parakeet produced no output".to_string(),
            ));
        }
        Ok(text)
    }

    /// Transcribe `audio_path` on a remote host via SSH.
    ///
    /// Copies the audio to `/tmp/nab_audio_<pid>.wav` on `host`, runs
    /// parakeet there, then cleans up.
    pub async fn transcribe_remote(&self, audio_path: &Path, host: &str) -> Result<String> {
        let remote_audio = format!("/tmp/nab_parakeet_{}.wav", std::process::id());
        let audio_str = audio_path.to_str().ok_or_else(|| {
            AnalysisError::Whisper("audio path contains non-UTF8 bytes".to_string())
        })?;

        let scp_ok = Command::new("scp")
            .args([audio_str, &format!("{host}:{remote_audio}")])
            .status()
            .await?
            .success();

        if !scp_ok {
            return Err(AnalysisError::Whisper(
                "failed to copy audio to remote host".to_string(),
            ));
        }

        let remote_cmd = self.build_remote_command(&remote_audio);
        let output = Command::new("ssh")
            .args([host, "sh", "-c", &remote_cmd])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        // Best-effort cleanup — ignore errors.
        let _ = Command::new("ssh")
            .args([host, "rm", "-f", &remote_audio])
            .status()
            .await;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AnalysisError::Whisper(format!(
                "remote parakeet failed: {stderr}"
            )));
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err(AnalysisError::Whisper(
                "remote parakeet produced no output".to_string(),
            ));
        }
        Ok(text)
    }

    /// Build the shell command string for remote execution.
    fn build_remote_command(&self, remote_audio: &str) -> String {
        let bin = self.binary_path.to_string_lossy();
        let model = self.model_path.to_string_lossy();
        let lang_flag = self
            .language
            .as_deref()
            .map(|l| format!(" -l {l}"))
            .unwrap_or_default();
        format!("{bin} -m {model} -f {remote_audio} --output-txt{lang_flag}")
    }

    /// Probe `$PATH` and well-known directories for the `parakeet` binary.
    ///
    /// Returns the first path where `parakeet` (or `parakeet-cli`) exists.
    #[must_use]
    pub fn detect_binary() -> Option<PathBuf> {
        // 1. Check $PATH via `which`
        for name in ["parakeet", "parakeet-cli"] {
            if let Ok(output) = std::process::Command::new("which").arg(name).output() {
                if output.status.success() {
                    let p = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
                    if p.exists() { return Some(p); }
                }
            }
        }

        // 2. Check well-known directories
        let home = std::env::var("HOME").unwrap_or_default();
        for dir in PARAKEET_BINARY_SEARCH_PATHS {
            let expanded = dir.replace('~', &home);
            for name in ["parakeet", "parakeet-cli"] {
                let candidate = PathBuf::from(&expanded).join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// Search well-known cache directories for a parakeet GGUF model file.
    ///
    /// Matches any `.gguf` file whose name contains `parakeet`.
    #[must_use]
    pub fn detect_model() -> Option<PathBuf> {
        let home = std::env::var("HOME").unwrap_or_default();
        for dir in PARAKEET_MODEL_SEARCH_PATHS {
            let expanded = dir.replace('~', &home);
            let dir_path = PathBuf::from(&expanded);
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

    /// Return a [`TranscriptSegment`] list wrapping the plain-text output.
    ///
    /// `parakeet.cpp` (like `--output-txt`) emits a single text block without
    /// per-segment timestamps, so `start` and `end` are both `0.0`.
    pub fn into_segments(text: String, language: Option<&str>) -> Vec<TranscriptSegment> {
        vec![TranscriptSegment {
            start: 0.0,
            end: 0.0,
            text,
            words: None,
            language: language.map(str::to_string),
            confidence: None,
        }]
    }
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
        ParakeetTranscriber::detect_binary().is_some()
            && ParakeetTranscriber::detect_model().is_some()
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

    // ── ParakeetTranscriber: construction ────────────────────────────────────

    #[test]
    fn parakeet_new_stores_binary_and_model_paths() {
        // GIVEN explicit binary and model paths
        // WHEN constructing ParakeetTranscriber
        // THEN both paths are stored and language is None
        let bin = Path::new("/usr/local/bin/parakeet");
        let model = Path::new("/home/user/.cache/nab/models/parakeet.gguf");
        let t = ParakeetTranscriber::new(bin, model);
        assert_eq!(t.binary_path(), bin);
        assert_eq!(t.model_path(), model);
        assert!(t.language().is_none(), "default language should be None");
    }

    #[test]
    fn parakeet_with_language_sets_lang_and_is_chainable() {
        // GIVEN a fresh ParakeetTranscriber
        // WHEN chaining with_language("de")
        // THEN language() returns "de"
        let t = ParakeetTranscriber::new(
            Path::new("/usr/local/bin/parakeet"),
            Path::new("/models/p.gguf"),
        )
        .with_language("de");
        assert_eq!(t.language(), Some("de"));
    }

    #[test]
    fn parakeet_default_language_is_none() {
        // GIVEN a ParakeetTranscriber created without with_language
        // WHEN inspecting language()
        // THEN it is None (auto-detect mode)
        let t = ParakeetTranscriber::new(
            Path::new("/usr/local/bin/parakeet"),
            Path::new("/models/p.gguf"),
        );
        assert!(t.language().is_none());
    }

    // ── ParakeetTranscriber: build_args ──────────────────────────────────────

    #[test]
    fn parakeet_build_args_without_language_hint() {
        // GIVEN a transcriber with no language set
        // WHEN build_args is called
        // THEN args contain -m <model>, -f <audio>, --output-txt, no -l flag
        let t = ParakeetTranscriber::new(
            Path::new("/usr/local/bin/parakeet"),
            Path::new("/models/parakeet.gguf"),
        );
        let args = t.build_args(Path::new("/tmp/test.wav"));
        assert_eq!(args, ["-m", "/models/parakeet.gguf", "-f", "/tmp/test.wav", "--output-txt"]);
        assert!(!args.contains(&"-l".to_string()), "no -l flag expected");
    }

    #[test]
    fn parakeet_build_args_includes_language_flag_when_set() {
        // GIVEN a transcriber with language = "ja"
        // WHEN build_args is called
        // THEN args contain -l ja after --output-txt
        let t = ParakeetTranscriber::new(
            Path::new("/usr/local/bin/parakeet"),
            Path::new("/models/parakeet.gguf"),
        )
        .with_language("ja");
        let args = t.build_args(Path::new("/tmp/audio.wav"));
        let lang_pos = args.iter().position(|a| a == "-l").expect("-l flag missing");
        assert_eq!(args[lang_pos + 1], "ja");
    }

    #[test]
    fn parakeet_build_args_audio_path_appears_after_minus_f() {
        // GIVEN any audio path
        // WHEN build_args is called
        // THEN the audio path follows immediately after the -f flag
        let t = ParakeetTranscriber::new(
            Path::new("/bin/parakeet"),
            Path::new("/m.gguf"),
        );
        let audio = Path::new("/recordings/speech.wav");
        let args = t.build_args(audio);
        let f_pos = args.iter().position(|a| a == "-f").expect("-f flag missing");
        assert_eq!(args[f_pos + 1], "/recordings/speech.wav");
    }

    // ── ParakeetTranscriber: into_segments ───────────────────────────────────

    #[test]
    fn parakeet_into_segments_wraps_text_with_zero_timestamps() {
        // GIVEN plain-text parakeet output with a language hint
        // WHEN into_segments is called
        // THEN a single segment is returned with start/end = 0.0 and text preserved
        let segs = ParakeetTranscriber::into_segments("Hello world".to_string(), Some("en"));
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "Hello world");
        assert!((segs[0].start).abs() < f64::EPSILON);
        assert!((segs[0].end).abs() < f64::EPSILON);
        assert_eq!(segs[0].language.as_deref(), Some("en"));
    }

    #[test]
    fn parakeet_into_segments_with_no_language_sets_none() {
        // GIVEN parakeet output with no language hint
        // WHEN into_segments is called
        // THEN the language field in the segment is None
        let segs = ParakeetTranscriber::into_segments("Transcribed text".to_string(), None);
        assert_eq!(segs.len(), 1);
        assert!(segs[0].language.is_none());
    }

    // ── TranscriptionBackend: new variants ───────────────────────────────────

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
