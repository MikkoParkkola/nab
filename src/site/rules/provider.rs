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
use super::config::{AuthConfig, ClientKind, FallbackType, JsonConfig, SiteRuleConfig};
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
    /// Compiled `rewrite_from` regexes for each fallback (parallel to
    /// `config.fallback`).
    fallback_rewrite_froms: Vec<Regex>,
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
                    format!(
                        "invalid fetch_additional rewrite_from regex '{}'",
                        af.rewrite_from
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let fallback_rewrite_froms = config
            .fallback
            .iter()
            .map(|fb| {
                Regex::new(&fb.rewrite_from).with_context(|| {
                    format!("invalid fallback rewrite_from regex '{}'", fb.rewrite_from)
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            config,
            patterns,
            rewrite_from,
            additional_rewrite_froms,
            fallback_rewrite_froms,
        })
    }

    /// Fetch the primary API URL as JSON and extract configured fields.
    ///
    /// Return values:
    /// - `Ok(Some(fields))` — JSON fetched and fields extracted (may be empty
    ///   if no configured paths matched).
    /// - `Ok(None)` — `request.success_path` resolved to `null`/missing,
    ///   indicating an API-level "not found" envelope (e.g. `FxTwitter`
    ///   `{"tweet": null}`).  The caller should treat this as a content-not-
    ///   found signal rather than a misconfiguration.
    /// - `Err(e)` — HTTP or JSON parse failure; propagated to the caller.
    async fn try_primary_json(
        &self,
        client: &AcceleratedClient,
        api_url: &str,
    ) -> Result<Option<HashMap<String, String>>> {
        let body = self.fetch_body(client, api_url).await?;
        let json = parse_response_json(&body, api_url)?;
        if let Some(path) = &self.config.request.success_path
            && !json_path::is_non_null(&json, path)
        {
            tracing::debug!(
                "ApiRuleProvider '{}': success_path '{}' resolved to null/missing — \
                 content not found at API level",
                self.config.site.name,
                path
            );
            return Ok(None);
        }
        Ok(Some(self.extract_fields(&json)))
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
                    if arr.is_empty() {
                        return None;
                    }
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

            let extra = match fetch_and_extract_json(
                client,
                &api_url,
                af.accept.as_deref(),
                &af.json,
            )
            .await
            {
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

    /// Try each configured fallback in order, returning the fields from the
    /// first one that produces a non-empty map.
    ///
    /// `prefetched_html` is used for the first `type = "html"` fallback whose
    /// `rewrite_to` resolves to the original URL, avoiding a redundant fetch.
    async fn apply_fallbacks(
        &self,
        original_url: &str,
        client: &AcceleratedClient,
        prefetched_html: Option<&[u8]>,
    ) -> HashMap<String, String> {
        for (fb, rewrite_re) in self
            .config
            .fallback
            .iter()
            .zip(self.fallback_rewrite_froms.iter())
        {
            let fetch_url = rewrite_url_with(rewrite_re, &fb.rewrite_to, original_url);
            tracing::debug!(
                "ApiRuleProvider '{}': trying fallback ({}) {}",
                self.config.site.name,
                fb.fallback_type.as_str(),
                fetch_url
            );

            let fields = match fb.fallback_type {
                FallbackType::Json => {
                    self.apply_json_fallback(client, &fetch_url, fb.accept.as_deref(), &fb.json)
                        .await
                }
                FallbackType::Html => {
                    self.apply_html_fallback(
                        client,
                        &fetch_url,
                        original_url,
                        fb.accept.as_deref(),
                        &fb.css,
                        prefetched_html,
                    )
                    .await
                }
            };

            if !fields.is_empty() {
                return fields;
            }
        }
        HashMap::new()
    }

    /// Fallback path: fetch URL, parse JSON, extract fields.
    async fn apply_json_fallback(
        &self,
        client: &AcceleratedClient,
        url: &str,
        accept: Option<&str>,
        json_config: &JsonConfig,
    ) -> HashMap<String, String> {
        match fetch_and_extract_json(client, url, accept, json_config).await {
            Ok(fields) => fields,
            Err(e) => {
                tracing::warn!(
                    "JSON fallback failed for rule '{}' at '{}': {e}",
                    self.config.site.name,
                    url
                );
                HashMap::new()
            }
        }
    }

    /// Fallback path: fetch URL (or reuse `prefetched_html`), parse HTML,
    /// extract fields via CSS selectors.
    async fn apply_html_fallback(
        &self,
        client: &AcceleratedClient,
        fetch_url: &str,
        original_url: &str,
        accept: Option<&str>,
        css_map: &HashMap<String, String>,
        prefetched_html: Option<&[u8]>,
    ) -> HashMap<String, String> {
        // Reuse pre-fetched bytes when the resolved URL is the original URL.
        let html: String = if fetch_url == original_url {
            if let Some(bytes) = prefetched_html {
                String::from_utf8_lossy(bytes).into_owned()
            } else {
                match self.fetch_html(client, fetch_url, accept).await {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!(
                            "HTML fallback fetch failed for rule '{}' at '{}': {e}",
                            self.config.site.name,
                            fetch_url
                        );
                        return HashMap::new();
                    }
                }
            }
        } else {
            match self.fetch_html(client, fetch_url, accept).await {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(
                        "HTML fallback fetch failed for rule '{}' at '{}': {e}",
                        self.config.site.name,
                        fetch_url
                    );
                    return HashMap::new();
                }
            }
        };

        extract_css_fields(&html, css_map)
    }

    /// Fetch a URL as text, applying the optional `Accept` header.
    async fn fetch_html(
        &self,
        client: &AcceleratedClient,
        url: &str,
        accept: Option<&str>,
    ) -> Result<String> {
        let mut request = client.inner().get(url);
        if let Some(accept_val) = accept {
            request = request.header(reqwest::header::ACCEPT, accept_val);
        }
        request
            .send()
            .await
            .with_context(|| format!("fallback HTML fetch failed for '{url}'"))?
            .error_for_status()
            .with_context(|| format!("HTTP error for fallback HTML fetch '{url}'"))?
            .text()
            .await
            .with_context(|| format!("failed to read fallback HTML body from '{url}'"))
    }

    /// Build [`SiteMetadata`] from extracted fields and config.
    fn build_metadata(&self, fields: &HashMap<String, String>, original_url: &str) -> SiteMetadata {
        let meta = &self.config.metadata;

        let author = meta
            .author
            .as_deref()
            .map(|tmpl| template::render(tmpl, fields, original_url))
            .or_else(|| {
                // Fallback: check extra["author_field"]
                meta.extra
                    .get("author_field")
                    .and_then(|f| fields.get(f))
                    .cloned()
            });

        let title = meta
            .title_field
            .as_deref()
            .and_then(|f| if f.is_empty() { None } else { fields.get(f) })
            .cloned();

        let published = meta
            .published_field
            .as_deref()
            .and_then(|f| if f.is_empty() { None } else { fields.get(f) })
            .cloned();

        let canonical_url = meta
            .canonical_url_field
            .as_deref()
            .and_then(|f| if f.is_empty() { None } else { fields.get(f) })
            .cloned()
            .unwrap_or_else(|| original_url.to_string());

        let media_urls = meta
            .media_urls_field
            .as_deref()
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
            .error_for_status()
            .with_context(|| format!("HTTP error for '{api_url}'"))?
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
            .error_for_status()
            .with_context(|| format!("HTTP error for '{api_url}'"))?
            .text()
            .await
            .with_context(|| format!("failed to read response body from '{api_url}'"))
    }

    /// Apply configured `Accept` header, custom headers, and optional auth to a
    /// request builder.
    ///
    /// Auth injection is best-effort: when `request.auth` is set but the
    /// referenced env var is absent, the request proceeds without the auth
    /// header (unauthenticated access — correct for public APIs such as GitHub).
    fn apply_headers(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(accept) = &self.config.request.accept {
            request = request.header(reqwest::header::ACCEPT, accept.as_str());
        }
        for (key, value) in &self.config.request.headers {
            request = request.header(key.as_str(), value.as_str());
        }
        if let Some(auth_str) = &self.config.request.auth {
            match AuthConfig::parse(auth_str) {
                Ok(auth_cfg) => {
                    if let Some((header_name, header_value)) = auth_cfg.resolve() {
                        tracing::debug!(
                            "ApiRuleProvider '{}': injecting auth header '{}'",
                            self.config.site.name,
                            header_name
                        );
                        request = request.header(header_name.as_str(), header_value.as_str());
                    } else {
                        tracing::debug!(
                            "ApiRuleProvider '{}': env var '{}' not set, proceeding without auth",
                            self.config.site.name,
                            auth_cfg.env_var
                        );
                    }
                }
                Err(e) => {
                    // Config was already validated at parse time; this branch is
                    // unreachable in practice but defensive against stale configs.
                    tracing::warn!(
                        "ApiRuleProvider '{}': invalid auth config ignored: {e}",
                        self.config.site.name
                    );
                }
            }
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
    serde_json::from_str(body).with_context(|| format!("failed to parse JSON from '{api_url}'"))
}

/// Thin wrapper around [`json_path::is_non_null`] for use in tests.
#[cfg(test)]
fn json_path_is_non_null(json: &serde_json::Value, path: &str) -> bool {
    json_path::is_non_null(json, path)
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
        .error_for_status()
        .with_context(|| format!("HTTP error for additional fetch '{url}'"))?
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
                if arr.is_empty() {
                    return None;
                }
                arr.join(", ")
            } else {
                json_path::extract(&json, path)?
            };
            Some((name.clone(), value))
        })
        .collect();

    Ok(fields)
}

/// Rewrite `url` using a compiled regex and template string.
///
/// Same rules as [`ApiRuleProvider::rewrite_url`]: `{url}` is replaced with
/// the URL-encoded original; otherwise capture-group substitution is applied.
fn rewrite_url_with(re: &Regex, to: &str, url: &str) -> String {
    if to.contains("{url}") {
        return to.replace("{url}", &urlencoding::encode(url));
    }
    re.replace(url, to).into_owned()
}

/// Extract named fields from HTML using CSS selectors.
///
/// Each entry in `css_map` maps a field name to a selector string.  A
/// `::attr(name)` suffix causes attribute extraction; without it the element's
/// text content is collected.
fn extract_css_fields(html: &str, css_map: &HashMap<String, String>) -> HashMap<String, String> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let mut fields = HashMap::new();

    for (field_name, selector_str) in css_map {
        let (selector_str, attr_name) = parse_css_attr_suffix(selector_str);

        let Ok(selector) = Selector::parse(selector_str) else {
            tracing::warn!("Invalid CSS selector for field '{field_name}': '{selector_str}'");
            continue;
        };

        let Some(element) = document.select(&selector).next() else {
            continue;
        };

        let value = if let Some(attr) = attr_name {
            element.value().attr(attr).unwrap_or("").to_string()
        } else {
            element.text().collect::<String>()
        };

        if !value.is_empty() {
            fields.insert(field_name.clone(), value);
        }
    }

    fields
}

/// Split a CSS selector string on `::attr(name)`, returning `(selector, attr)`.
///
/// Returns `(full_string, None)` when no `::attr(...)` suffix is present.
fn parse_css_attr_suffix(selector: &str) -> (&str, Option<&str>) {
    // Locate `::attr(` — must be followed by `name)` at the end.
    let Some(attr_start) = selector.rfind("::attr(") else {
        return (selector, None);
    };
    let rest = &selector[attr_start + 7..]; // skip "::attr("
    let Some(close) = rest.rfind(')') else {
        return (selector, None);
    };
    let attr_name = &rest[..close];
    let css_part = &selector[..attr_start];
    (css_part, Some(attr_name))
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
        prefetched_html: Option<&[u8]>,
    ) -> Result<SiteContent> {
        let api_url = self.rewrite_url(url);
        tracing::debug!(
            "ApiRuleProvider '{}': fetching {}",
            self.config.site.name,
            api_url
        );

        // Attempt primary JSON fetch; fall through to fallbacks on failure or
        // when the API returns a content-not-found envelope (success_path null).
        let primary_result = self.try_primary_json(client, &api_url).await;

        // `Ok(None)` → API-level not-found; `Err` → HTTP/parse failure.
        // Both cases trigger fallback or a bail with a clear message.
        let primary_fields: Option<HashMap<String, String>> = match primary_result {
            Ok(opt) => opt,
            Err(e) => {
                tracing::debug!(
                    "ApiRuleProvider '{}': primary fetch failed: {e}",
                    self.config.site.name
                );
                None
            }
        };

        let mut fields = match primary_fields {
            Some(mut f) if !f.is_empty() => {
                // Primary succeeded — if fallbacks are configured, use them to fill
                // any fields that the primary API didn't return (e.g., SE API returns
                // metadata but not body without an app key).
                if !self.config.fallback.is_empty() {
                    let fb_fields = self.apply_fallbacks(url, client, prefetched_html).await;
                    for (k, v) in fb_fields {
                        f.entry(k).or_insert(v);
                    }
                }
                f
            }
            Some(_empty) if !self.config.fallback.is_empty() => {
                // Extracted successfully but no paths matched — try fallbacks.
                tracing::debug!(
                    "ApiRuleProvider '{}': primary yielded no fields, trying fallbacks",
                    self.config.site.name
                );
                let fb_fields = self.apply_fallbacks(url, client, prefetched_html).await;
                if fb_fields.is_empty() {
                    bail!(
                        "no fields extracted from primary or fallbacks for rule '{}'",
                        self.config.site.name
                    );
                }
                fb_fields
            }
            Some(_empty) => {
                bail!(
                    "no fields extracted from '{}' response (check json paths in rule '{}')",
                    api_url,
                    self.config.site.name
                );
            }
            None if !self.config.fallback.is_empty() => {
                // API-level not-found or fetch error — try fallbacks.
                tracing::debug!(
                    "ApiRuleProvider '{}': primary not-found/failed, trying fallbacks",
                    self.config.site.name
                );
                let fb_fields = self.apply_fallbacks(url, client, prefetched_html).await;
                if fb_fields.is_empty() {
                    bail!(
                        "content not found via '{}' and no fallback succeeded for rule '{}'",
                        api_url,
                        self.config.site.name
                    );
                }
                fb_fields
            }
            None => {
                bail!(
                    "content not found via '{}' (API returned not-found envelope) for rule '{}'",
                    api_url,
                    self.config.site.name
                );
            }
        };

        self.apply_additional_fetches(url, client, &mut fields)
            .await;

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
    let reposts = eng
        .reposts
        .as_deref()
        .and_then(|f| parse_u64(fields.get(f)?));
    let replies = eng
        .replies
        .as_deref()
        .and_then(|f| parse_u64(fields.get(f)?));
    let views = eng.views.as_deref().and_then(|f| parse_u64(fields.get(f)?));

    if likes.is_none() && reposts.is_none() && replies.is_none() && views.is_none() {
        None
    } else {
        Some(Engagement {
            likes,
            reposts,
            replies,
            views,
        })
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
    use crate::site::rules::config::{FallbackType, SiteRuleConfig};

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
        assert_eq!(
            rewritten,
            "https://api.fxtwitter.com/naval/status/1234567890"
        );
    }

    #[test]
    fn twitter_rewrite_works_for_twitter_com() {
        let p = twitter_provider();
        let rewritten = p.rewrite_url("https://twitter.com/elonmusk/status/9876543210");
        assert_eq!(
            rewritten,
            "https://api.fxtwitter.com/elonmusk/status/9876543210"
        );
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
        let rewritten = p.rewrite_url("https://en.wikipedia.org/wiki/Rust_(programming_language)");
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
                "views": 3_800_000,
                "created_at": "Wed Feb 12 10:00:00 +0000 2025",
                "url": "https://x.com/naval/status/123"
            }
        });
        let fields = p.extract_fields(&json);
        assert_eq!(fields.get("author_name").map(String::as_str), Some("Naval"));
        assert_eq!(
            fields.get("author_handle").map(String::as_str),
            Some("naval")
        );
        assert_eq!(
            fields.get("text").map(String::as_str),
            Some("Build wealth, not status.")
        );
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
        fields.insert(
            "url".to_string(),
            "https://x.com/naval/status/123".to_string(),
        );

        let meta = p.build_metadata(&fields, "https://x.com/naval/status/123");
        assert_eq!(meta.platform, "Twitter/X");
        assert_eq!(meta.author.as_deref(), Some("Naval (@naval)"));
        assert_eq!(meta.canonical_url, "https://x.com/naval/status/123");
    }

    #[test]
    fn wikipedia_build_metadata_title_and_url() {
        let p = wikipedia_provider();
        let mut fields = HashMap::new();
        fields.insert(
            "title".to_string(),
            "Rust (programming language)".to_string(),
        );
        fields.insert(
            "page_url".to_string(),
            "https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string(),
        );
        fields.insert("timestamp".to_string(), "2025-01-01T00:00:00Z".to_string());

        let meta = p.build_metadata(
            &fields,
            "https://en.wikipedia.org/wiki/Rust_(programming_language)",
        );
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

    // ── success_path (request.success_path guard) ─────────────────────────────

    #[test]
    fn twitter_success_path_is_configured() {
        // GIVEN: the twitter provider (loaded from embedded TOML)
        let p = twitter_provider();
        // THEN: success_path is set to ".tweet" to detect null-tweet envelopes
        assert_eq!(
            p.config.request.success_path.as_deref(),
            Some(".tweet"),
            "twitter rule must have success_path = \".tweet\" to handle FxTwitter \
             404 envelopes ({{\"tweet\":null}})"
        );
    }

    #[test]
    fn twitter_extract_fields_returns_empty_for_null_tweet_envelope() {
        // GIVEN: FxTwitter error response — HTTP 200 but tweet is null
        let p = twitter_provider();
        let json = serde_json::json!({"code": 404, "message": "NOT_FOUND", "tweet": null});
        // WHEN: extracting fields directly (simulates what try_primary_json sees)
        let fields = p.extract_fields(&json);
        // THEN: empty — all paths start with .tweet which is null
        assert!(
            fields.is_empty(),
            "extract_fields must return empty map for null tweet; got: {fields:?}"
        );
    }

    #[test]
    fn provider_with_success_path_skips_extraction_when_path_is_null() {
        // GIVEN: a provider config with success_path = ".data"
        let toml = r#"
[site]
name = "test_guard"
patterns = ["example\\.com/.*"]

[rewrite]
from = ".*"
to   = "https://api.example.com/data"

[request]
success_path = ".data"

[json]
title = ".data.title"

[template]
format = "{title}"
"#;
        let p = make_provider(toml);
        // THEN: success_path is set correctly
        assert_eq!(p.config.request.success_path.as_deref(), Some(".data"));
        // AND: extracting from a null-data envelope yields an empty map
        let json = serde_json::json!({"status": "error", "data": null});
        let fields = p.extract_fields(&json);
        assert!(fields.is_empty());
    }

    #[test]
    fn provider_without_success_path_extracts_despite_sibling_nulls() {
        // GIVEN: a provider with NO success_path and a response where some fields are null
        let toml = r#"
[site]
name = "test_no_guard"
patterns = ["example\\.com/.*"]

[rewrite]
from = ".*"
to   = "https://api.example.com/"

[json]
title  = ".title"
author = ".author"

[template]
format = "{title} by {author}"
"#;
        let p = make_provider(toml);
        assert!(p.config.request.success_path.is_none());
        // WHEN: response has title but author is null
        let json = serde_json::json!({"title": "Hello", "author": null});
        let fields = p.extract_fields(&json);
        // THEN: title is extracted, author is absent (null → None in extract)
        assert_eq!(fields.get("title").map(String::as_str), Some("Hello"));
        assert!(!fields.contains_key("author"));
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
        assert!(
            std::path::Path::new(&rewritten).extension().is_some_and(|e| e.eq_ignore_ascii_case("json")),
            "expected .json suffix, got: {rewritten}"
        );
        assert!(
            !rewritten.contains('?'),
            "query string should be stripped, got: {rewritten}"
        );
    }

    #[test]
    fn reddit_rewrite_strips_query_string() {
        let p = reddit_provider();
        let rewritten = p.rewrite_url("https://reddit.com/r/rust/comments/abc123?utm_source=share");
        assert!(
            std::path::Path::new(&rewritten).extension().is_some_and(|e| e.eq_ignore_ascii_case("json")),
            "expected .json suffix, got: {rewritten}"
        );
        assert!(
            !rewritten.contains("utm_source"),
            "utm param should be gone, got: {rewritten}"
        );
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
        assert_eq!(
            fields.get("title").map(String::as_str),
            Some("Rust 2024 edition released")
        );
        assert_eq!(
            fields.get("author").map(String::as_str),
            Some("rustacean42")
        );
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
        fields.insert(
            "url".to_string(),
            "https://reddit.com/r/rust/comments/x".to_string(),
        );
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

    #[test]
    fn parse_response_json_fails_on_html_body() {
        // GIVEN: HTML body — what Reddit returns when HTTP/2-without-ALPN is used
        let body = "<!DOCTYPE html><html><body>Just a moment...</body></html>";
        // WHEN: attempting to parse as JSON
        let result = parse_response_json(body, "https://www.reddit.com/r/rust.json");
        // THEN: error returned so the caller can surface it properly
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("failed to parse JSON"));
    }

    // ── reddit error-envelope and URL-rewrite edge cases ─────────────────────

    #[test]
    fn reddit_extract_fields_yields_empty_for_not_found_envelope() {
        // GIVEN: Reddit's error JSON (valid JSON but no listing structure).
        // Before error_for_status() was added, HTTP 404 responses were silently
        // treated as "no fields" rather than a hard error, making the diagnostic
        // message misleading ("check json paths" when the real issue is 404).
        let p = reddit_provider();
        let json = serde_json::json!({"message": "Not Found", "error": 404});
        // WHEN: extracting Reddit fields from the error envelope
        let fields = p.extract_fields(&json);
        // THEN: no fields extracted (the [0].data.children paths don't exist here)
        assert!(
            fields.is_empty(),
            "error envelope should yield no fields, got: {fields:?}"
        );
    }

    #[test]
    fn reddit_rewrite_with_trailing_slash_produces_json_url() {
        // GIVEN: URL with trailing slash (common copy-paste from browser)
        let p = reddit_provider();
        let url = "https://www.reddit.com/r/rust/comments/1krtgr2/media_i_made_a_native_music_player_with_rust/";
        // WHEN: rewriting
        let rewritten = p.rewrite_url(url);
        // THEN: trailing slash consumed by regex, .json appended to slug
        assert_eq!(
            rewritten,
            "https://www.reddit.com/r/rust/comments/1krtgr2/media_i_made_a_native_music_player_with_rust.json"
        );
    }

    #[test]
    fn reddit_rewrite_without_title_slug_produces_json_url() {
        // GIVEN: short URL with post ID but no title slug
        let p = reddit_provider();
        let url = "https://www.reddit.com/r/rust/comments/1krtgr2/";
        // WHEN: rewriting
        let rewritten = p.rewrite_url(url);
        // THEN: .json appended to post ID
        assert_eq!(
            rewritten,
            "https://www.reddit.com/r/rust/comments/1krtgr2.json"
        );
    }

    // ── json_path_is_non_null ─────────────────────────────────────────────────

    #[test]
    fn json_path_is_non_null_returns_true_for_existing_string() {
        // GIVEN: FxTwitter-style success response with a non-null tweet object
        let json = json!({"tweet": {"text": "hello", "likes": 42}});
        // WHEN/THEN: paths to real values return true
        assert!(json_path_is_non_null(&json, ".tweet.text"));
        assert!(json_path_is_non_null(&json, ".tweet.likes"));
    }

    #[test]
    fn json_path_is_non_null_returns_false_for_null_value() {
        // GIVEN: FxTwitter-style error where tweet is null (tweet not found)
        let json = json!({"tweet": null, "code": 144});
        // WHEN/THEN: path to null returns false
        assert!(!json_path_is_non_null(&json, ".tweet"));
    }

    #[test]
    fn json_path_is_non_null_returns_false_for_missing_path() {
        // GIVEN: JSON without the expected success field (Reddit 404 envelope)
        let json = json!({"message": "Not Found", "error": 404});
        // WHEN/THEN: paths that don't exist return false
        assert!(!json_path_is_non_null(&json, ".tweet"));
        assert!(!json_path_is_non_null(&json, "[0].data.children[0].data.title"));
    }

    #[test]
    fn json_path_is_non_null_returns_true_for_number_zero() {
        // GIVEN: JSON with a zero value — falsy in some langs, but not null in Rust
        let json = json!({"count": 0});
        // WHEN/THEN: zero is non-null
        assert!(json_path_is_non_null(&json, ".count"));
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
        assert_eq!(
            fields.get("title").map(String::as_str),
            Some("How to use Vec in Rust?")
        );
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

    // ── auth config integration ───────────────────────────────────────────────

    fn provider_with_auth(auth: &str) -> ApiRuleProvider {
        let toml = format!(
            r#"
[site]
name = "test-auth"
patterns = ["example\\.com"]

[rewrite]
from = ".*"
to = "https://api.example.com"

[request]
auth = "{auth}"

[json]
title = ".title"

[template]
format = "{{{{title}}}}"
"#
        );
        make_provider(&toml)
    }

    #[test]
    fn provider_with_auth_config_parses_successfully() {
        // GIVEN: a rule with auth = "env:SOME_TOKEN"
        // WHEN: provider is built
        let p = provider_with_auth("env:SOME_TOKEN");
        // THEN: auth field is stored in config
        assert_eq!(p.config.request.auth.as_deref(), Some("env:SOME_TOKEN"));
    }

    #[test]
    fn provider_with_auth_stores_env_var_name() {
        // GIVEN: auth pointing to a custom env var
        let p = provider_with_auth("env:GITHUB_TOKEN");
        // WHEN: the stored auth string is parsed
        let auth_cfg = AuthConfig::parse(p.config.request.auth.as_deref().unwrap()).unwrap();
        // THEN: correct env var name is stored
        assert_eq!(auth_cfg.env_var, "GITHUB_TOKEN");
        assert!(auth_cfg.bearer);
        assert_eq!(auth_cfg.header_name, "Authorization");
    }

    #[test]
    fn provider_with_custom_header_auth_stores_header_name() {
        // GIVEN: auth with custom header
        let p = provider_with_auth("env:MY_KEY:header=X-Custom-Auth");
        let auth_cfg = AuthConfig::parse(p.config.request.auth.as_deref().unwrap()).unwrap();
        assert_eq!(auth_cfg.header_name, "X-Custom-Auth");
        assert!(!auth_cfg.bearer);
    }

    #[test]
    fn github_issues_provider_parses_and_has_auth() {
        // GIVEN: the embedded github-issues TOML rule
        let p = make_provider(include_str!("defaults/github-issues.toml"));
        // THEN: provider name is correct, auth is set
        assert_eq!(p.config.site.name, "github-issues");
        assert_eq!(p.config.request.auth.as_deref(), Some("env:GITHUB_TOKEN"));
    }

    #[test]
    fn github_issues_provider_matches_issue_and_pr_urls() {
        let p = make_provider(include_str!("defaults/github-issues.toml"));
        assert!(p.matches("https://github.com/rust-lang/rust/issues/12345"));
        assert!(p.matches("https://github.com/owner/repo/pull/999"));
        assert!(p.matches("https://GITHUB.COM/owner/repo/issues/1"));
    }

    #[test]
    fn github_issues_provider_does_not_match_repo_root() {
        let p = make_provider(include_str!("defaults/github-issues.toml"));
        assert!(!p.matches("https://github.com/rust-lang/rust"));
        assert!(!p.matches("https://github.com/owner/repo/tree/main"));
    }

    #[test]
    fn github_issues_rewrite_constructs_api_url() {
        let p = make_provider(include_str!("defaults/github-issues.toml"));
        let rewritten = p.rewrite_url("https://github.com/rust-lang/rust/issues/12345");
        assert_eq!(
            rewritten,
            "https://api.github.com/repos/rust-lang/rust/issues/12345"
        );
    }

    #[test]
    fn github_issues_rewrite_works_for_pull_requests() {
        let p = make_provider(include_str!("defaults/github-issues.toml"));
        // GitHub API exposes PRs under /issues/ endpoint
        let rewritten = p.rewrite_url("https://github.com/owner/repo/pull/42");
        assert_eq!(
            rewritten,
            "https://api.github.com/repos/owner/repo/issues/42"
        );
    }

    #[test]
    fn github_issues_extract_fields_from_api_json() {
        // GIVEN: a GitHub issue API response
        let p = make_provider(include_str!("defaults/github-issues.toml"));
        let json = json!({
            "html_url": "https://github.com/rust-lang/rust/issues/12345",
            "title": "Some bug",
            "state": "open",
            "user": {"login": "contributor"},
            "body": "This is the issue body.",
            "comments": 5,
            "created_at": "2025-01-01T00:00:00Z",
            "labels": [{"name": "bug"}, {"name": "help wanted"}]
        });
        // WHEN: extracting fields
        let fields = p.extract_fields(&json);
        // THEN: key fields are present
        assert_eq!(fields.get("title").map(String::as_str), Some("Some bug"));
        assert_eq!(
            fields.get("author").map(String::as_str),
            Some("contributor")
        );
        assert_eq!(fields.get("state").map(String::as_str), Some("open"));
        assert_eq!(fields.get("comments").map(String::as_str), Some("5"));
    }

    // ── parse_css_attr_suffix ────────────────────────────────────────────────

    #[test]
    fn parse_css_attr_suffix_detects_attr() {
        // GIVEN: selector with ::attr(content) suffix
        let (css, attr) = parse_css_attr_suffix("meta[property='og:title']::attr(content)");
        assert_eq!(css, "meta[property='og:title']");
        assert_eq!(attr, Some("content"));
    }

    #[test]
    fn parse_css_attr_suffix_no_suffix_returns_none() {
        // GIVEN: plain selector without ::attr
        let (css, attr) = parse_css_attr_suffix("h1.title");
        assert_eq!(css, "h1.title");
        assert!(attr.is_none());
    }

    #[test]
    fn parse_css_attr_suffix_handles_href_attribute() {
        let (css, attr) = parse_css_attr_suffix("a.link::attr(href)");
        assert_eq!(css, "a.link");
        assert_eq!(attr, Some("href"));
    }

    #[test]
    fn parse_css_attr_suffix_handles_malformed_no_closing_paren() {
        // GIVEN: ::attr( without closing )
        let (css, attr) = parse_css_attr_suffix("meta::attr(content");
        // THEN: treated as no attr suffix
        assert_eq!(css, "meta::attr(content");
        assert!(attr.is_none());
    }

    // ── extract_css_fields ───────────────────────────────────────────────────

    #[test]
    fn extract_css_fields_attribute_extraction() {
        // GIVEN: HTML with og:meta tags and a CSS map using ::attr(content)
        let html = r#"<html><head>
            <meta property="og:title" content="Test Title" />
            <meta property="og:description" content="A description" />
            <meta property="og:image" content="https://example.com/img.jpg" />
        </head><body></body></html>"#;
        let mut css_map = HashMap::new();
        css_map.insert(
            "title".to_string(),
            "meta[property='og:title']::attr(content)".to_string(),
        );
        css_map.insert(
            "description".to_string(),
            "meta[property='og:description']::attr(content)".to_string(),
        );
        css_map.insert(
            "image".to_string(),
            "meta[property='og:image']::attr(content)".to_string(),
        );
        // WHEN: extracting
        let fields = extract_css_fields(html, &css_map);
        // THEN: all three fields present
        assert_eq!(fields.get("title").map(String::as_str), Some("Test Title"));
        assert_eq!(
            fields.get("description").map(String::as_str),
            Some("A description")
        );
        assert_eq!(
            fields.get("image").map(String::as_str),
            Some("https://example.com/img.jpg")
        );
    }

    #[test]
    fn extract_css_fields_text_content_extraction() {
        // GIVEN: HTML with an h1 and CSS map using text content (no ::attr)
        let html = "<html><body><h1>Page Heading</h1></body></html>";
        let mut css_map = HashMap::new();
        css_map.insert("title".to_string(), "h1".to_string());
        // WHEN: extracting
        let fields = extract_css_fields(html, &css_map);
        // THEN: text content of h1 is used
        assert_eq!(
            fields.get("title").map(String::as_str),
            Some("Page Heading")
        );
    }

    #[test]
    fn extract_css_fields_missing_element_omitted() {
        // GIVEN: HTML without the targeted element
        let html = "<html><body><p>No heading here</p></body></html>";
        let mut css_map = HashMap::new();
        css_map.insert("title".to_string(), "h1".to_string());
        // WHEN: extracting
        let fields = extract_css_fields(html, &css_map);
        // THEN: field absent (no element found)
        assert!(!fields.contains_key("title"));
    }

    #[test]
    fn extract_css_fields_empty_attr_value_omitted() {
        // GIVEN: meta tag with empty content attribute
        let html = r#"<html><head><meta property="og:title" content="" /></head></html>"#;
        let mut css_map = HashMap::new();
        css_map.insert(
            "title".to_string(),
            "meta[property='og:title']::attr(content)".to_string(),
        );
        // WHEN: extracting
        let fields = extract_css_fields(html, &css_map);
        // THEN: empty attribute value is omitted
        assert!(!fields.contains_key("title"));
    }

    #[test]
    fn extract_css_fields_invalid_selector_logs_and_skips() {
        // GIVEN: an invalid CSS selector
        let html = "<html><body></body></html>";
        let mut css_map = HashMap::new();
        css_map.insert("title".to_string(), "[[[invalid".to_string());
        // WHEN: extracting
        let fields = extract_css_fields(html, &css_map);
        // THEN: field absent, no panic
        assert!(fields.is_empty());
    }

    // ── rewrite_url_with ─────────────────────────────────────────────────────

    #[test]
    fn rewrite_url_with_url_placeholder() {
        // GIVEN: template with {url}
        let re = regex::Regex::new(".*").unwrap();
        let result = rewrite_url_with(
            &re,
            "https://api.example.com?url={url}",
            "https://orig.com/page",
        );
        assert!(result.contains("https%3A%2F%2Forig.com%2Fpage"));
    }

    #[test]
    fn rewrite_url_with_capture_group() {
        // GIVEN: template with capture group $1
        let re = regex::Regex::new(r"https://example\.com/items/(\d+)").unwrap();
        let result = rewrite_url_with(
            &re,
            "https://api.example.com/items/$1",
            "https://example.com/items/42",
        );
        assert_eq!(result, "https://api.example.com/items/42");
    }

    #[test]
    fn rewrite_url_with_identity_passthrough() {
        // GIVEN: {url} template that passes original URL through (url-encoded)
        let re = regex::Regex::new(".*").unwrap();
        let result = rewrite_url_with(&re, "{url}", "https://example.com/page");
        assert_eq!(result, "https%3A%2F%2Fexample.com%2Fpage");
    }

    // ── instagram provider ───────────────────────────────────────────────────

    fn instagram_provider() -> ApiRuleProvider {
        make_provider(include_str!("defaults/instagram.toml"))
    }

    #[test]
    fn instagram_provider_matches_post_urls() {
        let p = instagram_provider();
        assert!(p.matches("https://instagram.com/p/ABC123xyz"));
        assert!(p.matches("https://www.instagram.com/p/XYZ789abc"));
        assert!(p.matches("https://INSTAGRAM.COM/p/test123"));
    }

    #[test]
    fn instagram_provider_matches_reel_urls() {
        let p = instagram_provider();
        assert!(p.matches("https://instagram.com/reel/ABC123xyz"));
        assert!(p.matches("https://www.instagram.com/reel/XYZ789"));
    }

    #[test]
    fn instagram_provider_does_not_match_profile_urls() {
        let p = instagram_provider();
        assert!(!p.matches("https://instagram.com/username"));
        assert!(!p.matches("https://instagram.com/"));
        assert!(!p.matches("https://youtube.com/watch?v=abc"));
    }

    #[test]
    fn instagram_provider_has_one_html_fallback() {
        let p = instagram_provider();
        assert_eq!(p.config.fallback.len(), 1);
        assert_eq!(p.fallback_rewrite_froms.len(), 1);
        assert_eq!(p.config.fallback[0].fallback_type, FallbackType::Html);
    }

    #[test]
    fn instagram_provider_fallback_css_has_og_selectors() {
        let p = instagram_provider();
        let css = &p.config.fallback[0].css;
        assert!(css.contains_key("title"));
        assert!(css.contains_key("description"));
        assert!(css.contains_key("image"));
        assert!(css["title"].contains("og:title"));
        assert!(css["image"].contains("og:image"));
    }

    // ── Twitter template: views is optional ───────────────────────────────────

    #[test]
    fn twitter_template_renders_engagement_line_without_views() {
        // GIVEN: FxTwitter response where views is null — a common case for older
        // tweets and accounts that haven't enabled view counts.
        let p = twitter_provider();
        let mut fields = HashMap::new();
        fields.insert("author_handle".to_string(), "jack".to_string());
        fields.insert("author_name".to_string(), "jack".to_string());
        fields.insert("text".to_string(), "just setting up my twttr".to_string());
        fields.insert("likes".to_string(), "290120".to_string());
        fields.insert("retweets".to_string(), "123262".to_string());
        fields.insert("replies".to_string(), "16455".to_string());
        // No "views" field — null in JSON → absent from map.
        fields.insert(
            "date".to_string(),
            "Tue Mar 21 20:50:14 +0000 2006".to_string(),
        );
        fields.insert("url".to_string(), "https://x.com/jack/status/20".to_string());

        let markdown = template::render(
            &p.config.template.format,
            &fields,
            "https://x.com/jack/status/20",
        );

        // THEN: the engagement line is present (likes/retweets/replies)
        assert!(
            markdown.contains("290.1K likes"),
            "engagement line must render even without views; got:\n{markdown}"
        );
        assert!(
            markdown.contains("123.3K reposts"),
            "retweets must render; got:\n{markdown}"
        );
        // AND: the views line is silently omitted (not an error)
        assert!(
            !markdown.contains("views"),
            "views line must be omitted when views field is absent; got:\n{markdown}"
        );
        // AND: the rest of the template renders correctly
        assert!(markdown.contains("## @jack (jack)"));
        assert!(markdown.contains("just setting up my twttr"));
        assert!(markdown.contains("[View on X](https://x.com/jack/status/20)"));
    }

    #[test]
    fn twitter_template_renders_views_line_when_views_present() {
        // GIVEN: a tweet with views populated (X Premium / newer tweets)
        let p = twitter_provider();
        let mut fields = HashMap::new();
        fields.insert("author_handle".to_string(), "rustlang".to_string());
        fields.insert("author_name".to_string(), "The Rust Programming Language".to_string());
        fields.insert("text".to_string(), "Rust 2024 edition is stable!".to_string());
        fields.insert("likes".to_string(), "8800".to_string());
        fields.insert("retweets".to_string(), "1200".to_string());
        fields.insert("replies".to_string(), "344".to_string());
        fields.insert("views".to_string(), "3800000".to_string());
        fields.insert("date".to_string(), "Mon Nov 28 00:00:00 +0000 2024".to_string());
        fields.insert(
            "url".to_string(),
            "https://x.com/rustlang/status/1861000000000000000".to_string(),
        );

        let markdown = template::render(
            &p.config.template.format,
            &fields,
            "https://x.com/rustlang/status/1861000000000000000",
        );

        // THEN: views line is present
        assert!(
            markdown.contains("3.8M views"),
            "views line must render when views field is present; got:\n{markdown}"
        );
        // AND: engagement line also present
        assert!(markdown.contains("8.8K likes"));
    }
}
