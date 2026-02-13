//! Instagram content extraction via oEmbed API.
//!
//! Uses Instagram's official oEmbed endpoint for limited public data.
//! Provides caption, author, and thumbnail for posts and reels.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, instagram::InstagramProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = InstagramProvider;
//!
//! let content = provider.extract(
//!     "https://instagram.com/p/ABC123xyz",
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

/// Instagram content provider using oEmbed API.
pub struct InstagramProvider;

#[async_trait]
impl SiteProvider for InstagramProvider {
    fn name(&self) -> &'static str {
        "instagram"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        normalized.contains("instagram.com/")
            && (normalized.contains("/p/") || normalized.contains("/reel/"))
    }

    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
        // Use oEmbed endpoint
        let oembed_url = format!("https://api.instagram.com/oembed?url={}",
            urlencoding::encode(url));
        tracing::debug!("Fetching from Instagram oEmbed: {}", oembed_url);

        let response = client
            .fetch_text(&oembed_url)
            .await
            .context("Failed to fetch from Instagram oEmbed API")?;

        let oembed: InstagramOEmbed =
            serde_json::from_str(&response).context("Failed to parse Instagram response")?;

        let markdown = format_instagram_markdown(&oembed, url);

        let metadata = SiteMetadata {
            author: Some(format!("@{}", oembed.author_name)),
            title: oembed.title.clone(),
            published: None,
            platform: "Instagram".to_string(),
            canonical_url: url.to_string(),
            media_urls: vec![oembed.thumbnail_url.clone()],
            engagement: None,
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// Format Instagram post as markdown.
fn format_instagram_markdown(oembed: &InstagramOEmbed, url: &str) -> String {
    let mut md = String::new();

    // Author
    md.push_str("## @");
    md.push_str(&oembed.author_name);
    md.push_str("\n\n");

    // Caption/title
    if let Some(title) = &oembed.title {
        md.push_str(title);
        md.push_str("\n\n");
    }

    // Thumbnail
    md.push_str("![Instagram post](");
    md.push_str(&oembed.thumbnail_url);
    md.push_str(")\n\n");

    // Link to original
    md.push_str("[View on Instagram](");
    md.push_str(url);
    md.push_str(")\n");

    md
}

// ============================================================================
// Instagram oEmbed API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct InstagramOEmbed {
    author_name: String,
    title: Option<String>,
    thumbnail_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_instagram_post_urls() {
        let provider = InstagramProvider;
        assert!(provider.matches("https://instagram.com/p/ABC123xyz"));
        assert!(provider.matches("https://www.instagram.com/p/XYZ789abc"));
        assert!(provider.matches("https://INSTAGRAM.COM/P/test123"));
    }

    #[test]
    fn matches_instagram_reel_urls() {
        let provider = InstagramProvider;
        assert!(provider.matches("https://instagram.com/reel/ABC123xyz"));
        assert!(provider.matches("https://www.instagram.com/reel/XYZ789"));
    }

    #[test]
    fn does_not_match_non_post_urls() {
        let provider = InstagramProvider;
        assert!(!provider.matches("https://instagram.com/username"));
        assert!(!provider.matches("https://instagram.com/"));
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
    }
}
