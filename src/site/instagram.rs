//! Instagram content extraction via oEmbed API with og:meta fallback.
//!
//! Tries Instagram's oEmbed endpoint first. If that fails (Meta has been
//! restricting it), falls back to extracting og:title, og:description, and
//! og:image meta tags from the Instagram HTML response.
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
        // Try oEmbed first, fall back to og:meta tags from HTML
        match self.try_oembed(url, client).await {
            Ok(content) => Ok(content),
            Err(oembed_err) => {
                tracing::warn!("Instagram oEmbed failed, trying og:meta fallback: {oembed_err}");
                self.try_og_meta(url, client)
                    .await
                    .context("Both oEmbed and og:meta extraction failed for Instagram")
            }
        }
    }
}

impl InstagramProvider {
    /// Try extracting content via Instagram's oEmbed API.
    async fn try_oembed(
        &self,
        url: &str,
        client: &AcceleratedClient,
    ) -> Result<SiteContent> {
        let oembed_url = format!(
            "https://api.instagram.com/oembed?url={}",
            urlencoding::encode(url)
        );
        tracing::debug!("Fetching from Instagram oEmbed: {}", oembed_url);

        let response = client
            .fetch_text(&oembed_url)
            .await
            .context("Failed to fetch from Instagram oEmbed API")?;

        let oembed: InstagramOEmbed =
            serde_json::from_str(&response).context("Failed to parse Instagram oEmbed response")?;

        let markdown = format_oembed_markdown(&oembed, url);

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

    /// Fallback: extract content from og:meta tags in Instagram's HTML.
    ///
    /// Instagram HTML includes og:title, og:description, and og:image meta tags
    /// even though the page content is JS-rendered. This provides basic metadata
    /// when the oEmbed API is unavailable.
    async fn try_og_meta(
        &self,
        url: &str,
        client: &AcceleratedClient,
    ) -> Result<SiteContent> {
        tracing::debug!("Fetching Instagram HTML for og:meta extraction: {}", url);

        let html = client
            .fetch_text(url)
            .await
            .context("Failed to fetch Instagram HTML")?;

        let og = extract_og_meta(&html);

        // We need at least some content to be useful
        anyhow::ensure!(
            og.title.is_some() || og.description.is_some(),
            "No og:title or og:description found in Instagram HTML"
        );

        let markdown = format_og_markdown(&og, url);

        let media_urls = og.image.iter().cloned().collect();

        let metadata = SiteMetadata {
            author: extract_author_from_title(og.title.as_deref()),
            title: og.title.clone(),
            published: None,
            platform: "Instagram".to_string(),
            canonical_url: url.to_string(),
            media_urls,
            engagement: None,
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// `OpenGraph` metadata extracted from HTML head.
#[derive(Debug, Default)]
struct OgMeta {
    title: Option<String>,
    description: Option<String>,
    image: Option<String>,
}

/// Extract og:title, og:description, og:image from HTML meta tags.
fn extract_og_meta(html: &str) -> OgMeta {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);

    // Selectors for og:meta tags: <meta property="og:X" content="...">
    let selector = Selector::parse(r#"meta[property^="og:"]"#).unwrap_or_else(|_| {
        // Fallback: this selector should always parse
        Selector::parse("meta").expect("meta selector must parse")
    });

    let mut og = OgMeta::default();

    for element in document.select(&selector) {
        let property = element.value().attr("property").unwrap_or_default();
        let content = element.value().attr("content").unwrap_or_default();

        if content.is_empty() {
            continue;
        }

        match property {
            "og:title" => og.title = Some(content.to_string()),
            "og:description" => og.description = Some(content.to_string()),
            "og:image" => og.image = Some(content.to_string()),
            _ => {}
        }
    }

    og
}

/// Try to extract the author handle from an og:title like "@username on Instagram".
fn extract_author_from_title(title: Option<&str>) -> Option<String> {
    let title = title?;
    // Instagram og:title often follows the pattern: "Author Name (@handle) ..." or "@handle on Instagram"
    if let Some(start) = title.find('@') {
        let rest = &title[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ')')
            .unwrap_or(rest.len());
        Some(rest[..end].to_string())
    } else {
        None
    }
}

/// Format Instagram post as markdown from oEmbed data.
fn format_oembed_markdown(oembed: &InstagramOEmbed, url: &str) -> String {
    let mut md = String::new();

    md.push_str("## @");
    md.push_str(&oembed.author_name);
    md.push_str("\n\n");

    if let Some(title) = &oembed.title {
        md.push_str(title);
        md.push_str("\n\n");
    }

    md.push_str("![Instagram post](");
    md.push_str(&oembed.thumbnail_url);
    md.push_str(")\n\n");

    md.push_str("[View on Instagram](");
    md.push_str(url);
    md.push_str(")\n");

    md
}

/// Format Instagram post as markdown from og:meta tags.
fn format_og_markdown(og: &OgMeta, url: &str) -> String {
    let mut md = String::new();

    if let Some(title) = &og.title {
        md.push_str("## ");
        md.push_str(title);
        md.push_str("\n\n");
    }

    if let Some(description) = &og.description {
        md.push_str(description);
        md.push_str("\n\n");
    }

    if let Some(image) = &og.image {
        md.push_str("![Instagram post](");
        md.push_str(image);
        md.push_str(")\n\n");
    }

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

    #[test]
    fn extract_og_meta_from_html() {
        let html = r#"
            <html><head>
                <meta property="og:title" content="Photo by @testuser on Instagram" />
                <meta property="og:description" content="A beautiful sunset" />
                <meta property="og:image" content="https://scontent.cdninstagram.com/v/photo.jpg" />
            </head><body></body></html>
        "#;
        let og = extract_og_meta(html);
        assert_eq!(og.title.as_deref(), Some("Photo by @testuser on Instagram"));
        assert_eq!(og.description.as_deref(), Some("A beautiful sunset"));
        assert_eq!(
            og.image.as_deref(),
            Some("https://scontent.cdninstagram.com/v/photo.jpg")
        );
    }

    #[test]
    fn extract_og_meta_missing_tags() {
        let html = "<html><head><title>Page</title></head><body></body></html>";
        let og = extract_og_meta(html);
        assert!(og.title.is_none());
        assert!(og.description.is_none());
        assert!(og.image.is_none());
    }

    #[test]
    fn extract_og_meta_empty_content() {
        let html = r#"
            <html><head>
                <meta property="og:title" content="" />
                <meta property="og:description" content="Has content" />
            </head><body></body></html>
        "#;
        let og = extract_og_meta(html);
        assert!(og.title.is_none()); // empty content is skipped
        assert_eq!(og.description.as_deref(), Some("Has content"));
    }

    #[test]
    fn extract_author_from_og_title_at_handle() {
        let result = extract_author_from_title(Some("Photo by @cooluser on Instagram"));
        assert_eq!(result.as_deref(), Some("@cooluser"));
    }

    #[test]
    fn extract_author_from_og_title_with_parens() {
        let result = extract_author_from_title(Some("John Doe (@johndoe) posted"));
        assert_eq!(result.as_deref(), Some("@johndoe"));
    }

    #[test]
    fn extract_author_from_og_title_no_handle() {
        let result = extract_author_from_title(Some("Just a title without handle"));
        assert!(result.is_none());
    }

    #[test]
    fn extract_author_from_og_title_none() {
        let result = extract_author_from_title(None);
        assert!(result.is_none());
    }

    #[test]
    fn format_og_markdown_full() {
        let og = OgMeta {
            title: Some("Photo by @user on Instagram".to_string()),
            description: Some("Check out this view".to_string()),
            image: Some("https://cdn.example.com/photo.jpg".to_string()),
        };
        let md = format_og_markdown(&og, "https://instagram.com/p/ABC123");
        assert!(md.contains("## Photo by @user on Instagram"));
        assert!(md.contains("Check out this view"));
        assert!(md.contains("![Instagram post](https://cdn.example.com/photo.jpg)"));
        assert!(md.contains("[View on Instagram](https://instagram.com/p/ABC123)"));
    }

    #[test]
    fn format_og_markdown_minimal() {
        let og = OgMeta {
            title: Some("Post title".to_string()),
            description: None,
            image: None,
        };
        let md = format_og_markdown(&og, "https://instagram.com/p/XYZ");
        assert!(md.contains("## Post title"));
        assert!(!md.contains("![Instagram post]"));
        assert!(md.contains("[View on Instagram]"));
    }

    #[test]
    fn format_oembed_markdown_output() {
        let oembed = InstagramOEmbed {
            author_name: "testuser".to_string(),
            title: Some("My caption".to_string()),
            thumbnail_url: "https://cdn.example.com/thumb.jpg".to_string(),
        };
        let md = format_oembed_markdown(&oembed, "https://instagram.com/p/ABC");
        assert!(md.contains("## @testuser"));
        assert!(md.contains("My caption"));
        assert!(md.contains("![Instagram post](https://cdn.example.com/thumb.jpg)"));
        assert!(md.contains("[View on Instagram](https://instagram.com/p/ABC)"));
    }
}
