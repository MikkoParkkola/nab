//! ffmpeg-based video compositor for burning overlays into video streams
//!
//! Supports:
//! - ASS subtitle burning
//! - Multiple overlay tracks
//! - Real-time streaming mode
//! - Batch file processing

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::overlay::OverlayTrack;
use super::subtitle::{AssGenerator, SubtitleEntry, SubtitleGenerator, SubtitleStyle};

/// Output format for compositor
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompositorOutput {
    /// MPEG-TS (streamable, for piping)
    #[default]
    MpegTs,
    /// MP4 (fragmented, for live streaming)
    FragmentedMp4,
    /// MP4 (standard, for file output)
    Mp4,
    /// MKV (Matroska)
    Mkv,
    /// Raw video (for further processing)
    RawVideo,
}

impl CompositorOutput {
    /// Get ffmpeg format name
    #[must_use]
    pub fn ffmpeg_format(&self) -> &'static str {
        match self {
            Self::MpegTs => "mpegts",
            Self::FragmentedMp4 => "mp4",
            Self::Mp4 => "mp4",
            Self::Mkv => "matroska",
            Self::RawVideo => "rawvideo",
        }
    }

    /// Get file extension
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self {
            Self::MpegTs => "ts",
            Self::FragmentedMp4 | Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::RawVideo => "raw",
        }
    }
}

/// Configuration for the compositor
#[derive(Debug, Clone)]
pub struct CompositorConfig {
    /// Path to ffmpeg binary
    pub ffmpeg_path: String,
    /// Output format
    pub output_format: CompositorOutput,
    /// Video codec (None = copy)
    pub video_codec: Option<String>,
    /// Audio codec (None = copy)
    pub audio_codec: Option<String>,
    /// Video bitrate (e.g., "5M")
    pub video_bitrate: Option<String>,
    /// Audio bitrate (e.g., "128k")
    pub audio_bitrate: Option<String>,
    /// Hardware acceleration (e.g., "videotoolbox", "cuda")
    pub hwaccel: Option<String>,
    /// Additional ffmpeg input arguments
    pub input_args: Vec<String>,
    /// Additional ffmpeg output arguments
    pub output_args: Vec<String>,
    /// Buffer size for streaming (bytes)
    pub buffer_size: usize,
}

impl Default for CompositorConfig {
    fn default() -> Self {
        Self {
            ffmpeg_path: which::which("ffmpeg").map_or_else(
                |_| "ffmpeg".to_string(),
                |p| p.to_string_lossy().to_string(),
            ),
            output_format: CompositorOutput::default(),
            video_codec: None,
            audio_codec: None,
            video_bitrate: None,
            audio_bitrate: None,
            hwaccel: None,
            input_args: Vec::new(),
            output_args: Vec::new(),
            buffer_size: 64 * 1024, // 64KB
        }
    }
}

impl CompositorConfig {
    /// Create config for real-time streaming
    #[must_use]
    pub fn streaming() -> Self {
        Self {
            output_format: CompositorOutput::MpegTs,
            video_codec: Some("libx264".to_string()),
            output_args: vec![
                "-preset".to_string(),
                "ultrafast".to_string(),
                "-tune".to_string(),
                "zerolatency".to_string(),
            ],
            ..Default::default()
        }
    }

    /// Create config for high-quality file output
    #[must_use]
    pub fn high_quality() -> Self {
        Self {
            output_format: CompositorOutput::Mp4,
            video_codec: Some("libx264".to_string()),
            video_bitrate: Some("10M".to_string()),
            audio_codec: Some("aac".to_string()),
            audio_bitrate: Some("192k".to_string()),
            output_args: vec![
                "-preset".to_string(),
                "slow".to_string(),
                "-crf".to_string(),
                "18".to_string(),
            ],
            ..Default::default()
        }
    }

    /// Enable hardware acceleration
    #[must_use]
    pub fn with_hwaccel(mut self, accel: &str) -> Self {
        self.hwaccel = Some(accel.to_string());

        // Set appropriate video codec for the accelerator
        self.video_codec = Some(match accel {
            "videotoolbox" => "h264_videotoolbox".to_string(),
            "cuda" | "nvenc" => "h264_nvenc".to_string(),
            "vaapi" => "h264_vaapi".to_string(),
            "qsv" => "h264_qsv".to_string(),
            _ => "libx264".to_string(),
        });

        self
    }
}

/// ffmpeg-based video compositor
pub struct Compositor {
    config: CompositorConfig,
}

impl Compositor {
    /// Create a new compositor with default config
    pub fn new() -> Result<Self> {
        Ok(Self {
            config: CompositorConfig::default(),
        })
    }

    /// Create a new compositor with custom config
    #[must_use]
    pub fn with_config(config: CompositorConfig) -> Self {
        Self { config }
    }

    /// Check if ffmpeg is available
    pub async fn check_available(&self) -> bool {
        Command::new(&self.config.ffmpeg_path)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Build filter complex string for overlays
    fn build_filter_complex(
        &self,
        subtitle_file: Option<&Path>,
        overlay_tracks: &[OverlayTrack],
    ) -> String {
        let mut filters = Vec::new();

        // ASS subtitle filter (primary subtitles)
        if let Some(ass_path) = subtitle_file {
            let path_escaped = ass_path
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace(':', "\\:")
                .replace('\'', "\\'");
            filters.push(format!("ass='{path_escaped}'"));
        }

        // Drawtext filters for additional overlay tracks
        for track in overlay_tracks {
            if track.entries.is_empty() {
                continue;
            }

            // For each entry, create a drawtext with enable condition
            for entry in &track.entries {
                let style = entry.style.as_ref().unwrap_or(&track.default_style);
                let position = entry.position;

                let (x, y) = position.to_drawtext_position(20);

                // Escape text for ffmpeg
                let text = entry
                    .text
                    .replace('\\', "\\\\")
                    .replace(':', "\\:")
                    .replace('\'', "\\'")
                    .replace('\n', "\\n");

                // Time-based enable expression
                let start_sec = entry.start_ms as f64 / 1000.0;
                let end_sec = entry.end_ms as f64 / 1000.0;

                let drawtext = format!(
                    "drawtext=text='{text}':\
                     fontfile=/System/Library/Fonts/Supplemental/{}.ttf:\
                     fontsize={fontsize}:\
                     fontcolor=0x{color}:\
                     x={x}:y={y}:\
                     borderw={borderw}:\
                     bordercolor=0x{bordercolor}:\
                     enable='between(t,{start_sec},{end_sec})'",
                    style.font_name,
                    fontsize = style.font_size,
                    color = style.color,
                    borderw = style.outline_width as u32,
                    bordercolor = style.outline_color,
                );

                filters.push(drawtext);
            }
        }

        filters.join(",")
    }

    /// Build ffmpeg arguments
    fn build_args(&self, input: &str, output: Option<&str>, filter_complex: &str) -> Vec<String> {
        let mut args = Vec::new();

        // Hide banner, show stats
        args.extend(
            ["-hide_banner", "-loglevel", "warning", "-stats"]
                .iter()
                .map(std::string::ToString::to_string),
        );

        // Hardware acceleration
        if let Some(ref accel) = self.config.hwaccel {
            args.push("-hwaccel".to_string());
            args.push(accel.clone());
        }

        // Custom input args
        args.extend(self.config.input_args.clone());

        // Input
        args.push("-i".to_string());
        args.push(input.to_string());

        // Video filter
        if !filter_complex.is_empty() {
            args.push("-vf".to_string());
            args.push(filter_complex.to_string());
        }

        // Video codec
        if let Some(ref codec) = self.config.video_codec {
            args.push("-c:v".to_string());
            args.push(codec.clone());
        } else {
            args.push("-c:v".to_string());
            args.push("copy".to_string());
        }

        // Video bitrate
        if let Some(ref bitrate) = self.config.video_bitrate {
            args.push("-b:v".to_string());
            args.push(bitrate.clone());
        }

        // Audio codec
        if let Some(ref codec) = self.config.audio_codec {
            args.push("-c:a".to_string());
            args.push(codec.clone());
        } else {
            args.push("-c:a".to_string());
            args.push("copy".to_string());
        }

        // Audio bitrate
        if let Some(ref bitrate) = self.config.audio_bitrate {
            args.push("-b:a".to_string());
            args.push(bitrate.clone());
        }

        // Custom output args
        args.extend(self.config.output_args.clone());

        // Output
        if let Some(path) = output {
            args.push("-y".to_string()); // Overwrite
            args.push(path.to_string());
        } else {
            // Pipe output
            args.push("-f".to_string());
            args.push(self.config.output_format.ffmpeg_format().to_string());

            // Fragmented MP4 for streaming
            if self.config.output_format == CompositorOutput::FragmentedMp4 {
                args.push("-movflags".to_string());
                args.push("frag_keyframe+empty_moov+default_base_moof".to_string());
            }

            args.push("pipe:1".to_string());
        }

        args
    }

    /// Composite video with subtitles to a file
    pub async fn composite_to_file(
        &self,
        input: &str,
        output: &Path,
        subtitle_file: Option<&Path>,
        overlay_tracks: &[OverlayTrack],
    ) -> Result<()> {
        let filter = self.build_filter_complex(subtitle_file, overlay_tracks);
        let args = self.build_args(input, Some(&output.to_string_lossy()), &filter);

        debug!("ffmpeg args: {:?}", args);

        let status = Command::new(&self.config.ffmpeg_path)
            .args(&args)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;

        if !status.success() {
            return Err(anyhow!("ffmpeg exited with status: {status}"));
        }

        info!("Composited video to {:?}", output);
        Ok(())
    }

    /// Composite video with subtitles to an async writer (streaming)
    pub async fn composite_to_stream<W: AsyncWrite + Unpin + Send>(
        &self,
        input: &str,
        subtitle_file: Option<&Path>,
        overlay_tracks: &[OverlayTrack],
        output: &mut W,
    ) -> Result<u64> {
        let filter = self.build_filter_complex(subtitle_file, overlay_tracks);
        let args = self.build_args(input, None, &filter);

        debug!("ffmpeg streaming args: {:?}", args);

        let mut child = Command::new(&self.config.ffmpeg_path)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture ffmpeg stdout"))?;

        let stderr = child.stderr.take();

        // Spawn stderr reader for logging
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = tokio::io::AsyncBufReadExt::lines(reader);
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.contains("Error") || line.contains("Warning") {
                        warn!("ffmpeg: {}", line);
                    } else {
                        debug!("ffmpeg: {}", line);
                    }
                }
            });
        }

        // Copy stdout to output
        let mut stdout_reader = BufReader::new(stdout);
        let mut buffer = vec![0u8; self.config.buffer_size];
        let mut total_bytes = 0u64;

        loop {
            let n = stdout_reader.read(&mut buffer).await?;
            if n == 0 {
                break;
            }

            output.write_all(&buffer[..n]).await?;
            total_bytes += n as u64;
        }

        let status = child.wait().await?;

        if !status.success() {
            return Err(anyhow!("ffmpeg exited with status: {status}"));
        }

        output.flush().await?;
        info!("Streamed {} bytes via compositor", total_bytes);

        Ok(total_bytes)
    }

    /// Generate ASS file from subtitle entries and overlay tracks
    pub async fn generate_combined_ass(
        &self,
        subtitles: &[SubtitleEntry],
        overlay_tracks: &[OverlayTrack],
        output_path: &Path,
    ) -> Result<()> {
        // Collect all styles
        let mut styles = vec![
            SubtitleStyle::default(),
            SubtitleStyle::speaker_label(),
            SubtitleStyle::analysis_overlay(),
        ];

        for track in overlay_tracks {
            styles.push(track.to_ass_style());
        }

        // Create generator with all styles
        let mut generator = AssGenerator::new();
        for style in styles {
            generator = generator.with_style(style);
        }

        // Combine all entries
        let mut all_entries = subtitles.to_vec();
        for track in overlay_tracks {
            all_entries.extend(track.to_subtitle_entries());
        }

        // Sort by start time
        all_entries.sort_by_key(|e| e.start_ms);

        // Write to file
        generator.write_to_file(&all_entries, output_path).await?;

        Ok(())
    }
}

impl Default for Compositor {
    fn default() -> Self {
        Self::new().expect("Failed to create compositor")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_compositor_config_streaming() {
        let config = CompositorConfig::streaming();
        assert_eq!(config.output_format, CompositorOutput::MpegTs);
        assert!(config.output_args.contains(&"ultrafast".to_string()));
    }

    #[test]
    fn test_compositor_config_hwaccel() {
        let config = CompositorConfig::default().with_hwaccel("videotoolbox");
        assert_eq!(config.hwaccel, Some("videotoolbox".to_string()));
        assert_eq!(config.video_codec, Some("h264_videotoolbox".to_string()));
    }

    #[test]
    fn test_build_filter_complex_subtitle_only() {
        let compositor = Compositor::default();
        let path = PathBuf::from("/tmp/test.ass");
        let filter = compositor.build_filter_complex(Some(&path), &[]);

        assert!(filter.contains("ass="));
        assert!(filter.contains("/tmp/test.ass"));
    }

    #[test]
    fn test_build_args_file_output() {
        let compositor = Compositor::default();
        let args = compositor.build_args("input.mp4", Some("output.mp4"), "");

        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"input.mp4".to_string()));
        assert!(args.contains(&"-y".to_string()));
        assert!(args.contains(&"output.mp4".to_string()));
    }

    #[test]
    fn test_build_args_pipe_output() {
        let compositor = Compositor::default();
        let args = compositor.build_args("input.mp4", None, "");

        assert!(args.contains(&"pipe:1".to_string()));
        assert!(args.contains(&"-f".to_string()));
    }

    #[test]
    fn test_output_format_extension() {
        assert_eq!(CompositorOutput::Mp4.extension(), "mp4");
        assert_eq!(CompositorOutput::MpegTs.extension(), "ts");
        assert_eq!(CompositorOutput::Mkv.extension(), "mkv");
    }
}
