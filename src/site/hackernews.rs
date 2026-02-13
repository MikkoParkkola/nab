//! Hacker News content extraction via Algolia API.
//!
//! Uses the official Algolia HN Search API for structured data access.
//! Extracts story content and top-level comments.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, hackernews::HackerNewsProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = HackerNewsProvider;
//!
//! let content = provider.extract(
//!     "https://news.ycombinator.com/item?id=38471822",
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

use super::{Engagement, SiteContent, SiteMetadata, SiteProvider};
use crate::http_client::AcceleratedClient;

/// Hacker News content provider using Algolia API.
pub struct HackerNewsProvider;

#[async_trait]
impl SiteProvider for HackerNewsProvider {
    fn name(&self) -> &'static str {
        "hackernews"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        normalized.contains("news.ycombinator.com/item")
    }

    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
        let item_id = parse_hn_url(url)?;

        let api_url = format!("https://hn.algolia.com/api/v1/items/{item_id}");
        tracing::debug!("Fetching from Hacker News: {}", api_url);

        let response = client
            .fetch_text(&api_url)
            .await
            .context("Failed to fetch from Hacker News API")?;

        let item: HNItem =
            serde_json::from_str(&response).context("Failed to parse Hacker News response")?;

        let markdown = format_hn_markdown(&item);

        let engagement = Engagement {
            likes: item.points,
            reposts: None,
            replies: Some(item.children.len() as u64),
            views: None,
        };

        let canonical_url = format!("https://news.ycombinator.com/item?id={}", item.id);

        let metadata = SiteMetadata {
            author: item.author.clone(),
            title: item.title.clone(),
            published: item.created_at.clone(),
            platform: "Hacker News".to_string(),
            canonical_url,
            media_urls: vec![],
            engagement: Some(engagement),
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// Parse Hacker News URL to extract item ID.
fn parse_hn_url(url: &str) -> Result<String> {
    let url = url.split('#').next().unwrap_or(url);

    // Extract id parameter from query string
    for part in url.split('?').skip(1).flat_map(|q| q.split('&')) {
        if let Some(id) = part.strip_prefix("id=") {
            return Ok(id.to_string());
        }
    }

    anyhow::bail!("Could not extract item ID from URL: {}", url)
}

/// Format Hacker News item and comments as markdown.
fn format_hn_markdown(item: &HNItem) -> String {
    let mut md = String::new();

    // Title
    if let Some(title) = &item.title {
        md.push_str("## ");
        md.push_str(title);
        md.push_str("\n\n");
    }

    // Metadata line
    let points_str = item
        .points
        .map(|p| format!("{} points", format_number(p)))
        .unwrap_or_else(|| "0 points".to_string());

    let author_str = item
        .author
        .as_ref()
        .map(|a| format!("by {} · ", a))
        .unwrap_or_default();

    md.push_str(&format!(
        "{}{} · {} comments\n\n",
        author_str,
        points_str,
        item.children.len()
    ));

    // Link URL (if it's a link post)
    if let Some(url) = &item.url {
        md.push_str("🔗 ");
        md.push_str(url);
        md.push_str("\n\n");
    }

    // Post text (if present)
    if let Some(text) = &item.text {
        md.push_str(text);
        md.push_str("\n\n");
    }

    // Top comments (up to 10 first-level children)
    if !item.children.is_empty() {
        md.push_str("### Top Comments\n\n");

        let mut count = 0;
        for comment in &item.children {
            if count >= 10 {
                break;
            }

            if let Some(text) = &comment.text {
                let author = comment.author.as_deref().unwrap_or("unknown");

                md.push_str(&format!("**{}**:\n\n{}\n\n---\n\n", author, text));
                count += 1;
            }
        }
    }

    md
}

/// Format large numbers with K/M suffixes.
fn format_number(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ============================================================================
// Hacker News API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct HNItem {
    id: u64,
    title: Option<String>,
    author: Option<String>,
    points: Option<u64>,
    url: Option<String>,
    text: Option<String>,
    created_at: Option<String>,
    #[serde(default)]
    children: Vec<HNComment>,
}

#[derive(Debug, Deserialize)]
struct HNComment {
    author: Option<String>,
    text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_hn_item_urls() {
        let provider = HackerNewsProvider;
        assert!(provider.matches("https://news.ycombinator.com/item?id=38471822"));
        assert!(provider.matches("https://NEWS.YCOMBINATOR.COM/ITEM?ID=999"));
    }

    #[test]
    fn does_not_match_non_item_urls() {
        let provider = HackerNewsProvider;
        assert!(!provider.matches("https://news.ycombinator.com/"));
        assert!(!provider.matches("https://news.ycombinator.com/newest"));
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
    }

    #[test]
    fn parse_hn_url_extracts_id() {
        let id = parse_hn_url("https://news.ycombinator.com/item?id=38471822").unwrap();
        assert_eq!(id, "38471822");

        let id2 = parse_hn_url("https://news.ycombinator.com/item?id=999&foo=bar").unwrap();
        assert_eq!(id2, "999");
    }

    #[test]
    fn parse_hn_url_strips_fragment() {
        let id = parse_hn_url("https://news.ycombinator.com/item?id=123#comment").unwrap();
        assert_eq!(id, "123");
    }

    #[test]
    fn format_number_uses_k_suffix() {
        assert_eq!(format_number(1_500), "1.5K");
        assert_eq!(format_number(8_800), "8.8K");
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn format_number_uses_m_suffix() {
        assert_eq!(format_number(1_000_000), "1.0M");
        assert_eq!(format_number(3_800_000), "3.8M");
    }
}
