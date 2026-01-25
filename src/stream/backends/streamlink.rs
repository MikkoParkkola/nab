//! Streamlink bridge backend for streaming
//!
//! Uses streamlink subprocess for:
//! - `YouTube`, Twitch, and 1000+ other streaming sites
//! - HLS/DASH streams with site-specific extraction
//! - Live streams with real-time output

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
use crate::stream::StreamQuality;

/// Streamlink-based streaming backend
pub struct StreamlinkBackend {
    /// Path to streamlink binary
    streamlink_path: String,
    /// Additional streamlink arguments
    extra_args: Vec<String>,
}

impl StreamlinkBackend {
    /// Create new streamlink backend, searching for binary in PATH
    pub fn new() -> Result<Self> {
        let streamlink_path = which::which("streamlink")
            .map_or_else(|_| "streamlink".to_string(), |p| p.to_string_lossy().to_string());

        Ok(Self {
            streamlink_path,
            extra_args: Vec::new(),
        })
    }

    /// Specify custom streamlink binary path
    #[must_use]
    pub fn with_streamlink_path(mut self, path: &str) -> Self {
        self.streamlink_path = path.to_string();
        self
    }

    /// Add extra streamlink arguments
    #[must_use]
    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    /// Convert `StreamQuality` to streamlink quality string
    fn quality_to_string(quality: &StreamQuality) -> String {
        match quality {
            StreamQuality::Best => "best".to_string(),
            StreamQuality::Worst => "worst".to_string(),
            StreamQuality::Specific(height) => format!("{height}p"),
        }
    }

    /// Build streamlink command arguments for stdout output
    fn build_args_stdout(&self, url: &str, config: &StreamConfig) -> Vec<String> {
        let mut args = Vec::new();

        // Output to stdout
        args.push("-O".to_string());

        // Headers
        for (key, value) in &config.headers {
            args.push("--http-header".to_string());
            args.push(format!("{key}={value}"));
        }

        // Cookies
        if let Some(ref cookies) = config.cookies {
            args.push("--http-cookies".to_string());
            args.push(cookies.clone());
        }

        // Extra args
        args.extend(self.extra_args.clone());

        // URL and quality
        args.push(url.to_string());
        args.push(Self::quality_to_string(&config.quality));

        args
    }

    /// Build streamlink command arguments for file output
    fn build_args_file(&self, url: &str, config: &StreamConfig, output_path: &str) -> Vec<String> {
        let mut args = Vec::new();

        // Output to file
        args.push("-o".to_string());
        args.push(output_path.to_string());

        // Force overwrite
        args.push("-f".to_string());

        // Headers
        for (key, value) in &config.headers {
            args.push("--http-header".to_string());
            args.push(format!("{key}={value}"));
        }

        // Cookies
        if let Some(ref cookies) = config.cookies {
            args.push("--http-cookies".to_string());
            args.push(cookies.clone());
        }

        // Extra args
        args.extend(self.extra_args.clone());

        // URL and quality
        args.push(url.to_string());
        args.push(Self::quality_to_string(&config.quality));

        args
    }

    /// Check if streamlink is available
    pub async fn check_available(&self) -> bool {
        Command::new(&self.streamlink_path)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Check if streamlink supports a URL (via `streamlink --can-handle-url`)
    pub async fn can_handle_url(&self, url: &str) -> bool {
        Command::new(&self.streamlink_path)
            .arg("--can-handle-url")
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Parse progress from streamlink stderr
    /// Streamlink outputs: "[download][stream] Downloaded X.XX MiB"
    fn parse_progress(line: &str) -> Option<StreamlinkProgress> {
        if !line.contains("Downloaded") {
            return None;
        }

        // Try to parse "Downloaded X.XX MiB" or "Downloaded X.XX KiB"
        let downloaded_part = line.split("Downloaded").nth(1)?;
        let parts: Vec<&str> = downloaded_part.split_whitespace().collect();

        if parts.len() >= 2 {
            let value: f64 = parts[0].parse().ok()?;
            let unit = parts[1];

            let bytes = match unit {
                "KiB" => (value * 1024.0) as u64,
                "MiB" => (value * 1024.0 * 1024.0) as u64,
                "GiB" => (value * 1024.0 * 1024.0 * 1024.0) as u64,
                _ => return None,
            };

            return Some(StreamlinkProgress { bytes_downloaded: bytes });
        }

        None
    }
}

#[async_trait]
impl StreamBackend for StreamlinkBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::Streamlink
    }

    fn can_handle(&self, manifest_url: &str, _encrypted: bool) -> bool {
        // Streamlink handles many streaming sites by URL pattern
        // Common supported domains
        let supported_patterns = [
            "youtube.com",
            "youtu.be",
            "twitch.tv",
            "dailymotion.com",
            "vimeo.com",
            "facebook.com",
            "mixer.com",
            "crunchyroll.com",
            "mlg.com",
            "livestream.com",
            "ustream.tv",
            "afreeca.com",
            "bilibili.com",
            "huya.com",
            "douyu.com",
            "nimo.tv",
            "picarto.tv",
            "pluto.tv",
            "tv.se",  // Swedish TV
            "svtplay.se",
            "tv4play.se",
            "ruv.is",
            "dr.dk",
            "nrk.no",
        ];

        // Check URL patterns
        for pattern in &supported_patterns {
            if manifest_url.contains(pattern) {
                return true;
            }
        }

        // Also handle generic HLS/DASH that streamlink might support
        // But prefer ffmpeg for raw manifest URLs
        false
    }

    async fn stream_to<W: AsyncWrite + Unpin + Send>(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        output: &mut W,
        progress: Option<ProgressCallback>,
    ) -> Result<()> {
        let args = self.build_args_stdout(manifest_url, config);
        debug!("streamlink args: {:?}", args);

        let mut child = Command::new(&self.streamlink_path)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture streamlink stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to capture streamlink stderr"))?;

        let start_time = std::time::Instant::now();

        // Spawn stderr reader for progress and error logging
        let stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = tokio::io::AsyncBufReadExt::lines(reader);

            while let Ok(Some(line)) = lines.next_line().await {
                debug!("streamlink: {}", line);
                // Log errors/warnings
                if line.contains("error") || line.contains("Error") {
                    warn!("streamlink: {}", line);
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
            return Err(anyhow!("streamlink exited with status: {status}"));
        }

        output.flush().await?;
        info!("Streamed {} bytes via streamlink", total_bytes);

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
        let args = self.build_args_file(manifest_url, config, &path_str);
        debug!("streamlink args: {:?}", args);

        let mut child = Command::new(&self.streamlink_path)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to capture streamlink stderr"))?;

        let start_time = std::time::Instant::now();

        // Read stderr for progress
        let reader = BufReader::new(stderr);
        let mut lines = tokio::io::AsyncBufReadExt::lines(reader);

        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(prog) = Self::parse_progress(&line) {
                if let Some(ref cb) = progress {
                    cb(StreamProgress {
                        bytes_downloaded: prog.bytes_downloaded,
                        segments_completed: 0,
                        segments_total: None,
                        elapsed_seconds: start_time.elapsed().as_secs_f64(),
                    });
                }
            }

            if line.contains("error") || line.contains("Error") {
                warn!("streamlink: {}", line);
            }
        }

        let status = child.wait().await?;

        if !status.success() {
            return Err(anyhow!("streamlink exited with status: {status}"));
        }

        info!("Saved stream to {:?} via streamlink", path);
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct StreamlinkProgress {
    bytes_downloaded: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_quality_to_string() {
        assert_eq!(StreamlinkBackend::quality_to_string(&StreamQuality::Best), "best");
        assert_eq!(StreamlinkBackend::quality_to_string(&StreamQuality::Worst), "worst");
        assert_eq!(
            StreamlinkBackend::quality_to_string(&StreamQuality::Specific(720)),
            "720p"
        );
    }

    #[test]
    fn test_build_args_stdout() {
        let backend = StreamlinkBackend {
            streamlink_path: "streamlink".to_string(),
            extra_args: vec![],
        };

        let config = StreamConfig {
            quality: StreamQuality::Best,
            headers: HashMap::new(),
            cookies: None,
        };

        let args = backend.build_args_stdout("https://www.twitch.tv/example", &config);

        assert!(args.contains(&"-O".to_string()));
        assert!(args.contains(&"https://www.twitch.tv/example".to_string()));
        assert!(args.contains(&"best".to_string()));
    }

    #[test]
    fn test_build_args_file() {
        let backend = StreamlinkBackend {
            streamlink_path: "streamlink".to_string(),
            extra_args: vec![],
        };

        let config = StreamConfig {
            quality: StreamQuality::Specific(720),
            headers: HashMap::new(),
            cookies: None,
        };

        let args = backend.build_args_file("https://www.twitch.tv/example", &config, "/tmp/output.ts");

        assert!(args.contains(&"-o".to_string()));
        assert!(args.contains(&"/tmp/output.ts".to_string()));
        assert!(args.contains(&"-f".to_string()));
        assert!(args.contains(&"https://www.twitch.tv/example".to_string()));
        assert!(args.contains(&"720p".to_string()));
    }

    #[test]
    fn test_build_args_with_headers() {
        let backend = StreamlinkBackend {
            streamlink_path: "streamlink".to_string(),
            extra_args: vec![],
        };

        let mut headers = HashMap::new();
        headers.insert("Referer".to_string(), "https://example.com".to_string());
        headers.insert("User-Agent".to_string(), "Custom/1.0".to_string());

        let config = StreamConfig {
            quality: StreamQuality::Best,
            headers,
            cookies: None,
        };

        let args = backend.build_args_stdout("https://www.twitch.tv/example", &config);

        // Check that --http-header appears
        let header_count = args.iter().filter(|a| *a == "--http-header").count();
        assert_eq!(header_count, 2);
    }

    #[test]
    fn test_build_args_with_cookies() {
        let backend = StreamlinkBackend {
            streamlink_path: "streamlink".to_string(),
            extra_args: vec![],
        };

        let config = StreamConfig {
            quality: StreamQuality::Best,
            headers: HashMap::new(),
            cookies: Some("session=abc123".to_string()),
        };

        let args = backend.build_args_stdout("https://www.twitch.tv/example", &config);

        assert!(args.contains(&"--http-cookies".to_string()));
        assert!(args.contains(&"session=abc123".to_string()));
    }

    #[test]
    fn test_can_handle() {
        let backend = StreamlinkBackend {
            streamlink_path: "streamlink".to_string(),
            extra_args: vec![],
        };

        // Supported sites
        assert!(backend.can_handle("https://www.twitch.tv/example", false));
        assert!(backend.can_handle("https://www.youtube.com/watch?v=abc123", false));
        assert!(backend.can_handle("https://youtu.be/abc123", false));
        assert!(backend.can_handle("https://www.dailymotion.com/video/abc", false));
        assert!(backend.can_handle("https://svtplay.se/video/abc", false));

        // Not directly supported (raw manifests go to ffmpeg)
        assert!(!backend.can_handle("https://example.com/master.m3u8", false));
        assert!(!backend.can_handle("https://example.com/stream.mpd", false));
    }

    #[test]
    fn test_parse_progress() {
        // Test MiB format
        let line = "[download][stream] Downloaded 10.50 MiB";
        let prog = StreamlinkBackend::parse_progress(line).unwrap();
        assert_eq!(prog.bytes_downloaded, 11_010_048); // 10.5 * 1024 * 1024

        // Test KiB format
        let line = "[download][stream] Downloaded 512.00 KiB";
        let prog = StreamlinkBackend::parse_progress(line).unwrap();
        assert_eq!(prog.bytes_downloaded, 524_288); // 512 * 1024

        // Test non-progress line
        let line = "[cli][info] Found matching plugin twitch";
        assert!(StreamlinkBackend::parse_progress(line).is_none());
    }

    #[test]
    fn test_extra_args() {
        let backend = StreamlinkBackend {
            streamlink_path: "streamlink".to_string(),
            extra_args: vec!["--player-passthrough".to_string(), "hls".to_string()],
        };

        let config = StreamConfig::default();
        let args = backend.build_args_stdout("https://www.twitch.tv/example", &config);

        assert!(args.contains(&"--player-passthrough".to_string()));
        assert!(args.contains(&"hls".to_string()));
    }
}
