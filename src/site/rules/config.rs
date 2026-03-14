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
    /// Concurrent item expansion from a list endpoint.
    ///
    /// Each entry fetches a single list URL, walks an item array, and expands
    /// fields as `{prefix}_{idx}_{field}`.  Useful for multi-item pages
    /// (e.g., `HackerNews` stories, Reddit comment threads).
    #[serde(default, rename = "fetch_concurrent")]
    pub concurrent_fetches: Vec<ConcurrentFetchConfig>,
    /// Fallback strategies tried when the primary fetch returns no fields.
    ///
    /// Tried in order; the first that produces any fields wins.  This enables
    /// patterns like "try oEmbed JSON first, fall back to og:meta HTML tags".
    #[serde(default)]
    pub fallback: Vec<FallbackConfig>,
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

/// `[[fetch_concurrent]]` — list-endpoint expansion.
///
/// Fetches a single JSON list URL and expands each item in the array into
/// numbered fields: `{prefix}_{0}_{field}`, `{prefix}_{1}_{field}`, etc.
///
/// # Example (TOML)
///
/// ```toml
/// [[fetch_concurrent]]
/// prefix       = "story"
/// rewrite_from = "(?i)https?://news.ycombinator.com.*"
/// rewrite_to   = "https://hacker-news.firebaseio.com/v0/topstories.json"
/// items_path   = "."
/// max_items    = 10
///
/// [fetch_concurrent.json]
/// title = ".title"
/// url   = ".url"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct ConcurrentFetchConfig {
    /// Short prefix for expanded field names.
    pub prefix: String,
    /// Regex applied to the **original** URL to produce the list endpoint URL.
    pub rewrite_from: String,
    /// Replacement template for the list URL.
    pub rewrite_to: String,
    /// JSON dot-path to the array of items (e.g., `.items` or `.` for root array).
    pub items_path: String,
    /// JSON field extraction paths applied to each array element.
    #[serde(default)]
    pub json: JsonConfig,
    /// `Accept` header for this request.
    pub accept: Option<String>,
    /// Maximum number of items to expand (default 10).
    pub max_items: Option<usize>,
}

impl ConcurrentFetchConfig {
    /// Return the effective item limit (default 10).
    #[must_use]
    pub fn item_limit(&self) -> usize {
        self.max_items.unwrap_or(10)
    }
}

/// Extraction type for a `[[fallback]]` entry.
///
/// - `Json`: parse the response as JSON and apply `.json` dot-path selectors.
/// - `Html`: parse the response as HTML and apply `.css` CSS selectors.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FallbackType {
    /// Parse response as JSON and extract fields via dot-path selectors.
    #[default]
    Json,
    /// Parse response as HTML and extract fields via CSS selectors.
    Html,
}

impl FallbackType {
    /// Return a lowercase string representation for logging.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Html => "html",
        }
    }
}

/// `[[fallback]]` — one fallback strategy tried when the primary fetch yields
/// no fields (e.g., API returns an error or non-JSON body).
///
/// Fallbacks are tried in TOML order; the first that produces any fields wins.
/// After the fallback fields are collected, the normal template + metadata
/// pipeline runs as usual.
///
/// # Example (TOML)
///
/// ```toml
/// [[fallback]]
/// rewrite_from = ".*"
/// rewrite_to   = "{url}"   # fetch the original URL verbatim
/// type         = "html"
///
/// [fallback.css]
/// title       = "meta[property='og:title']::attr(content)"
/// description = "meta[property='og:description']::attr(content)"
/// image       = "meta[property='og:image']::attr(content)"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct FallbackConfig {
    /// Regex matched against the **original** URL to produce the fetch URL.
    pub rewrite_from: String,
    /// Replacement template (supports `$1`/`$2` captures and `{url}`
    /// placeholder for the URL-encoded original URL).
    pub rewrite_to: String,
    /// Extraction type: `"json"` (default) or `"html"`.
    #[serde(default, rename = "type")]
    pub fallback_type: FallbackType,
    /// `Accept` header for this fallback request.
    pub accept: Option<String>,
    /// JSON dot-path selectors used when `type = "json"`.
    #[serde(default)]
    pub json: JsonConfig,
    /// CSS selectors used when `type = "html"`.
    ///
    /// Values support a `::attr(name)` suffix to extract an attribute value
    /// (e.g. `"meta[property='og:title']::attr(content)"`).  Without the
    /// suffix, the element's text content is used.
    #[serde(default)]
    pub css: HashMap<String, String>,
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

/// Parsed representation of a `[request] auth = "..."` value.
///
/// Supported formats:
/// - `"env:VAR_NAME"` — reads `$VAR_NAME` and injects `Authorization: Bearer $value`
/// - `"env:VAR_NAME:header=X-Custom-Header"` — reads `$VAR_NAME` and injects
///   `X-Custom-Header: $value` verbatim
///
/// When the env var is absent the request proceeds without authentication,
/// which is correct for APIs that allow unauthenticated access to public
/// resources (e.g. GitHub API rate-limits unauthenticated requests but does
/// not block them entirely).
///
/// # Examples
///
/// ```
/// use nab::site::rules::config::AuthConfig;
///
/// // Bearer token from env var
/// let cfg = AuthConfig::parse("env:GITHUB_TOKEN").unwrap();
/// assert_eq!(cfg.env_var, "GITHUB_TOKEN");
/// assert_eq!(cfg.header_name, "Authorization");
/// assert!(cfg.bearer);
///
/// // Custom header name
/// let cfg = AuthConfig::parse("env:MY_KEY:header=X-Api-Key").unwrap();
/// assert_eq!(cfg.header_name, "X-Api-Key");
/// assert!(!cfg.bearer);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    /// Environment variable holding the credential value.
    pub env_var: String,
    /// HTTP header name to inject.  Defaults to `"Authorization"`.
    pub header_name: String,
    /// Whether to wrap the value as `"Bearer $value"`.
    ///
    /// `true` (default) → `Authorization: Bearer $value`.
    /// `false` (custom header) → `X-Header: $value` verbatim.
    pub bearer: bool,
}

impl AuthConfig {
    /// Parse an `auth` string into an [`AuthConfig`].
    ///
    /// # Errors
    ///
    /// Returns an error if the format is not `"env:VAR"` or
    /// `"env:VAR:header=NAME"`.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let rest = s
            .strip_prefix("env:")
            .ok_or_else(|| anyhow::anyhow!("auth value must start with 'env:' (got '{s}')"))?;

        match rest.split_once(':') {
            None => Ok(Self {
                env_var: rest.to_string(),
                header_name: "Authorization".to_string(),
                bearer: true,
            }),
            Some((var, suffix)) => {
                let header_name = suffix
                    .strip_prefix("header=")
                    .ok_or_else(|| {
                        anyhow::anyhow!("auth suffix must be 'header=NAME' (got '{suffix}')")
                    })?
                    .to_string();
                anyhow::ensure!(
                    !header_name.is_empty(),
                    "auth header name must not be empty"
                );
                Ok(Self {
                    env_var: var.to_string(),
                    header_name,
                    bearer: false,
                })
            }
        }
    }

    /// Resolve the auth header `(name, value)` from the environment.
    ///
    /// Returns `None` when the env var is not set (auth is optional).
    #[must_use]
    pub fn resolve(&self) -> Option<(String, String)> {
        let value = std::env::var(&self.env_var).ok()?;
        let header_value = if self.bearer {
            format!("Bearer {value}")
        } else {
            value
        };
        Some((self.header_name.clone(), header_value))
    }
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
    /// Optional auth injection from an environment variable.
    ///
    /// Supported formats:
    /// - `"env:VAR_NAME"` → `Authorization: Bearer $value`
    /// - `"env:VAR_NAME:header=X-Custom-Header"` → `X-Custom-Header: $value`
    ///
    /// When the env var is absent the request proceeds without the auth header.
    ///
    /// # TOML example
    ///
    /// ```toml
    /// [request]
    /// auth = "env:GITHUB_TOKEN"
    /// ```
    pub auth: Option<String>,
    /// Optional JSON dot-path that must resolve to a non-null value for the
    /// primary extraction to proceed.
    ///
    /// When set and the path resolves to `null` or is absent in the response,
    /// the provider treats the primary fetch as yielding no fields (triggering
    /// fallbacks if configured, or a clean bail).  Use this to detect
    /// application-level error envelopes where the API returns HTTP 200 but
    /// signals failure via a null payload field — e.g. `FxTwitter` returns
    /// `{"code":404,"tweet":null}` for deleted tweets.
    ///
    /// # TOML example
    ///
    /// ```toml
    /// [request]
    /// success_path = ".tweet"   # extraction aborts if tweet is null/missing
    /// ```
    pub success_path: Option<String>,
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
        let config: Self = toml::from_str(toml_str).context("failed to parse site rule TOML")?;
        config.validate()?;
        Ok(config)
    }

    /// Validate that required fields are present and regexes compile.
    fn validate(&self) -> Result<()> {
        if self.site.name.is_empty() {
            bail!("site.name must not be empty");
        }
        if self.site.patterns.is_empty() {
            bail!(
                "site.patterns must not be empty for rule '{}'",
                self.site.name
            );
        }
        // Validate that each pattern compiles as a regex.
        for pattern in &self.site.patterns {
            regex::Regex::new(pattern).with_context(|| {
                format!(
                    "invalid pattern regex '{}' in rule '{}'",
                    pattern, self.site.name
                )
            })?;
        }
        // Validate that the rewrite `from` regex compiles.
        regex::Regex::new(&self.rewrite.from).with_context(|| {
            format!(
                "invalid rewrite.from regex '{}' in rule '{}'",
                self.rewrite.from, self.site.name
            )
        })?;
        if self.template.format.is_empty() {
            bail!(
                "template.format must not be empty in rule '{}'",
                self.site.name
            );
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
        // Validate concurrent fetch regexes.
        for (i, cf) in self.concurrent_fetches.iter().enumerate() {
            if cf.prefix.is_empty() {
                bail!(
                    "fetch_concurrent[{i}].prefix must not be empty in rule '{}'",
                    self.site.name
                );
            }
            regex::Regex::new(&cf.rewrite_from).with_context(|| {
                format!(
                    "invalid fetch_concurrent[{i}].rewrite_from regex '{}' in rule '{}'",
                    cf.rewrite_from, self.site.name
                )
            })?;
        }
        // Validate fallback regexes.
        for (i, fb) in self.fallback.iter().enumerate() {
            regex::Regex::new(&fb.rewrite_from).with_context(|| {
                format!(
                    "invalid fallback[{i}].rewrite_from regex '{}' in rule '{}'",
                    fb.rewrite_from, self.site.name
                )
            })?;
        }
        // Validate auth string format if present.
        if let Some(auth) = &self.request.auth {
            AuthConfig::parse(auth).with_context(|| {
                format!(
                    "invalid request.auth '{}' in rule '{}'",
                    auth, self.site.name
                )
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
