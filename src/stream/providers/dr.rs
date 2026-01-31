//! DR (Danish) streaming provider

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

use crate::stream::provider::{EpisodeInfo, SeriesInfo, StreamInfo, StreamProvider};

const DR_MU_API_BASE: &str = "https://www.dr.dk/mu-online/api/1.4";
const DR_TOKEN_API: &str = "https://www.dr.dk/mu-online/api/1.4/bar";

pub struct DrProvider {
    client: Client,
}

impl DrProvider {
    pub fn new() -> Result<Self> {
        let client = Client::builder().user_agent("microfetch/1.0").build()?;
        Ok(Self { client })
    }

    /// Extract program URN from URL or return as-is if already an ID
    /// URLs: <https://www.dr.dk/drtv/episode/gintberg-til-gaes_363891>
    /// URLs: <https://www.dr.dk/drtv/se/gintberg-til-gaes_363891>
    fn extract_program_id(url_or_id: &str) -> String {
        if url_or_id.starts_with("http") {
            // Extract the ID with underscore (slug_id format)
            let parts: Vec<&str> = url_or_id.split('/').collect();

            // Get the last path segment which contains slug_id
            let last_segment = parts
                .iter()
                .rfind(|p| !p.is_empty() && !p.starts_with('?'))
                .unwrap_or(&url_or_id);

            // Extract just the numeric ID after underscore
            let segment = last_segment.split('?').next().unwrap_or(last_segment);

            // Return the full slug_id for MU API lookup
            segment.to_string()
        } else {
            url_or_id.to_string()
        }
    }

    /// Extract series slug from URL
    fn extract_series_slug(url_or_id: &str) -> String {
        if url_or_id.starts_with("http") {
            let parts: Vec<&str> = url_or_id.split('/').collect();

            // For series URLs: /drtv/serie/gintberg-til-gaes_123456
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

    /// Get the numeric ID from a `slug_id` format
    fn get_numeric_id(slug_id: &str) -> String {
        // Format: title-slug_12345
        if let Some(pos) = slug_id.rfind('_') {
            slug_id[pos + 1..].to_string()
        } else {
            slug_id.to_string()
        }
    }

    async fn fetch_program_card(&self, product_number: &str) -> Result<DrProgramCard> {
        let url = format!("{DR_MU_API_BASE}/programcard/{product_number}");

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("DR MU API error: {}", resp.status()));
        }

        let data: DrProgramCard = resp.json().await?;
        Ok(data)
    }

    async fn fetch_manifest(&self, program_id: &str) -> Result<DrManifestResponse> {
        // First get an anonymous token
        let token_resp = self.client.get(DR_TOKEN_API).send().await?;

        let _token: Option<String> = if token_resp.status().is_success() {
            token_resp.json().await.ok()
        } else {
            None
        };

        // Fetch the manifest
        let url = format!("{DR_MU_API_BASE}/programcard/{program_id}/manifest");

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("DR Manifest API error: {}", resp.status()));
        }

        let data: DrManifestResponse = resp.json().await?;
        Ok(data)
    }

    async fn fetch_series(&self, series_slug: &str) -> Result<DrSeriesResponse> {
        let numeric_id = Self::get_numeric_id(series_slug);
        let url = format!("{DR_MU_API_BASE}/series/{numeric_id}");

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("DR Series API error: {}", resp.status()));
        }

        let data: DrSeriesResponse = resp.json().await?;
        Ok(data)
    }
}

impl Default for DrProvider {
    fn default() -> Self {
        Self::new().expect("Failed to create DrProvider")
    }
}

#[async_trait]
impl StreamProvider for DrProvider {
    fn name(&self) -> &'static str {
        "dr"
    }

    fn matches(&self, url: &str) -> bool {
        url.contains("dr.dk/drtv") || url.contains("dr.dk/tv")
    }

    async fn get_stream_info(&self, id: &str) -> Result<StreamInfo> {
        let slug_id = Self::extract_program_id(id);
        let product_number = Self::get_numeric_id(&slug_id);

        // Fetch program card for metadata
        let program = self.fetch_program_card(&product_number).await?;

        // Fetch manifest for streaming URL
        let manifest = self.fetch_manifest(&product_number).await?;

        // Find HLS manifest URL
        let manifest_url = manifest
            .links
            .iter()
            .find(|l| l.target == "HLS")
            .map(|l| l.uri.clone())
            .ok_or_else(|| anyhow!("No HLS manifest found"))?;

        let duration = program
            .primary_asset
            .as_ref()
            .and_then(|a| a.duration_in_milliseconds)
            .map(|ms| ms / 1000);

        let thumbnail_url = program.primary_image_uri.map(|uri| {
            // DR image URLs need resolution suffix
            format!("{}/{}x{}", uri, 960, 540)
        });

        let is_live = program
            .primary_asset
            .as_ref()
            .is_some_and(|a| a.kind == "VideoLive");

        Ok(StreamInfo {
            id: product_number,
            title: program.title,
            description: program.description,
            duration_seconds: duration,
            manifest_url,
            is_live,
            qualities: vec![],
            thumbnail_url,
        })
    }

    async fn list_series(&self, series_id: &str) -> Result<SeriesInfo> {
        let slug = Self::extract_series_slug(series_id);
        let series = self.fetch_series(&slug).await?;

        let episodes = series
            .episodes
            .unwrap_or_default()
            .into_iter()
            .map(|ep| {
                let duration = ep
                    .primary_asset
                    .as_ref()
                    .and_then(|a| a.duration_in_milliseconds)
                    .map(|ms| ms / 1000);

                EpisodeInfo {
                    id: ep.product_number,
                    title: ep.title,
                    episode_number: ep.episode_number.map(|n| n as u32),
                    season_number: ep.season_number.map(|n| n as u32),
                    duration_seconds: duration,
                    publish_date: ep.primary_broadcast_date,
                }
            })
            .collect();

        Ok(SeriesInfo {
            id: slug,
            title: series.title,
            episodes,
        })
    }
}

// Serde structures for DR API responses

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DrProgramCard {
    #[allow(dead_code)]
    product_number: String,
    title: String,
    description: Option<String>,
    primary_asset: Option<DrAsset>,
    primary_image_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DrAsset {
    #[allow(dead_code)]
    kind: String,
    duration_in_milliseconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DrManifestResponse {
    links: Vec<DrLink>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DrLink {
    uri: String,
    target: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DrSeriesResponse {
    title: String,
    episodes: Option<Vec<DrEpisode>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DrEpisode {
    product_number: String,
    title: String,
    episode_number: Option<i32>,
    season_number: Option<i32>,
    primary_asset: Option<DrAsset>,
    primary_broadcast_date: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_program_id() {
        assert_eq!(DrProvider::extract_program_id("363891"), "363891");
        assert_eq!(
            DrProvider::extract_program_id(
                "https://www.dr.dk/drtv/episode/gintberg-til-gaes_363891"
            ),
            "gintberg-til-gaes_363891"
        );
        assert_eq!(
            DrProvider::extract_program_id("https://www.dr.dk/drtv/se/gintberg-til-gaes_363891"),
            "gintberg-til-gaes_363891"
        );
    }

    #[test]
    fn test_get_numeric_id() {
        assert_eq!(
            DrProvider::get_numeric_id("gintberg-til-gaes_363891"),
            "363891"
        );
        assert_eq!(DrProvider::get_numeric_id("363891"), "363891");
    }

    #[test]
    fn test_extract_series_slug() {
        assert_eq!(
            DrProvider::extract_series_slug(
                "https://www.dr.dk/drtv/serie/gintberg-til-gaes_123456"
            ),
            "gintberg-til-gaes_123456"
        );
    }

    #[test]
    fn test_matches() {
        let provider = DrProvider::default();
        assert!(provider.matches("https://www.dr.dk/drtv/episode/test_123"));
        assert!(provider.matches("https://dr.dk/tv/live"));
        assert!(!provider.matches("https://example.com"));
    }
}
