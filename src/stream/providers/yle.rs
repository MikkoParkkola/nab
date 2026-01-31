//! Yle Areena streaming provider

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

use crate::stream::provider::{EpisodeInfo, SeriesInfo, StreamInfo, StreamProvider};

const YLE_APP_ID: &str = "player_static_prod";
const YLE_APP_KEY: &str = "8930d72170e48303cf5f3867780d549b";
const YLE_API_BASE: &str = "https://player.api.yle.fi/v1/preview";

pub struct YleProvider {
    client: Client,
}

impl YleProvider {
    pub fn new() -> Result<Self> {
        let client = Client::builder().user_agent("nab/1.0").build()?;
        Ok(Self { client })
    }

    fn preview_url(program_id: &str) -> String {
        format!(
            "{YLE_API_BASE}/{program_id}.json?language=fin&ssl=true&countryCode=FI&host=areenaylefi&app_id={YLE_APP_ID}&app_key={YLE_APP_KEY}&isPortabilityRegion=true"
        )
    }

    fn extract_program_id(url_or_id: &str) -> String {
        // Handle both raw IDs and full URLs
        if url_or_id.starts_with("http") {
            // Extract from URL like https://areena.yle.fi/1-50552121
            url_or_id
                .split('/')
                .next_back()
                .unwrap_or(url_or_id)
                .split('?')
                .next()
                .unwrap_or(url_or_id)
                .to_string()
        } else {
            url_or_id.to_string()
        }
    }

    async fn fetch_preview(&self, program_id: &str) -> Result<YlePreviewResponse> {
        let url = Self::preview_url(program_id);
        let resp = self
            .client
            .get(&url)
            .header("Referer", "https://areena.yle.fi")
            .header("Origin", "https://areena.yle.fi")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("Yle API error: {}", resp.status()));
        }

        let data: YlePreviewResponse = resp.json().await?;
        Ok(data)
    }

    fn parse_episodes_from_next_data(&self, data: &serde_json::Value) -> Vec<EpisodeInfo> {
        let mut episodes = Vec::new();

        // Try to find episodes array in various locations
        let possible_paths = [
            "/props/pageProps/view/tabs/0/content/0/cards",
            "/props/pageProps/view/content/episodes",
            "/props/pageProps/initialData/episodes",
        ];

        for path in possible_paths {
            if let Some(items) = data.pointer(path).and_then(|v| v.as_array()) {
                for item in items {
                    if let Some(ep) = self.parse_episode_item(item) {
                        episodes.push(ep);
                    }
                }
                if !episodes.is_empty() {
                    break;
                }
            }
        }

        episodes
    }

    fn parse_episode_item(&self, item: &serde_json::Value) -> Option<EpisodeInfo> {
        let id = item
            .pointer("/id")
            .or_else(|| item.pointer("/uri"))
            .and_then(|v| v.as_str())?
            .to_string();

        let title = item
            .pointer("/title/fin")
            .or_else(|| item.pointer("/title"))
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();

        let episode_number = item
            .pointer("/episodeNumber")
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as u32);

        let season_number = item
            .pointer("/seasonNumber")
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as u32);

        let duration = item
            .pointer("/duration/duration_in_seconds")
            .or_else(|| item.pointer("/duration"))
            .and_then(serde_json::Value::as_u64);

        Some(EpisodeInfo {
            id,
            title,
            episode_number,
            season_number,
            duration_seconds: duration,
            publish_date: None,
        })
    }

    fn parse_episodes_from_html(&self, html: &str) -> Vec<EpisodeInfo> {
        let mut episodes = Vec::new();

        // Simple regex-like search for episode links
        // Pattern: /1-{digits} in href attributes
        let mut pos = 0;
        while let Some(href_start) = html[pos..].find("href=\"/1-") {
            let abs_start = pos + href_start + 6; // skip 'href="'
            if let Some(href_end) = html[abs_start..].find('"') {
                let href = &html[abs_start..abs_start + href_end];
                let id = href.trim_start_matches('/').to_string();

                // Avoid duplicates
                if !episodes.iter().any(|e: &EpisodeInfo| e.id == id) {
                    episodes.push(EpisodeInfo {
                        id,
                        title: "Episode".to_string(),
                        episode_number: None,
                        season_number: None,
                        duration_seconds: None,
                        publish_date: None,
                    });
                }
            }
            pos = abs_start + 1;
        }

        episodes
    }
}

impl Default for YleProvider {
    fn default() -> Self {
        Self::new().expect("Failed to create YleProvider")
    }
}

impl YleProvider {
    /// Get fresh, playable manifest URL using yle-dl as fallback
    /// The preview API returns short-lived Akamai tokens that expire quickly.
    /// yle-dl uses the Kaltura API to get fresh, long-lived URLs.
    pub async fn get_fresh_manifest_url(&self, program_id: &str) -> Result<String> {
        use tokio::process::Command;

        let id = Self::extract_program_id(program_id);
        let url = format!("https://areena.yle.fi/{id}");

        let output = Command::new("yle-dl")
            .arg("--showurl")
            .arg(&url)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("yle-dl failed: {stderr}"));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let urls: Vec<&str> = stdout.lines().collect();

        // yle-dl returns multiple quality options, pick the best (last is usually highest quality)
        urls.last()
            .map(std::string::ToString::to_string)
            .ok_or_else(|| anyhow!("No manifest URL returned by yle-dl"))
    }

    /// Check if yle-dl is available
    pub async fn yle_dl_available() -> bool {
        use tokio::process::Command;

        Command::new("yle-dl")
            .arg("--version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[async_trait]
impl StreamProvider for YleProvider {
    fn name(&self) -> &'static str {
        "yle"
    }

    fn matches(&self, url: &str) -> bool {
        url.contains("areena.yle.fi") || url.contains("arenan.yle.fi")
    }

    async fn get_stream_info(&self, id: &str) -> Result<StreamInfo> {
        let program_id = Self::extract_program_id(id);
        let preview = self.fetch_preview(&program_id).await?;

        // Check if live before consuming the data
        let is_live =
            preview.data.ongoing_channel.is_some() || preview.data.ongoing_event.is_some();

        let ongoing = preview
            .data
            .ongoing_ondemand
            .or(preview.data.ongoing_channel)
            .or(preview.data.ongoing_event)
            .ok_or_else(|| anyhow!("No active stream found (may be expired or pending)"))?;

        let manifest_url = ongoing
            .manifest_url
            .ok_or_else(|| anyhow!("No manifest URL in response"))?;

        let title = ongoing
            .title
            .and_then(|t| t.fin.or(t.swe).or(t.eng))
            .unwrap_or_else(|| program_id.clone());

        let description = ongoing.description.and_then(|d| d.fin.or(d.swe));

        let duration = ongoing.duration.map(|d| d.duration_in_seconds);

        let thumbnail_url = ongoing.image.map(|img| {
            format!(
                "https://images.cdn.yle.fi/image/upload/f_auto,c_limit,w_1080,q_auto/v{}/{}",
                img.version.unwrap_or(1),
                img.id
            )
        });

        Ok(StreamInfo {
            id: program_id,
            title,
            description,
            duration_seconds: duration,
            manifest_url,
            is_live,
            qualities: vec![], // Will be parsed from manifest
            thumbnail_url,
        })
    }

    async fn list_series(&self, series_id: &str) -> Result<SeriesInfo> {
        // Fetch the series page and parse __NEXT_DATA__
        let url = format!(
            "https://areena.yle.fi/{}",
            Self::extract_program_id(series_id)
        );
        let resp = self.client.get(&url).send().await?;
        let html = resp.text().await?;

        // Extract __NEXT_DATA__ JSON
        let next_data_start = html
            .find("__NEXT_DATA__")
            .and_then(|i| html[i..].find('{'))
            .map(|i| i + html.find("__NEXT_DATA__").unwrap());

        let next_data_end = next_data_start.and_then(|start| {
            let mut depth = 0;
            for (i, c) in html[start..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(start + i + 1);
                        }
                    }
                    _ => {}
                }
            }
            None
        });

        if let (Some(start), Some(end)) = (next_data_start, next_data_end) {
            let json_str = &html[start..end];
            if let Ok(next_data) = serde_json::from_str::<serde_json::Value>(json_str) {
                // Navigate to episodes in the Next.js data
                // Structure varies, try common paths
                let title = next_data
                    .pointer("/props/pageProps/meta/title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown Series")
                    .to_string();

                let episodes = self.parse_episodes_from_next_data(&next_data);

                return Ok(SeriesInfo {
                    id: series_id.to_string(),
                    title,
                    episodes,
                });
            }
        }

        // Fallback: parse episode links from HTML
        let episodes = self.parse_episodes_from_html(&html);

        Ok(SeriesInfo {
            id: series_id.to_string(),
            title: "Unknown Series".to_string(),
            episodes,
        })
    }
}

// Serde structures for Yle API response
#[derive(Debug, Deserialize)]
struct YlePreviewResponse {
    data: YlePreviewData,
}

#[derive(Debug, Deserialize)]
struct YlePreviewData {
    ongoing_ondemand: Option<YleOngoing>,
    ongoing_channel: Option<YleOngoing>,
    ongoing_event: Option<YleOngoing>,
    #[allow(dead_code)]
    pending_event: Option<YleOngoing>,
    #[allow(dead_code)]
    gone: Option<YleGone>,
}

#[derive(Debug, Deserialize)]
struct YleOngoing {
    #[allow(dead_code)]
    media_id: Option<String>,
    manifest_url: Option<String>,
    title: Option<LocalizedText>,
    description: Option<LocalizedText>,
    duration: Option<YleDuration>,
    #[allow(dead_code)]
    start_time: Option<String>,
    image: Option<YleImage>,
    #[allow(dead_code)]
    content_type: Option<String>,
    #[allow(dead_code)]
    region: Option<String>,
}

#[derive(Debug, Deserialize)]
struct YleGone {
    #[allow(dead_code)]
    title: Option<LocalizedText>,
    #[allow(dead_code)]
    description: Option<LocalizedText>,
}

#[derive(Debug, Deserialize)]
struct LocalizedText {
    fin: Option<String>,
    swe: Option<String>,
    eng: Option<String>,
}

#[derive(Debug, Deserialize)]
struct YleDuration {
    duration_in_seconds: u64,
}

#[derive(Debug, Deserialize)]
struct YleImage {
    id: String,
    version: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_program_id() {
        assert_eq!(YleProvider::extract_program_id("1-50552121"), "1-50552121");
        assert_eq!(
            YleProvider::extract_program_id("https://areena.yle.fi/1-50552121"),
            "1-50552121"
        );
        assert_eq!(
            YleProvider::extract_program_id("https://areena.yle.fi/1-50552121?foo=bar"),
            "1-50552121"
        );
    }

    #[test]
    fn test_preview_url() {
        let url = YleProvider::preview_url("1-50552121");
        assert!(url.contains("player.api.yle.fi"));
        assert!(url.contains("app_key="));
        assert!(url.contains("1-50552121"));
    }

    #[test]
    fn test_matches() {
        let provider = YleProvider::default();
        assert!(provider.matches("https://areena.yle.fi/1-50552121"));
        assert!(provider.matches("https://arenan.yle.fi/1-50552121"));
        assert!(!provider.matches("https://example.com"));
    }
}
