//! Hacker News content extraction via Algolia API and Firebase REST API.
//!
//! Uses the official Algolia HN Search API for individual item pages,
//! and the Firebase REST API for front-page listing views.
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
//!     &client,
//!     None
//! ).await?;
//!
//! println!("{}", content.markdown);
//! # Ok(())
//! # }
//! ```

use std::fmt::Write as _;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::{Engagement, SiteContent, SiteMetadata, SiteProvider};
use crate::http_client::AcceleratedClient;

/// Number of stories to fetch for front-page listing views.
const FRONT_PAGE_STORY_COUNT: usize = 30;

/// Firebase HN API base URL.
const HN_FIREBASE_BASE: &str = "https://hacker-news.firebaseio.com/v0";

/// Hacker News content provider using Algolia and Firebase APIs.
pub struct HackerNewsProvider;

#[async_trait]
impl SiteProvider for HackerNewsProvider {
    fn name(&self) -> &'static str {
        "hackernews"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        if !normalized.contains("news.ycombinator.com") {
            return false;
        }

        // Individual item page.
        if normalized.contains("/item") {
            return true;
        }

        // Front-page listing paths.
        front_page_list_type(normalized).is_some()
    }

    async fn extract(
        &self,
        url: &str,
        client: &AcceleratedClient,
        _cookies: Option<&str>,
        _prefetched_html: Option<&[u8]>,
    ) -> Result<SiteContent> {
        let normalized = url.to_lowercase();
        let path_part = normalized.split('?').next().unwrap_or(&normalized);

        if path_part.contains("/item") {
            extract_item(url, client).await
        } else {
            let list = front_page_list_type(path_part).unwrap_or("topstories");
            fetch_front_page(list, url, client).await
        }
    }
}

/// Map a HN URL path to the corresponding Firebase list name.
///
/// Returns `None` if the path is not a recognised front-page listing.
fn front_page_list_type(path: &str) -> Option<&'static str> {
    // Strip trailing slash for matching.
    let path = path.trim_end_matches('/');

    if path.ends_with("news.ycombinator.com") || path.ends_with("/news") || path.ends_with("/front")
    {
        Some("topstories")
    } else if path.ends_with("/newest") {
        Some("newstories")
    } else if path.ends_with("/best") {
        Some("beststories")
    } else if path.ends_with("/ask") {
        Some("askstories")
    } else if path.ends_with("/show") {
        Some("showstories")
    } else {
        None
    }
}

// ============================================================================
// Extraction helpers
// ============================================================================

/// Extract a single HN item (story/ask/show + comments) via Algolia API.
async fn extract_item(url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
    let item_id = parse_hn_item_id(url)?;

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

/// Fetch a front-page listing from Firebase and format as a numbered markdown list.
///
/// `list_name` is one of `topstories`, `newstories`, `beststories`, `askstories`,
/// `showstories` — matching the Firebase HN API endpoint names.
async fn fetch_front_page(
    list_name: &str,
    canonical_url: &str,
    client: &AcceleratedClient,
) -> Result<SiteContent> {
    // Step 1: fetch list of story IDs.
    let ids_url = format!("{HN_FIREBASE_BASE}/{list_name}.json");
    tracing::debug!("Fetching HN front page list: {}", ids_url);

    let ids_json = client
        .fetch_text(&ids_url)
        .await
        .context("Failed to fetch HN story ID list")?;

    let all_ids: Vec<u64> =
        serde_json::from_str(&ids_json).context("Failed to parse HN story ID list")?;

    // Step 2: fetch each story concurrently (capped at FRONT_PAGE_STORY_COUNT).
    let ids: Vec<u64> = all_ids.into_iter().take(FRONT_PAGE_STORY_COUNT).collect();

    let mut handles = Vec::with_capacity(ids.len());
    for id in &ids {
        let item_url = format!("{HN_FIREBASE_BASE}/item/{id}.json");
        let client_inner = client.inner().clone();
        handles.push(tokio::spawn(async move {
            client_inner.get(&item_url).send().await?.text().await
        }));
    }

    let mut stories: Vec<HNFirebaseItem> = Vec::with_capacity(handles.len());
    for handle in handles {
        let Ok(Ok(text)) = handle.await else {
            continue;
        };
        if let Ok(item) = serde_json::from_str::<HNFirebaseItem>(&text) {
            stories.push(item);
        }
    }

    let markdown = format_front_page_markdown(list_name, &stories);

    let title = match list_name {
        "newstories" => "Hacker News: Newest",
        "beststories" => "Hacker News: Best",
        "askstories" => "Hacker News: Ask HN",
        "showstories" => "Hacker News: Show HN",
        _ => "Hacker News: Top Stories",
    };

    let metadata = SiteMetadata {
        author: None,
        title: Some(title.to_string()),
        published: None,
        platform: "Hacker News".to_string(),
        canonical_url: canonical_url.to_string(),
        media_urls: vec![],
        engagement: None,
    };

    Ok(SiteContent { markdown, metadata })
}

/// Parse Hacker News URL to extract item ID.
fn parse_hn_item_id(url: &str) -> Result<String> {
    let url = url.split('#').next().unwrap_or(url);

    // Extract id parameter from query string
    for part in url.split('?').skip(1).flat_map(|q| q.split('&')) {
        if let Some(id) = part.strip_prefix("id=") {
            return Ok(id.to_string());
        }
    }

    anyhow::bail!("Could not extract item ID from URL: {url}")
}

/// Format a front-page listing as a numbered markdown list.
fn format_front_page_markdown(list_name: &str, stories: &[HNFirebaseItem]) -> String {
    let heading = match list_name {
        "newstories" => "Hacker News: Newest",
        "beststories" => "Hacker News: Best",
        "askstories" => "Hacker News: Ask HN",
        "showstories" => "Hacker News: Show HN",
        _ => "Hacker News: Top Stories",
    };

    let mut md = format!("## {heading}\n\n");

    for (i, story) in stories.iter().enumerate() {
        let title = story.title.as_deref().unwrap_or("(untitled)");
        let points = story.score.unwrap_or(0);
        let comments = story.descendants.unwrap_or(0);

        let domain = story
            .url
            .as_deref()
            .and_then(|u| u.split('/').nth(2))
            .unwrap_or_default();

        let domain_suffix = if domain.is_empty() {
            String::new()
        } else {
            format!(" — {domain}")
        };

        let _ = writeln!(
            md,
            "{}. **{}** ({} points, {} comments){}",
            i + 1,
            title,
            format_number(points),
            format_number(comments),
            domain_suffix,
        );
    }

    md
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
    let points_str = item.points.map_or_else(
        || "0 points".to_string(),
        |p| format!("{} points", format_number(p)),
    );

    let author_str = item
        .author
        .as_ref()
        .map(|a| format!("by {a} · "))
        .unwrap_or_default();

    let _ = write!(
        md,
        "{author_str}{points_str} · {} comments\n\n",
        item.children.len()
    );

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

                let _ = write!(md, "**{author}**:\n\n{text}\n\n---\n\n");
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

/// Minimal Firebase item shape used for front-page listings.
#[derive(Debug, Deserialize)]
struct HNFirebaseItem {
    title: Option<String>,
    url: Option<String>,
    score: Option<u64>,
    descendants: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- matches() tests -------------------------------------------------------

    #[test]
    fn matches_hn_item_urls() {
        let provider = HackerNewsProvider;
        assert!(provider.matches("https://news.ycombinator.com/item?id=38471822"));
        assert!(provider.matches("https://NEWS.YCOMBINATOR.COM/ITEM?ID=999"));
    }

    #[test]
    fn matches_hn_front_page_root() {
        let provider = HackerNewsProvider;
        assert!(provider.matches("https://news.ycombinator.com/"));
        assert!(provider.matches("https://news.ycombinator.com"));
        assert!(provider.matches("https://news.ycombinator.com/news"));
    }

    #[test]
    fn matches_hn_front_page_listing_paths() {
        let provider = HackerNewsProvider;
        assert!(provider.matches("https://news.ycombinator.com/newest"));
        assert!(provider.matches("https://news.ycombinator.com/best"));
        assert!(provider.matches("https://news.ycombinator.com/ask"));
        assert!(provider.matches("https://news.ycombinator.com/show"));
        assert!(provider.matches("https://news.ycombinator.com/front"));
    }

    #[test]
    fn does_not_match_non_hn_urls() {
        let provider = HackerNewsProvider;
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
        assert!(!provider.matches("https://example.com/news"));
    }

    // ---- front_page_list_type() tests ------------------------------------------

    #[test]
    fn front_page_list_type_maps_root_to_topstories() {
        assert_eq!(
            front_page_list_type("https://news.ycombinator.com"),
            Some("topstories")
        );
        assert_eq!(
            front_page_list_type("https://news.ycombinator.com/"),
            Some("topstories")
        );
        assert_eq!(
            front_page_list_type("https://news.ycombinator.com/news"),
            Some("topstories")
        );
        assert_eq!(
            front_page_list_type("https://news.ycombinator.com/front"),
            Some("topstories")
        );
    }

    #[test]
    fn front_page_list_type_maps_listing_paths() {
        assert_eq!(
            front_page_list_type("https://news.ycombinator.com/newest"),
            Some("newstories")
        );
        assert_eq!(
            front_page_list_type("https://news.ycombinator.com/best"),
            Some("beststories")
        );
        assert_eq!(
            front_page_list_type("https://news.ycombinator.com/ask"),
            Some("askstories")
        );
        assert_eq!(
            front_page_list_type("https://news.ycombinator.com/show"),
            Some("showstories")
        );
    }

    #[test]
    fn front_page_list_type_returns_none_for_item_urls() {
        assert_eq!(
            front_page_list_type("https://news.ycombinator.com/item"),
            None
        );
    }

    // ---- parse helpers ---------------------------------------------------------

    #[test]
    fn parse_hn_item_id_extracts_id() {
        let id = parse_hn_item_id("https://news.ycombinator.com/item?id=38471822").unwrap();
        assert_eq!(id, "38471822");

        let id2 = parse_hn_item_id("https://news.ycombinator.com/item?id=999&foo=bar").unwrap();
        assert_eq!(id2, "999");
    }

    #[test]
    fn parse_hn_item_id_strips_fragment() {
        let id = parse_hn_item_id("https://news.ycombinator.com/item?id=123#comment").unwrap();
        assert_eq!(id, "123");
    }

    // ---- format helpers --------------------------------------------------------

    #[test]
    fn format_front_page_markdown_produces_numbered_list() {
        let stories = vec![
            HNFirebaseItem {
                title: Some("Rust 2024 Edition".to_string()),
                url: Some("https://blog.rust-lang.org/rust-2024".to_string()),
                score: Some(350),
                descendants: Some(42),
            },
            HNFirebaseItem {
                title: Some("Ask HN: Best books 2024".to_string()),
                url: None,
                score: Some(120),
                descendants: Some(87),
            },
        ];

        let md = format_front_page_markdown("topstories", &stories);

        assert!(md.contains("## Hacker News: Top Stories"));
        assert!(md.contains("1. **Rust 2024 Edition**"));
        assert!(md.contains("350 points"));
        assert!(md.contains("42 comments"));
        assert!(md.contains("blog.rust-lang.org"));
        assert!(md.contains("2. **Ask HN: Best books 2024**"));
    }

    #[test]
    fn format_front_page_markdown_uses_list_name_for_heading() {
        let md = format_front_page_markdown("newstories", &[]);
        assert!(md.contains("## Hacker News: Newest"));

        let md = format_front_page_markdown("beststories", &[]);
        assert!(md.contains("## Hacker News: Best"));

        let md = format_front_page_markdown("askstories", &[]);
        assert!(md.contains("## Hacker News: Ask HN"));

        let md = format_front_page_markdown("showstories", &[]);
        assert!(md.contains("## Hacker News: Show HN"));
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
