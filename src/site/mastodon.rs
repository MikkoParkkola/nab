//! Mastodon content extraction via Mastodon API.
//!
//! Matches known instances (mastodon.social, hachyderm.io, fosstodon.org, etc.)
//! and uses the `/@user/{id}` URL pattern for detection. Falls back to the
//! public statuses API endpoint for content extraction.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, mastodon::MastodonProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = MastodonProvider;
//!
//! let content = provider.extract(
//!     "https://mastodon.social/@user/123456789",
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

/// Known Mastodon instances for reliable URL matching.
const KNOWN_INSTANCES: &[&str] = &[
    "mastodon.social",
    "mastodon.online",
    "hachyderm.io",
    "fosstodon.org",
    "infosec.exchange",
    "techhub.social",
    "mstdn.social",
    "mas.to",
    "mastodon.world",
    "universeodon.com",
    "mastodon.gamedev.place",
    "ruby.social",
    "mathstodon.xyz",
    "social.vivaldi.net",
    "toot.community",
];

/// Mastodon content provider using Mastodon API.
pub struct MastodonProvider;

#[async_trait]
impl SiteProvider for MastodonProvider {
    fn name(&self) -> &'static str {
        "mastodon"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        // Must have /@user/id pattern
        if !has_status_pattern(normalized) {
            return false;
        }

        // Check against known instances
        for instance in KNOWN_INSTANCES {
            if normalized.contains(instance) {
                return true;
            }
        }

        false
    }

    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
        let (instance, status_id) = parse_mastodon_url(url)?;

        let api_url = format!("https://{instance}/api/v1/statuses/{status_id}");
        tracing::debug!("Fetching from Mastodon: {}", api_url);

        let response = client
            .fetch_text(&api_url)
            .await
            .context("Failed to fetch from Mastodon API")?;

        let status: MastodonStatus =
            serde_json::from_str(&response).context("Failed to parse Mastodon response")?;

        let markdown = format_mastodon_markdown(&status);

        let engagement = Engagement {
            likes: Some(status.favourites_count),
            reposts: Some(status.reblogs_count),
            replies: Some(status.replies_count),
            views: None,
        };

        let metadata = SiteMetadata {
            author: Some(format!(
                "{} (@{}@{})",
                status.account.display_name, status.account.username, instance
            )),
            title: None,
            published: Some(status.created_at.clone()),
            platform: "Mastodon".to_string(),
            canonical_url: status.url.unwrap_or_else(|| url.to_string()),
            media_urls: status
                .media_attachments
                .iter()
                .filter_map(|m| m.url.clone())
                .collect(),
            engagement: Some(engagement),
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// Check if a normalized URL contains the `/@user/digits` pattern.
fn has_status_pattern(url: &str) -> bool {
    // Look for /@something/digits
    if let Some(at_pos) = url.find("/@") {
        let after_at = &url[at_pos + 2..];
        if let Some(slash_pos) = after_at.find('/') {
            let after_slash = &after_at[slash_pos + 1..];
            // The status ID should be numeric (or at least start with digits)
            let id_part = after_slash.split('/').next().unwrap_or("");
            return !id_part.is_empty() && id_part.chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

/// Parse Mastodon URL to extract instance domain and status ID.
fn parse_mastodon_url(url: &str) -> Result<(String, String)> {
    let url = url.split('?').next().unwrap_or(url);
    let url = url.split('#').next().unwrap_or(url);

    // Extract instance from host
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .context("Invalid Mastodon URL: missing scheme")?;

    let instance = after_scheme
        .split('/')
        .next()
        .context("Could not extract instance from URL")?
        .to_string();

    // Extract status ID: last numeric segment after /@user/
    let at_pos = url
        .find("/@")
        .context("URL does not contain /@user/ pattern")?;
    let after_at = &url[at_pos + 2..];
    let slash_pos = after_at
        .find('/')
        .context("URL missing status ID after username")?;
    let status_id = after_at[slash_pos + 1..]
        .split('/')
        .next()
        .context("Could not extract status ID")?
        .to_string();

    if status_id.is_empty() {
        anyhow::bail!("Empty status ID in URL");
    }

    Ok((instance, status_id))
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

    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Format Mastodon status as markdown.
fn format_mastodon_markdown(status: &MastodonStatus) -> String {
    let mut md = String::new();

    // Author header
    let display = if status.account.display_name.is_empty() {
        &status.account.username
    } else {
        &status.account.display_name
    };

    md.push_str("## ");
    md.push_str(display);
    md.push_str(" (@");
    md.push_str(&status.account.username);
    md.push_str(")\n\n");

    // Content (HTML stripped)
    let content = strip_html(&status.content);
    if !content.is_empty() {
        md.push_str(&content);
        md.push_str("\n\n");
    }

    // Media attachments
    for attachment in &status.media_attachments {
        if let Some(url) = &attachment.url {
            let alt = attachment
                .description
                .as_deref()
                .unwrap_or("Media attachment");
            md.push_str("![");
            md.push_str(alt);
            md.push_str("](");
            md.push_str(url);
            md.push_str(")\n\n");
        }
    }

    // Engagement metrics
    let engagement = format_engagement(
        status.favourites_count,
        status.reblogs_count,
        status.replies_count,
    );
    md.push_str(&engagement);

    // Timestamp
    md.push_str(&status.created_at);
    md.push('\n');

    // Link to original
    if let Some(url) = &status.url {
        md.push_str("\n[View on Mastodon](");
        md.push_str(url);
        md.push_str(")\n");
    }

    md
}

/// Format engagement metrics as a compact string.
fn format_engagement(favourites: u64, reblogs: u64, replies: u64) -> String {
    let mut parts = Vec::new();

    if favourites > 0 {
        parts.push(format!("{} favourites", format_number(favourites)));
    }
    if reblogs > 0 {
        parts.push(format!("{} boosts", format_number(reblogs)));
    }
    if replies > 0 {
        parts.push(format!("{} replies", format_number(replies)));
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("{}\n", parts.join(" · "))
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
// Mastodon API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct MastodonStatus {
    content: String,
    created_at: String,
    url: Option<String>,
    favourites_count: u64,
    reblogs_count: u64,
    replies_count: u64,
    account: MastodonAccount,
    #[serde(default)]
    media_attachments: Vec<MastodonMedia>,
}

#[derive(Debug, Deserialize)]
struct MastodonAccount {
    username: String,
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct MastodonMedia {
    url: Option<String>,
    description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_mastodon_social_urls() {
        let provider = MastodonProvider;
        assert!(provider.matches("https://mastodon.social/@user/123456789"));
        assert!(provider.matches("https://MASTODON.SOCIAL/@User/999"));
    }

    #[test]
    fn matches_hachyderm_urls() {
        let provider = MastodonProvider;
        assert!(provider.matches("https://hachyderm.io/@engineer/111222333"));
    }

    #[test]
    fn matches_fosstodon_urls() {
        let provider = MastodonProvider;
        assert!(provider.matches("https://fosstodon.org/@developer/444555666"));
    }

    #[test]
    fn matches_infosec_exchange_urls() {
        let provider = MastodonProvider;
        assert!(provider.matches("https://infosec.exchange/@researcher/777888999"));
    }

    #[test]
    fn matches_urls_with_query_params() {
        let provider = MastodonProvider;
        assert!(provider.matches("https://mastodon.social/@user/123?ref=share"));
    }

    #[test]
    fn does_not_match_profile_urls() {
        let provider = MastodonProvider;
        assert!(!provider.matches("https://mastodon.social/@user"));
        assert!(!provider.matches("https://mastodon.social/@user/"));
    }

    #[test]
    fn does_not_match_non_numeric_ids() {
        let provider = MastodonProvider;
        assert!(!provider.matches("https://mastodon.social/@user/followers"));
        assert!(!provider.matches("https://mastodon.social/@user/following"));
    }

    #[test]
    fn does_not_match_unknown_instances() {
        let provider = MastodonProvider;
        assert!(!provider.matches("https://unknown-instance.com/@user/123"));
        assert!(!provider.matches("https://twitter.com/@user/123"));
    }

    #[test]
    fn does_not_match_non_mastodon_urls() {
        let provider = MastodonProvider;
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
        assert!(!provider.matches("https://mastodon.social/about"));
    }

    #[test]
    fn parse_mastodon_url_extracts_instance_and_id() {
        let (instance, id) = parse_mastodon_url("https://mastodon.social/@user/123456789").unwrap();
        assert_eq!(instance, "mastodon.social");
        assert_eq!(id, "123456789");
    }

    #[test]
    fn parse_mastodon_url_handles_hachyderm() {
        let (instance, id) =
            parse_mastodon_url("https://hachyderm.io/@engineer/111222333").unwrap();
        assert_eq!(instance, "hachyderm.io");
        assert_eq!(id, "111222333");
    }

    #[test]
    fn parse_mastodon_url_strips_query_and_fragment() {
        let (instance, id) =
            parse_mastodon_url("https://fosstodon.org/@dev/999?ref=share#top").unwrap();
        assert_eq!(instance, "fosstodon.org");
        assert_eq!(id, "999");
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
    fn strip_html_handles_links() {
        let html = r#"<p>Check <a href="https://example.com">this</a> out</p>"#;
        assert_eq!(strip_html(html), "Check this out");
    }

    #[test]
    fn has_status_pattern_detects_valid_patterns() {
        assert!(has_status_pattern(
            "https://mastodon.social/@user/123456789"
        ));
        assert!(has_status_pattern("https://hachyderm.io/@test/999"));
    }

    #[test]
    fn has_status_pattern_rejects_profile_urls() {
        assert!(!has_status_pattern("https://mastodon.social/@user"));
        assert!(!has_status_pattern(
            "https://mastodon.social/@user/followers"
        ));
    }

    #[test]
    fn format_engagement_combines_metrics() {
        let result = format_engagement(100, 50, 25);
        assert_eq!(result, "100 favourites · 50 boosts · 25 replies\n");
    }

    #[test]
    fn format_engagement_omits_zero_metrics() {
        let result = format_engagement(10, 0, 0);
        assert_eq!(result, "10 favourites\n");
    }

    #[test]
    fn format_engagement_returns_empty_when_all_zero() {
        let result = format_engagement(0, 0, 0);
        assert_eq!(result, "");
    }

    #[test]
    fn format_number_uses_k_suffix() {
        assert_eq!(format_number(1_500), "1.5K");
        assert_eq!(format_number(8_800), "8.8K");
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn format_mastodon_markdown_includes_content_and_metadata() {
        let status = MastodonStatus {
            content: "<p>Hello Fediverse! This is a <b>test</b> post.</p>".to_string(),
            created_at: "2025-02-12T10:00:00.000Z".to_string(),
            url: Some("https://mastodon.social/@testuser/123".to_string()),
            favourites_count: 42,
            reblogs_count: 10,
            replies_count: 5,
            account: MastodonAccount {
                username: "testuser".to_string(),
                display_name: "Test User".to_string(),
            },
            media_attachments: vec![],
        };

        let md = format_mastodon_markdown(&status);

        assert!(md.contains("## Test User (@testuser)"));
        assert!(md.contains("Hello Fediverse! This is a test post."));
        assert!(md.contains("42 favourites"));
        assert!(md.contains("10 boosts"));
        assert!(md.contains("5 replies"));
        assert!(md.contains("[View on Mastodon]"));
    }

    #[test]
    fn format_mastodon_markdown_uses_username_when_display_empty() {
        let status = MastodonStatus {
            content: "<p>Test</p>".to_string(),
            created_at: "2025-02-12T10:00:00.000Z".to_string(),
            url: None,
            favourites_count: 0,
            reblogs_count: 0,
            replies_count: 0,
            account: MastodonAccount {
                username: "username".to_string(),
                display_name: String::new(),
            },
            media_attachments: vec![],
        };

        let md = format_mastodon_markdown(&status);
        assert!(md.contains("## username (@username)"));
    }

    #[test]
    fn format_mastodon_markdown_includes_media() {
        let status = MastodonStatus {
            content: "<p>Photo post</p>".to_string(),
            created_at: "2025-02-12T10:00:00.000Z".to_string(),
            url: Some("https://mastodon.social/@user/456".to_string()),
            favourites_count: 0,
            reblogs_count: 0,
            replies_count: 0,
            account: MastodonAccount {
                username: "user".to_string(),
                display_name: "User".to_string(),
            },
            media_attachments: vec![MastodonMedia {
                url: Some("https://files.mastodon.social/photo.jpg".to_string()),
                description: Some("A nice photo".to_string()),
            }],
        };

        let md = format_mastodon_markdown(&status);
        assert!(md.contains("![A nice photo](https://files.mastodon.social/photo.jpg)"));
    }
}
