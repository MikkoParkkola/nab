//! TOML deserialization types for site rule configuration.
//!
//! The schema supports `type = "api"` rules that specify:
//! - URL pattern matching
//! - URL rewriting (with regex capture groups or `{url}` placeholder)
//! - HTTP request configuration
//! - JSON field extraction via dot-path notation
//! - Markdown template rendering
//! - Metadata and engagement mapping

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

// ─────────────────────────────────────────────────────────────────────────────
// Public config types
// ─────────────────────────────────────────────────────────────────────────────

/// Top-level site rule configuration loaded from a TOML file.
#[derive(Debug, Clone, Deserialize)]
pub struct SiteRuleConfig {
    /// Site identity and URL matching.
    pub site: SiteConfig,
    /// URL rewrite rule.
    pub rewrite: RewriteConfig,
    /// HTTP request configuration.
    #[serde(default)]
    pub request: RequestConfig,
    /// JSON field extraction paths.
    pub json: JsonConfig,
    /// Additional sequential fetches merged into the main field map.
    ///
    /// Each entry specifies its own URL rewrite, optional `Accept` header,
    /// and a `json` mapping.  Extracted field names are prefixed with the
    /// entry's `prefix` value (e.g., `prefix = "ans"` turns `body` into
    /// `ans_body`) to avoid collisions with primary fields.
    #[serde(default, rename = "fetch_additional")]
    pub additional_fetches: Vec<AdditionalFetchConfig>,
    /// Markdown output template.
    pub template: TemplateConfig,
    /// Metadata field mapping.
    #[serde(default)]
    pub metadata: MetadataConfig,
    /// Engagement metrics field mapping.
    #[serde(default)]
    pub engagement: EngagementConfig,
}

/// `[[fetch_additional]]` — one additional sequential HTTP fetch.
///
/// After the primary fetch, each `fetch_additional` entry is executed in
/// order.  Fields extracted from the response are merged into the main fields
/// map under the given `prefix`.
///
/// # Example (TOML)
///
/// ```toml
/// [[fetch_additional]]
/// prefix     = "ans"
/// rewrite_from = "(?i)https?://stackoverflow\\.com/questions/(\\d+).*"
/// rewrite_to   = "https://api.stackexchange.com/2.3/questions/$1/answers?site=stackoverflow&filter=withbody&sort=votes"
/// accept     = "application/json"
///
/// [fetch_additional.json]
/// body  = ".items[0].body"
/// score = ".items[0].score"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct AdditionalFetchConfig {
    /// Short prefix prepended to every field name extracted from this fetch.
    ///
    /// A field named `body` with `prefix = "ans"` becomes `ans_body`.
    pub prefix: String,
    /// Regex applied to the **original** URL to produce the new fetch URL.
    pub rewrite_from: String,
    /// Replacement template for the URL (uses `$1`, `$2`, … capture groups).
    pub rewrite_to: String,
    /// Value for the `Accept` header on this request.
    pub accept: Option<String>,
    /// JSON field extraction paths for this response.
    #[serde(default)]
    pub json: JsonConfig,
}

/// `[site]` — name and URL patterns for a rule.
#[derive(Debug, Clone, Deserialize)]
pub struct SiteConfig {
    /// Provider name (e.g., `"twitter"`, `"youtube"`).
    pub name: String,
    /// List of regex patterns (case-insensitive) that URLs must match.
    pub patterns: Vec<String>,
}

/// `[rewrite]` — URL rewrite configuration.
///
/// Two modes:
/// 1. Capture-group rewrite: `from` is a regex with capture groups, `to` uses
///    `$1`, `$2`, … for substitution.
/// 2. oEmbed-style: `to` contains `{url}`, which is replaced with the
///    URL-encoded original URL.
#[derive(Debug, Clone, Deserialize)]
pub struct RewriteConfig {
    /// Regex to match against the original URL.
    pub from: String,
    /// Replacement template.  Use `$1`/`$2` for capture groups or `{url}` for
    /// the URL-encoded original URL.
    pub to: String,
}

/// HTTP client selection for `[request] client`.
///
/// - `Default` (or omitted): use the shared [`AcceleratedClient`] which forces
///   HTTP/2 via `http2_prior_knowledge`.  Works for most modern APIs.
/// - `Standard`: build a fresh `reqwest::Client` that negotiates HTTP version
///   via TLS ALPN.  Required for servers that return unexpected content (e.g.
///   HTML instead of JSON) when forced to HTTP/2 without ALPN, such as Reddit.
///
/// [`AcceleratedClient`]: crate::http_client::AcceleratedClient
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    /// Use the shared `AcceleratedClient` (HTTP/2 prior knowledge).  Default.
    #[default]
    Default,
    /// Use a plain `reqwest::Client` with ALPN protocol negotiation.
    Standard,
}

/// `[request]` — HTTP request options.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RequestConfig {
    /// HTTP client to use for this rule.
    ///
    /// `"standard"` builds a fresh `reqwest::Client` that negotiates the HTTP
    /// version via TLS ALPN, which is required for servers that misbehave when
    /// forced to HTTP/2 (e.g. Reddit).  Omitting this field (or `"default"`)
    /// uses the shared `AcceleratedClient`.
    #[serde(default)]
    pub client: ClientKind,
    /// Extra request headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Value for the `Accept` header (convenience shorthand).
    pub accept: Option<String>,
}

/// `[json]` — mapping of logical field names to JSON dot-path selectors.
///
/// Keys are user-defined field names referenced in templates.
/// Values are dot-path expressions like `.tweet.author.name`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct JsonConfig(pub HashMap<String, String>);

/// `[template]` — Markdown output template.
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateConfig {
    /// Handlebars-like template string.  `{field}` is replaced with extracted
    /// values; `{field|number}` applies K/M formatting.  Lines containing
    /// unresolved placeholders are omitted.
    pub format: String,
}

/// `[metadata]` — mapping extracted fields onto [`SiteMetadata`] fields.
///
/// Values are either a plain field name (`"title"`) or a template string
/// interpolated with extracted fields (`"{author_name} (@{author_handle})"`).
///
/// [`SiteMetadata`]: crate::site::SiteMetadata
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MetadataConfig {
    /// Platform label (e.g., `"Twitter/X"`).
    #[serde(default)]
    pub platform: String,
    /// Template for the author string.
    pub author: Option<String>,
    /// Field name whose value becomes the title.
    pub title_field: Option<String>,
    /// Field name whose value becomes the publication date.
    pub published_field: Option<String>,
    /// Field name whose value becomes the canonical URL.
    pub canonical_url_field: Option<String>,
    /// Field name whose value becomes the primary media URL.
    pub media_urls_field: Option<String>,
    /// Catch-all for provider-specific extra fields (e.g., `author_field`).
    #[serde(flatten)]
    pub extra: HashMap<String, String>,
}

/// `[engagement]` — maps engagement metric names to extracted field names.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct EngagementConfig {
    /// Field name for likes.
    pub likes: Option<String>,
    /// Field name for reposts/retweets.
    pub reposts: Option<String>,
    /// Field name for replies.
    pub replies: Option<String>,
    /// Field name for views.
    pub views: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Parsing & validation
// ─────────────────────────────────────────────────────────────────────────────

impl SiteRuleConfig {
    /// Parse and validate a TOML string into a [`SiteRuleConfig`].
    ///
    /// # Errors
    ///
    /// Returns an error if the TOML is malformed, required fields are missing,
    /// or patterns/regexes fail to compile.
    pub fn from_toml(toml_str: &str) -> Result<Self> {
        let config: Self =
            toml::from_str(toml_str).context("failed to parse site rule TOML")?;
        config.validate()?;
        Ok(config)
    }

    /// Validate that required fields are present and regexes compile.
    fn validate(&self) -> Result<()> {
        if self.site.name.is_empty() {
            bail!("site.name must not be empty");
        }
        if self.site.patterns.is_empty() {
            bail!("site.patterns must not be empty for rule '{}'", self.site.name);
        }
        // Validate that each pattern compiles as a regex.
        for pattern in &self.site.patterns {
            regex::Regex::new(pattern)
                .with_context(|| format!("invalid pattern regex '{}' in rule '{}'", pattern, self.site.name))?;
        }
        // Validate that the rewrite `from` regex compiles.
        regex::Regex::new(&self.rewrite.from)
            .with_context(|| format!("invalid rewrite.from regex '{}' in rule '{}'", self.rewrite.from, self.site.name))?;
        if self.template.format.is_empty() {
            bail!("template.format must not be empty in rule '{}'", self.site.name);
        }
        // Validate additional fetch regexes.
        for (i, af) in self.additional_fetches.iter().enumerate() {
            if af.prefix.is_empty() {
                bail!(
                    "fetch_additional[{i}].prefix must not be empty in rule '{}'",
                    self.site.name
                );
            }
            regex::Regex::new(&af.rewrite_from).with_context(|| {
                format!(
                    "invalid fetch_additional[{i}].rewrite_from regex '{}' in rule '{}'",
                    af.rewrite_from, self.site.name
                )
            })?;
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn twitter_toml() -> &'static str {
        include_str!("defaults/twitter.toml")
    }

    fn youtube_toml() -> &'static str {
        include_str!("defaults/youtube.toml")
    }

    fn wikipedia_toml() -> &'static str {
        include_str!("defaults/wikipedia.toml")
    }

    #[test]
    fn parse_twitter_toml_succeeds() {
        let cfg = SiteRuleConfig::from_toml(twitter_toml()).unwrap();
        assert_eq!(cfg.site.name, "twitter");
        assert_eq!(cfg.site.patterns.len(), 1);
        assert!(cfg.site.patterns[0].contains("status"));
    }

    #[test]
    fn parse_youtube_toml_succeeds() {
        let cfg = SiteRuleConfig::from_toml(youtube_toml()).unwrap();
        assert_eq!(cfg.site.name, "youtube");
        assert_eq!(cfg.site.patterns.len(), 2);
        assert_eq!(cfg.rewrite.to, "https://www.youtube.com/oembed?url={url}&format=json");
    }

    #[test]
    fn parse_wikipedia_toml_succeeds() {
        let cfg = SiteRuleConfig::from_toml(wikipedia_toml()).unwrap();
        assert_eq!(cfg.site.name, "wikipedia");
        assert!(cfg.request.headers.contains_key("User-Agent"));
        assert_eq!(cfg.metadata.platform, "Wikipedia");
    }

    #[test]
    fn parse_twitter_json_fields_all_present() {
        let cfg = SiteRuleConfig::from_toml(twitter_toml()).unwrap();
        let fields = &cfg.json.0;
        assert_eq!(fields.get("author_name").map(String::as_str), Some(".tweet.author.name"));
        assert_eq!(fields.get("text").map(String::as_str), Some(".tweet.text"));
        assert_eq!(fields.get("likes").map(String::as_str), Some(".tweet.likes"));
    }

    #[test]
    fn parse_twitter_engagement_fields() {
        let cfg = SiteRuleConfig::from_toml(twitter_toml()).unwrap();
        assert_eq!(cfg.engagement.likes.as_deref(), Some("likes"));
        assert_eq!(cfg.engagement.reposts.as_deref(), Some("retweets"));
        assert_eq!(cfg.engagement.replies.as_deref(), Some("replies"));
        assert_eq!(cfg.engagement.views.as_deref(), Some("views"));
    }

    #[test]
    fn validate_rejects_empty_name() {
        let toml_str = r#"
[site]
name = ""
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "{title}"
"#;
        let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn validate_rejects_empty_patterns() {
        let toml_str = r#"
[site]
name = "test"
patterns = []

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "{title}"
"#;
        let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
        assert!(err.to_string().contains("patterns"));
    }

    #[test]
    fn validate_rejects_invalid_pattern_regex() {
        let toml_str = r#"
[site]
name = "bad"
patterns = ["[invalid"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "{title}"
"#;
        let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
        assert!(err.to_string().contains("pattern") || err.to_string().contains("regex") || err.to_string().contains("invalid"));
    }

    #[test]
    fn validate_rejects_invalid_rewrite_from_regex() {
        let toml_str = r#"
[site]
name = "bad"
patterns = ["foo\\.com"]

[rewrite]
from = "[invalid"
to = "https://api.example.com"

[json]

[template]
format = "{title}"
"#;
        let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
        assert!(err.to_string().contains("rewrite") || err.to_string().contains("invalid"));
    }

    #[test]
    fn validate_rejects_empty_template_format() {
        let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = ""
"#;
        let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
        assert!(err.to_string().contains("template"));
    }

    #[test]
    fn request_config_defaults_to_empty() {
        let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"
"#;
        let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
        assert!(cfg.request.headers.is_empty());
        assert!(cfg.request.accept.is_none());
    }

    #[test]
    fn request_config_client_defaults_to_default_variant() {
        let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"
"#;
        let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
        assert_eq!(cfg.request.client, ClientKind::Default);
    }

    #[test]
    fn request_config_client_parses_standard_variant() {
        let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[request]
client = "standard"

[json]

[template]
format = "hello"
"#;
        let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
        assert_eq!(cfg.request.client, ClientKind::Standard);
    }

    #[test]
    fn request_config_client_parses_default_explicit() {
        let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[request]
client = "default"

[json]

[template]
format = "hello"
"#;
        let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
        assert_eq!(cfg.request.client, ClientKind::Default);
    }

    #[test]
    fn parse_reddit_toml_succeeds() {
        let cfg = SiteRuleConfig::from_toml(include_str!("defaults/reddit.toml")).unwrap();
        assert_eq!(cfg.site.name, "reddit");
        assert_eq!(cfg.request.client, ClientKind::Standard);
        assert!(cfg.request.headers.contains_key("User-Agent"));
        // Verify key JSON paths are present
        let fields = &cfg.json.0;
        assert!(fields.contains_key("title"));
        assert!(fields.contains_key("author"));
        assert!(fields.contains_key("score"));
        assert!(fields.contains_key("comments"));
    }

    #[test]
    fn additional_fetches_default_to_empty() {
        let toml_str = r#"
[site]
name = "test"
patterns = ["foo\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"
"#;
        let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
        assert!(cfg.additional_fetches.is_empty());
    }

    #[test]
    fn parse_additional_fetch_with_json_fields() {
        let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com/q/(\\d+)"]

[rewrite]
from = "(?i)https?://example\\.com/q/(\\d+)"
to = "https://api.example.com/questions/$1"

[json]
title = ".items[0].title"

[template]
format = "{title}"

[[fetch_additional]]
prefix = "ans"
rewrite_from = "(?i)https?://example\\.com/q/(\\d+)"
rewrite_to = "https://api.example.com/questions/$1/answers"
accept = "application/json"

[fetch_additional.json]
body  = ".items[0].body"
score = ".items[0].score"
"#;
        let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
        assert_eq!(cfg.additional_fetches.len(), 1);
        let af = &cfg.additional_fetches[0];
        assert_eq!(af.prefix, "ans");
        assert_eq!(
            af.rewrite_to,
            "https://api.example.com/questions/$1/answers"
        );
        assert_eq!(af.accept.as_deref(), Some("application/json"));
        assert_eq!(af.json.0.get("body").map(String::as_str), Some(".items[0].body"));
        assert_eq!(af.json.0.get("score").map(String::as_str), Some(".items[0].score"));
    }

    #[test]
    fn parse_multiple_additional_fetches() {
        let toml_str = r#"
[site]
name = "multi"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com/primary"

[json]
title = ".title"

[template]
format = "{title}"

[[fetch_additional]]
prefix = "first"
rewrite_from = ".*"
rewrite_to = "https://api.example.com/first"

[fetch_additional.json]
x = ".x"

[[fetch_additional]]
prefix = "second"
rewrite_from = ".*"
rewrite_to = "https://api.example.com/second"

[fetch_additional.json]
y = ".y"
"#;
        let cfg = SiteRuleConfig::from_toml(toml_str).unwrap();
        assert_eq!(cfg.additional_fetches.len(), 2);
        assert_eq!(cfg.additional_fetches[0].prefix, "first");
        assert_eq!(cfg.additional_fetches[1].prefix, "second");
    }

    #[test]
    fn validate_rejects_empty_additional_fetch_prefix() {
        let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"

[[fetch_additional]]
prefix = ""
rewrite_from = ".*"
rewrite_to = "https://api.example.com/extra"
"#;
        let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
        assert!(err.to_string().contains("prefix"));
    }

    #[test]
    fn validate_rejects_invalid_additional_fetch_regex() {
        let toml_str = r#"
[site]
name = "test"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[json]

[template]
format = "hello"

[[fetch_additional]]
prefix = "ans"
rewrite_from = "[invalid"
rewrite_to = "https://api.example.com/extra"
"#;
        let err = SiteRuleConfig::from_toml(toml_str).unwrap_err();
        assert!(err.to_string().contains("fetch_additional") || err.to_string().contains("invalid"));
    }

    fn stackoverflow_toml() -> &'static str {
        include_str!("defaults/stackoverflow.toml")
    }

    #[test]
    fn parse_stackoverflow_toml_succeeds() {
        let cfg = SiteRuleConfig::from_toml(stackoverflow_toml()).unwrap();
        assert_eq!(cfg.site.name, "stackoverflow");
        assert_eq!(cfg.additional_fetches.len(), 1);
        let af = &cfg.additional_fetches[0];
        assert_eq!(af.prefix, "ans");
        assert!(af.json.0.contains_key("body"));
        assert!(af.json.0.contains_key("score"));
    }
}
