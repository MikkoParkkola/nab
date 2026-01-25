//! Stream backend trait for actual data fetching

use anyhow::Result;
use async_trait::async_trait;
use tokio::io::AsyncWrite;
use std::collections::HashMap;

/// Type of backend
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    Native,
    Ffmpeg,
    Streamlink,
}

/// Configuration for stream output
#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub quality: super::StreamQuality,
    pub headers: HashMap<String, String>,
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

/// Progress callback for streaming
pub type ProgressCallback = Box<dyn Fn(StreamProgress) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct StreamProgress {
    pub bytes_downloaded: u64,
    pub segments_completed: u32,
    pub segments_total: Option<u32>,
    pub elapsed_seconds: f64,
}

/// Backend for streaming data
#[async_trait]
pub trait StreamBackend: Send + Sync {
    /// Backend type
    fn backend_type(&self) -> BackendType;

    /// Check if this backend can handle the manifest
    fn can_handle(&self, manifest_url: &str, encrypted: bool) -> bool;

    /// Stream to an async writer (for piping)
    async fn stream_to<W: AsyncWrite + Unpin + Send>(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        output: &mut W,
        progress: Option<ProgressCallback>,
    ) -> Result<()>;

    /// Stream to a file
    async fn stream_to_file(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        path: &std::path::Path,
        progress: Option<ProgressCallback>,
    ) -> Result<()>;
}
