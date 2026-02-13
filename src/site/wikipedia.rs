//! Wikipedia content extraction via REST API.
//!
//! Uses the Wikimedia REST API (`/api/rest_v1/page/summary/`) for structured
//! article summaries. Supports all language editions (en, fi, de, etc.).
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, wikipedia::WikipediaProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = WikipediaProvider;
//!
//! let content = provider.extract(
//!     "https://en.wikipedia.org/wiki/Rust_(programming_language)",
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

/// Wikipedia content provider using Wikimedia REST API.
pub struct WikipediaProvider;

#[async_trait]
impl SiteProvider for WikipediaProvider {
    fn name(&self) -> &'static str {
        "wikipedia"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        normalized.contains(".wikipedia.org/wiki/")
    }

    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
        let (lang, title) = parse_wikipedia_url(url)?;

        let api_url = format!(
            "https://{lang}.wikipedia.org/api/rest_v1/page/summary/{}",
            urlencoding::encode(&title)
        );
        tracing::debug!("Fetching from Wikipedia: {}", api_url);

        let response = client
            .inner()
            .get(&api_url)
            .header(
                "User-Agent",
                "nab/0.3.0 (https://github.com/MikkoParkkola/nab)",
            )
            .send()
            .await
            .context("Failed to fetch from Wikipedia API")?
            .text()
            .await
            .context("Failed to read Wikipedia response body")?;

        let summary: WikipediaSummary =
            serde_json::from_str(&response).context("Failed to parse Wikipedia response")?;

        let markdown = format_wikipedia_markdown(&summary, &lang);

        let metadata = SiteMetadata {
            author: None,
            title: Some(summary.title.clone()),
            published: summary.timestamp.clone(),
            platform: "Wikipedia".to_string(),
            canonical_url: summary
                .content_urls
                .as_ref()
                .and_then(|u| u.desktop.as_ref())
                .map(|d| d.page.clone())
                .unwrap_or_else(|| url.to_string()),
            media_urls: summary
                .thumbnail
                .as_ref()
                .map(|t| vec![t.source.clone()])
                .unwrap_or_default(),
            engagement: None,
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// Parse Wikipedia URL to extract language code and article title.
fn parse_wikipedia_url(url: &str) -> Result<(String, String)> {
    let url = url.split('?').next().unwrap_or(url);
    let url = url.split('#').next().unwrap_or(url);

    // Extract language from subdomain: https://{lang}.wikipedia.org/wiki/{title}
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .context("Invalid Wikipedia URL: missing scheme")?;

    let lang = after_scheme
        .split('.')
        .next()
        .context("Could not extract language from Wikipedia URL")?
        .to_string();

    // Extract title after /wiki/
    let wiki_idx = url.find("/wiki/").context("URL does not contain /wiki/")?;
    let title = url[wiki_idx + 6..].to_string();

    Ok((lang, title))
}

/// Format Wikipedia summary as markdown.
fn format_wikipedia_markdown(summary: &WikipediaSummary, lang: &str) -> String {
    let mut md = String::new();

    // Title
    md.push_str("## ");
    md.push_str(&summary.title);
    md.push_str("\n\n");

    // Description (short tagline)
    if let Some(desc) = &summary.description {
        md.push('*');
        md.push_str(desc);
        md.push_str("*\n\n");
    }

    // Thumbnail
    if let Some(thumb) = &summary.thumbnail {
        md.push_str("![");
        md.push_str(&summary.title);
        md.push_str("](");
        md.push_str(&thumb.source);
        md.push_str(")\n\n");
    }

    // Extract (summary text)
    if let Some(extract) = &summary.extract {
        md.push_str(extract);
        md.push_str("\n\n");
    }

    // Link to full article
    let article_url = summary
        .content_urls
        .as_ref()
        .and_then(|u| u.desktop.as_ref())
        .map(|d| d.page.clone())
        .unwrap_or_else(|| {
            format!(
                "https://{}.wikipedia.org/wiki/{}",
                lang,
                urlencoding::encode(&summary.title)
            )
        });

    md.push_str("[Read full article on Wikipedia](");
    md.push_str(&article_url);
    md.push_str(")\n");

    md
}

// ============================================================================
// Wikipedia REST API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct WikipediaSummary {
    title: String,
    description: Option<String>,
    extract: Option<String>,
    thumbnail: Option<WikipediaThumbnail>,
    timestamp: Option<String>,
    content_urls: Option<WikipediaContentUrls>,
}

#[derive(Debug, Deserialize)]
struct WikipediaThumbnail {
    source: String,
}

#[derive(Debug, Deserialize)]
struct WikipediaContentUrls {
    desktop: Option<WikipediaDesktopUrl>,
}

#[derive(Debug, Deserialize)]
struct WikipediaDesktopUrl {
    page: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_english_wikipedia_urls() {
        let provider = WikipediaProvider;
        assert!(provider.matches("https://en.wikipedia.org/wiki/Rust_(programming_language)"));
        assert!(provider.matches("https://EN.WIKIPEDIA.ORG/WIKI/Test_Article"));
    }

    #[test]
    fn matches_other_language_wikipedia_urls() {
        let provider = WikipediaProvider;
        assert!(provider.matches("https://fi.wikipedia.org/wiki/Ruoste"));
        assert!(provider.matches("https://de.wikipedia.org/wiki/Rost"));
        assert!(provider.matches("https://ja.wikipedia.org/wiki/Rust"));
    }

    #[test]
    fn matches_wikipedia_urls_with_query_params() {
        let provider = WikipediaProvider;
        assert!(provider.matches("https://en.wikipedia.org/wiki/Rust?ref=foo"));
    }

    #[test]
    fn does_not_match_non_wiki_urls() {
        let provider = WikipediaProvider;
        assert!(!provider.matches("https://en.wikipedia.org/"));
        assert!(!provider.matches("https://en.wikipedia.org/w/index.php"));
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
        assert!(!provider.matches("https://wiktionary.org/wiki/rust"));
    }

    #[test]
    fn parse_wikipedia_url_extracts_lang_and_title() {
        let (lang, title) =
            parse_wikipedia_url("https://en.wikipedia.org/wiki/Rust_(programming_language)")
                .unwrap();
        assert_eq!(lang, "en");
        assert_eq!(title, "Rust_(programming_language)");
    }

    #[test]
    fn parse_wikipedia_url_handles_finnish() {
        let (lang, title) = parse_wikipedia_url("https://fi.wikipedia.org/wiki/Helsinki").unwrap();
        assert_eq!(lang, "fi");
        assert_eq!(title, "Helsinki");
    }

    #[test]
    fn parse_wikipedia_url_strips_query_and_fragment() {
        let (lang, title) =
            parse_wikipedia_url("https://en.wikipedia.org/wiki/Rust?action=edit#History").unwrap();
        assert_eq!(lang, "en");
        assert_eq!(title, "Rust");
    }

    #[test]
    fn format_wikipedia_markdown_includes_title_and_extract() {
        let summary = WikipediaSummary {
            title: "Rust (programming language)".to_string(),
            description: Some("General-purpose programming language".to_string()),
            extract: Some("Rust is a multi-paradigm programming language.".to_string()),
            thumbnail: Some(WikipediaThumbnail {
                source: "https://upload.wikimedia.org/rust_logo.png".to_string(),
            }),
            timestamp: Some("2025-01-01T00:00:00Z".to_string()),
            content_urls: Some(WikipediaContentUrls {
                desktop: Some(WikipediaDesktopUrl {
                    page: "https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string(),
                }),
            }),
        };

        let md = format_wikipedia_markdown(&summary, "en");

        assert!(md.contains("## Rust (programming language)"));
        assert!(md.contains("*General-purpose programming language*"));
        assert!(md.contains("Rust is a multi-paradigm programming language."));
        assert!(md.contains(
            "![Rust (programming language)](https://upload.wikimedia.org/rust_logo.png)"
        ));
        assert!(md.contains("[Read full article on Wikipedia]"));
    }

    #[test]
    fn format_wikipedia_markdown_handles_missing_optional_fields() {
        let summary = WikipediaSummary {
            title: "Test Article".to_string(),
            description: None,
            extract: Some("Some text.".to_string()),
            thumbnail: None,
            timestamp: None,
            content_urls: None,
        };

        let md = format_wikipedia_markdown(&summary, "en");

        assert!(md.contains("## Test Article"));
        assert!(md.contains("Some text."));
        assert!(!md.contains("!["));
        assert!(md.contains("[Read full article on Wikipedia]"));
    }
}
