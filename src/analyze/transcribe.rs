//! Audio transcription via Whisper
//!
//! Supports both local Whisper (via Python subprocess) and
//! remote execution on DGX Spark for GPU acceleration.

use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use serde::{Deserialize, Serialize};

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
        let script = format!(r#"
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
            return Err(AnalysisError::Whisper(format!(
                "Whisper failed: {stderr}"
            )));
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
        let remote_path = format!("/tmp/microfetch_audio_{}.wav", std::process::id());

        let scp_status = Command::new("scp")
            .args([
                audio_path.to_str().unwrap(),
                &format!("{host}:{remote_path}"),
            ])
            .status()
            .await?;

        if !scp_status.success() {
            return Err(AnalysisError::Whisper("Failed to copy audio to DGX".to_string()));
        }

        // Run Whisper on DGX with GPU acceleration
        let script = format!(r#"
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
            model = if self.model == "base" { "large-v3" } else { &self.model },
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
        let script = format!(r#"
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
            return Err(AnalysisError::Whisper(format!(
                "Whisper failed: {stderr}"
            )));
        }

        let segments: Vec<TranscriptSegment> = serde_json::from_slice(&output.stdout)?;
        Ok(segments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
