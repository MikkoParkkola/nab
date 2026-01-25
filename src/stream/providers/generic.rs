//! Generic HLS/DASH provider for direct URLs

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::stream::provider::{SeriesInfo, StreamInfo, StreamProvider};

pub struct GenericHlsProvider;

impl GenericHlsProvider {
    #[must_use] 
    pub fn new() -> Self {
        Self
    }
}

impl Default for GenericHlsProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StreamProvider for GenericHlsProvider {
    fn name(&self) -> &'static str {
        "generic"
    }

    fn matches(&self, url: &str) -> bool {
        url.ends_with(".m3u8") || url.ends_with(".mpd")
    }

    async fn get_stream_info(&self, url: &str) -> Result<StreamInfo> {
        Ok(StreamInfo {
            id: url.to_string(),
            title: "Direct Stream".to_string(),
            description: None,
            duration_seconds: None,
            manifest_url: url.to_string(),
            is_live: false, // Could detect from manifest
            qualities: vec![],
            thumbnail_url: None,
        })
    }

    async fn list_series(&self, _series_id: &str) -> Result<SeriesInfo> {
        Err(anyhow!(
            "Generic provider does not support series listing"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_m3u8() {
        let provider = GenericHlsProvider::new();
        assert!(provider.matches("https://example.com/stream.m3u8"));
        assert!(!provider.matches("https://example.com/stream.mp4"));
    }

    #[test]
    fn test_matches_mpd() {
        let provider = GenericHlsProvider::new();
        assert!(provider.matches("https://example.com/stream.mpd"));
        assert!(!provider.matches("https://example.com"));
    }

    #[tokio::test]
    async fn test_get_stream_info() {
        let provider = GenericHlsProvider::new();
        let info = provider
            .get_stream_info("https://example.com/stream.m3u8")
            .await
            .unwrap();
        assert_eq!(info.manifest_url, "https://example.com/stream.m3u8");
        assert_eq!(info.title, "Direct Stream");
    }

    #[tokio::test]
    async fn test_list_series_unsupported() {
        let provider = GenericHlsProvider::new();
        let result = provider.list_series("test").await;
        assert!(result.is_err());
    }
}
