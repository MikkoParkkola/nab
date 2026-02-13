//! YouTube content extraction via oEmbed API.
//!
//! Uses YouTube's official oEmbed endpoint for video metadata.
//! Provides title, author, and thumbnail for videos.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, youtube::YouTubeProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = YouTubeProvider;
//!
//! let content = provider.extract(
//!     "https://youtube.com/watch?v=dQw4w9WgXcQ",
//!     &client
//! ).await?;
//!
//! println!("{}", content.markdown);
//! # Ok(())
//! # }
//! ```

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::{SiteContent, SiteMetadata, SiteProvider};
use crate::http_client::AcceleratedClient;

/// YouTube content provider using oEmbed API.
pub struct YouTubeProvider;

#[async_trait]
impl SiteProvider for YouTubeProvider {
    fn name(&self) -> &'static str {
        "youtube"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        normalized.contains("youtube.com/watch") || normalized.contains("youtu.be/")
    }

    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
        // Use oEmbed endpoint
        let oembed_url = format!(
            "https://www.youtube.com/oembed?url={}&format=json",
            urlencoding::encode(url)
        );
        tracing::debug!("Fetching from YouTube oEmbed: {}", oembed_url);

        let response = client
            .fetch_text(&oembed_url)
            .await
            .context("Failed to fetch from YouTube oEmbed API")?;

        let oembed: YouTubeOEmbed =
            serde_json::from_str(&response).context("Failed to parse YouTube response")?;

        let markdown = format_youtube_markdown(&oembed, url);

        let metadata = SiteMetadata {
            author: Some(oembed.author_name.clone()),
            title: Some(oembed.title.clone()),
            published: None,
            platform: "YouTube".to_string(),
            canonical_url: url.to_string(),
            media_urls: vec![oembed.thumbnail_url.clone()],
            engagement: None,
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// Format YouTube video as markdown.
fn format_youtube_markdown(oembed: &YouTubeOEmbed, url: &str) -> String {
    let mut md = String::new();

    // Title
    md.push_str("## ");
    md.push_str(&oembed.title);
    md.push_str("\n\n");

    // Author
    md.push_str("by ");
    md.push_str(&oembed.author_name);
    md.push_str("\n\n");

    // Thumbnail
    md.push_str("![YouTube video](");
    md.push_str(&oembed.thumbnail_url);
    md.push_str(")\n\n");

    // Link to watch
    md.push_str("[Watch on YouTube](");
    md.push_str(url);
    md.push_str(")\n");

    md
}

// ============================================================================
// YouTube oEmbed API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct YouTubeOEmbed {
    title: String,
    author_name: String,
    thumbnail_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_youtube_watch_urls() {
        let provider = YouTubeProvider;
        assert!(provider.matches("https://youtube.com/watch?v=dQw4w9WgXcQ"));
        assert!(provider.matches("https://www.youtube.com/watch?v=ABC123"));
        assert!(provider.matches("https://YOUTUBE.COM/WATCH?V=test"));
    }

    #[test]
    fn matches_youtu_be_short_urls() {
        let provider = YouTubeProvider;
        assert!(provider.matches("https://youtu.be/dQw4w9WgXcQ"));
        assert!(provider.matches("https://YOUTU.BE/ABC123"));
    }

    #[test]
    fn does_not_match_non_video_urls() {
        let provider = YouTubeProvider;
        assert!(!provider.matches("https://youtube.com/"));
        assert!(!provider.matches("https://youtube.com/channel/UCxyz"));
        assert!(!provider.matches("https://instagram.com/p/abc"));
    }
}
