//! NRK (Norwegian) streaming provider

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

use crate::stream::provider::{EpisodeInfo, SeriesInfo, StreamInfo, StreamProvider};

const NRK_PSAPI_BASE: &str = "https://psapi.nrk.no";
const NRK_PLAYBACK_BASE: &str = "https://psapi.nrk.no/playback";

pub struct NrkProvider {
    client: Client,
}

impl NrkProvider {
    pub fn new() -> Result<Self> {
        let client = Client::builder().user_agent("nab/1.0").build()?;
        Ok(Self { client })
    }

    /// Extract program ID from URL or return as-is if already an ID
    /// URLs: <https://tv.nrk.no/program/KMTE50001219>
    /// URLs: <https://tv.nrk.no/serie/nytt-paa-nytt/sesong/59/episode/7>
    fn extract_program_id(url_or_id: &str) -> String {
        if url_or_id.starts_with("http") {
            let parts: Vec<&str> = url_or_id.split('/').collect();

            // Check for /program/ID pattern
            for (i, part) in parts.iter().enumerate() {
                if *part == "program" && i + 1 < parts.len() {
                    return parts[i + 1]
                        .split('?')
                        .next()
                        .unwrap_or(parts[i + 1])
                        .to_string();
                }
            }

            // For series URLs with episode, return the full path for later handling
            if url_or_id.contains("/serie/") && url_or_id.contains("/episode/") {
                // Return series-id/season/episode format
                let serie_idx = parts.iter().position(|&p| p == "serie");
                if let Some(idx) = serie_idx {
                    if idx + 4 < parts.len() {
                        return format!(
                            "{}/s{}/e{}",
                            parts[idx + 1],
                            parts[idx + 3], // sesong number
                            parts[idx + 5].split('?').next().unwrap_or(parts[idx + 5]) // episode number
                        );
                    }
                }
            }

            // Fallback: last path segment
            parts
                .iter()
                .rfind(|p| !p.is_empty() && !p.starts_with('?'))
                .unwrap_or(&url_or_id)
                .split('?')
                .next()
                .unwrap_or(url_or_id)
                .to_string()
        } else {
            url_or_id.to_string()
        }
    }

    /// Extract series ID from URL
    fn extract_series_id(url_or_id: &str) -> String {
        if url_or_id.starts_with("http") {
            let parts: Vec<&str> = url_or_id.split('/').collect();

            for (i, part) in parts.iter().enumerate() {
                if *part == "serie" && i + 1 < parts.len() {
                    return parts[i + 1]
                        .split('?')
                        .next()
                        .unwrap_or(parts[i + 1])
                        .to_string();
                }
            }

            // Fallback
            parts
                .iter()
                .rfind(|p| !p.is_empty() && !p.starts_with('?'))
                .unwrap_or(&url_or_id)
                .to_string()
        } else {
            url_or_id.to_string()
        }
    }

    async fn fetch_playback_manifest(&self, program_id: &str) -> Result<NrkPlaybackResponse> {
        // NRK uses different endpoints for different content types
        let url = format!("{NRK_PLAYBACK_BASE}/manifest/program/{program_id}");

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("NRK Playback API error: {}", resp.status()));
        }

        let data: NrkPlaybackResponse = resp.json().await?;
        Ok(data)
    }

    async fn fetch_program_metadata(&self, program_id: &str) -> Result<NrkProgramMetadata> {
        let url = format!("{NRK_PSAPI_BASE}/tv/catalog/programs/{program_id}");

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("NRK PSAPI error: {}", resp.status()));
        }

        let data: NrkProgramMetadata = resp.json().await?;
        Ok(data)
    }

    async fn fetch_series(&self, series_id: &str) -> Result<NrkSeriesResponse> {
        let url = format!("{NRK_PSAPI_BASE}/tv/catalog/series/{series_id}");

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("NRK Series API error: {}", resp.status()));
        }

        let data: NrkSeriesResponse = resp.json().await?;
        Ok(data)
    }
}

impl Default for NrkProvider {
    fn default() -> Self {
        Self::new().expect("Failed to create NrkProvider")
    }
}

#[async_trait]
impl StreamProvider for NrkProvider {
    fn name(&self) -> &'static str {
        "nrk"
    }

    fn matches(&self, url: &str) -> bool {
        url.contains("tv.nrk.no") || url.contains("nrk.no/tv") || url.contains("radio.nrk.no")
    }

    async fn get_stream_info(&self, id: &str) -> Result<StreamInfo> {
        let program_id = Self::extract_program_id(id);

        // Fetch playback manifest
        let playback = self.fetch_playback_manifest(&program_id).await?;

        // Find the best playable asset
        let playable = playback
            .playable
            .ok_or_else(|| anyhow!("No playable content found"))?;

        // Get HLS manifest URL
        let manifest_url = playable
            .assets
            .iter()
            .find(|a| a.format == "HLS")
            .map(|a| a.url.clone())
            .ok_or_else(|| anyhow!("No HLS manifest found"))?;

        // Try to get additional metadata
        let metadata = self.fetch_program_metadata(&program_id).await.ok();

        let title = metadata
            .as_ref()
            .map_or_else(|| program_id.clone(), |m| m.titles.title.clone());

        let description = metadata.as_ref().and_then(|m| m.titles.subtitle.clone());

        let duration = playable.duration.map(|d| {
            // Duration is in ISO 8601 format like "PT45M" or "PT1H30M"
            parse_iso8601_duration(&d).unwrap_or(0)
        });

        let thumbnail_url = metadata.as_ref().and_then(|m| {
            m.image.as_ref().and_then(|img| {
                img.web_images
                    .iter()
                    .find(|w| w.pixel_width >= 960)
                    .or(img.web_images.first())
                    .map(|w| w.image_url.clone())
            })
        });

        let is_live = playable.live.unwrap_or(false);

        Ok(StreamInfo {
            id: program_id,
            title,
            description,
            duration_seconds: duration,
            manifest_url,
            is_live,
            qualities: vec![],
            thumbnail_url,
        })
    }

    async fn list_series(&self, series_id: &str) -> Result<SeriesInfo> {
        let id = Self::extract_series_id(series_id);
        let series = self.fetch_series(&id).await?;

        let mut episodes = Vec::new();

        // NRK organizes by seasons
        for season in series.seasons.unwrap_or_default() {
            for episode in season.episodes.unwrap_or_default() {
                episodes.push(EpisodeInfo {
                    id: episode.id,
                    title: episode.titles.title,
                    episode_number: episode.episode_number.map(|n| n as u32),
                    season_number: Some(season.season_number as u32),
                    duration_seconds: episode.duration.and_then(|d| parse_iso8601_duration(&d)),
                    publish_date: episode.availability.and_then(|a| a.published),
                });
            }
        }

        Ok(SeriesInfo {
            id,
            title: series.titles.title,
            episodes,
        })
    }
}

/// Parse ISO 8601 duration format (PT1H30M45S) to seconds
fn parse_iso8601_duration(duration: &str) -> Option<u64> {
    let duration = duration.trim_start_matches("PT");
    let mut seconds: u64 = 0;
    let mut current_num = String::new();

    for c in duration.chars() {
        if c.is_ascii_digit() {
            current_num.push(c);
        } else {
            let num: u64 = current_num.parse().unwrap_or(0);
            current_num.clear();
            match c {
                'H' => seconds += num * 3600,
                'M' => seconds += num * 60,
                'S' => seconds += num,
                _ => {}
            }
        }
    }

    if seconds > 0 {
        Some(seconds)
    } else {
        None
    }
}

// Serde structures for NRK API responses

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkPlaybackResponse {
    playable: Option<NrkPlayable>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkPlayable {
    assets: Vec<NrkAsset>,
    duration: Option<String>,
    live: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkAsset {
    url: String,
    format: String,
    #[allow(dead_code)]
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkProgramMetadata {
    titles: NrkTitles,
    image: Option<NrkImage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkTitles {
    title: String,
    subtitle: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkImage {
    web_images: Vec<NrkWebImage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkWebImage {
    image_url: String,
    pixel_width: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkSeriesResponse {
    titles: NrkTitles,
    seasons: Option<Vec<NrkSeason>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkSeason {
    season_number: i32,
    episodes: Option<Vec<NrkEpisode>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkEpisode {
    id: String,
    titles: NrkTitles,
    episode_number: Option<i32>,
    duration: Option<String>,
    availability: Option<NrkAvailability>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NrkAvailability {
    published: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_program_id() {
        assert_eq!(
            NrkProvider::extract_program_id("KMTE50001219"),
            "KMTE50001219"
        );
        assert_eq!(
            NrkProvider::extract_program_id("https://tv.nrk.no/program/KMTE50001219"),
            "KMTE50001219"
        );
    }

    #[test]
    fn test_extract_series_id() {
        assert_eq!(
            NrkProvider::extract_series_id("https://tv.nrk.no/serie/nytt-paa-nytt"),
            "nytt-paa-nytt"
        );
    }

    #[test]
    fn test_parse_iso8601_duration() {
        assert_eq!(parse_iso8601_duration("PT1H"), Some(3600));
        assert_eq!(parse_iso8601_duration("PT30M"), Some(1800));
        assert_eq!(parse_iso8601_duration("PT1H30M"), Some(5400));
        assert_eq!(parse_iso8601_duration("PT45M30S"), Some(2730));
    }

    #[test]
    fn test_matches() {
        let provider = NrkProvider::default();
        assert!(provider.matches("https://tv.nrk.no/program/KMTE50001219"));
        assert!(provider.matches("https://nrk.no/tv/program/KMTE50001219"));
        assert!(provider.matches("https://radio.nrk.no/program/ABC123"));
        assert!(!provider.matches("https://example.com"));
    }
}
