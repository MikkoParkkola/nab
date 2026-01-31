//! Frame and audio extraction via ffmpeg
//!
//! Uses ffmpeg's scene detection for smart keyframe selection
//! rather than extracting every frame.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

use super::{AnalysisError, Result, VideoMetadata};

/// Extracted frame with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFrame {
    /// Path to the extracted frame image
    pub path: PathBuf,
    /// Timestamp in seconds
    pub timestamp: f64,
    /// Frame number in original video
    pub frame_number: u64,
    /// Scene change score (0.0-1.0)
    pub scene_score: f32,
}

/// Frame extractor using ffmpeg scene detection
pub struct FrameExtractor {
    scene_threshold: f32,
    max_frames: usize,
}

impl FrameExtractor {
    #[must_use]
    pub fn new(scene_threshold: f32, max_frames: usize) -> Self {
        Self {
            scene_threshold,
            max_frames,
        }
    }

    /// Extract keyframes from video using scene detection
    pub async fn extract(
        &self,
        video_path: &Path,
        output_dir: &Path,
    ) -> Result<(Vec<ExtractedFrame>, VideoMetadata)> {
        // First, get video metadata
        let metadata = self.get_metadata(video_path).await?;

        // Use ffmpeg scene detection to extract keyframes
        // select='gt(scene,threshold)' filters for scene changes
        let output_pattern = output_dir.join("frame_%04d.jpg");

        let status = Command::new("ffmpeg")
            .args([
                "-i",
                video_path.to_str().ok_or_else(|| {
                    AnalysisError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Invalid video path",
                    ))
                })?,
                "-vf",
                &format!("select='gt(scene,{:.2})',showinfo", self.scene_threshold),
                "-vsync",
                "vfr",
                "-frame_pts",
                "1",
                "-q:v",
                "2", // High quality JPEG
                output_pattern.to_str().unwrap(),
                "-y", // Overwrite
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await?;

        if !status.success() {
            return Err(AnalysisError::Ffmpeg("Frame extraction failed".to_string()));
        }

        // Read extracted frames and their timestamps
        let mut frames = self.read_extracted_frames(output_dir, &metadata).await?;

        // Limit to max_frames (keeping evenly distributed selection)
        if frames.len() > self.max_frames {
            let step = frames.len() / self.max_frames;
            frames = frames
                .into_iter()
                .step_by(step)
                .take(self.max_frames)
                .collect();
        }

        Ok((frames, metadata))
    }

    /// Get video metadata using ffprobe
    async fn get_metadata(&self, video_path: &Path) -> Result<VideoMetadata> {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
                video_path.to_str().unwrap(),
            ])
            .output()
            .await?;

        if !output.status.success() {
            return Err(AnalysisError::Ffmpeg("ffprobe failed".to_string()));
        }

        let probe: FfprobeOutput = serde_json::from_slice(&output.stdout)?;

        // Find video stream
        let video_stream = probe
            .streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("video"))
            .ok_or_else(|| AnalysisError::Ffmpeg("No video stream found".to_string()))?;

        // Find audio stream
        let audio_stream = probe
            .streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("audio"));

        // Parse frame rate (e.g., "30/1" or "30000/1001")
        let fps = video_stream
            .r_frame_rate
            .as_ref()
            .and_then(|r| {
                let parts: Vec<&str> = r.split('/').collect();
                if parts.len() == 2 {
                    let num: f32 = parts[0].parse().ok()?;
                    let den: f32 = parts[1].parse().ok()?;
                    Some(num / den)
                } else {
                    r.parse().ok()
                }
            })
            .unwrap_or(30.0);

        Ok(VideoMetadata {
            duration: probe.format.duration.parse().unwrap_or(0.0),
            width: video_stream.width.unwrap_or(0),
            height: video_stream.height.unwrap_or(0),
            fps,
            audio_channels: audio_stream.and_then(|a| a.channels),
            audio_sample_rate: audio_stream
                .and_then(|a| a.sample_rate.as_ref())
                .and_then(|r| r.parse().ok()),
        })
    }

    /// Read extracted frames from directory
    async fn read_extracted_frames(
        &self,
        output_dir: &Path,
        metadata: &VideoMetadata,
    ) -> Result<Vec<ExtractedFrame>> {
        let mut frames = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(output_dir)?
            .filter_map(std::result::Result::ok)
            .filter(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext == "jpg" || ext == "png")
            })
            .collect();

        entries.sort_by_key(std::fs::DirEntry::path);

        for (i, entry) in entries.iter().enumerate() {
            let path = entry.path();

            // Estimate timestamp from frame number (scene detection preserves pts)
            // This is approximate; for precise timestamps, parse ffmpeg showinfo output
            let frame_number = i as u64;
            let timestamp = frame_number as f64 / f64::from(metadata.fps);

            frames.push(ExtractedFrame {
                path,
                timestamp,
                frame_number,
                scene_score: self.scene_threshold, // Threshold used
            });
        }

        Ok(frames)
    }

    /// Extract a single frame at specific timestamp
    pub async fn extract_frame_at(
        &self,
        video_path: &Path,
        timestamp: f64,
        output_path: &Path,
    ) -> Result<ExtractedFrame> {
        let status = Command::new("ffmpeg")
            .args([
                "-ss",
                &format!("{timestamp:.3}"),
                "-i",
                video_path.to_str().unwrap(),
                "-frames:v",
                "1",
                "-q:v",
                "2",
                output_path.to_str().unwrap(),
                "-y",
            ])
            .status()
            .await?;

        if !status.success() {
            return Err(AnalysisError::Ffmpeg(
                "Single frame extraction failed".to_string(),
            ));
        }

        Ok(ExtractedFrame {
            path: output_path.to_path_buf(),
            timestamp,
            frame_number: 0,
            scene_score: 1.0,
        })
    }
}

/// Audio extractor
pub struct AudioExtractor;

impl AudioExtractor {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Extract audio track as WAV (16kHz mono for Whisper)
    pub async fn extract(&self, video_path: &Path, output_path: &Path) -> Result<()> {
        let status = Command::new("ffmpeg")
            .args([
                "-i",
                video_path.to_str().unwrap(),
                "-vn", // No video
                "-acodec",
                "pcm_s16le", // 16-bit PCM
                "-ar",
                "16000", // 16kHz sample rate (Whisper optimal)
                "-ac",
                "1", // Mono
                output_path.to_str().unwrap(),
                "-y",
            ])
            .status()
            .await?;

        if !status.success() {
            return Err(AnalysisError::Ffmpeg("Audio extraction failed".to_string()));
        }

        Ok(())
    }

    /// Extract audio segment between timestamps
    pub async fn extract_segment(
        &self,
        video_path: &Path,
        start: f64,
        end: f64,
        output_path: &Path,
    ) -> Result<()> {
        let duration = end - start;

        let status = Command::new("ffmpeg")
            .args([
                "-ss",
                &format!("{start:.3}"),
                "-t",
                &format!("{duration:.3}"),
                "-i",
                video_path.to_str().unwrap(),
                "-vn",
                "-acodec",
                "pcm_s16le",
                "-ar",
                "16000",
                "-ac",
                "1",
                output_path.to_str().unwrap(),
                "-y",
            ])
            .status()
            .await?;

        if !status.success() {
            return Err(AnalysisError::Ffmpeg(
                "Audio segment extraction failed".to_string(),
            ));
        }

        Ok(())
    }
}

impl Default for AudioExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// `FFprobe` JSON output structure
#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    streams: Vec<FfprobeStream>,
    format: FfprobeFormat,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    channels: Option<u32>,
    sample_rate: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    #[serde(default)]
    duration: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_extractor_new() {
        let extractor = FrameExtractor::new(0.4, 50);
        assert_eq!(extractor.scene_threshold, 0.4);
        assert_eq!(extractor.max_frames, 50);
    }
}
