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
//! Provider loading order (first match wins):
//! 1. **Rule-based providers** from `~/.config/nab/sites/*.toml` (user overrides)
//! 2. **Rule-based providers** from embedded defaults (twitter, youtube, wikipedia, etc.)
//! 3. **Hardcoded Rust providers** for platforms NOT covered by a rule (hackernews, github, google, linkedin, reddit)
//! 4. **CSS extractor plugins** from `~/.config/nab/plugins.toml`
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
//! if let Some(content) = router.try_extract("https://x.com/user/status/123", &client, None).await {
//!     println!("{}", content.markdown);
//! }
//! # Ok(())
//! # }
//! ```

pub mod css_extractor;
pub mod github;
pub mod google;
pub mod hackernews;
pub mod linkedin;
pub mod reddit;
pub mod rules;
pub mod wasm_manifest;
#[cfg(feature = "wasm-providers")]
pub mod wasm_provider;

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

/// Format large numbers with K/M suffixes for compact display.
///
/// Shared by hardcoded providers (hackernews, reddit) and the TOML template
/// engine via its string-based `format_number` wrapper.
///
/// ```
/// # use nab::site::format_number_compact;
/// assert_eq!(format_number_compact(1_500), "1.5K");
/// assert_eq!(format_number_compact(3_800_000), "3.8M");
/// assert_eq!(format_number_compact(42), "42");
/// ```
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn format_number_compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
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
    ///
    /// `cookies` carries the browser cookie header (e.g., `"SID=abc; HSID=def"`) for
    /// providers that require authentication. Most providers ignore this parameter.
    ///
    /// `prefetched_html` is an optional pre-fetched raw HTML body for the URL.
    /// When present, providers that need the HTML body can use it directly to
    /// avoid a redundant HTTP round-trip (e.g. CSS extractor providers).
    /// All built-in providers ignore this parameter.
    async fn extract(
        &self,
        url: &str,
        client: &AcceleratedClient,
        cookies: Option<&str>,
        prefetched_html: Option<&[u8]>,
    ) -> Result<SiteContent>;
}

/// Routes URLs to specialized site providers.
///
/// Built-in providers are checked first (in registration order).  CSS extractor
/// providers loaded from `~/.config/nab/plugins.toml` are appended after the
/// built-ins.  First match wins.
///
/// Returns `None` if no provider matches or extraction fails.
pub struct SiteRouter {
    providers: Vec<Box<dyn SiteProvider>>,
}

impl SiteRouter {
    /// Create a router with all providers in priority order:
    ///
    /// 1. Rule-based providers (user overrides + embedded defaults)
    /// 2. Hardcoded Rust providers for platforms not covered by a rule
    /// 3. CSS extractor plugins from `~/.config/nab/plugins.toml`
    ///
    /// Invalid rule/CSS plugin entries are skipped with a warning.
    #[must_use]
    pub fn new() -> Self {
        // Load rule-based providers first; track which names they cover.
        let mut providers: Vec<Box<dyn SiteProvider>> = rules::load_site_rules();
        let rule_names = rules::rule_overridden_names();

        // Hardcoded providers — only for platforms NOT covered by a rule.
        // Rule-covered sites (twitter, youtube, wikipedia, mastodon, instagram,
        // stackoverflow, reddit) have been removed; the rule engine handles them.
        // hackernews-item rule handles item pages; the hardcoded HackerNewsProvider
        // still handles front-page listings.
        let hardcoded: Vec<Box<dyn SiteProvider>> = vec![
            Box::new(hackernews::HackerNewsProvider),
            Box::new(github::GitHubProvider),
            Box::new(google::GoogleWorkspaceProvider),
            Box::new(linkedin::LinkedInProvider),
        ];

        for p in hardcoded {
            if !rule_names.contains(p.name()) {
                providers.push(p);
            }
        }

        append_css_providers(&mut providers);

        #[cfg(feature = "wasm-providers")]
        append_wasm_providers(&mut providers);

        Self { providers }
    }

    /// Create a router with the built-in providers plus the given additional
    /// providers appended at the end.  Useful for testing without touching the
    /// plugins config file.
    #[must_use]
    pub fn with_extra_providers(mut extra: Vec<Box<dyn SiteProvider>>) -> Self {
        let mut router = Self::new();
        router.providers.append(&mut extra);
        router
    }

    /// Number of registered providers (built-ins + CSS plugins).
    #[must_use]
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// Try to extract content using a specialized provider.
    ///
    /// Returns `None` if no provider matches or extraction fails (logged as warning).
    pub async fn try_extract(
        &self,
        url: &str,
        client: &AcceleratedClient,
        cookies: Option<&str>,
    ) -> Option<SiteContent> {
        self.try_extract_with_html(url, client, cookies, None).await
    }

    /// Like [`try_extract`] but accepts pre-fetched HTML bytes for providers that can use them.
    pub async fn try_extract_with_html(
        &self,
        url: &str,
        client: &AcceleratedClient,
        cookies: Option<&str>,
        prefetched_html: Option<&[u8]>,
    ) -> Option<SiteContent> {
        for provider in &self.providers {
            if provider.matches(url) {
                tracing::debug!("Matched site provider: {}", provider.name());
                match provider
                    .extract(url, client, cookies, prefetched_html)
                    .await
                {
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

// ─────────────────────────────────────────────────────────────────────────────
// CSS provider loading
// ─────────────────────────────────────────────────────────────────────────────

/// Load CSS extractor configs from plugins.toml and append valid providers.
/// Invalid entries are skipped with a warning.
fn append_css_providers(providers: &mut Vec<Box<dyn SiteProvider>>) {
    use crate::plugin::config::load_all_plugins;
    use css_extractor::{CssExtractorConfig, CssExtractorProvider};

    let loaded = match load_all_plugins() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("Failed to load plugins.toml: {e}");
            return;
        }
    };

    for css_cfg in loaded.css {
        // CSS configs support multiple patterns; we compile one provider per
        // config and pass the first pattern as the URL regex.  For multiple
        // patterns the caller should create separate entries.  This matches
        // how the binary PluginRunner handles `patterns` (any match wins).
        let url_pattern = build_pattern_regex(&css_cfg.patterns);
        let config = CssExtractorConfig {
            name: css_cfg.name.clone(),
            url_pattern,
            content_selector: css_cfg.content.selector,
            title_selector: css_cfg.metadata.title,
            author_selector: css_cfg.metadata.author,
            date_selector: css_cfg.metadata.published,
            remove_selectors: css_cfg.content.remove,
        };

        match CssExtractorProvider::new(config) {
            Ok(provider) => {
                tracing::debug!("Loaded CSS extractor plugin: {}", css_cfg.name);
                providers.push(Box::new(provider));
            }
            Err(e) => {
                tracing::warn!("CSS extractor '{}' failed to load: {e}", css_cfg.name);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WASM provider loading (feature-gated)
// ─────────────────────────────────────────────────────────────────────────────

/// Load WASM providers from `~/.config/nab/wasm_providers/` and append them.
///
/// Each provider is loaded with automatic ABI detection: Component Model is
/// tried first; plain Wasm modules fall back to the legacy raw-C ABI.
/// Invalid or incompatible entries are skipped with a warning.
#[cfg(feature = "wasm-providers")]
fn append_wasm_providers(providers: &mut Vec<Box<dyn SiteProvider>>) {
    use wasm_manifest::{load_installed_providers, wasm_providers_dir};
    use wasm_provider::load_provider_from_file;

    let base = wasm_providers_dir();
    let installed = load_installed_providers(&base);

    for p in installed {
        let url_pattern = build_pattern_regex(&p.manifest.url_patterns);
        match load_provider_from_file(&p.manifest.name, &p.wasm_path, &url_pattern) {
            Ok(provider) => {
                tracing::debug!("Loaded WASM provider: {}", p.manifest.name);
                providers.push(provider);
            }
            Err(e) => {
                tracing::warn!("WASM provider '{}' failed to load: {e}", p.manifest.name);
            }
        }
    }
}

/// Build a single regex from a list of patterns using `|` alternation.
/// An empty list produces a regex that never matches.
fn build_pattern_regex(patterns: &[String]) -> String {
    if patterns.is_empty() {
        // `\A\z` anchors start+end with nothing in between — only matches the
        // empty string, which no URL ever is.  The `regex` crate does not
        // support look-ahead (`(?!x)x`), so we use this anchor pair instead.
        return r"\A\z".to_string();
    }
    if patterns.len() == 1 {
        return patterns[0].clone();
    }
    patterns
        .iter()
        .map(|p| format!("(?:{p})"))
        .collect::<Vec<_>>()
        .join("|")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_registers_all_builtin_providers() {
        let router = SiteRouter::new();
        // Rule-based providers (9: twitter, youtube, wikipedia, mastodon, reddit,
        // stackoverflow, instagram, github-issues, hackernews-item) + hardcoded
        // providers (4: hackernews, github, google-workspace, linkedin) = 13
        // minimum; CSS plugins may add more.
        assert!(router.providers.len() >= 13);

        // All expected names must appear somewhere in the provider list.
        let names: Vec<&str> = router.providers.iter().map(|p| p.name()).collect();
        for expected in &[
            "twitter",
            "reddit",
            "hackernews",
            "hackernews-item",
            "github",
            "github-issues",
            "google-workspace",
            "instagram",
            "youtube",
            "wikipedia",
            "stackoverflow",
            "mastodon",
            "linkedin",
        ] {
            assert!(names.contains(expected), "missing provider '{expected}'");
        }
    }

    #[test]
    fn router_rule_providers_come_before_hardcoded() {
        let router = SiteRouter::new();
        // Both twitter (embedded index 0) and reddit (embedded index 4) are
        // rule-based; twitter should appear before hackernews (hardcoded).
        let twitter_pos = router.providers.iter().position(|p| p.name() == "twitter");
        let hn_pos = router
            .providers
            .iter()
            .position(|p| p.name() == "hackernews");
        assert!(
            twitter_pos < hn_pos,
            "rule-based twitter should precede hardcoded hackernews"
        );
    }

    #[test]
    fn router_matches_twitter_urls() {
        let router = SiteRouter::new();
        // Find the first provider that matches twitter URLs.
        let twitter = router
            .providers
            .iter()
            .find(|p| p.matches("https://x.com/user/status/123"))
            .expect("some provider should match twitter URLs");
        assert_eq!(twitter.name(), "twitter");
        assert!(twitter.matches("https://twitter.com/user/status/456"));
    }

    #[test]
    fn router_does_not_match_non_provider_urls() {
        let router = SiteRouter::new();
        let generic_url = "https://example.com/page";
        // All providers (rule-based + hardcoded) must not match a generic URL.
        for provider in &router.providers {
            assert!(
                !provider.matches(generic_url),
                "provider '{}' should not match generic URL",
                provider.name()
            );
        }
    }

    #[test]
    fn router_with_extra_provider_increases_count() {
        use css_extractor::{CssExtractorConfig, CssExtractorProvider};

        let base_count = SiteRouter::new().provider_count();
        let config = CssExtractorConfig {
            name: "extra".to_string(),
            url_pattern: r"extra\.example\.com".to_string(),
            content_selector: "main".to_string(),
            title_selector: None,
            author_selector: None,
            date_selector: None,
            remove_selectors: vec![],
        };
        let provider = CssExtractorProvider::new(config).unwrap();
        let router = SiteRouter::with_extra_providers(vec![Box::new(provider)]);
        assert_eq!(router.provider_count(), base_count + 1);
    }

    #[test]
    fn extra_css_provider_matches_its_url() {
        use css_extractor::{CssExtractorConfig, CssExtractorProvider};

        let config = CssExtractorConfig {
            name: "my-extra".to_string(),
            url_pattern: r"myextra\.com".to_string(),
            content_selector: "article".to_string(),
            title_selector: None,
            author_selector: None,
            date_selector: None,
            remove_selectors: vec![],
        };
        let provider = CssExtractorProvider::new(config).unwrap();
        let router = SiteRouter::with_extra_providers(vec![Box::new(provider)]);

        // All built-in providers (rule-based + hardcoded) must not claim myextra.com.
        // The extra provider is the last one and should match.
        let base_count = SiteRouter::new().provider_count();
        for p in router.providers.iter().take(base_count) {
            assert!(!p.matches("https://myextra.com/article/1"));
        }
        // Extra provider should match
        let last = router.providers.last().unwrap();
        assert!(last.matches("https://myextra.com/article/1"));
    }

    #[test]
    fn build_pattern_regex_empty_never_matches() {
        let pattern = build_pattern_regex(&[]);
        let re = regex::Regex::new(&pattern).unwrap();
        assert!(!re.is_match("anything"));
    }

    #[test]
    fn build_pattern_regex_single_pattern_unchanged() {
        let pattern = build_pattern_regex(&[r"foo\.com".to_string()]);
        assert_eq!(pattern, r"foo\.com");
    }

    #[test]
    fn build_pattern_regex_multiple_patterns_alternate() {
        let pattern = build_pattern_regex(&[r"foo\.com".to_string(), r"bar\.com".to_string()]);
        let re = regex::Regex::new(&pattern).unwrap();
        assert!(re.is_match("https://foo.com/page"));
        assert!(re.is_match("https://bar.com/page"));
        assert!(!re.is_match("https://baz.com/page"));
    }
}
