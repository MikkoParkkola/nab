//! LinkedIn content extraction via oEmbed API.
//!
//! LinkedIn has no public content API, so we use the oEmbed endpoint
//! for limited data extraction (title, author, thumbnail).
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, linkedin::LinkedInProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = LinkedInProvider;
//!
//! let content = provider.extract(
//!     "https://www.linkedin.com/posts/someuser_topic-activity-123456789",
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

/// LinkedIn content provider using oEmbed API.
pub struct LinkedInProvider;

#[async_trait]
impl SiteProvider for LinkedInProvider {
    fn name(&self) -> &'static str {
        "linkedin"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        normalized.contains("linkedin.com/posts/")
            || normalized.contains("linkedin.com/pulse/")
            || normalized.contains("linkedin.com/feed/update/")
    }

    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
        let oembed_url = format!(
            "https://www.linkedin.com/oembed?url={}&format=json",
            urlencoding::encode(url)
        );
        tracing::debug!("Fetching from LinkedIn oEmbed: {}", oembed_url);

        let response = client
            .fetch_text(&oembed_url)
            .await
            .context("Failed to fetch from LinkedIn oEmbed API")?;

        let oembed: LinkedInOEmbed =
            serde_json::from_str(&response).context("Failed to parse LinkedIn response")?;

        let markdown = format_linkedin_markdown(&oembed, url);

        let metadata = SiteMetadata {
            author: oembed.author_name.clone(),
            title: oembed.title.clone(),
            published: None,
            platform: "LinkedIn".to_string(),
            canonical_url: oembed.author_url.clone().unwrap_or_else(|| url.to_string()),
            media_urls: oembed
                .thumbnail_url
                .as_ref()
                .map(|t| vec![t.clone()])
                .unwrap_or_default(),
            engagement: None,
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// Format LinkedIn oEmbed data as markdown.
fn format_linkedin_markdown(oembed: &LinkedInOEmbed, url: &str) -> String {
    let mut md = String::new();

    // Title or fallback
    if let Some(title) = &oembed.title {
        md.push_str("## ");
        md.push_str(title);
        md.push_str("\n\n");
    }

    // Author
    if let Some(author) = &oembed.author_name {
        md.push_str("by ");
        md.push_str(author);
        md.push_str("\n\n");
    }

    // Thumbnail
    if let Some(thumb) = &oembed.thumbnail_url {
        md.push_str("![LinkedIn post](");
        md.push_str(thumb);
        md.push_str(")\n\n");
    }

    // Embedded HTML content (extract text if available)
    if let Some(html) = &oembed.html {
        let text = strip_html(html);
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            md.push_str(trimmed);
            md.push_str("\n\n");
        }
    }

    // Link to original
    md.push_str("[View on LinkedIn](");
    md.push_str(url);
    md.push_str(")\n");

    md
}

/// Strip HTML tags for plain text display.
fn strip_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

// ============================================================================
// LinkedIn oEmbed API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct LinkedInOEmbed {
    title: Option<String>,
    author_name: Option<String>,
    author_url: Option<String>,
    thumbnail_url: Option<String>,
    html: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_linkedin_posts_urls() {
        let provider = LinkedInProvider;
        assert!(
            provider.matches("https://www.linkedin.com/posts/someuser_topic-activity-123456789")
        );
        assert!(provider.matches("https://LINKEDIN.COM/POSTS/user_title-123"));
    }

    #[test]
    fn matches_linkedin_pulse_urls() {
        let provider = LinkedInProvider;
        assert!(provider.matches("https://www.linkedin.com/pulse/some-article-title-author"));
        assert!(provider.matches("https://linkedin.com/pulse/tech-trends-2025-user"));
    }

    #[test]
    fn matches_linkedin_feed_update_urls() {
        let provider = LinkedInProvider;
        assert!(provider
            .matches("https://www.linkedin.com/feed/update/urn:li:activity:7654321098765432109"));
    }

    #[test]
    fn matches_urls_with_query_params() {
        let provider = LinkedInProvider;
        assert!(provider.matches("https://www.linkedin.com/posts/user_title-123?utm_source=share"));
    }

    #[test]
    fn does_not_match_profile_urls() {
        let provider = LinkedInProvider;
        assert!(!provider.matches("https://www.linkedin.com/in/someuser"));
        assert!(!provider.matches("https://www.linkedin.com/company/somecompany"));
    }

    #[test]
    fn does_not_match_non_linkedin_urls() {
        let provider = LinkedInProvider;
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
        assert!(!provider.matches("https://twitter.com/user/status/123"));
    }

    #[test]
    fn strip_html_removes_tags() {
        assert_eq!(strip_html("<p>Hello <b>world</b></p>"), "Hello world");
    }

    #[test]
    fn strip_html_decodes_entities() {
        assert_eq!(strip_html("&amp; &lt; &gt;"), "& < >");
    }

    #[test]
    fn format_linkedin_markdown_with_full_data() {
        let oembed = LinkedInOEmbed {
            title: Some("The Future of Rust".to_string()),
            author_name: Some("Jane Engineer".to_string()),
            author_url: Some("https://www.linkedin.com/in/janeengineer".to_string()),
            thumbnail_url: Some("https://media.linkedin.com/thumb.jpg".to_string()),
            html: Some("<p>Great insights on systems programming.</p>".to_string()),
        };

        let url = "https://www.linkedin.com/posts/janeengineer_rust-123";
        let md = format_linkedin_markdown(&oembed, url);

        assert!(md.contains("## The Future of Rust"));
        assert!(md.contains("by Jane Engineer"));
        assert!(md.contains("![LinkedIn post](https://media.linkedin.com/thumb.jpg)"));
        assert!(md.contains("Great insights on systems programming."));
        assert!(md.contains("[View on LinkedIn]"));
    }

    #[test]
    fn format_linkedin_markdown_with_minimal_data() {
        let oembed = LinkedInOEmbed {
            title: None,
            author_name: Some("John Doe".to_string()),
            author_url: None,
            thumbnail_url: None,
            html: None,
        };

        let url = "https://www.linkedin.com/posts/johndoe_post-456";
        let md = format_linkedin_markdown(&oembed, url);

        assert!(!md.contains("##"));
        assert!(md.contains("by John Doe"));
        assert!(!md.contains("!["));
        assert!(md.contains("[View on LinkedIn]"));
    }

    #[test]
    fn format_linkedin_markdown_with_empty_html() {
        let oembed = LinkedInOEmbed {
            title: Some("Test".to_string()),
            author_name: None,
            author_url: None,
            thumbnail_url: None,
            html: Some("   ".to_string()),
        };

        let url = "https://www.linkedin.com/posts/test-789";
        let md = format_linkedin_markdown(&oembed, url);

        assert!(md.contains("## Test"));
        // Empty trimmed HTML should not produce extra content
        assert!(!md.contains("   \n"));
    }
}
