//! SVT Play (Swedish) streaming provider

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

use crate::stream::provider::{EpisodeInfo, SeriesInfo, StreamInfo, StreamProvider};

const SVT_API_BASE: &str = "https://api.svt.se/video";
const SVT_GRAPHQL_BASE: &str = "https://api.svt.se/contento/graphql";

pub struct SvtProvider {
    client: Client,
}

impl SvtProvider {
    pub fn new() -> Result<Self> {
        let client = Client::builder().user_agent("microfetch/1.0").build()?;
        Ok(Self { client })
    }

    /// Extract video ID from URL or return as-is if already an ID
    /// URLs: <https://www.svtplay.se/video/ABC123/title-slug>
    /// URLs: <https://www.svtplay.se/ABC123>
    fn extract_video_id(url_or_id: &str) -> String {
        if url_or_id.starts_with("http") {
            // Parse SVT Play URLs
            let parts: Vec<&str> = url_or_id.split('/').collect();

            // Find the video ID - it's after /video/ or the path segment
            for (i, part) in parts.iter().enumerate() {
                if *part == "video" && i + 1 < parts.len() {
                    // Return the ID after /video/
                    return parts[i + 1]
                        .split('?')
                        .next()
                        .unwrap_or(parts[i + 1])
                        .to_string();
                }
            }

            // Fallback: last meaningful path segment
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

    /// Extract series slug from URL
    /// URLs: <https://www.svtplay.se/rapport>
    fn extract_series_slug(url_or_id: &str) -> String {
        if url_or_id.starts_with("http") {
            url_or_id
                .split('/')
                .rfind(|p| !p.is_empty() && !p.starts_with('?') && *p != "www.svtplay.se")
                .unwrap_or(url_or_id)
                .split('?')
                .next()
                .unwrap_or(url_or_id)
                .to_string()
        } else {
            url_or_id.to_string()
        }
    }

    async fn fetch_video_info(&self, video_id: &str) -> Result<SvtVideoResponse> {
        let url = format!("{SVT_API_BASE}/{video_id}");

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("SVT API error: {}", resp.status()));
        }

        let data: SvtVideoResponse = resp.json().await?;
        Ok(data)
    }

    async fn fetch_series_info(&self, slug: &str) -> Result<SvtGraphQLResponse> {
        let query = r"
            query TitlePage($titleSlugs: [String!]) {
                listablesBySlug(slugs: $titleSlugs) {
                    ... on TvShow {
                        name
                        id
                        associatedContent(include: [EPISODE, CLIP]) {
                            items {
                                item {
                                    ... on Episode {
                                        id
                                        name
                                        positionInSeason
                                        parent {
                                            ... on Season {
                                                seasonNumber
                                            }
                                        }
                                        duration
                                        publishDate
                                        videoSvtId
                                    }
                                }
                            }
                        }
                    }
                }
            }
        ";

        let body = serde_json::json!({
            "query": query,
            "variables": {
                "titleSlugs": [slug]
            }
        });

        let resp = self
            .client
            .post(SVT_GRAPHQL_BASE)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("SVT GraphQL error: {}", resp.status()));
        }

        let data: SvtGraphQLResponse = resp.json().await?;
        Ok(data)
    }
}

impl Default for SvtProvider {
    fn default() -> Self {
        Self::new().expect("Failed to create SvtProvider")
    }
}

#[async_trait]
impl StreamProvider for SvtProvider {
    fn name(&self) -> &'static str {
        "svt"
    }

    fn matches(&self, url: &str) -> bool {
        url.contains("svtplay.se") || url.contains("svt.se/play")
    }

    async fn get_stream_info(&self, id: &str) -> Result<StreamInfo> {
        let video_id = Self::extract_video_id(id);
        let video = self.fetch_video_info(&video_id).await?;

        // Find the best HLS manifest
        let manifest_url = video
            .video_references
            .iter()
            .find(|r| r.format == "hls" || r.format == "dash-avc")
            .map(|r| r.url.clone())
            .ok_or_else(|| anyhow!("No HLS manifest found"))?;

        let duration = video.content_duration.map(|d| d as u64);

        let thumbnail_url = video.poster.map(|p| {
            // SVT image service URL
            format!("https://www.svtstatic.se/image/wide/992/{p}")
        });

        Ok(StreamInfo {
            id: video_id,
            title: video
                .program_title
                .unwrap_or_else(|| video.episode_title.clone().unwrap_or_default()),
            description: video.description,
            duration_seconds: duration,
            manifest_url,
            is_live: video.live.unwrap_or(false),
            qualities: vec![],
            thumbnail_url,
        })
    }

    async fn list_series(&self, series_id: &str) -> Result<SeriesInfo> {
        let slug = Self::extract_series_slug(series_id);
        let graphql_resp = self.fetch_series_info(&slug).await?;

        let show = graphql_resp
            .data
            .listables_by_slug
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Series not found: {slug}"))?;

        let episodes = show
            .associated_content
            .unwrap_or_default()
            .into_iter()
            .flat_map(|ac| ac.items.unwrap_or_default())
            .filter_map(|item| {
                let ep = item.item?;
                Some(EpisodeInfo {
                    id: ep.video_svt_id.unwrap_or(ep.id),
                    title: ep.name,
                    episode_number: ep.position_in_season.map(|p| p as u32),
                    season_number: ep.parent.and_then(|p| p.season_number).map(|s| s as u32),
                    duration_seconds: ep.duration.map(|d| d as u64),
                    publish_date: ep.publish_date,
                })
            })
            .collect();

        Ok(SeriesInfo {
            id: slug,
            title: show.name,
            episodes,
        })
    }
}

// Serde structures for SVT API responses

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SvtVideoResponse {
    #[allow(dead_code)]
    svt_id: Option<String>,
    program_title: Option<String>,
    episode_title: Option<String>,
    description: Option<String>,
    content_duration: Option<i64>,
    live: Option<bool>,
    video_references: Vec<SvtVideoReference>,
    poster: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SvtVideoReference {
    url: String,
    format: String,
    #[allow(dead_code)]
    redirect: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SvtGraphQLResponse {
    data: SvtGraphQLData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SvtGraphQLData {
    listables_by_slug: Vec<SvtShow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SvtShow {
    name: String,
    #[allow(dead_code)]
    id: String,
    associated_content: Option<Vec<SvtAssociatedContent>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SvtAssociatedContent {
    items: Option<Vec<SvtContentItem>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SvtContentItem {
    item: Option<SvtEpisode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SvtEpisode {
    id: String,
    name: String,
    position_in_season: Option<i32>,
    parent: Option<SvtSeason>,
    duration: Option<i64>,
    publish_date: Option<String>,
    video_svt_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SvtSeason {
    season_number: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_video_id() {
        assert_eq!(SvtProvider::extract_video_id("ABC123"), "ABC123");
        assert_eq!(
            SvtProvider::extract_video_id("https://www.svtplay.se/video/ABC123/title-slug"),
            "ABC123"
        );
        assert_eq!(
            SvtProvider::extract_video_id("https://www.svtplay.se/video/ABC123?foo=bar"),
            "ABC123"
        );
    }

    #[test]
    fn test_extract_series_slug() {
        assert_eq!(SvtProvider::extract_series_slug("rapport"), "rapport");
        assert_eq!(
            SvtProvider::extract_series_slug("https://www.svtplay.se/rapport"),
            "rapport"
        );
    }

    #[test]
    fn test_matches() {
        let provider = SvtProvider::default();
        assert!(provider.matches("https://www.svtplay.se/video/ABC123"));
        assert!(provider.matches("https://svt.se/play/video/ABC123"));
        assert!(!provider.matches("https://example.com"));
    }
}
