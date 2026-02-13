//! Reddit content extraction via Reddit JSON API.
//!
//! Reddit provides a JSON API by appending `.json` to any URL.
//! This extracts post content, author info, and top comments.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, reddit::RedditProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = RedditProvider;
//!
//! let content = provider.extract(
//!     "https://reddit.com/r/rust/comments/abc123",
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

/// Reddit content provider using Reddit JSON API.
pub struct RedditProvider;

#[async_trait]
impl SiteProvider for RedditProvider {
    fn name(&self) -> &'static str {
        "reddit"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        (normalized.contains("reddit.com/r/") || normalized.contains("old.reddit.com/r/"))
            && normalized.contains("/comments/")
    }

    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
        // Normalize URL and append .json
        let json_url = parse_reddit_url(url)?;
        tracing::debug!("Fetching from Reddit: {}", json_url);

        let response = client
            .inner()
            .get(&json_url)
            .header("User-Agent", "nab/0.3.0")
            .send()
            .await
            .context("Failed to fetch from Reddit API")?
            .text()
            .await
            .context("Failed to read Reddit response body")?;

        let api_response: Vec<RedditListing> =
            serde_json::from_str(&response).context("Failed to parse Reddit response")?;

        // First listing is the post, second is comments
        let post_data = api_response
            .first()
            .and_then(|l| l.data.children.first())
            .context("No post data found")?;

        let empty_comments = vec![];
        let comments_data = api_response
            .get(1)
            .map(|l| &l.data.children)
            .unwrap_or(&empty_comments);

        let markdown = format_reddit_markdown(&post_data.data, comments_data);

        let engagement = Engagement {
            likes: Some(post_data.data.score),
            reposts: None,
            replies: Some(post_data.data.num_comments),
            views: None,
        };

        let metadata = SiteMetadata {
            author: Some(format!("u/{}", post_data.data.author)),
            title: Some(post_data.data.title.clone()),
            published: Some(format_timestamp(post_data.data.created_utc)),
            platform: "Reddit".to_string(),
            canonical_url: post_data.data.url.clone(),
            media_urls: vec![],
            engagement: Some(engagement),
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// Parse Reddit URL and convert to JSON API endpoint.
fn parse_reddit_url(url: &str) -> Result<String> {
    let url = url.split('?').next().unwrap_or(url);
    let mut json_url = url.to_string();

    // Ensure it ends with .json
    if !json_url.ends_with(".json") {
        json_url.push_str(".json");
    }

    Ok(json_url)
}

/// Format Reddit post and comments as markdown.
fn format_reddit_markdown(post: &RedditPost, comments: &[RedditChild]) -> String {
    let mut md = String::new();

    // Title
    md.push_str("## ");
    md.push_str(&post.title);
    md.push_str("\n\n");

    // Metadata line
    md.push_str(&format!(
        "by u/{} · {} points · {} comments\n\n",
        post.author,
        format_number(post.score),
        format_number(post.num_comments)
    ));

    // Post body (selftext for text posts, url for link posts)
    if let Some(selftext) = &post.selftext {
        if !selftext.is_empty() {
            md.push_str(selftext);
            md.push_str("\n\n");
        }
    }

    // If it's a link post, include the URL
    if !post.is_self {
        md.push_str("🔗 ");
        md.push_str(&post.url);
        md.push_str("\n\n");
    }

    // Top comments (up to 10)
    if !comments.is_empty() {
        md.push_str("### Top Comments\n\n");

        let mut count = 0;
        for comment in comments {
            if count >= 10 {
                break;
            }

            if let Some(body) = &comment.data.body {
                md.push_str(&format!(
                    "**u/{}** ({} points):\n\n{}\n\n---\n\n",
                    comment.data.author,
                    format_number(comment.data.score),
                    body
                ));
                count += 1;
            }
        }
    }

    md
}

/// Format Unix timestamp as human-readable string.
fn format_timestamp(timestamp: u64) -> String {
    use std::time::UNIX_EPOCH;

    #[allow(clippy::cast_possible_truncation)]
    let duration = std::time::Duration::from_secs(timestamp);
    let datetime = UNIX_EPOCH + duration;

    datetime
        .duration_since(UNIX_EPOCH)
        .map(|d| format!("{} seconds since epoch", d.as_secs()))
        .unwrap_or_else(|_| "Unknown".to_string())
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
// Reddit API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct RedditListing {
    data: RedditListingData,
}

#[derive(Debug, Deserialize)]
struct RedditListingData {
    children: Vec<RedditChild>,
}

#[derive(Debug, Deserialize)]
struct RedditChild {
    data: RedditPost,
}

#[derive(Debug, Deserialize)]
struct RedditPost {
    title: String,
    author: String,
    score: u64,
    num_comments: u64,
    created_utc: u64,
    selftext: Option<String>,
    url: String,
    is_self: bool,
    body: Option<String>, // For comments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reddit_dot_com_comments_urls() {
        let provider = RedditProvider;
        assert!(provider.matches("https://reddit.com/r/rust/comments/abc123"));
        assert!(provider.matches("https://www.reddit.com/r/programming/comments/xyz789/some_title"));
    }

    #[test]
    fn matches_old_reddit_dot_com_urls() {
        let provider = RedditProvider;
        assert!(provider.matches("https://old.reddit.com/r/rust/comments/abc123"));
        assert!(provider.matches("https://OLD.REDDIT.COM/r/rust/COMMENTS/123"));
    }

    #[test]
    fn does_not_match_non_comment_urls() {
        let provider = RedditProvider;
        assert!(!provider.matches("https://reddit.com/r/rust"));
        assert!(!provider.matches("https://reddit.com/user/someone"));
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
    }

    #[test]
    fn parse_reddit_url_appends_json() {
        let result = parse_reddit_url("https://reddit.com/r/rust/comments/abc123").unwrap();
        assert_eq!(result, "https://reddit.com/r/rust/comments/abc123.json");
    }

    #[test]
    fn parse_reddit_url_strips_query() {
        let result = parse_reddit_url("https://reddit.com/r/rust/comments/abc123?utm_source=x").unwrap();
        assert_eq!(result, "https://reddit.com/r/rust/comments/abc123.json");
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
