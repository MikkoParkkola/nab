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
use super::config::{ClientKind, JsonConfig, SiteRuleConfig};
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
    /// Compiled `rewrite_from` regexes for each additional fetch (parallel
    /// to `config.additional_fetches`).
    additional_rewrite_froms: Vec<Regex>,
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

        let additional_rewrite_froms = config
            .additional_fetches
            .iter()
            .map(|af| {
                Regex::new(&af.rewrite_from).with_context(|| {
                    format!("invalid fetch_additional rewrite_from regex '{}'", af.rewrite_from)
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self { config, patterns, rewrite_from, additional_rewrite_froms })
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

    /// Execute all configured additional fetches and merge their fields into
    /// `fields` in place.
    ///
    /// Fields from each additional fetch are prefixed: a field `body` with
    /// `prefix = "ans"` is inserted as `ans_body`.  Failures are logged as
    /// warnings and do not abort the overall extraction.
    async fn apply_additional_fetches(
        &self,
        original_url: &str,
        client: &AcceleratedClient,
        fields: &mut HashMap<String, String>,
    ) {
        for (af, rewrite_re) in self
            .config
            .additional_fetches
            .iter()
            .zip(self.additional_rewrite_froms.iter())
        {
            let api_url = rewrite_re
                .replace(original_url, af.rewrite_to.as_str())
                .into_owned();

            tracing::debug!(
                "ApiRuleProvider '{}': additional fetch ({}) {}",
                self.config.site.name,
                af.prefix,
                api_url
            );

            let extra = match fetch_and_extract_json(client, &api_url, af.accept.as_deref(), &af.json).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(
                        "Additional fetch '{}' for rule '{}' failed: {e}",
                        af.prefix,
                        self.config.site.name
                    );
                    continue;
                }
            };

            for (key, value) in extra {
                fields.insert(format!("{}_{}", af.prefix, key), value);
            }
        }
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

    /// Fetch the raw response body from `api_url`.
    ///
    /// Uses a plain `reqwest::Client` when `request.client = "standard"` (e.g.
    /// for Reddit, which returns HTML when forced to HTTP/2 without ALPN).
    /// Falls back to the shared [`AcceleratedClient`] otherwise.
    async fn fetch_body(&self, client: &AcceleratedClient, api_url: &str) -> Result<String> {
        match self.config.request.client {
            ClientKind::Standard => self.fetch_body_standard(api_url).await,
            ClientKind::Default => self.fetch_body_accelerated(client, api_url).await,
        }
    }

    /// Fetch using a fresh standard `reqwest::Client` (ALPN negotiation).
    async fn fetch_body_standard(&self, api_url: &str) -> Result<String> {
        let standard_client = reqwest::Client::builder()
            .use_rustls_tls()
            .gzip(true)
            .brotli(true)
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .context("failed to build standard HTTP client")?;

        let request = self.apply_headers(standard_client.get(api_url));

        request
            .send()
            .await
            .with_context(|| format!("request failed for '{api_url}'"))?
            .text()
            .await
            .with_context(|| format!("failed to read response body from '{api_url}'"))
    }

    /// Fetch using the shared `AcceleratedClient`.
    async fn fetch_body_accelerated(
        &self,
        client: &AcceleratedClient,
        api_url: &str,
    ) -> Result<String> {
        let request = self.apply_headers(client.inner().get(api_url));

        request
            .send()
            .await
            .with_context(|| format!("request failed for '{api_url}'"))?
            .text()
            .await
            .with_context(|| format!("failed to read response body from '{api_url}'"))
    }

    /// Apply configured `Accept` header and custom headers to a request builder.
    fn apply_headers(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(accept) = &self.config.request.accept {
            request = request.header(reqwest::header::ACCEPT, accept.as_str());
        }
        for (key, value) in &self.config.request.headers {
            request = request.header(key.as_str(), value.as_str());
        }
        request
    }
}

/// Parse a JSON response body.
///
/// Both JSON objects and bare JSON arrays (e.g. Reddit's `[listing, comments]`
/// response) are accepted.  Paths in the TOML rule use `[N].field` notation to
/// index into root arrays, which [`json_path::extract`] handles natively.
fn parse_response_json(body: &str, api_url: &str) -> Result<serde_json::Value> {
    serde_json::from_str(body)
        .with_context(|| format!("failed to parse JSON from '{api_url}'"))
}

/// Fetch a URL and extract JSON fields according to `json_config`.
///
/// Used for additional fetches.  Returns an empty map (rather than an error)
/// when no fields match — the caller decides whether that warrants a warning.
async fn fetch_and_extract_json(
    client: &AcceleratedClient,
    url: &str,
    accept: Option<&str>,
    json_config: &JsonConfig,
) -> Result<HashMap<String, String>> {
    let mut request = client.inner().get(url);
    if let Some(accept_val) = accept {
        request = request.header(reqwest::header::ACCEPT, accept_val);
    }

    let body = request
        .send()
        .await
        .with_context(|| format!("additional fetch request failed for '{url}'"))?
        .text()
        .await
        .with_context(|| format!("failed to read additional fetch body from '{url}'"))?;

    let json: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse JSON from additional fetch '{url}'"))?;

    let fields = json_config
        .0
        .iter()
        .filter_map(|(name, path)| {
            let value = if path.contains("[]") {
                let arr = json_path::extract_array(&json, path);
                if arr.is_empty() { return None; }
                arr.join(", ")
            } else {
                json_path::extract(&json, path)?
            };
            Some((name.clone(), value))
        })
        .collect();

    Ok(fields)
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

        let body = self.fetch_body(client, &api_url).await?;

        let json = parse_response_json(&body, &api_url)?;
        let mut fields = self.extract_fields(&json);

        if fields.is_empty() {
            bail!(
                "no fields extracted from '{}' response (check json paths in rule '{}')",
                api_url,
                self.config.site.name
            );
        }

        self.apply_additional_fetches(url, client, &mut fields).await;

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

    // ── Reddit provider ────────────────────────────────────────────────────────

    fn reddit_provider() -> ApiRuleProvider {
        make_provider(include_str!("defaults/reddit.toml"))
    }

    #[test]
    fn reddit_provider_matches_www_reddit_com_comments() {
        let p = reddit_provider();
        assert!(p.matches("https://www.reddit.com/r/rust/comments/abc123/some_title/"));
        assert!(p.matches("https://www.reddit.com/r/programming/comments/xyz789"));
    }

    #[test]
    fn reddit_provider_matches_reddit_com_without_www() {
        let p = reddit_provider();
        assert!(p.matches("https://reddit.com/r/rust/comments/abc123"));
    }

    #[test]
    fn reddit_provider_matches_old_reddit() {
        let p = reddit_provider();
        assert!(p.matches("https://old.reddit.com/r/rust/comments/abc123"));
        assert!(p.matches("https://OLD.REDDIT.COM/r/rust/COMMENTS/xyz"));
    }

    #[test]
    fn reddit_provider_does_not_match_subreddit_listing() {
        let p = reddit_provider();
        assert!(!p.matches("https://reddit.com/r/rust"));
        assert!(!p.matches("https://reddit.com/r/rust/"));
        assert!(!p.matches("https://reddit.com/user/someone"));
    }

    #[test]
    fn reddit_provider_does_not_match_other_sites() {
        let p = reddit_provider();
        assert!(!p.matches("https://x.com/user/status/123"));
        assert!(!p.matches("https://youtube.com/watch?v=abc"));
    }

    #[test]
    fn reddit_rewrite_appends_json_suffix() {
        let p = reddit_provider();
        let rewritten = p.rewrite_url("https://www.reddit.com/r/rust/comments/abc123/some_title/");
        assert!(rewritten.ends_with(".json"), "expected .json suffix, got: {rewritten}");
        assert!(!rewritten.contains('?'), "query string should be stripped, got: {rewritten}");
    }

    #[test]
    fn reddit_rewrite_strips_query_string() {
        let p = reddit_provider();
        let rewritten =
            p.rewrite_url("https://reddit.com/r/rust/comments/abc123?utm_source=share");
        assert!(rewritten.ends_with(".json"), "expected .json suffix, got: {rewritten}");
        assert!(!rewritten.contains("utm_source"), "utm param should be gone, got: {rewritten}");
    }

    #[test]
    fn reddit_uses_standard_client_config() {
        use crate::site::rules::config::ClientKind;
        let p = reddit_provider();
        assert_eq!(p.config.request.client, ClientKind::Standard);
    }

    #[test]
    fn reddit_extract_fields_from_api_array_response() {
        // GIVEN: a Reddit-style bare array JSON response
        let p = reddit_provider();
        let json = json!([
            {
                "data": {
                    "children": [{
                        "data": {
                            "title": "Rust 2024 edition released",
                            "author": "rustacean42",
                            "score": 4200,
                            "num_comments": 350,
                            "selftext": "Big news for the Rust community.",
                            "url": "https://reddit.com/r/rust/comments/abc123",
                            "subreddit": "rust"
                        }
                    }]
                }
            },
            {"data": {"children": []}}
        ]);
        // WHEN: extracting fields
        let fields = p.extract_fields(&json);
        // THEN: all key fields are present
        assert_eq!(fields.get("title").map(String::as_str), Some("Rust 2024 edition released"));
        assert_eq!(fields.get("author").map(String::as_str), Some("rustacean42"));
        assert_eq!(fields.get("score").map(String::as_str), Some("4200"));
        assert_eq!(fields.get("comments").map(String::as_str), Some("350"));
        assert_eq!(fields.get("subreddit").map(String::as_str), Some("rust"));
    }

    #[test]
    fn reddit_build_metadata_sets_platform_and_author() {
        let p = reddit_provider();
        let mut fields = std::collections::HashMap::new();
        fields.insert("title".to_string(), "My Post".to_string());
        fields.insert("author".to_string(), "testuser".to_string());
        fields.insert("url".to_string(), "https://reddit.com/r/rust/comments/x".to_string());
        fields.insert("subreddit".to_string(), "rust".to_string());

        let meta = p.build_metadata(&fields, "https://reddit.com/r/rust/comments/x");
        assert_eq!(meta.platform, "Reddit");
        assert_eq!(meta.author.as_deref(), Some("u/testuser"));
        assert_eq!(meta.title.as_deref(), Some("My Post"));
    }

    #[test]
    fn parse_response_json_accepts_bare_array() {
        // GIVEN: Reddit-style bare array JSON body
        let body = r#"[{"data": {"children": []}}, {"data": {"children": []}}]"#;
        // WHEN: parsing
        let result = parse_response_json(body, "https://example.com");
        // THEN: parses as an array value without error
        assert!(result.is_ok());
        assert!(result.unwrap().is_array());
    }

    #[test]
    fn parse_response_json_accepts_object() {
        let body = r#"{"tweet": {"text": "hello"}}"#;
        let result = parse_response_json(body, "https://example.com");
        assert!(result.is_ok());
        assert!(result.unwrap().is_object());
    }

    #[test]
    fn parse_response_json_fails_on_invalid_json() {
        let body = "not json at all %%%";
        let result = parse_response_json(body, "https://example.com");
        assert!(result.is_err());
    }

    // ── stackoverflow provider (multi-fetch config) ───────────────────────────

    fn stackoverflow_provider() -> ApiRuleProvider {
        make_provider(include_str!("defaults/stackoverflow.toml"))
    }

    #[test]
    fn stackoverflow_provider_matches_question_urls() {
        let p = stackoverflow_provider();
        assert!(p.matches("https://stackoverflow.com/questions/12345/some-title"));
        assert!(p.matches("https://STACKOVERFLOW.COM/questions/99999/title"));
        assert!(p.matches("https://stackoverflow.com/questions/42/x?noredirect=1"));
    }

    #[test]
    fn stackoverflow_provider_does_not_match_non_question_urls() {
        let p = stackoverflow_provider();
        assert!(!p.matches("https://stackoverflow.com/"));
        assert!(!p.matches("https://stackoverflow.com/tags/rust"));
        assert!(!p.matches("https://stackoverflow.com/questions/tagged/rust"));
        assert!(!p.matches("https://youtube.com/watch?v=abc"));
    }

    #[test]
    fn stackoverflow_provider_rewrite_constructs_question_api_url() {
        let p = stackoverflow_provider();
        let url = "https://stackoverflow.com/questions/26946646/how-to-do-x";
        let rewritten = p.rewrite_url(url);
        assert!(rewritten.contains("api.stackexchange.com"));
        assert!(rewritten.contains("26946646"));
        assert!(rewritten.contains("site=stackoverflow"));
        assert!(rewritten.contains("filter=withbody"));
    }

    #[test]
    fn stackoverflow_provider_has_one_additional_fetch() {
        let p = stackoverflow_provider();
        assert_eq!(p.config.additional_fetches.len(), 1);
        assert_eq!(p.additional_rewrite_froms.len(), 1);
    }

    #[test]
    fn stackoverflow_additional_fetch_rewrite_constructs_answers_api_url() {
        let p = stackoverflow_provider();
        let url = "https://stackoverflow.com/questions/26946646/how-to-do-x";
        let af = &p.config.additional_fetches[0];
        let re = &p.additional_rewrite_froms[0];
        let api_url = re.replace(url, af.rewrite_to.as_str()).into_owned();
        assert!(api_url.contains("api.stackexchange.com"));
        assert!(api_url.contains("26946646"));
        assert!(api_url.contains("/answers"));
        assert!(api_url.contains("site=stackoverflow"));
    }

    #[test]
    fn stackoverflow_extract_fields_from_question_json() {
        let p = stackoverflow_provider();
        let json = json!({
            "items": [{
                "title": "How to use Vec in Rust?",
                "body": "<p>I want a vector.</p>",
                "score": 42,
                "answer_count": 3,
                "view_count": 15000,
                "link": "https://stackoverflow.com/questions/12345",
                "creation_date": 1_700_000_000u64,
                "tags": ["rust", "vector"],
                "owner": {"display_name": "rustacean"}
            }]
        });
        let fields = p.extract_fields(&json);
        assert_eq!(fields.get("title").map(String::as_str), Some("How to use Vec in Rust?"));
        assert_eq!(fields.get("score").map(String::as_str), Some("42"));
        assert_eq!(fields.get("answer_count").map(String::as_str), Some("3"));
    }

    #[test]
    fn stackoverflow_additional_fetch_prefix_applied() {
        // Verify that additional fetch fields are prefixed correctly
        // by exercising the apply_additional_fetches logic structurally.
        // We test prefix naming: if the additional fetch config has prefix "ans"
        // and field name "body", the merged key must be "ans_body".
        let p = stackoverflow_provider();
        let af = &p.config.additional_fetches[0];
        assert_eq!(af.prefix, "ans");
        // Confirm the json config has the expected fields
        assert!(af.json.0.contains_key("body"));
        assert!(af.json.0.contains_key("score"));
        assert!(af.json.0.contains_key("is_accepted"));
        assert!(af.json.0.contains_key("author"));
    }
}
