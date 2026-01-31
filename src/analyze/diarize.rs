//! Speaker diarization via pyannote
//!
//! Identifies who speaks when in the audio track.
//! Requires pyannote.audio Python package and `HuggingFace` token.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

use super::{AnalysisError, Result};

/// Speaker segment identifying who speaks when
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerSegment {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// Speaker diarization engine using pyannote
pub struct Diarizer {
    dgx_host: Option<String>,
}

impl Diarizer {
    pub fn new(dgx_host: Option<String>) -> Result<Self> {
        Ok(Self { dgx_host })
    }

    /// Perform speaker diarization on audio file
    pub async fn diarize(&self, audio_path: &Path) -> Result<Vec<SpeakerSegment>> {
        if let Some(host) = &self.dgx_host {
            self.diarize_remote(audio_path, host).await
        } else {
            self.diarize_local(audio_path).await
        }
    }

    /// Local diarization using pyannote
    async fn diarize_local(&self, audio_path: &Path) -> Result<Vec<SpeakerSegment>> {
        let script = format!(
            r#"
import json
import os
from pyannote.audio import Pipeline

# Load pipeline (requires HF_TOKEN env var)
pipeline = Pipeline.from_pretrained(
    "pyannote/speaker-diarization-3.1",
    use_auth_token=os.environ.get("HF_TOKEN")
)

# Run diarization
diarization = pipeline("{audio_path}")

# Convert to segments
segments = []
for turn, _, speaker in diarization.itertracks(yield_label=True):
    segments.append({{
        "speaker": speaker,
        "start": turn.start,
        "end": turn.end,
    }})

print(json.dumps(segments))
"#,
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

            // Check for common errors
            if stderr.contains("HF_TOKEN") || stderr.contains("authentication") {
                return Err(AnalysisError::Diarization(
                    "HuggingFace token required. Set HF_TOKEN environment variable.".to_string(),
                ));
            }

            return Err(AnalysisError::Diarization(format!(
                "Diarization failed: {stderr}"
            )));
        }

        let segments: Vec<SpeakerSegment> = serde_json::from_slice(&output.stdout)?;
        Ok(segments)
    }

    /// Remote diarization on DGX Spark
    async fn diarize_remote(&self, audio_path: &Path, host: &str) -> Result<Vec<SpeakerSegment>> {
        let remote_path = format!("/tmp/microfetch_diarize_{}.wav", std::process::id());

        // Copy audio to DGX
        let scp_status = Command::new("scp")
            .args([
                audio_path.to_str().unwrap(),
                &format!("{host}:{remote_path}"),
            ])
            .status()
            .await?;

        if !scp_status.success() {
            return Err(AnalysisError::Diarization(
                "Failed to copy audio to DGX".to_string(),
            ));
        }

        // Run pyannote on DGX with GPU
        let script = format!(
            r#"
import json
import os
import torch
from pyannote.audio import Pipeline

pipeline = Pipeline.from_pretrained(
    "pyannote/speaker-diarization-3.1",
    use_auth_token=os.environ.get("HF_TOKEN")
)

# Move to GPU
pipeline.to(torch.device("cuda"))

diarization = pipeline("{remote_path}")

segments = []
for turn, _, speaker in diarization.itertracks(yield_label=True):
    segments.append({{
        "speaker": speaker,
        "start": turn.start,
        "end": turn.end,
    }})

print(json.dumps(segments))
"#
        );

        let output = Command::new("ssh")
            .args([host, "python3", "-c", &format!("'{script}'")])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        // Clean up
        let _ = Command::new("ssh")
            .args([host, "rm", "-f", &remote_path])
            .status()
            .await;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AnalysisError::Diarization(format!(
                "Remote diarization failed: {stderr}"
            )));
        }

        let segments: Vec<SpeakerSegment> = serde_json::from_slice(&output.stdout)?;
        Ok(segments)
    }

    /// Diarize with known number of speakers
    pub async fn diarize_with_speakers(
        &self,
        audio_path: &Path,
        num_speakers: u32,
    ) -> Result<Vec<SpeakerSegment>> {
        let script = format!(
            r#"
import json
import os
from pyannote.audio import Pipeline

pipeline = Pipeline.from_pretrained(
    "pyannote/speaker-diarization-3.1",
    use_auth_token=os.environ.get("HF_TOKEN")
)

diarization = pipeline("{audio_path}", num_speakers={num_speakers})

segments = []
for turn, _, speaker in diarization.itertracks(yield_label=True):
    segments.append({{
        "speaker": speaker,
        "start": turn.start,
        "end": turn.end,
    }})

print(json.dumps(segments))
"#,
            audio_path = audio_path.display(),
            num_speakers = num_speakers
        );

        let output = Command::new("python3")
            .args(["-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AnalysisError::Diarization(format!(
                "Diarization failed: {stderr}"
            )));
        }

        let segments: Vec<SpeakerSegment> = serde_json::from_slice(&output.stdout)?;
        Ok(segments)
    }

    /// Merge overlapping or adjacent speaker segments
    #[must_use]
    pub fn merge_segments(segments: &[SpeakerSegment], gap_threshold: f64) -> Vec<SpeakerSegment> {
        if segments.is_empty() {
            return Vec::new();
        }

        let mut merged = Vec::new();
        let mut current = segments[0].clone();

        for seg in segments.iter().skip(1) {
            if seg.speaker == current.speaker && seg.start - current.end <= gap_threshold {
                // Extend current segment
                current.end = seg.end;
            } else {
                merged.push(current);
                current = seg.clone();
            }
        }
        merged.push(current);

        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_speaker_segment_serialization() {
        let segment = SpeakerSegment {
            speaker: "SPEAKER_01".to_string(),
            start: 0.0,
            end: 5.5,
            confidence: Some(0.92),
        };

        let json = serde_json::to_string(&segment).unwrap();
        assert!(json.contains("SPEAKER_01"));
        assert!(json.contains("5.5"));
    }

    #[test]
    fn test_merge_segments() {
        let segments = vec![
            SpeakerSegment {
                speaker: "A".to_string(),
                start: 0.0,
                end: 2.0,
                confidence: None,
            },
            SpeakerSegment {
                speaker: "A".to_string(),
                start: 2.1,
                end: 4.0,
                confidence: None,
            },
            SpeakerSegment {
                speaker: "B".to_string(),
                start: 4.5,
                end: 6.0,
                confidence: None,
            },
        ];

        let merged = Diarizer::merge_segments(&segments, 0.5);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].end, 4.0); // A segments merged
        assert_eq!(merged[1].speaker, "B");
    }
}
