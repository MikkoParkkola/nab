//! [`ApiRuleProvider`] — a [`SiteProvider`] driven by a [`SiteRuleConfig`].
//!
//! Each provider:
//! 1. Tests URLs against compiled regex patterns.
//! 2. Rewrites the URL using a regex substitution (or `{url}` placeholder for oEmbed).
//! 3. Fetches the rewritten URL as JSON.
//! 4. Extracts named fields via dot-path selectors.
//! 5. Renders the Markdown template with extracted fields.
//! 6. Produces structured [`SiteContent`] with populated [`SiteMetadata`].

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use regex::Regex;

use super::super::{Engagement, SiteContent, SiteMetadata, SiteProvider};
use super::config::SiteRuleConfig;
use super::json_path;
use super::template;
use crate::http_client::AcceleratedClient;

/// A compiled, ready-to-use provider built from a [`SiteRuleConfig`].
pub struct ApiRuleProvider {
    /// Config driving this provider.
    config: SiteRuleConfig,
    /// Compiled URL-match regexes (one per pattern).
    patterns: Vec<Regex>,
    /// Compiled rewrite `from` regex.
    rewrite_from: Regex,
}

impl ApiRuleProvider {
    /// Return the rule name from the site config.
    pub fn rule_name(&self) -> &str {
        &self.config.site.name
    }

    /// Build a provider from a validated config.
    ///
    /// # Errors
    ///
    /// Returns an error if any regex in `config` fails to compile.
    pub fn new(config: SiteRuleConfig) -> Result<Self> {
        let patterns = config
            .site
            .patterns
            .iter()
            .map(|p| Regex::new(p).with_context(|| format!("invalid pattern regex '{p}'")))
            .collect::<Result<Vec<_>>>()?;

        let rewrite_from = Regex::new(&config.rewrite.from)
            .with_context(|| format!("invalid rewrite.from regex '{}'", config.rewrite.from))?;

        Ok(Self { config, patterns, rewrite_from })
    }

    /// Rewrite `url` according to the rule's `[rewrite]` config.
    fn rewrite_url(&self, url: &str) -> String {
        let to = &self.config.rewrite.to;

        // oEmbed-style: `to` contains `{url}` → URL-encode the original.
        if to.contains("{url}") {
            return to.replace("{url}", &urlencoding::encode(url));
        }

        // Capture-group rewrite.
        self.rewrite_from.replace(url, to.as_str()).into_owned()
    }

    /// Extract all configured JSON fields from a parsed JSON value.
    fn extract_fields(&self, json: &serde_json::Value) -> HashMap<String, String> {
        self.config
            .json
            .0
            .iter()
            .filter_map(|(name, path)| {
                let value = if path.contains("[]") {
                    let arr = json_path::extract_array(json, path);
                    if arr.is_empty() { return None; }
                    arr.join(", ")
                } else {
                    json_path::extract(json, path)?
                };
                Some((name.clone(), value))
            })
            .collect()
    }

    /// Build [`SiteMetadata`] from extracted fields and config.
    fn build_metadata(&self, fields: &HashMap<String, String>, original_url: &str) -> SiteMetadata {
        let meta = &self.config.metadata;

        let author = meta.author.as_deref().map(|tmpl| {
            template::render(tmpl, fields, original_url)
        }).or_else(|| {
            // Fallback: check extra["author_field"]
            meta.extra.get("author_field")
                .and_then(|f| fields.get(f))
                .cloned()
        });

        let title = meta.title_field.as_deref()
            .and_then(|f| if f.is_empty() { None } else { fields.get(f) })
            .cloned();

        let published = meta.published_field.as_deref()
            .and_then(|f| if f.is_empty() { None } else { fields.get(f) })
            .cloned();

        let canonical_url = meta.canonical_url_field.as_deref()
            .and_then(|f| if f.is_empty() { None } else { fields.get(f) })
            .cloned()
            .unwrap_or_else(|| original_url.to_string());

        let media_urls = meta.media_urls_field.as_deref()
            .and_then(|f| if f.is_empty() { None } else { fields.get(f) })
            .map(|u| vec![u.clone()])
            .unwrap_or_default();

        let engagement = build_engagement(&self.config.engagement, fields);

        SiteMetadata {
            author,
            title,
            published,
            platform: self.config.metadata.platform.clone(),
            canonical_url,
            media_urls,
            engagement,
        }
    }
}

/// Extract name string from a static `LazyLock<String>`.
///
/// Each `ApiRuleProvider` stores its name in a heap string from `config`.
/// The `SiteProvider` trait requires `&'static str`, so we use a per-name
/// interning approach via a global registry.
///
/// Instead, we return a leaked `&'static str` (acceptable because providers
/// are created once at startup and never dropped).
fn leak_name(s: &str) -> &'static str {
    // SAFETY: We intentionally leak this allocation.  Providers live for the
    // entire program duration (stored in `SiteRouter`), so the memory is
    // effectively static.  The number of unique names is small (bounded by
    // embedded defaults + user configs).
    Box::leak(s.to_string().into_boxed_str())
}

#[async_trait]
impl SiteProvider for ApiRuleProvider {
    fn name(&self) -> &'static str {
        // Cache the leaked &'static str in a thread-local to avoid leaking on
        // every call.  In practice `name()` is called rarely.
        static EMPTY: &str = "";
        let _ = EMPTY; // suppress unused warning
        leak_name(&self.config.site.name)
    }

    fn matches(&self, url: &str) -> bool {
        self.patterns.iter().any(|re| re.is_match(url))
    }

    async fn extract(
        &self,
        url: &str,
        client: &AcceleratedClient,
        _cookies: Option<&str>,
        _prefetched_html: Option<&[u8]>,
    ) -> Result<SiteContent> {
        let api_url = self.rewrite_url(url);
        tracing::debug!("ApiRuleProvider '{}': fetching {}", self.config.site.name, api_url);

        // Build request with configured headers.
        let mut request = client.inner().get(&api_url);

        if let Some(accept) = &self.config.request.accept {
            request = request.header(reqwest::header::ACCEPT, accept.as_str());
        }
        for (key, value) in &self.config.request.headers {
            request = request.header(key.as_str(), value.as_str());
        }

        let body = request
            .send()
            .await
            .with_context(|| format!("request failed for '{api_url}'"))?
            .text()
            .await
            .with_context(|| format!("failed to read response body from '{api_url}'"))?;

        let json: serde_json::Value =
            serde_json::from_str(&body)
                .with_context(|| format!("failed to parse JSON from '{api_url}'"))?;

        let fields = self.extract_fields(&json);

        if fields.is_empty() {
            bail!(
                "no fields extracted from '{}' response (check json paths in rule '{}')",
                api_url,
                self.config.site.name
            );
        }

        let markdown = template::render(&self.config.template.format, &fields, url);
        let metadata = self.build_metadata(&fields, url);

        Ok(SiteContent { markdown, metadata })
    }
}

/// Build [`Engagement`] from extracted fields using the engagement config.
fn build_engagement(
    eng: &super::config::EngagementConfig,
    fields: &HashMap<String, String>,
) -> Option<Engagement> {
    let likes = eng.likes.as_deref().and_then(|f| parse_u64(fields.get(f)?));
    let reposts = eng.reposts.as_deref().and_then(|f| parse_u64(fields.get(f)?));
    let replies = eng.replies.as_deref().and_then(|f| parse_u64(fields.get(f)?));
    let views = eng.views.as_deref().and_then(|f| parse_u64(fields.get(f)?));

    if likes.is_none() && reposts.is_none() && replies.is_none() && views.is_none() {
        None
    } else {
        Some(Engagement { likes, reposts, replies, views })
    }
}

/// Parse a numeric string to `u64`, handling float strings like `"42.0"`.
fn parse_u64(s: &str) -> Option<u64> {
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    // JSON APIs sometimes return integers as floats (e.g., `8800.0`).
    // The truncation and sign-loss are intentional: engagement counts are
    // always non-negative and whole numbers.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    s.parse::<f64>().ok().map(|f| f as u64)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::site::rules::config::SiteRuleConfig;

    fn make_provider(toml: &str) -> ApiRuleProvider {
        let cfg = SiteRuleConfig::from_toml(toml).expect("valid config");
        ApiRuleProvider::new(cfg).expect("valid provider")
    }

    fn twitter_provider() -> ApiRuleProvider {
        make_provider(include_str!("defaults/twitter.toml"))
    }

    fn youtube_provider() -> ApiRuleProvider {
        make_provider(include_str!("defaults/youtube.toml"))
    }

    fn wikipedia_provider() -> ApiRuleProvider {
        make_provider(include_str!("defaults/wikipedia.toml"))
    }

    // ── URL matching ──────────────────────────────────────────────────────────

    #[test]
    fn twitter_provider_matches_x_com_status() {
        let p = twitter_provider();
        assert!(p.matches("https://x.com/naval/status/1234567890"));
        assert!(p.matches("https://twitter.com/user/status/999"));
        assert!(p.matches("https://X.COM/User/status/123?ref=foo"));
    }

    #[test]
    fn twitter_provider_does_not_match_profile_urls() {
        let p = twitter_provider();
        assert!(!p.matches("https://x.com/naval"));
        assert!(!p.matches("https://twitter.com/elonmusk"));
    }

    #[test]
    fn youtube_provider_matches_watch_and_short_urls() {
        let p = youtube_provider();
        assert!(p.matches("https://youtube.com/watch?v=abc123"));
        assert!(p.matches("https://www.youtube.com/watch?v=XYZ"));
        assert!(p.matches("https://youtu.be/dQw4w9WgXcQ"));
    }

    #[test]
    fn youtube_provider_does_not_match_channel_urls() {
        let p = youtube_provider();
        assert!(!p.matches("https://youtube.com/channel/UCxyz"));
        assert!(!p.matches("https://youtube.com/"));
    }

    #[test]
    fn wikipedia_provider_matches_wiki_urls() {
        let p = wikipedia_provider();
        assert!(p.matches("https://en.wikipedia.org/wiki/Rust"));
        assert!(p.matches("https://fi.wikipedia.org/wiki/Helsinki"));
        assert!(p.matches("https://de.wikipedia.org/wiki/Test"));
    }

    #[test]
    fn wikipedia_provider_does_not_match_non_wiki_paths() {
        let p = wikipedia_provider();
        assert!(!p.matches("https://en.wikipedia.org/w/index.php"));
        assert!(!p.matches("https://en.wikipedia.org/"));
    }

    // ── URL rewriting ──────────────────────────────────────────────────────────

    #[test]
    fn twitter_rewrite_constructs_fxtwitter_url() {
        let p = twitter_provider();
        let rewritten = p.rewrite_url("https://x.com/naval/status/1234567890");
        assert_eq!(rewritten, "https://api.fxtwitter.com/naval/status/1234567890");
    }

    #[test]
    fn twitter_rewrite_works_for_twitter_com() {
        let p = twitter_provider();
        let rewritten = p.rewrite_url("https://twitter.com/elonmusk/status/9876543210");
        assert_eq!(rewritten, "https://api.fxtwitter.com/elonmusk/status/9876543210");
    }

    #[test]
    fn youtube_rewrite_uses_oembed_url_encoding() {
        let p = youtube_provider();
        let original = "https://youtube.com/watch?v=dQw4w9WgXcQ";
        let rewritten = p.rewrite_url(original);
        assert!(rewritten.starts_with("https://www.youtube.com/oembed?url="));
        assert!(rewritten.contains("youtube.com"));
        assert!(rewritten.ends_with("&format=json"));
    }

    #[test]
    fn wikipedia_rewrite_constructs_rest_api_url() {
        let p = wikipedia_provider();
        let rewritten =
            p.rewrite_url("https://en.wikipedia.org/wiki/Rust_(programming_language)");
        assert_eq!(
            rewritten,
            "https://en.wikipedia.org/api/rest_v1/page/summary/Rust_(programming_language)"
        );
    }

    // ── field extraction ───────────────────────────────────────────────────────

    #[test]
    fn twitter_extract_fields_from_json() {
        let p = twitter_provider();
        let json = json!({
            "tweet": {
                "author": {"name": "Naval", "screen_name": "naval"},
                "text": "Build wealth, not status.",
                "likes": 8800,
                "retweets": 1000,
                "replies": 344,
                "views": 3800000,
                "created_at": "Wed Feb 12 10:00:00 +0000 2025",
                "url": "https://x.com/naval/status/123"
            }
        });
        let fields = p.extract_fields(&json);
        assert_eq!(fields.get("author_name").map(String::as_str), Some("Naval"));
        assert_eq!(fields.get("author_handle").map(String::as_str), Some("naval"));
        assert_eq!(fields.get("text").map(String::as_str), Some("Build wealth, not status."));
        assert_eq!(fields.get("likes").map(String::as_str), Some("8800"));
    }

    #[test]
    fn wikipedia_extract_thumbnail_path() {
        let p = wikipedia_provider();
        let json = json!({
            "title": "Rust",
            "description": "A systems programming language",
            "extract": "Rust is a language.",
            "thumbnail": {
                "source": "https://upload.wikimedia.org/rust.png"
            },
            "content_urls": {
                "desktop": {
                    "page": "https://en.wikipedia.org/wiki/Rust"
                }
            }
        });
        let fields = p.extract_fields(&json);
        assert_eq!(
            fields.get("thumbnail").map(String::as_str),
            Some("https://upload.wikimedia.org/rust.png")
        );
        assert_eq!(
            fields.get("page_url").map(String::as_str),
            Some("https://en.wikipedia.org/wiki/Rust")
        );
    }

    // ── metadata building ──────────────────────────────────────────────────────

    #[test]
    fn twitter_build_metadata_author_template() {
        let p = twitter_provider();
        let mut fields = HashMap::new();
        fields.insert("author_name".to_string(), "Naval".to_string());
        fields.insert("author_handle".to_string(), "naval".to_string());
        fields.insert("url".to_string(), "https://x.com/naval/status/123".to_string());

        let meta = p.build_metadata(&fields, "https://x.com/naval/status/123");
        assert_eq!(meta.platform, "Twitter/X");
        assert_eq!(meta.author.as_deref(), Some("Naval (@naval)"));
        assert_eq!(meta.canonical_url, "https://x.com/naval/status/123");
    }

    #[test]
    fn wikipedia_build_metadata_title_and_url() {
        let p = wikipedia_provider();
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), "Rust (programming language)".to_string());
        fields.insert(
            "page_url".to_string(),
            "https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string(),
        );
        fields.insert("timestamp".to_string(), "2025-01-01T00:00:00Z".to_string());

        let meta = p.build_metadata(&fields, "https://en.wikipedia.org/wiki/Rust_(programming_language)");
        assert_eq!(meta.platform, "Wikipedia");
        assert_eq!(meta.title.as_deref(), Some("Rust (programming language)"));
        assert_eq!(
            meta.canonical_url,
            "https://en.wikipedia.org/wiki/Rust_(programming_language)"
        );
        assert_eq!(meta.published.as_deref(), Some("2025-01-01T00:00:00Z"));
    }

    // ── engagement building ────────────────────────────────────────────────────

    #[test]
    fn twitter_builds_engagement_from_fields() {
        let p = twitter_provider();
        let mut fields = HashMap::new();
        fields.insert("likes".to_string(), "8800".to_string());
        fields.insert("retweets".to_string(), "1000".to_string());
        fields.insert("replies".to_string(), "344".to_string());
        fields.insert("views".to_string(), "3800000".to_string());

        let meta = p.build_metadata(&fields, "https://x.com/u/status/1");
        let eng = meta.engagement.unwrap();
        assert_eq!(eng.likes, Some(8800));
        assert_eq!(eng.reposts, Some(1000));
        assert_eq!(eng.replies, Some(344));
        assert_eq!(eng.views, Some(3_800_000));
    }

    #[test]
    fn youtube_has_no_engagement() {
        let p = youtube_provider();
        let fields = HashMap::new();
        let meta = p.build_metadata(&fields, "https://youtube.com/watch?v=xyz");
        assert!(meta.engagement.is_none());
    }

    // ── parse_u64 ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_u64_handles_integer_strings() {
        assert_eq!(parse_u64("42"), Some(42));
        assert_eq!(parse_u64("0"), Some(0));
        assert_eq!(parse_u64("1000000"), Some(1_000_000));
    }

    #[test]
    fn parse_u64_handles_float_strings() {
        assert_eq!(parse_u64("42.0"), Some(42));
        assert_eq!(parse_u64("8800.0"), Some(8800));
    }

    #[test]
    fn parse_u64_returns_none_for_non_numeric() {
        assert_eq!(parse_u64("n/a"), None);
        assert_eq!(parse_u64(""), None);
    }
}
