//! ffmpeg bridge backend for streaming
//!
//! Uses ffmpeg subprocess for:
//! - DASH streams (.mpd)
//! - Encrypted HLS (Widevine/AES)
//! - Transcoding
//! - Complex format handling

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::stream::backend::{
    BackendType, ProgressCallback, StreamBackend, StreamConfig, StreamProgress,
};

/// ffmpeg-based streaming backend
pub struct FfmpegBackend {
    /// Path to ffmpeg binary
    ffmpeg_path: String,
    /// Additional ffmpeg arguments
    extra_args: Vec<String>,
    /// Transcoding options (e.g., "-c:v libx265 -crf 28")
    transcode_opts: Option<String>,
}

impl FfmpegBackend {
    /// Create new ffmpeg backend, searching for binary in PATH
    pub fn new() -> Result<Self> {
        let ffmpeg_path = which::which("ffmpeg").map_or_else(|_| "ffmpeg".to_string(), |p| p.to_string_lossy().to_string());

        Ok(Self {
            ffmpeg_path,
            extra_args: Vec::new(),
            transcode_opts: None,
        })
    }

    /// Specify custom ffmpeg binary path
    #[must_use] 
    pub fn with_ffmpeg_path(mut self, path: &str) -> Self {
        self.ffmpeg_path = path.to_string();
        self
    }

    /// Enable transcoding (e.g., "-c:v libx265 -crf 28")
    #[must_use] 
    pub fn with_transcode_opts(mut self, opts: &str) -> Self {
        self.transcode_opts = Some(opts.to_string());
        self
    }

    /// Add extra ffmpeg arguments
    #[must_use] 
    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    /// Build ffmpeg command arguments
    fn build_args(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        output_path: Option<&str>,
        duration_secs: Option<u64>,
    ) -> Vec<String> {
        let mut args = Vec::new();

        // Quiet mode (less stderr noise)
        args.extend(
            ["-hide_banner", "-loglevel", "warning", "-stats"]
                .iter()
                .map(std::string::ToString::to_string),
        );

        // Headers
        if !config.headers.is_empty() {
            let header_str = config
                .headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join("\r\n");
            args.push("-headers".to_string());
            args.push(format!("{header_str}\r\n"));
        }

        // Duration limit for live streams
        if let Some(dur) = duration_secs {
            args.push("-t".to_string());
            args.push(dur.to_string());
        }

        // HLS/DASH specific options (from yle-dl) + speed optimizations
        args.extend(
            [
                // Buffer and queue settings
                "-thread_queue_size", "2048",
                "-seekable", "0",
                "-allowed_extensions", "ts,aac,vtt",
                // Speed optimizations for VOD (download as fast as possible)
                "-fflags", "+genpts+discardcorrupt",
                // Reconnection for reliability
                "-reconnect", "1",
                "-reconnect_streamed", "1",
                "-reconnect_delay_max", "2",
            ]
                .iter()
                .map(std::string::ToString::to_string),
        );

        // Input
        args.push("-i".to_string());
        args.push(manifest_url.to_string());

        // Transcoding or copy
        if let Some(ref opts) = self.transcode_opts {
            // Parse transcode options
            args.extend(opts.split_whitespace().map(String::from));
        } else {
            // Copy streams without re-encoding
            args.extend(["-c", "copy"].iter().map(std::string::ToString::to_string));
        }

        // Extra args
        args.extend(self.extra_args.clone());

        // Output
        if let Some(path) = output_path {
            args.push("-y".to_string()); // Overwrite
            args.push(path.to_string());
        } else {
            // Output to stdout as MPEG-TS (streamable)
            args.extend(["-f", "mpegts", "pipe:1"].iter().map(std::string::ToString::to_string));
        }

        args
    }

    /// Check if ffmpeg is available
    pub async fn check_available(&self) -> bool {
        Command::new(&self.ffmpeg_path)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Parse progress from ffmpeg stderr
    fn parse_progress(line: &str) -> Option<FfmpegProgress> {
        // ffmpeg progress format: "frame=  123 fps= 30 ... time=00:01:23.45 bitrate=1234.5kbits/s speed=1.5x"
        if !line.contains("time=") {
            return None;
        }

        let time = line.split("time=").nth(1)?.split_whitespace().next()?;

        // Parse time (HH:MM:SS.ms)
        let parts: Vec<&str> = time.split(':').collect();
        if parts.len() != 3 {
            return None;
        }

        let hours: f64 = parts[0].parse().ok()?;
        let minutes: f64 = parts[1].parse().ok()?;
        let seconds: f64 = parts[2].parse().ok()?;
        let total_seconds = hours * 3600.0 + minutes * 60.0 + seconds;

        let speed = line
            .split("speed=")
            .nth(1)
            .and_then(|s| s.trim_end_matches('x').parse().ok());

        let bitrate = line.split("bitrate=").nth(1).and_then(|s| {
            let s = s.split_whitespace().next()?;
            let s = s.trim_end_matches("kbits/s");
            s.parse::<f64>().ok().map(|b| (b * 1000.0) as u64)
        });

        Some(FfmpegProgress {
            time_seconds: total_seconds,
            speed,
            bitrate_bps: bitrate,
        })
    }

    /// Stream with a duration limit (useful for live streams)
    pub async fn stream_with_duration<W: AsyncWrite + Unpin + Send>(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        output: &mut W,
        duration_secs: u64,
        progress: Option<ProgressCallback>,
    ) -> Result<()> {
        let args = self.build_args(manifest_url, config, None, Some(duration_secs));
        debug!("ffmpeg args (with duration): {:?}", args);

        let mut child = Command::new(&self.ffmpeg_path)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture ffmpeg stdout"))?;

        let start_time = std::time::Instant::now();
        let mut stdout_reader = BufReader::new(stdout);
        let mut buffer = [0u8; 64 * 1024];
        let mut total_bytes = 0u64;

        loop {
            let n = stdout_reader.read(&mut buffer).await?;
            if n == 0 {
                break;
            }

            output.write_all(&buffer[..n]).await?;
            total_bytes += n as u64;

            if let Some(ref cb) = progress {
                cb(StreamProgress {
                    bytes_downloaded: total_bytes,
                    segments_completed: 0,
                    segments_total: None,
                    elapsed_seconds: start_time.elapsed().as_secs_f64(),
                });
            }
        }

        let status = child.wait().await?;

        if !status.success() {
            // Duration limit often causes ffmpeg to exit with signal, which is ok
            let code = status.code();
            if code != Some(255) && code.is_some() {
                return Err(anyhow!("ffmpeg exited with status: {status}"));
            }
        }

        output.flush().await?;
        Ok(())
    }
}

#[async_trait]
impl StreamBackend for FfmpegBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::Ffmpeg
    }

    fn can_handle(&self, manifest_url: &str, encrypted: bool) -> bool {
        // ffmpeg can handle everything
        manifest_url.contains(".m3u8") || manifest_url.contains(".mpd") || encrypted
    }

    async fn stream_to<W: AsyncWrite + Unpin + Send>(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        output: &mut W,
        progress: Option<ProgressCallback>,
    ) -> Result<()> {
        let args = self.build_args(manifest_url, config, None, None);
        debug!("ffmpeg args: {:?}", args);

        let mut child = Command::new(&self.ffmpeg_path)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture ffmpeg stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to capture ffmpeg stderr"))?;

        let start_time = std::time::Instant::now();

        // Spawn stderr reader for progress
        let progress_active = progress.is_some();
        let stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = tokio::io::AsyncBufReadExt::lines(reader);

            while let Ok(Some(line)) = lines.next_line().await {
                if progress_active {
                    if let Some(_prog) = FfmpegBackend::parse_progress(&line) {
                        // Progress is available but we can't easily pass it back
                        // due to ownership. Log it instead.
                        debug!("ffmpeg: {}", line);
                    }
                }
                // Log warnings/errors
                if line.contains("Error") || line.contains("Warning") {
                    warn!("ffmpeg: {}", line);
                }
            }
        });

        // Copy stdout to output
        let mut stdout_reader = BufReader::new(stdout);
        let mut buffer = [0u8; 64 * 1024]; // 64KB buffer
        let mut total_bytes = 0u64;

        loop {
            let n = stdout_reader.read(&mut buffer).await?;
            if n == 0 {
                break;
            }

            output.write_all(&buffer[..n]).await?;
            total_bytes += n as u64;

            if let Some(ref cb) = progress {
                cb(StreamProgress {
                    bytes_downloaded: total_bytes,
                    segments_completed: 0,
                    segments_total: None,
                    elapsed_seconds: start_time.elapsed().as_secs_f64(),
                });
            }
        }

        // Wait for process to complete
        let status = child.wait().await?;
        stderr_handle.abort(); // Stop stderr reader

        if !status.success() {
            return Err(anyhow!("ffmpeg exited with status: {status}"));
        }

        output.flush().await?;
        info!("Streamed {} bytes via ffmpeg", total_bytes);

        Ok(())
    }

    async fn stream_to_file(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        path: &Path,
        progress: Option<ProgressCallback>,
    ) -> Result<()> {
        let path_str = path.to_string_lossy();
        let args = self.build_args(manifest_url, config, Some(&path_str), None);
        debug!("ffmpeg args: {:?}", args);

        let mut child = Command::new(&self.ffmpeg_path)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to capture ffmpeg stderr"))?;

        let start_time = std::time::Instant::now();

        // Read stderr for progress
        let reader = BufReader::new(stderr);
        let mut lines = tokio::io::AsyncBufReadExt::lines(reader);

        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(prog) = Self::parse_progress(&line) {
                if let Some(ref cb) = progress {
                    cb(StreamProgress {
                        bytes_downloaded: 0, // Not easily available for file output
                        segments_completed: prog.time_seconds as u32,
                        segments_total: None,
                        elapsed_seconds: start_time.elapsed().as_secs_f64(),
                    });
                }
            }

            if line.contains("Error") {
                warn!("ffmpeg: {}", line);
            }
        }

        let status = child.wait().await?;

        if !status.success() {
            return Err(anyhow!("ffmpeg exited with status: {status}"));
        }

        info!("Saved stream to {:?} via ffmpeg", path);
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct FfmpegProgress {
    time_seconds: f64,
    #[allow(dead_code)]
    speed: Option<f64>,
    #[allow(dead_code)]
    bitrate_bps: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_parse_progress() {
        let line = "frame=  123 fps= 30 q=28.0 size=   1234kB time=00:01:23.45 bitrate=1234.5kbits/s speed=1.5x";
        let prog = FfmpegBackend::parse_progress(line).unwrap();

        assert!((prog.time_seconds - 83.45).abs() < 0.01);
        assert_eq!(prog.speed, Some(1.5));
        assert!(prog.bitrate_bps.is_some());
    }

    #[test]
    fn test_build_args_basic() {
        let backend = FfmpegBackend {
            ffmpeg_path: "ffmpeg".to_string(),
            extra_args: vec![],
            transcode_opts: None,
        };

        let config = StreamConfig {
            quality: crate::stream::StreamQuality::Best,
            headers: HashMap::new(),
            cookies: None,
        };

        let args = backend.build_args("https://example.com/master.m3u8", &config, None, None);

        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"https://example.com/master.m3u8".to_string()));
        assert!(args.contains(&"-c".to_string()));
        assert!(args.contains(&"copy".to_string()));
        assert!(args.contains(&"pipe:1".to_string()));
    }

    #[test]
    fn test_build_args_with_transcode() {
        let backend = FfmpegBackend {
            ffmpeg_path: "ffmpeg".to_string(),
            extra_args: vec![],
            transcode_opts: Some("-c:v libx265 -crf 28".to_string()),
        };

        let config = StreamConfig {
            quality: crate::stream::StreamQuality::Best,
            headers: HashMap::new(),
            cookies: None,
        };

        let args = backend.build_args("https://example.com/master.m3u8", &config, None, None);

        assert!(args.contains(&"-c:v".to_string()));
        assert!(args.contains(&"libx265".to_string()));
        assert!(!args.contains(&"copy".to_string()));
    }

    #[test]
    fn test_build_args_with_headers() {
        let backend = FfmpegBackend {
            ffmpeg_path: "ffmpeg".to_string(),
            extra_args: vec![],
            transcode_opts: None,
        };

        let mut headers = HashMap::new();
        headers.insert("Referer".to_string(), "https://example.com".to_string());
        headers.insert("Cookie".to_string(), "session=abc123".to_string());

        let config = StreamConfig {
            quality: crate::stream::StreamQuality::Best,
            headers,
            cookies: None,
        };

        let args = backend.build_args("https://example.com/master.m3u8", &config, None, None);

        assert!(args.contains(&"-headers".to_string()));
        // Check that headers string contains both headers
        let headers_idx = args.iter().position(|a| a == "-headers").unwrap();
        let headers_value = &args[headers_idx + 1];
        assert!(headers_value.contains("Referer:"));
        assert!(headers_value.contains("Cookie:"));
    }

    #[test]
    fn test_build_args_with_duration() {
        let backend = FfmpegBackend {
            ffmpeg_path: "ffmpeg".to_string(),
            extra_args: vec![],
            transcode_opts: None,
        };

        let config = StreamConfig::default();

        let args = backend.build_args(
            "https://example.com/master.m3u8",
            &config,
            None,
            Some(3600),
        );

        assert!(args.contains(&"-t".to_string()));
        assert!(args.contains(&"3600".to_string()));
    }

    #[test]
    fn test_can_handle() {
        let backend = FfmpegBackend::new().unwrap();

        assert!(backend.can_handle("https://example.com/master.m3u8", false));
        assert!(backend.can_handle("https://example.com/stream.mpd", false));
        assert!(backend.can_handle("https://example.com/other", true)); // encrypted
        assert!(!backend.can_handle("https://example.com/video.mp4", false));
    }
}
