//! Twitter/X content extraction via `FxTwitter` API.
//!
//! `FxTwitter` provides a clean JSON API for tweet data, including:
//! - Long-form article content (tweet.article.content.blocks)
//! - Engagement metrics (likes, retweets, replies, views)
//! - Author information
//! - Media URLs
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, twitter::TwitterProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = TwitterProvider;
//!
//! let content = provider.extract(
//!     "https://x.com/naval/status/1234567890",
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

/// Twitter/X content provider using `FxTwitter` API.
pub struct TwitterProvider;

#[async_trait]
impl SiteProvider for TwitterProvider {
    fn name(&self) -> &'static str {
        "twitter"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        (normalized.contains("x.com/") || normalized.contains("twitter.com/"))
            && normalized.contains("/status/")
    }

    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
        // Extract user and status ID from URL
        let (user, id) = parse_twitter_url(url)?;

        // Call FxTwitter API
        let api_url = format!("https://api.fxtwitter.com/{user}/status/{id}");
        tracing::debug!("Fetching from FxTwitter: {}", api_url);

        let response = client
            .fetch_text(&api_url)
            .await
            .context("Failed to fetch from FxTwitter API")?;

        let api_response: FxTwitterResponse =
            serde_json::from_str(&response).context("Failed to parse FxTwitter response")?;

        // Convert to SiteContent
        let tweet = &api_response.tweet;
        let markdown = format_tweet_markdown(tweet);

        let engagement = Engagement {
            likes: tweet.likes,
            reposts: tweet.retweets,
            replies: tweet.replies,
            views: tweet.views,
        };

        let metadata = SiteMetadata {
            author: Some(format!(
                "{} (@{})",
                tweet.author.name, tweet.author.screen_name
            )),
            title: None, // Tweets don't have titles
            published: tweet.created_at.clone(),
            platform: "Twitter/X".to_string(),
            canonical_url: tweet.url.clone(),
            media_urls: tweet
                .media
                .as_ref()
                .map(|m| {
                    m.all
                        .iter()
                        .filter_map(|item| item.url.clone())
                        .collect()
                })
                .unwrap_or_default(),
            engagement: Some(engagement),
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// Parse Twitter URL to extract username and status ID.
fn parse_twitter_url(url: &str) -> Result<(String, String)> {
    let url = url.split('?').next().unwrap_or(url);
    let parts: Vec<&str> = url.split('/').collect();

    let status_idx = parts
        .iter()
        .position(|&p| p == "status")
        .context("URL does not contain /status/")?;

    let user = parts
        .get(status_idx - 1)
        .context("Could not extract username from URL")?
        .to_string();

    let id = parts
        .get(status_idx + 1)
        .context("Could not extract status ID from URL")?
        .to_string();

    Ok((user, id))
}

/// Format tweet data as markdown.
fn format_tweet_markdown(tweet: &Tweet) -> String {
    let mut md = String::new();

    // Header with author
    md.push_str("## @");
    md.push_str(&tweet.author.screen_name);
    md.push_str(" (");
    md.push_str(&tweet.author.name);
    md.push_str(")\n\n");

    // Content (prefer article blocks for long-form, fallback to text)
    if let Some(article) = &tweet.article {
        if let Some(content) = &article.content {
            for block in &content.blocks {
                if let Some(text) = &block.text {
                    md.push_str(text);
                    md.push_str("\n\n");
                }
            }
        }
    } else if let Some(text) = &tweet.text {
        md.push_str(text);
        md.push_str("\n\n");
    }

    // Engagement metrics
    let metrics = format_engagement(
        tweet.likes,
        tweet.retweets,
        tweet.replies,
        tweet.views,
    );
    md.push_str(&metrics);

    // Timestamp
    if let Some(created) = &tweet.created_at {
        md.push_str("🕐 ");
        md.push_str(created);
        md.push('\n');
    }

    // Link to original
    md.push_str("\n[View on X](");
    md.push_str(&tweet.url);
    md.push_str(")\n");

    md
}

/// Format engagement metrics as a compact string.
fn format_engagement(
    likes: Option<u64>,
    reposts: Option<u64>,
    replies: Option<u64>,
    views: Option<u64>,
) -> String {
    let mut parts = Vec::new();

    if let Some(l) = likes {
        parts.push(format!("{} likes", format_number(l)));
    }
    if let Some(r) = reposts {
        parts.push(format!("{} reposts", format_number(r)));
    }
    if let Some(rep) = replies {
        parts.push(format!("{} replies", format_number(rep)));
    }
    if let Some(v) = views {
        parts.push(format!("{} views", format_number(v)));
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("📊 {}\n", parts.join(" · "))
    }
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
// FxTwitter API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct FxTwitterResponse {
    tweet: Tweet,
}

#[derive(Debug, Deserialize)]
struct Tweet {
    url: String,
    text: Option<String>,
    author: Author,
    likes: Option<u64>,
    retweets: Option<u64>,
    replies: Option<u64>,
    views: Option<u64>,
    created_at: Option<String>,
    article: Option<Article>,
    media: Option<Media>,
}

#[derive(Debug, Deserialize)]
struct Author {
    name: String,
    screen_name: String,
}

#[derive(Debug, Deserialize)]
struct Article {
    content: Option<ArticleContent>,
}

#[derive(Debug, Deserialize)]
struct ArticleContent {
    blocks: Vec<Block>,
}

#[derive(Debug, Deserialize)]
struct Block {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Media {
    all: Vec<MediaItem>,
}

#[derive(Debug, Deserialize)]
struct MediaItem {
    url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_x_dot_com_status_urls() {
        let provider = TwitterProvider;
        assert!(provider.matches("https://x.com/naval/status/1234567890"));
        assert!(provider.matches("https://X.COM/user/STATUS/999?ref=foo"));
    }

    #[test]
    fn matches_twitter_dot_com_status_urls() {
        let provider = TwitterProvider;
        assert!(provider.matches("https://twitter.com/elonmusk/status/1234567890"));
        assert!(provider.matches("https://TWITTER.COM/user/status/999?utm_source=x"));
    }

    #[test]
    fn does_not_match_non_status_urls() {
        let provider = TwitterProvider;
        assert!(!provider.matches("https://x.com/naval"));
        assert!(!provider.matches("https://twitter.com/elonmusk"));
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
    }

    #[test]
    fn does_not_match_urls_with_status_elsewhere() {
        let provider = TwitterProvider;
        // "status" appears but not in the right position
        assert!(!provider.matches("https://example.com/status"));
    }

    #[test]
    fn parse_twitter_url_extracts_user_and_id() {
        let (user, id) = parse_twitter_url("https://x.com/naval/status/1234567890").unwrap();
        assert_eq!(user, "naval");
        assert_eq!(id, "1234567890");

        let (user2, id2) =
            parse_twitter_url("https://twitter.com/elonmusk/status/999?ref=foo").unwrap();
        assert_eq!(user2, "elonmusk");
        assert_eq!(id2, "999");
    }

    #[test]
    fn parse_twitter_url_handles_trailing_slash() {
        let (user, id) = parse_twitter_url("https://x.com/user/status/123/").unwrap();
        assert_eq!(user, "user");
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
        assert_eq!(format_number(999_999), "1000.0K"); // Just below 1M
    }

    #[test]
    fn format_engagement_combines_all_metrics() {
        let result = format_engagement(Some(8_800), Some(1_000), Some(344), Some(3_800_000));
        assert_eq!(result, "📊 8.8K likes · 1.0K reposts · 344 replies · 3.8M views\n");
    }

    #[test]
    fn format_engagement_handles_missing_metrics() {
        let result = format_engagement(Some(100), None, Some(50), None);
        assert_eq!(result, "📊 100 likes · 50 replies\n");
    }

    #[test]
    fn format_engagement_returns_empty_when_no_metrics() {
        let result = format_engagement(None, None, None, None);
        assert_eq!(result, "");
    }

    #[test]
    fn format_tweet_markdown_includes_author_and_content() {
        let tweet = Tweet {
            url: "https://x.com/test/status/123".to_string(),
            text: Some("This is a test tweet.".to_string()),
            author: Author {
                name: "Test User".to_string(),
                screen_name: "testuser".to_string(),
            },
            likes: Some(42),
            retweets: Some(10),
            replies: Some(5),
            views: Some(1000),
            created_at: Some("Wed Feb 12 10:00:00 +0000 2025".to_string()),
            article: None,
            media: None,
        };

        let md = format_tweet_markdown(&tweet);

        assert!(md.contains("@testuser (Test User)"));
        assert!(md.contains("This is a test tweet."));
        assert!(md.contains("42 likes"));
        assert!(md.contains("10 reposts"));
        assert!(md.contains("5 replies"));
        assert!(md.contains("1.0K views"));
        assert!(md.contains("Wed Feb 12 10:00:00 +0000 2025"));
        assert!(md.contains("[View on X](https://x.com/test/status/123)"));
    }

    #[test]
    fn format_tweet_markdown_prefers_article_blocks() {
        let tweet = Tweet {
            url: "https://x.com/test/status/456".to_string(),
            text: Some("Short text".to_string()),
            author: Author {
                name: "Author".to_string(),
                screen_name: "author".to_string(),
            },
            likes: None,
            retweets: None,
            replies: None,
            views: None,
            created_at: None,
            article: Some(Article {
                content: Some(ArticleContent {
                    blocks: vec![
                        Block {
                            text: Some("First paragraph.".to_string()),
                        },
                        Block {
                            text: Some("Second paragraph.".to_string()),
                        },
                    ],
                }),
            }),
            media: None,
        };

        let md = format_tweet_markdown(&tweet);

        assert!(md.contains("First paragraph."));
        assert!(md.contains("Second paragraph."));
        assert!(!md.contains("Short text")); // Article blocks take precedence
    }
}
