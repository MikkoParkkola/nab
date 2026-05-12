// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

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

use super::super::{SiteContent, SiteMetadata, SiteProvider};
use super::config::{AuthConfig, ClientKind, FallbackType, JsonConfig, SiteRuleConfig};
use super::helpers::{
    build_engagement, extract_css_fields, fetch_and_expand_items, fetch_and_extract_json,
    intern_name, parse_response_json, rewrite_url_with,
};
use super::json_path;
use super::template;
use crate::http_client::AcceleratedClient;

/// Shared context for HTML/JSON fallback extraction.
struct FallbackContext<'a> {
    fetch_url: &'a str,
    original_url: &'a str,
    accept: Option<&'a str>,
    cookies: Option<&'a str>,
    prefetched_html: Option<&'a [u8]>,
}

/// A compiled, ready-to-use provider built from a [`SiteRuleConfig`].
pub struct ApiRuleProvider {
    /// Config driving this provider.
    config: SiteRuleConfig,
    /// Interned static name for `SiteProvider::name()` — leaked once at
    /// construction so `name()` never allocates.
    static_name: &'static str,
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
    /// Compiled `rewrite_from` regexes for each concurrent fetch (parallel to
    /// `config.concurrent_fetches`).
    concurrent_rewrite_froms: Vec<Regex>,
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
        if config.site.engine.is_browser() {
            bail!(
                "rule '{}' uses engine='{}'; ApiRuleProvider only supports engine='api'",
                config.site.name,
                config.site.engine.as_str()
            );
        }

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

        let concurrent_rewrite_froms = config
            .concurrent_fetches
            .iter()
            .map(|cf| {
                Regex::new(&cf.rewrite_from).with_context(|| {
                    format!(
                        "invalid fetch_concurrent rewrite_from regex '{}'",
                        cf.rewrite_from
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;

        // Intern the name once — leaked intentionally because providers live
        // for the entire program.  The set of names is small (embedded + user).
        let static_name = intern_name(&config.site.name);

        Ok(Self {
            config,
            static_name,
            patterns,
            rewrite_from,
            additional_rewrite_froms,
            fallback_rewrite_froms,
            concurrent_rewrite_froms,
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
        cookies: Option<&str>,
    ) -> Result<Option<HashMap<String, String>>> {
        let body = self.fetch_body(client, api_url, cookies).await?;
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
                    // Use paragraph breaks for content/body fields (articles, long-form);
                    // comma-join for short list fields (tags, categories).
                    let sep = if name.contains("content") || name.contains("body") {
                        "\n\n"
                    } else {
                        ", "
                    };
                    arr.join(sep)
                } else {
                    json_path::extract(json, path)?
                };
                // Field extraction succeeded — tracing::debug! for production logging.
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
        cookies: Option<&str>,
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
                cookies,
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

    /// Execute all configured concurrent fetches and expand items into `fields`.
    ///
    /// Each `[[fetch_concurrent]]` entry fetches a single list URL, walks the
    /// item array at `items_path`, and inserts fields as
    /// `{prefix}_{idx}_{field}` (e.g., `story_0_title`, `story_1_title`).
    async fn apply_concurrent_fetches(
        &self,
        original_url: &str,
        client: &AcceleratedClient,
        cookies: Option<&str>,
        fields: &mut HashMap<String, String>,
    ) {
        for (cf, rewrite_re) in self
            .config
            .concurrent_fetches
            .iter()
            .zip(self.concurrent_rewrite_froms.iter())
        {
            let list_url = rewrite_url_with(rewrite_re, &cf.rewrite_to, original_url);
            tracing::debug!(
                "ApiRuleProvider '{}': concurrent fetch ({}) {}",
                self.config.site.name,
                cf.prefix,
                list_url
            );

            match fetch_and_expand_items(client, &list_url, cf, cookies).await {
                Ok(expanded) => {
                    for (key, value) in expanded {
                        fields.insert(key, value);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Concurrent fetch '{}' for rule '{}' failed: {e}",
                        cf.prefix,
                        self.config.site.name
                    );
                }
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
        cookies: Option<&str>,
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
                    self.apply_json_fallback(
                        client,
                        &fetch_url,
                        fb.accept.as_deref(),
                        &fb.json,
                        cookies,
                    )
                    .await
                }
                FallbackType::Html => {
                    let ctx = FallbackContext {
                        fetch_url: &fetch_url,
                        original_url,
                        accept: fb.accept.as_deref(),
                        cookies,
                        prefetched_html,
                    };
                    self.apply_html_fallback(client, &ctx, &fb.css).await
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
        cookies: Option<&str>,
    ) -> HashMap<String, String> {
        match fetch_and_extract_json(client, url, accept, json_config, cookies).await {
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
        ctx: &FallbackContext<'_>,
        css_map: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        // Reuse pre-fetched bytes when the resolved URL is the original URL.
        let html: String = if ctx.fetch_url == ctx.original_url {
            if let Some(bytes) = ctx.prefetched_html {
                String::from_utf8_lossy(bytes).into_owned()
            } else {
                match self
                    .fetch_html(client, ctx.fetch_url, ctx.accept, ctx.cookies)
                    .await
                {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!(
                            "HTML fallback fetch failed for rule '{}' at '{}': {e}",
                            self.config.site.name,
                            ctx.fetch_url
                        );
                        return HashMap::new();
                    }
                }
            }
        } else {
            match self
                .fetch_html(client, ctx.fetch_url, ctx.accept, ctx.cookies)
                .await
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(
                        "HTML fallback fetch failed for rule '{}' at '{}': {e}",
                        self.config.site.name,
                        ctx.fetch_url
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
        cookies: Option<&str>,
    ) -> Result<String> {
        let mut request = client.inner().get(url);
        if let Some(accept_val) = accept {
            request = request.header(reqwest::header::ACCEPT, accept_val);
        }
        if let Some(cookie_val) = cookies {
            request = request.header(reqwest::header::COOKIE, cookie_val);
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
    async fn fetch_body(
        &self,
        client: &AcceleratedClient,
        api_url: &str,
        cookies: Option<&str>,
    ) -> Result<String> {
        match self.config.request.client {
            ClientKind::Standard => self.fetch_body_standard(api_url, cookies).await,
            ClientKind::Default => self.fetch_body_accelerated(client, api_url, cookies).await,
        }
    }

    /// Fetch using a fresh standard `reqwest::Client` (ALPN negotiation).
    async fn fetch_body_standard(&self, api_url: &str, cookies: Option<&str>) -> Result<String> {
        let standard_client = reqwest::Client::builder()
            .use_rustls_tls()
            .gzip(true)
            .brotli(true)
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .context("failed to build standard HTTP client")?;

        let mut request = self.apply_headers(standard_client.get(api_url));
        if let Some(cookie_val) = cookies {
            request = request.header(reqwest::header::COOKIE, cookie_val);
        }

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
        cookies: Option<&str>,
    ) -> Result<String> {
        let mut request = self.apply_headers(client.inner().get(api_url));
        if let Some(cookie_val) = cookies {
            request = request.header(reqwest::header::COOKIE, cookie_val);
        }

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

#[async_trait]
impl SiteProvider for ApiRuleProvider {
    fn name(&self) -> &'static str {
        self.static_name
    }

    fn matches(&self, url: &str) -> bool {
        self.patterns.iter().any(|re| re.is_match(url))
    }

    async fn extract(
        &self,
        url: &str,
        client: &AcceleratedClient,
        cookies: Option<&str>,
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
        let primary_result = self.try_primary_json(client, &api_url, cookies).await;

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
                    let fb_fields = self
                        .apply_fallbacks(url, client, cookies, prefetched_html)
                        .await;
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
                let fb_fields = self
                    .apply_fallbacks(url, client, cookies, prefetched_html)
                    .await;
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
                let fb_fields = self
                    .apply_fallbacks(url, client, cookies, prefetched_html)
                    .await;
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

        self.apply_additional_fetches(url, client, cookies, &mut fields)
            .await;

        self.apply_concurrent_fetches(url, client, cookies, &mut fields)
            .await;

        let markdown = template::render(&self.config.template.format, &fields, url);
        let metadata = self.build_metadata(&fields, url);

        Ok(SiteContent { markdown, metadata })
    }
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
