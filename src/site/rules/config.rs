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
    /// Markdown output template.
    pub template: TemplateConfig,
    /// Metadata field mapping.
    #[serde(default)]
    pub metadata: MetadataConfig,
    /// Engagement metrics field mapping.
    #[serde(default)]
    pub engagement: EngagementConfig,
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

/// `[request]` — HTTP request options.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RequestConfig {
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
}
