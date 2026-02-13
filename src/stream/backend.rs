//! Stream backend trait for downloading media data.
//!
//! A [`StreamBackend`] takes an HLS/DASH manifest URL and streams the
//! actual media data to a writer or file. Three implementations are
//! available: [`NativeHls`](super::backends::NativeHlsBackend) (pure
//! Rust, no deps), [`Ffmpeg`](super::backends::FfmpegBackend), and
//! [`Streamlink`](super::backends::StreamlinkBackend).

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::io::AsyncWrite;

/// Identifies which backend implementation is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    /// Pure Rust HLS segment fetcher.
    Native,
    /// ffmpeg subprocess.
    Ffmpeg,
    /// streamlink subprocess.
    Streamlink,
}

/// Configuration passed to a backend when starting a stream.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Desired quality level.
    pub quality: super::StreamQuality,
    /// Extra HTTP headers to include in segment requests.
    pub headers: HashMap<String, String>,
    /// Cookie header value, if authentication is required.
    pub cookies: Option<String>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            quality: super::StreamQuality::Best,
            headers: HashMap::new(),
            cookies: None,
        }
    }
}

/// Callback invoked periodically with download progress.
pub type ProgressCallback = Box<dyn Fn(StreamProgress) + Send + Sync>;

/// Snapshot of streaming progress at a point in time.
#[derive(Debug, Clone)]
pub struct StreamProgress {
    /// Total bytes written so far.
    pub bytes_downloaded: u64,
    /// Number of segments successfully fetched and written.
    pub segments_completed: u32,
    /// Total segment count (if known ahead of time, e.g., VOD).
    pub segments_total: Option<u32>,
    /// Wall-clock seconds since the stream started.
    pub elapsed_seconds: f64,
}

/// Trait for media streaming backends.
///
/// Backends are responsible for fetching segments (or invoking an external
/// tool) and writing the resulting media bytes to either an async writer
/// or a file on disk.
#[async_trait]
pub trait StreamBackend: Send + Sync {
    /// The type of this backend.
    fn backend_type(&self) -> BackendType;

    /// Returns `true` if this backend can handle the given manifest.
    fn can_handle(&self, manifest_url: &str, encrypted: bool) -> bool;

    /// Stream media data to an async writer (e.g., stdout pipe).
    async fn stream_to<W: AsyncWrite + Unpin + Send>(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        output: &mut W,
        progress: Option<ProgressCallback>,
    ) -> Result<()>;

    /// Stream media data to a file on disk.
    async fn stream_to_file(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        path: &std::path::Path,
        progress: Option<ProgressCallback>,
        duration_secs: Option<u64>,
    ) -> Result<()>;
}
