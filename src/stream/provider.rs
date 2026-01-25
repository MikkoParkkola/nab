//! Stream provider trait and common types

use anyhow::Result;
use async_trait::async_trait;

/// Quality level for stream selection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamQuality {
    Best,
    Worst,
    Specific(u32), // Height in pixels (720, 1080, etc.)
}

/// Quality information for a stream variant
#[derive(Debug, Clone)]
pub struct QualityInfo {
    pub height: u32,
    pub bandwidth: u64,
    pub codecs: Option<String>,
}

/// Information about an available stream
#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub duration_seconds: Option<u64>,
    pub manifest_url: String,
    pub is_live: bool,
    pub qualities: Vec<QualityInfo>,
    pub thumbnail_url: Option<String>,
}

/// Series/playlist information
#[derive(Debug, Clone)]
pub struct SeriesInfo {
    pub id: String,
    pub title: String,
    pub episodes: Vec<EpisodeInfo>,
}

#[derive(Debug, Clone)]
pub struct EpisodeInfo {
    pub id: String,
    pub title: String,
    pub episode_number: Option<u32>,
    pub season_number: Option<u32>,
    pub duration_seconds: Option<u64>,
    pub publish_date: Option<String>,
}

/// Provider for a streaming service
#[async_trait]
pub trait StreamProvider: Send + Sync {
    /// Provider name (e.g., "yle", "youtube")
    fn name(&self) -> &str;

    /// Check if this provider handles the given URL
    fn matches(&self, url: &str) -> bool;

    /// Get stream info for a program/video ID
    async fn get_stream_info(&self, id: &str) -> Result<StreamInfo>;

    /// List episodes in a series
    async fn list_series(&self, series_id: &str) -> Result<SeriesInfo>;

    /// Search for content (optional)
    async fn search(&self, query: &str) -> Result<Vec<EpisodeInfo>> {
        let _ = query;
        Ok(vec![])
    }
}
