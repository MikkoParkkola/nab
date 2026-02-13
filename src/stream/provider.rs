//! Stream provider trait and common types.
//!
//! A [`StreamProvider`] knows how to extract stream metadata (manifest
//! URLs, titles, durations) from a specific streaming service (Yle, SVT,
//! NRK, DR, or generic HLS/DASH endpoints).

use anyhow::Result;
use async_trait::async_trait;

/// Quality selection strategy for stream variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamQuality {
    /// Highest available bitrate / resolution.
    Best,
    /// Lowest available bitrate / resolution.
    Worst,
    /// Closest match to the given height in pixels (e.g., 720, 1080).
    Specific(u32),
}

/// Metadata about a single quality variant in a multi-bitrate stream.
#[derive(Debug, Clone)]
pub struct QualityInfo {
    /// Vertical resolution in pixels.
    pub height: u32,
    /// Bitrate in bits per second.
    pub bandwidth: u64,
    /// Codec string (e.g., `"avc1.4d401f,mp4a.40.2"`).
    pub codecs: Option<String>,
}

/// Metadata and manifest URL for a single stream/program.
#[derive(Debug, Clone)]
pub struct StreamInfo {
    /// Provider-specific program or video ID.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Optional description or subtitle.
    pub description: Option<String>,
    /// Duration in seconds (if known, `None` for live).
    pub duration_seconds: Option<u64>,
    /// URL of the HLS/DASH manifest.
    pub manifest_url: String,
    /// Whether this is a live stream.
    pub is_live: bool,
    /// Available quality variants (may be empty if not yet parsed from manifest).
    pub qualities: Vec<QualityInfo>,
    /// URL for a representative thumbnail image.
    pub thumbnail_url: Option<String>,
}

/// Information about a series/playlist and its episodes.
#[derive(Debug, Clone)]
pub struct SeriesInfo {
    /// Provider-specific series identifier.
    pub id: String,
    /// Series title.
    pub title: String,
    /// Episodes in broadcast order.
    pub episodes: Vec<EpisodeInfo>,
}

/// Metadata for a single episode within a series.
#[derive(Debug, Clone)]
pub struct EpisodeInfo {
    /// Provider-specific episode identifier.
    pub id: String,
    /// Episode title.
    pub title: String,
    /// Episode number within its season.
    pub episode_number: Option<u32>,
    /// Season number.
    pub season_number: Option<u32>,
    /// Duration in seconds.
    pub duration_seconds: Option<u64>,
    /// ISO 8601 publish date string.
    pub publish_date: Option<String>,
}

/// Trait for streaming service providers.
///
/// Implementors extract stream metadata from service-specific APIs and
/// return normalized [`StreamInfo`] / [`SeriesInfo`] that the backends
/// can consume.
#[async_trait]
pub trait StreamProvider: Send + Sync {
    /// Short lowercase provider name (e.g., `"yle"`, `"svt"`, `"generic"`).
    fn name(&self) -> &'static str;

    /// Returns `true` if this provider can handle the given URL.
    fn matches(&self, url: &str) -> bool;

    /// Fetch stream metadata for a program/video identified by `id`.
    async fn get_stream_info(&self, id: &str) -> Result<StreamInfo>;

    /// List all episodes in a series or playlist.
    async fn list_series(&self, series_id: &str) -> Result<SeriesInfo>;

    /// Search the provider's catalog. Returns an empty vec by default.
    async fn search(&self, query: &str) -> Result<Vec<EpisodeInfo>> {
        let _ = query;
        Ok(vec![])
    }
}
