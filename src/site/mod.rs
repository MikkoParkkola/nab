//! Site-specific content extraction.
//!
//! Provides specialized extractors for platforms where direct API access
//! yields better structured content than HTML parsing (e.g., Twitter/X via `FxTwitter`).
//!
//! # Architecture
//!
//! - [`SiteProvider`]: Async trait for platform-specific extraction
//! - [`SiteRouter`]: Dispatches URLs to the appropriate provider
//! - [`SiteContent`]: Structured content with metadata
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::SiteRouter;
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let router = SiteRouter::new();
//!
//! if let Some(content) = router.try_extract("https://x.com/user/status/123", &client).await {
//!     println!("{}", content.markdown);
//! }
//! # Ok(())
//! # }
//! ```

pub mod github;
pub mod hackernews;
pub mod instagram;
pub mod linkedin;
pub mod mastodon;
pub mod reddit;
pub mod stackoverflow;
mod twitter;
pub mod wikipedia;
pub mod youtube;

use anyhow::Result;
use async_trait::async_trait;

use crate::http_client::AcceleratedClient;

/// Engagement metrics for social media content.
#[derive(Debug, Clone, Default)]
pub struct Engagement {
    pub likes: Option<u64>,
    pub reposts: Option<u64>,
    pub replies: Option<u64>,
    pub views: Option<u64>,
}

/// Metadata about extracted site content.
#[derive(Debug, Clone)]
pub struct SiteMetadata {
    pub author: Option<String>,
    pub title: Option<String>,
    pub published: Option<String>,
    pub platform: String,
    pub canonical_url: String,
    pub media_urls: Vec<String>,
    pub engagement: Option<Engagement>,
}

/// Extracted and formatted site content.
#[derive(Debug, Clone)]
pub struct SiteContent {
    /// Markdown-formatted content ready for LLM consumption.
    pub markdown: String,
    /// Structured metadata about the content.
    pub metadata: SiteMetadata,
}

/// Provider for extracting content from a specific platform.
#[async_trait]
pub trait SiteProvider: Send + Sync {
    /// Provider name (e.g., "twitter", "youtube").
    fn name(&self) -> &'static str;

    /// Check if this provider handles the given URL.
    fn matches(&self, url: &str) -> bool;

    /// Extract content from the URL using the provider's API/method.
    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent>;
}

/// Routes URLs to specialized site providers.
///
/// Providers are checked in registration order. First match wins.
/// Returns `None` if no provider matches or extraction fails.
pub struct SiteRouter {
    providers: Vec<Box<dyn SiteProvider>>,
}

impl SiteRouter {
    /// Create a router with all available site providers.
    #[must_use]
    pub fn new() -> Self {
        let providers: Vec<Box<dyn SiteProvider>> = vec![
            Box::new(twitter::TwitterProvider),
            Box::new(reddit::RedditProvider),
            Box::new(hackernews::HackerNewsProvider),
            Box::new(github::GitHubProvider),
            Box::new(instagram::InstagramProvider),
            Box::new(youtube::YouTubeProvider),
            Box::new(wikipedia::WikipediaProvider),
            Box::new(stackoverflow::StackOverflowProvider),
            Box::new(mastodon::MastodonProvider),
            Box::new(linkedin::LinkedInProvider),
        ];

        Self { providers }
    }

    /// Try to extract content using a specialized provider.
    ///
    /// Returns `None` if:
    /// - No provider matches the URL
    /// - Provider extraction fails (logged as warning)
    pub async fn try_extract(
        &self,
        url: &str,
        client: &AcceleratedClient,
    ) -> Option<SiteContent> {
        for provider in &self.providers {
            if provider.matches(url) {
                tracing::debug!("Matched site provider: {}", provider.name());
                match provider.extract(url, client).await {
                    Ok(content) => return Some(content),
                    Err(e) => {
                        tracing::warn!(
                            "Site provider {} failed for {}: {}",
                            provider.name(),
                            url,
                            e
                        );
                        return None;
                    }
                }
            }
        }
        None
    }
}

impl Default for SiteRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_registers_all_providers() {
        let router = SiteRouter::new();
        assert_eq!(router.providers.len(), 10);
        assert_eq!(router.providers[0].name(), "twitter");
        assert_eq!(router.providers[1].name(), "reddit");
        assert_eq!(router.providers[2].name(), "hackernews");
        assert_eq!(router.providers[3].name(), "github");
        assert_eq!(router.providers[4].name(), "instagram");
        assert_eq!(router.providers[5].name(), "youtube");
        assert_eq!(router.providers[6].name(), "wikipedia");
        assert_eq!(router.providers[7].name(), "stackoverflow");
        assert_eq!(router.providers[8].name(), "mastodon");
        assert_eq!(router.providers[9].name(), "linkedin");
    }

    #[test]
    fn router_matches_twitter_urls() {
        let router = SiteRouter::new();
        assert!(router.providers[0].matches("https://x.com/user/status/123"));
        assert!(router.providers[0].matches("https://twitter.com/user/status/456"));
    }

    #[test]
    fn router_does_not_match_non_provider_urls() {
        let router = SiteRouter::new();
        // None of the providers should match this generic URL
        let generic_url = "https://example.com/page";
        for provider in &router.providers {
            assert!(!provider.matches(generic_url));
        }
    }
}
