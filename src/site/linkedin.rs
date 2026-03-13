//! `LinkedIn` content extraction.
//!
//! Supports two extraction paths:
//! 1. **Authenticated** (default with `impersonate` feature): Uses TLS fingerprint
//!    impersonation via `rquest` to bypass `LinkedIn`'s JA3/JA4 bot detection.
//!    Required for profiles, companies, and activity pages. Falls back to oEmbed
//!    for posts/pulse when authentication fails.
//!
//!    Primary parsing strategy: `<code>` tag JSON extraction. `LinkedIn` serves a 1.3 MB
//!    SPA shell with no server-rendered CSS-selectable content. All profile and feed data
//!    is embedded as JSON inside hidden `<code>` elements:
//!    `<code style="display:none" id="bpr-guid-XXXX"><!--{...}--></code>`.
//!    JSON-LD and CSS selectors are tried as fallbacks only.
//!
//! 2. **oEmbed** (fallback): Limited data (title, author, thumbnail) for public posts.
//!
//! # URL Coverage
//!
//! - `/in/username` — profile pages (requires cookies)
//! - `/company/name` — company pages (requires cookies)
//! - `/posts/` — individual posts (oEmbed fallback available)
//! - `/pulse/` — articles (oEmbed fallback available)
//! - `/feed/update/` — feed updates (oEmbed fallback available)
//! - `/in/username/recent-activity/` — activity feed (requires cookies)

use std::fmt::Write as _;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use scraper::{Html, Selector};
use serde::Deserialize;

use super::{SiteContent, SiteMetadata, SiteProvider};
use crate::http_client::AcceleratedClient;

/// `LinkedIn` URL classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkedInUrlKind {
    /// `/in/username` — personal profile
    Profile,
    /// `/company/name` — company page
    Company,
    /// `/posts/...` — individual post
    Post,
    /// `/pulse/...` — long-form article
    Pulse,
    /// `/feed/update/...` — feed activity item
    FeedUpdate,
    /// `/in/username/recent-activity/...` — activity feed
    Activity,
}

impl LinkedInUrlKind {
    /// Whether this URL kind can fall back to oEmbed when authenticated fetch fails.
    fn has_oembed_fallback(self) -> bool {
        matches!(self, Self::Post | Self::Pulse | Self::FeedUpdate)
    }

    /// Whether this URL kind requires cookies (no public fallback).
    fn requires_auth(self) -> bool {
        matches!(self, Self::Profile | Self::Company | Self::Activity)
    }
}

/// Classify a `LinkedIn` URL into its content type.
#[must_use]
pub fn classify_linkedin_url(url: &str) -> Option<LinkedInUrlKind> {
    let lower = url.to_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);

    if !path.contains("linkedin.com/") {
        return None;
    }

    // Order matters: more specific patterns first
    if path.contains("/recent-activity/") {
        return Some(LinkedInUrlKind::Activity);
    }
    if path.contains("/feed/update/") {
        return Some(LinkedInUrlKind::FeedUpdate);
    }
    // /feed/ or /feed (home feed) — requires auth, no oEmbed fallback
    if path.ends_with("/feed/") || path.ends_with("/feed") {
        return Some(LinkedInUrlKind::Activity);
    }
    // Other authenticated sections: mynetwork, jobs, messaging, notifications
    for section in &["/mynetwork/", "/jobs/", "/messaging/", "/notifications/"] {
        if path.contains(section) {
            return Some(LinkedInUrlKind::Activity);
        }
    }
    if path.contains("/posts/") {
        return Some(LinkedInUrlKind::Post);
    }
    if path.contains("/pulse/") {
        return Some(LinkedInUrlKind::Pulse);
    }
    if path.contains("/company/") {
        return Some(LinkedInUrlKind::Company);
    }
    // Profile: /in/username — must have something after /in/
    if let Some(after) = path.split("/in/").nth(1) {
        let segment = after.split('/').next().unwrap_or("");
        if !segment.is_empty() {
            return Some(LinkedInUrlKind::Profile);
        }
    }

    None
}

/// `LinkedIn` content provider.
pub struct LinkedInProvider;

#[async_trait]
impl SiteProvider for LinkedInProvider {
    fn name(&self) -> &'static str {
        "linkedin"
    }

    fn matches(&self, url: &str) -> bool {
        classify_linkedin_url(url).is_some()
    }

    async fn extract(
        &self,
        url: &str,
        client: &AcceleratedClient,
        cookies: Option<&str>,
        _prefetched_html: Option<&[u8]>,
    ) -> Result<SiteContent> {
        let kind = classify_linkedin_url(url).context("URL does not match any LinkedIn pattern")?;

        // Try authenticated extraction first (requires impersonate feature + cookies)
        #[cfg(feature = "impersonate")]
        {
            if let Some(cookie_header) = cookies
                && !cookie_header.is_empty()
            {
                match fetch_authenticated(url, cookie_header, kind).await {
                    Ok(content) => return Ok(content),
                    Err(e) => {
                        tracing::warn!("LinkedIn authenticated fetch failed for {}: {}", url, e);
                        // Fall through to oEmbed for compatible URL kinds
                        if !kind.has_oembed_fallback() {
                            return Err(e);
                        }
                        tracing::debug!("Falling back to oEmbed for {}", url);
                    }
                }
            }

            // No cookies provided for auth-required URLs
            if kind.requires_auth() && cookies.is_none_or(str::is_empty) {
                bail!(
                    "LinkedIn {} pages require authentication.\n\
                     Use: nab fetch {} --cookies brave",
                    match kind {
                        LinkedInUrlKind::Profile => "profile",
                        LinkedInUrlKind::Company => "company",
                        LinkedInUrlKind::Activity => "activity",
                        _ => "content",
                    },
                    url
                );
            }
        }

        // Without impersonate feature, auth-required URLs cannot be fetched
        #[cfg(not(feature = "impersonate"))]
        if kind.requires_auth() {
            bail!(
                "LinkedIn {} pages require the `impersonate` feature.\n\
                 Build with: cargo build --features impersonate\n\
                 Then: nab fetch {} --cookies brave",
                match kind {
                    LinkedInUrlKind::Profile => "profile",
                    LinkedInUrlKind::Company => "company",
                    LinkedInUrlKind::Activity => "activity",
                    _ => "content",
                },
                url
            );
        }

        // oEmbed fallback for posts/pulse/feed
        fetch_oembed(url, client).await
    }
}

// ============================================================================
// Authenticated Extraction (impersonate feature)
// ============================================================================

/// Top-level authenticated extraction: fetch HTML, parse `<code>` JSON first.
///
/// The Voyager REST API (`/voyager/api/identity/profiles/{id}`) was deprecated
/// and returns HTTP 410 Gone. `LinkedIn` now embeds all profile/feed data as JSON
/// inside hidden `<code>` elements in the initial HTML response — that is the
/// only reliable server-side data source.
#[cfg(feature = "impersonate")]
async fn fetch_authenticated(
    url: &str,
    cookies: &str,
    kind: LinkedInUrlKind,
) -> Result<SiteContent> {
    fetch_authenticated_html(url, cookies, kind).await
}

/// Extract the `csrf-token` value from the raw cookie header string.
///
/// `JSESSIONID` is stored as `"ajax:NNNN"` (with surrounding double quotes).
/// The bare `ajax:NNNN` value (without quotes) is returned.
///
/// Returns `None` if no `JSESSIONID` cookie is present.
#[must_use]
pub fn extract_csrf_token(cookies: &str) -> Option<String> {
    cookies.split(';').find_map(|part| {
        let kv = part.trim();
        let (key, value) = kv.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("jsessionid") {
            let raw = value.trim();
            Some(raw.trim_matches('"').to_string())
        } else {
            None
        }
    })
}

/// Extract the `LinkedIn` username from a `/in/{username}` URL.
///
/// Returns `None` for non-profile URLs or malformed input.
#[must_use]
pub fn extract_username_from_url(url: &str) -> Option<String> {
    // Strip query string; preserve original casing for use in API calls.
    let without_query = url.split('?').next().unwrap_or(url);

    // Locate /in/ using case-insensitive search via lowercase copy.
    let lower = without_query.to_lowercase();
    let in_offset = lower.find("/in/")?;
    let after_in = &without_query[in_offset + 4..]; // 4 == len("/in/")

    let username = after_in.split('/').next()?;
    if username.is_empty() {
        None
    } else {
        Some(username.to_string())
    }
}

fn build_full_name(first: Option<&str>, last: Option<&str>) -> Option<String> {
    match (first, last) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f.to_string()),
        (None, Some(l)) => Some(l.to_string()),
        (None, None) => None,
    }
}

/// Fetch a `LinkedIn` URL via impersonated HTTP, parse HTML as fallback.
#[cfg(feature = "impersonate")]
async fn fetch_authenticated_html(
    url: &str,
    cookies: &str,
    kind: LinkedInUrlKind,
) -> Result<SiteContent> {
    use crate::impersonate_client;

    let response = impersonate_client::fetch_impersonated(url, Some(cookies), None).await?;

    let status = response.status.as_u16();

    // HTTP 999 = LinkedIn bot detection (even with impersonation, cookies may be expired)
    if status == 999 {
        bail!(
            "LinkedIn returned HTTP 999 (bot detection).\n\
             Your session cookies may have expired. Try:\n\
             1. Log into LinkedIn in your browser\n\
             2. Retry: nab fetch {url} --cookies brave"
        );
    }

    // Redirect to login page = missing/invalid auth
    if (300..400).contains(&status)
        || response.body.contains("login") && response.body.contains("session_redirect")
    {
        bail!(
            "LinkedIn redirected to login. Cookies missing or expired.\n\
             Use: nab fetch {url} --cookies brave"
        );
    }

    if !response.status.is_success() {
        bail!("LinkedIn returned HTTP {status} for {url}");
    }

    parse_linkedin_html(&response.body, url, kind)
}

/// Parse `LinkedIn` HTML into structured markdown content.
///
/// Extraction priority:
/// 1. `<code>` tag JSON — primary data source on 2026 `LinkedIn` SPA pages.
/// 2. JSON-LD (`<script type="application/ld+json">`) — present on some pages.
/// 3. CSS selectors — last resort; unreliable on the fully JS-rendered shell.
#[cfg(feature = "impersonate")]
fn parse_linkedin_html(html: &str, url: &str, kind: LinkedInUrlKind) -> Result<SiteContent> {
    let document = Html::parse_document(html);

    // Priority 1: <code> tag JSON (LinkedIn's 2026 SPA data embedding)
    if let Some(content) = extract_code_json(&document, url, kind) {
        return Ok(content);
    }

    // Priority 2: JSON-LD structured data (public pages)
    if let Some(content) = extract_json_ld(&document, url, kind) {
        return Ok(content);
    }

    // Priority 3: CSS selector extraction (legacy / public pages)
    extract_from_selectors(&document, url, kind)
}

/// Extract `LinkedIn` profile and post data from hidden `<code>` elements.
///
/// `LinkedIn`'s 2026 SPA architecture embeds all server-side rendered data as JSON
/// inside `<code style="display:none"><!--{...}--></code>` elements. This is the
/// only reliable extraction path for authenticated pages — the rest of the DOM is
/// a skeleton shell with no meaningful content.
///
/// The JSON comment wrapper (`<!--` / `-->`) must be stripped before parsing.
/// Returns `None` when no useful data is found across all `<code>` elements.
#[cfg(feature = "impersonate")]
fn extract_code_json(document: &Html, url: &str, kind: LinkedInUrlKind) -> Option<SiteContent> {
    let selector = Selector::parse("code").ok()?;

    let mut profile: Option<VoyagerProfileResponse> = None;
    let mut posts: Vec<String> = Vec::new();
    for element in document.select(&selector) {
        // scraper's .text() strips HTML comment nodes — use inner_html() which
        // preserves the raw "<!--{...}-->" content that LinkedIn embeds.
        let raw = element.inner_html();
        let json_str = strip_html_comment(raw.trim());
        if json_str.is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) else {
            continue;
        };

        // Walk every JSON value recursively looking for profile and post data.
        scan_json_value(&value, &mut profile, &mut posts);

        // Type 2: Pre-fetched API response envelopes — parse the body string
        if let Some(obj) = value.as_object()
            && let (Some(status), Some(body_str)) = (
                obj.get("status").and_then(serde_json::Value::as_u64),
                obj.get("body").and_then(|v| v.as_str()),
            )
            && status == 200
            && !body_str.is_empty()
            && let Ok(body_json) = serde_json::from_str::<serde_json::Value>(body_str)
        {
            scan_json_value(&body_json, &mut profile, &mut posts);
        }
    }

    build_code_json_content(url, kind, profile.as_ref(), &posts)
}

/// Recursively walk a JSON value tree looking for `LinkedIn` data objects.
///
/// `LinkedIn` embeds many small JSON blobs; relevant objects can appear at any
/// nesting depth. We search until we find a profile object (one with at least
/// `firstName` or `headline`) and collect post commentary strings.
#[cfg(feature = "impersonate")]
fn scan_json_value(
    value: &serde_json::Value,
    profile: &mut Option<VoyagerProfileResponse>,
    posts: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Object(map) => {
            // Check if this object looks like a profile — keep the richest one.
            if looks_like_profile(map) {
                let p = extract_profile_manual(map);
                let new_field_count = [
                    &p.first_name,
                    &p.last_name,
                    &p.headline,
                    &p.summary,
                    &p.location_name,
                    &p.industry_name,
                ]
                .iter()
                .filter(|f| f.is_some())
                .count();
                let old_field_count = profile.as_ref().map_or(0, |old| {
                    [
                        &old.first_name,
                        &old.last_name,
                        &old.headline,
                        &old.summary,
                        &old.location_name,
                        &old.industry_name,
                    ]
                    .iter()
                    .filter(|f| f.is_some())
                    .count()
                });
                if new_field_count > old_field_count {
                    *profile = Some(p);
                }
            }

            // Check if this object looks like a post/commentary.
            if let Some(text) = extract_post_text(map)
                && !posts.contains(&text)
            {
                posts.push(text);
            }

            // Recurse into all values.
            for v in map.values() {
                scan_json_value(v, profile, posts);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                scan_json_value(v, profile, posts);
            }
        }
        _ => {}
    }
}

/// Manually extract profile fields from a `LinkedIn` JSON object.
///
/// `LinkedIn`'s `<code>` JSON uses several naming conventions:
/// - Simple: `firstName`, `headline` (string values)
/// - Multi-locale: `multiLocaleHeadline` (object with locale keys)
/// - Nested geo: `geoLocation` → lookup in `included` array
#[cfg(feature = "impersonate")]
fn extract_profile_manual(
    map: &serde_json::Map<String, serde_json::Value>,
) -> VoyagerProfileResponse {
    /// Try to get a string value from a field, handling both plain strings
    /// and multi-locale objects like `{"en_US": "value"}`.
    fn get_str(
        map: &serde_json::Map<String, serde_json::Value>,
        key: &str,
        multi_key: &str,
    ) -> Option<String> {
        // Try plain string first
        if let Some(v) = map.get(key).and_then(|v| v.as_str())
            && !v.is_empty()
        {
            return Some(decode_html_entities(v));
        }
        // Try multi-locale object: {"en_US": "value"}
        if let Some(obj) = map.get(multi_key).and_then(|v| v.as_object()) {
            // Take the first non-empty locale value
            for v in obj.values() {
                if let Some(s) = v.as_str()
                    && !s.is_empty()
                {
                    return Some(decode_html_entities(s));
                }
            }
        }
        None
    }

    VoyagerProfileResponse {
        first_name: get_str(map, "firstName", "multiLocaleFirstName"),
        last_name: get_str(map, "lastName", "multiLocaleLastName"),
        headline: get_str(map, "headline", "multiLocaleHeadline"),
        summary: get_str(map, "summary", "multiLocaleSummary"),
        location_name: map
            .get("geoLocationName")
            .and_then(|v| v.as_str())
            .map(decode_html_entities)
            .or_else(|| {
                map.get("locationName")
                    .and_then(|v| v.as_str())
                    .map(decode_html_entities)
            }),
        industry_name: map
            .get("industryName")
            .and_then(|v| v.as_str())
            .map(decode_html_entities),
    }
}

/// Return `true` when a JSON object has enough fields to be a `LinkedIn` profile.
#[cfg(feature = "impersonate")]
fn looks_like_profile(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    let profile_keys = ["firstName", "lastName", "headline", "summary"];
    profile_keys
        .iter()
        .filter(|k| map.contains_key(**k))
        .count()
        >= 2
}

/// - `{"commentary": {"text": {"text": "..."}}}` — Voyager activity feed format
/// - `{"commentary": "..."}` — flat string commentary
/// - `{"text": {"text": "..."}}` — text wrapper
#[cfg(feature = "impersonate")]
fn extract_post_text(map: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    // Shape 1: {"commentary": {"text": {"text": "actual text"}}}
    if let Some(commentary) = map.get("commentary").and_then(|c| c.as_object()) {
        if let Some(text) = commentary
            .get("text")
            .and_then(|t| t.as_object())
            .and_then(|t| t.get("text"))
            .and_then(|t| t.as_str())
        {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        // Shape 2: {"commentary": {"text": "actual text"}} (flat)
        if let Some(text) = commentary.get("text").and_then(|t| t.as_str()) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // Shape 3: {"commentary": "actual text"} (string value)
    if let Some(text) = map.get("commentary").and_then(|c| c.as_str()) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

/// Strip HTML comment wrappers (`<!--` ... `-->`) from a string.
///
/// `LinkedIn`'s `<code>` element content is `<!--{...}-->` — the JSON is
/// wrapped in an HTML comment so browsers ignore it until JS reads it.
fn strip_html_comment(s: &str) -> &str {
    s.strip_prefix("<!--")
        .and_then(|inner| inner.strip_suffix("-->"))
        .map_or(s, str::trim)
}

/// Decode common HTML entities in a string.
///
/// Profile fields extracted from `LinkedIn`'s embedded JSON arrive pre-HTML-escaped
/// (e.g. `&amp;` instead of `&`). This helper decodes the five standard XML/HTML
/// entities that appear in practice.
fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Build `SiteContent` from extracted `<code>` JSON data.
///
/// Returns `None` when neither a profile nor any posts were found.
#[cfg(feature = "impersonate")]
fn build_code_json_content(
    url: &str,
    kind: LinkedInUrlKind,
    profile: Option<&VoyagerProfileResponse>,
    posts: &[String],
) -> Option<SiteContent> {
    let mut md = String::new();

    let (author, title) = if let Some(p) = profile {
        let name = build_full_name(p.first_name.as_deref(), p.last_name.as_deref());

        if let Some(ref n) = name {
            let _ = writeln!(md, "## {n}\n");
        }
        if let Some(ref h) = p.headline {
            let _ = writeln!(md, "{h}\n");
        }
        if let Some(ref loc) = p.location_name {
            let _ = writeln!(md, "Location: {loc}");
        }
        if let Some(ref ind) = p.industry_name {
            let _ = writeln!(md, "Industry: {ind}\n");
        } else if name.is_some() {
            md.push('\n');
        }
        if let Some(ref summary) = p.summary {
            let trimmed = summary.trim();
            if !trimmed.is_empty() {
                let _ = writeln!(md, "### About\n\n{trimmed}\n");
            }
        }
        (name.clone(), name)
    } else {
        (None, None)
    };

    if !posts.is_empty() {
        if profile.is_some() {
            let _ = writeln!(md, "### Recent Activity\n");
        }
        for post in posts.iter().take(10) {
            let _ = writeln!(md, "---\n\n{post}\n");
        }
    }

    if md.trim().is_empty() {
        return None;
    }

    let _ = writeln!(md, "[View on LinkedIn]({url})");

    Some(SiteContent {
        markdown: md,
        metadata: SiteMetadata {
            author,
            title,
            published: None,
            platform: format!("LinkedIn ({})", kind_label(kind)),
            canonical_url: url.to_string(),
            media_urls: vec![],
            engagement: None,
        },
    })
}

/// Extract content from JSON-LD (`<script type="application/ld+json">`).
#[cfg(feature = "impersonate")]
fn extract_json_ld(document: &Html, url: &str, kind: LinkedInUrlKind) -> Option<SiteContent> {
    let selector = Selector::parse(r#"script[type="application/ld+json"]"#).ok()?;

    for element in document.select(&selector) {
        let json_text = element.text().collect::<String>();
        if let Ok(ld) = serde_json::from_str::<serde_json::Value>(&json_text) {
            let name = ld
                .get("name")
                .or_else(|| ld.get("headline"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let description = ld
                .get("description")
                .or_else(|| ld.get("articleBody"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let author = ld.get("author").and_then(|a| {
                a.get("name")
                    .and_then(|n| n.as_str())
                    .map(String::from)
                    .or_else(|| a.as_str().map(String::from))
            });

            let image = ld.get("image").and_then(|i| {
                i.as_str()
                    .map(String::from)
                    .or_else(|| i.get("url").and_then(|u| u.as_str()).map(String::from))
            });

            if name.is_some() || description.is_some() {
                let mut md = String::new();
                if let Some(ref n) = name {
                    let _ = writeln!(md, "## {n}\n");
                }
                if let Some(ref a) = author {
                    let _ = writeln!(md, "by {a}\n");
                }
                if let Some(ref d) = description {
                    let _ = writeln!(md, "{d}\n");
                }
                let _ = writeln!(md, "[View on LinkedIn]({url})");

                let metadata = SiteMetadata {
                    author,
                    title: name,
                    published: ld
                        .get("datePublished")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    platform: format!("LinkedIn ({})", kind_label(kind)),
                    canonical_url: url.to_string(),
                    media_urls: image.into_iter().collect(),
                    engagement: None,
                };

                return Some(SiteContent {
                    markdown: md,
                    metadata,
                });
            }
        }
    }
    None
}

/// Extract content from HTML using CSS selectors.
#[cfg(feature = "impersonate")]
#[allow(clippy::too_many_lines)]
fn extract_from_selectors(
    document: &Html,
    url: &str,
    kind: LinkedInUrlKind,
) -> Result<SiteContent> {
    let mut md = String::new();
    let mut title = None;
    let mut author = None;

    // Profile name
    if let Ok(sel) = Selector::parse("h1")
        && let Some(el) = document.select(&sel).next()
    {
        let text = el.text().collect::<String>().trim().to_string();
        if !text.is_empty() {
            title = Some(text.clone());
            let _ = writeln!(md, "## {text}\n");
        }
    }

    // Profile headline / tagline
    for selector_str in &[
        ".text-body-medium",          // Profile headline
        ".top-card-layout__headline", // Public profile
        ".break-words",               // Various content
    ] {
        if let Ok(sel) = Selector::parse(selector_str)
            && let Some(el) = document.select(&sel).next()
        {
            let text = el.text().collect::<String>().trim().to_string();
            if !text.is_empty() && Some(&text) != title.as_ref() {
                let _ = writeln!(md, "{text}\n");
                break;
            }
        }
    }

    // About / description section
    for selector_str in &[
        "#about ~ .display-flex .pv-shared-text-with-see-more span[aria-hidden=true]",
        ".pv-about__summary-text",
        "section.summary .description",
    ] {
        if let Ok(sel) = Selector::parse(selector_str)
            && let Some(el) = document.select(&sel).next()
        {
            let text = el.text().collect::<String>().trim().to_string();
            if !text.is_empty() {
                let _ = writeln!(md, "### About\n\n{text}\n");
                break;
            }
        }
    }

    // Experience section
    if let Ok(sel) = Selector::parse("#experience ~ .pvs-list__outer-container li") {
        let items: Vec<_> = document.select(&sel).take(5).collect();
        if !items.is_empty() {
            let _ = writeln!(md, "### Experience\n");
            for item in items {
                let text = item.text().collect::<String>();
                let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !clean.is_empty() {
                    let _ = writeln!(md, "- {clean}");
                }
            }
            md.push('\n');
        }
    }

    // Post content (for post/feed pages)
    for selector_str in &[
        ".feed-shared-update-v2__description",
        ".feed-shared-text",
        ".update-components-text",
    ] {
        if let Ok(sel) = Selector::parse(selector_str) {
            for el in document.select(&sel).take(10) {
                let text = el.text().collect::<String>().trim().to_string();
                if !text.is_empty() {
                    let _ = writeln!(md, "---\n\n{text}\n");
                }
            }
        }
    }

    // Author from meta tag
    if author.is_none()
        && let Ok(sel) = Selector::parse(r#"meta[name="author"]"#)
        && let Some(el) = document.select(&sel).next()
    {
        author = el.attr("content").map(String::from);
    }

    // Page title from <title> or og:title as fallback
    if title.is_none()
        && let Ok(sel) = Selector::parse("title")
        && let Some(el) = document.select(&sel).next()
    {
        let text = el.text().collect::<String>().trim().to_string();
        // LinkedIn titles often end with " | LinkedIn"
        title = Some(
            text.strip_suffix(" | LinkedIn")
                .unwrap_or(&text)
                .to_string(),
        );
    }

    if md.trim().is_empty() {
        // Last resort: try og:description
        if let Ok(sel) = Selector::parse(r#"meta[property="og:description"]"#)
            && let Some(el) = document.select(&sel).next()
            && let Some(desc) = el.attr("content")
        {
            let _ = writeln!(md, "{desc}\n");
        }
    }

    if md.trim().is_empty() {
        bail!("Could not extract meaningful content from LinkedIn page: {url}");
    }

    let _ = writeln!(md, "[View on LinkedIn]({url})");

    let metadata = SiteMetadata {
        author,
        title,
        published: None,
        platform: format!("LinkedIn ({})", kind_label(kind)),
        canonical_url: url.to_string(),
        media_urls: extract_og_image(document),
        engagement: None,
    };

    Ok(SiteContent {
        markdown: md,
        metadata,
    })
}

#[cfg(feature = "impersonate")]
fn extract_og_image(document: &Html) -> Vec<String> {
    Selector::parse(r#"meta[property="og:image"]"#)
        .ok()
        .and_then(|sel| document.select(&sel).next())
        .and_then(|el| el.attr("content"))
        .map(|url| vec![url.to_string()])
        .unwrap_or_default()
}

/// Render a `VoyagerProfileResponse` as markdown text.
///
/// Used both by the `<code>` tag extraction path (which deserializes the same
/// field names) and directly in tests against raw Voyager-shaped JSON.
#[cfg(feature = "impersonate")]
#[must_use]
pub fn parse_voyager_profile(profile: &VoyagerProfileResponse) -> String {
    let mut md = String::new();

    let full_name = build_full_name(profile.first_name.as_deref(), profile.last_name.as_deref());
    if let Some(ref name) = full_name {
        let _ = writeln!(md, "## {name}\n");
    }
    if let Some(ref headline) = profile.headline {
        let _ = writeln!(md, "{headline}\n");
    }
    if let Some(ref location) = profile.location_name {
        let _ = writeln!(md, "Location: {location}");
    }
    if let Some(ref industry) = profile.industry_name {
        let _ = writeln!(md, "Industry: {industry}\n");
    } else {
        md.push('\n');
    }
    if let Some(ref summary) = profile.summary {
        let trimmed = summary.trim();
        if !trimmed.is_empty() {
            let _ = writeln!(md, "### About\n\n{trimmed}\n");
        }
    }

    md
}

/// Render a `VoyagerActivityResponse` as markdown text.
///
/// Skips elements without commentary (e.g. share-only items).
#[cfg(feature = "impersonate")]
#[must_use]
pub fn parse_voyager_activity(activity: &VoyagerActivityResponse) -> String {
    let mut md = String::new();

    for element in activity.elements.iter().take(10) {
        let text = element
            .value
            .as_ref()
            .and_then(|v| v.commentary.as_ref())
            .map(|c| c.text.text.trim().to_string())
            .filter(|t| !t.is_empty());

        if let Some(post_text) = text {
            let _ = writeln!(md, "---\n\n{post_text}\n");
        }
    }

    md
}

fn kind_label(kind: LinkedInUrlKind) -> &'static str {
    match kind {
        LinkedInUrlKind::Profile => "Profile",
        LinkedInUrlKind::Company => "Company",
        LinkedInUrlKind::Post => "Post",
        LinkedInUrlKind::Pulse => "Article",
        LinkedInUrlKind::FeedUpdate => "Feed Update",
        LinkedInUrlKind::Activity => "Activity",
    }
}

// ============================================================================
// oEmbed Fallback
// ============================================================================

async fn fetch_oembed(url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
    let oembed_url = format!(
        "https://www.linkedin.com/oembed?url={}&format=json",
        urlencoding::encode(url)
    );
    let response = client
        .fetch_text(&oembed_url)
        .await
        .context("Failed to fetch from LinkedIn oEmbed API")?;

    let oembed: LinkedInOEmbed =
        serde_json::from_str(&response).context("Failed to parse LinkedIn oEmbed response")?;

    let markdown = format_oembed_markdown(&oembed, url);

    let metadata = SiteMetadata {
        author: oembed.author_name.clone(),
        title: oembed.title.clone(),
        published: None,
        platform: "LinkedIn".to_string(),
        canonical_url: oembed.author_url.clone().unwrap_or_else(|| url.to_string()),
        media_urls: oembed
            .thumbnail_url
            .as_ref()
            .map(|t| vec![t.clone()])
            .unwrap_or_default(),
        engagement: None,
    };

    Ok(SiteContent { markdown, metadata })
}

/// Format `LinkedIn` oEmbed data as markdown.
fn format_oembed_markdown(oembed: &LinkedInOEmbed, url: &str) -> String {
    let mut md = String::new();

    if let Some(title) = &oembed.title {
        let _ = writeln!(md, "## {title}\n");
    }

    if let Some(author) = &oembed.author_name {
        let _ = writeln!(md, "by {author}\n");
    }

    if let Some(thumb) = &oembed.thumbnail_url {
        let _ = writeln!(md, "![LinkedIn post]({thumb})\n");
    }

    if let Some(html) = &oembed.html {
        let text = strip_html(html);
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let _ = writeln!(md, "{trimmed}\n");
        }
    }

    let _ = writeln!(md, "[View on LinkedIn]({url})");

    md
}

/// Strip HTML tags for plain text display.
fn strip_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

// ============================================================================
// Types
// ============================================================================

// ── Voyager API response types ───────────────────────────────────────────────

/// Top-level Voyager profile response (`/voyager/api/identity/profiles/{id}`).
///
/// Fields marked `serde(default)` tolerate partial responses — the Voyager API
/// omits fields that are empty rather than setting them to null.
#[cfg(feature = "impersonate")]
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VoyagerProfileResponse {
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub headline: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub industry_name: Option<String>,
    #[serde(rename = "geoLocationName", default)]
    pub location_name: Option<String>,
}

/// Top-level Voyager activity/feed response (`/voyager/api/feed/updates?profileUrn=...`).
#[cfg(feature = "impersonate")]
#[derive(Debug, Deserialize, Default)]
pub struct VoyagerActivityResponse {
    #[serde(default)]
    pub elements: Vec<VoyagerFeedElement>,
}

/// Single feed element inside a Voyager activity response.
#[cfg(feature = "impersonate")]
#[derive(Debug, Deserialize, Default)]
pub struct VoyagerFeedElement {
    /// The actual update payload; absent for share-only items without commentary.
    #[serde(default)]
    pub value: Option<VoyagerUpdateValue>,
}

/// The `value` object inside a feed element.
#[cfg(feature = "impersonate")]
#[derive(Debug, Deserialize, Default)]
pub struct VoyagerUpdateValue {
    /// Author's written text for this post.
    #[serde(default)]
    pub commentary: Option<VoyagerCommentary>,
}

/// Text commentary attached to a feed update.
#[cfg(feature = "impersonate")]
#[derive(Debug, Deserialize, Default)]
pub struct VoyagerCommentary {
    pub text: VoyagerText,
}

/// Plain-text wrapper inside Voyager commentary.
#[cfg(feature = "impersonate")]
#[derive(Debug, Deserialize, Default)]
pub struct VoyagerText {
    #[serde(default)]
    pub text: String,
}

// ── oEmbed types ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LinkedInOEmbed {
    title: Option<String>,
    author_name: Option<String>,
    author_url: Option<String>,
    thumbnail_url: Option<String>,
    html: Option<String>,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── URL Classification ──────────────────────────────────────────────

    #[test]
    fn classifies_profile_urls() {
        assert_eq!(
            classify_linkedin_url("https://www.linkedin.com/in/mikko-parkkola/"),
            Some(LinkedInUrlKind::Profile)
        );
        assert_eq!(
            classify_linkedin_url("https://linkedin.com/in/someuser"),
            Some(LinkedInUrlKind::Profile)
        );
    }

    #[test]
    fn classifies_company_urls() {
        assert_eq!(
            classify_linkedin_url("https://www.linkedin.com/company/anthropic/"),
            Some(LinkedInUrlKind::Company)
        );
    }

    #[test]
    fn classifies_post_urls() {
        assert_eq!(
            classify_linkedin_url(
                "https://www.linkedin.com/posts/someuser_topic-activity-123456789"
            ),
            Some(LinkedInUrlKind::Post)
        );
    }

    #[test]
    fn classifies_pulse_urls() {
        assert_eq!(
            classify_linkedin_url("https://www.linkedin.com/pulse/some-article-title-author"),
            Some(LinkedInUrlKind::Pulse)
        );
    }

    #[test]
    fn classifies_feed_update_urls() {
        assert_eq!(
            classify_linkedin_url(
                "https://www.linkedin.com/feed/update/urn:li:activity:7654321098765432109"
            ),
            Some(LinkedInUrlKind::FeedUpdate)
        );
    }

    #[test]
    fn classifies_activity_urls() {
        assert_eq!(
            classify_linkedin_url(
                "https://www.linkedin.com/in/mikko-parkkola/recent-activity/all/"
            ),
            Some(LinkedInUrlKind::Activity)
        );
    }

    #[test]
    fn handles_query_params() {
        assert_eq!(
            classify_linkedin_url("https://www.linkedin.com/in/user?utm_source=share"),
            Some(LinkedInUrlKind::Profile)
        );
    }

    #[test]
    fn rejects_non_linkedin_urls() {
        assert_eq!(
            classify_linkedin_url("https://youtube.com/watch?v=abc"),
            None
        );
        assert_eq!(
            classify_linkedin_url("https://twitter.com/user/status/123"),
            None
        );
    }

    #[test]
    fn rejects_bare_linkedin() {
        assert_eq!(classify_linkedin_url("https://www.linkedin.com/"), None);
        // /in/ without a username segment
        assert_eq!(classify_linkedin_url("https://www.linkedin.com/in/"), None);
    }

    // ── URL Matching ────────────────────────────────────────────────────

    #[test]
    fn matches_all_linkedin_url_kinds() {
        let provider = LinkedInProvider;
        assert!(provider.matches("https://www.linkedin.com/in/someuser"));
        assert!(provider.matches("https://www.linkedin.com/company/somecompany"));
        assert!(provider.matches("https://www.linkedin.com/posts/user_title-123"));
        assert!(provider.matches("https://www.linkedin.com/pulse/article-title"));
        assert!(provider.matches("https://www.linkedin.com/feed/update/urn:li:activity:123"));
        assert!(provider.matches("https://www.linkedin.com/in/user/recent-activity/all/"));
    }

    #[test]
    fn does_not_match_non_linkedin() {
        let provider = LinkedInProvider;
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
        assert!(!provider.matches("https://twitter.com/user/status/123"));
    }

    // ── Kind Properties ─────────────────────────────────────────────────

    #[test]
    fn auth_required_kinds() {
        assert!(LinkedInUrlKind::Profile.requires_auth());
        assert!(LinkedInUrlKind::Company.requires_auth());
        assert!(LinkedInUrlKind::Activity.requires_auth());
        assert!(!LinkedInUrlKind::Post.requires_auth());
        assert!(!LinkedInUrlKind::Pulse.requires_auth());
    }

    #[test]
    fn oembed_fallback_kinds() {
        assert!(LinkedInUrlKind::Post.has_oembed_fallback());
        assert!(LinkedInUrlKind::Pulse.has_oembed_fallback());
        assert!(LinkedInUrlKind::FeedUpdate.has_oembed_fallback());
        assert!(!LinkedInUrlKind::Profile.has_oembed_fallback());
    }

    // ── HTML Stripping ──────────────────────────────────────────────────

    #[test]
    fn strip_html_removes_tags() {
        assert_eq!(strip_html("<p>Hello <b>world</b></p>"), "Hello world");
    }

    #[test]
    fn strip_html_decodes_entities() {
        assert_eq!(strip_html("&amp; &lt; &gt;"), "& < >");
    }

    // ── oEmbed Formatting ───────────────────────────────────────────────

    #[test]
    fn format_oembed_with_full_data() {
        let oembed = LinkedInOEmbed {
            title: Some("The Future of Rust".to_string()),
            author_name: Some("Jane Engineer".to_string()),
            author_url: Some("https://www.linkedin.com/in/janeengineer".to_string()),
            thumbnail_url: Some("https://media.linkedin.com/thumb.jpg".to_string()),
            html: Some("<p>Great insights on systems programming.</p>".to_string()),
        };

        let url = "https://www.linkedin.com/posts/janeengineer_rust-123";
        let md = format_oembed_markdown(&oembed, url);

        assert!(md.contains("## The Future of Rust"));
        assert!(md.contains("by Jane Engineer"));
        assert!(md.contains("![LinkedIn post](https://media.linkedin.com/thumb.jpg)"));
        assert!(md.contains("Great insights on systems programming."));
        assert!(md.contains("[View on LinkedIn]"));
    }

    #[test]
    fn format_oembed_with_minimal_data() {
        let oembed = LinkedInOEmbed {
            title: None,
            author_name: Some("John Doe".to_string()),
            author_url: None,
            thumbnail_url: None,
            html: None,
        };

        let url = "https://www.linkedin.com/posts/johndoe_post-456";
        let md = format_oembed_markdown(&oembed, url);

        assert!(!md.contains("##"));
        assert!(md.contains("by John Doe"));
        assert!(!md.contains("!["));
        assert!(md.contains("[View on LinkedIn]"));
    }

    // ── Voyager helpers ─────────────────────────────────────────────────

    #[test]
    fn extract_csrf_token_with_quotes() {
        // GIVEN: JSESSIONID stored with surrounding quotes (standard LinkedIn format)
        let cookies = r#"li_at=AQEDARabcd; JSESSIONID="ajax:1234567890""#;
        // WHEN
        let token = extract_csrf_token(cookies);
        // THEN: quotes stripped
        assert_eq!(token, Some("ajax:1234567890".to_string()));
    }

    #[test]
    fn extract_csrf_token_without_quotes() {
        // GIVEN: JSESSIONID without surrounding quotes (some client formats)
        let cookies = "li_at=AQEDARabcd; JSESSIONID=ajax:9876543210";
        // WHEN
        let token = extract_csrf_token(cookies);
        // THEN: value returned as-is
        assert_eq!(token, Some("ajax:9876543210".to_string()));
    }

    #[test]
    fn extract_csrf_token_missing_jsessionid() {
        // GIVEN: cookies without JSESSIONID
        let cookies = "li_at=AQEDARabcd; lang=en";
        // WHEN / THEN: None returned
        assert_eq!(extract_csrf_token(cookies), None);
    }

    #[test]
    fn extract_csrf_token_case_insensitive_key() {
        // GIVEN: key casing varies
        let cookies = r#"Jsessionid="ajax:5555""#;
        assert_eq!(extract_csrf_token(cookies), Some("ajax:5555".to_string()));
    }

    #[test]
    fn extract_username_simple_profile_url() {
        // GIVEN: canonical profile URL
        let url = "https://www.linkedin.com/in/mikko-parkkola/";
        // WHEN
        let username = extract_username_from_url(url);
        // THEN
        assert_eq!(username, Some("mikko-parkkola".to_string()));
    }

    #[test]
    fn extract_username_strips_query_string() {
        // GIVEN: URL with query params
        let url = "https://www.linkedin.com/in/someuser?utm_source=share";
        assert_eq!(extract_username_from_url(url), Some("someuser".to_string()));
    }

    #[test]
    fn extract_username_activity_subpath() {
        // GIVEN: activity subpath after username
        let url = "https://www.linkedin.com/in/johndoe/recent-activity/all/";
        assert_eq!(extract_username_from_url(url), Some("johndoe".to_string()));
    }

    #[test]
    fn extract_username_non_profile_url() {
        // GIVEN: company URL (no /in/ segment)
        let url = "https://www.linkedin.com/company/anthropic/";
        assert_eq!(extract_username_from_url(url), None);
    }

    // ── Voyager parsers (impersonate feature) ────────────────────────────

    #[cfg(feature = "impersonate")]
    #[test]
    fn parse_voyager_profile_full_response() {
        // GIVEN: full Voyager profile JSON
        let json = r#"{
            "firstName": "Jane",
            "lastName": "Engineer",
            "headline": "Staff Engineer at Acme Corp",
            "summary": "Passionate about distributed systems and Rust.",
            "industryName": "Computer Software",
            "geoLocationName": "San Francisco, California"
        }"#;
        let profile: VoyagerProfileResponse = serde_json::from_str(json).unwrap();

        // WHEN
        let md = parse_voyager_profile(&profile);

        // THEN
        assert!(md.contains("## Jane Engineer"));
        assert!(md.contains("Staff Engineer at Acme Corp"));
        assert!(md.contains("Location: San Francisco, California"));
        assert!(md.contains("Industry: Computer Software"));
        assert!(md.contains("### About"));
        assert!(md.contains("Passionate about distributed systems and Rust."));
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn parse_voyager_profile_minimal_response() {
        // GIVEN: partial response — only first name available
        let json = r#"{"firstName": "Jane"}"#;
        let profile: VoyagerProfileResponse = serde_json::from_str(json).unwrap();

        // WHEN
        let md = parse_voyager_profile(&profile);

        // THEN: no panic, renders what's available
        assert!(md.contains("## Jane"));
        assert!(!md.contains("Industry:"));
        assert!(!md.contains("### About"));
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn parse_voyager_profile_empty_summary_omitted() {
        // GIVEN: summary present but blank
        let json = r#"{"firstName": "Bob", "summary": "   "}"#;
        let profile: VoyagerProfileResponse = serde_json::from_str(json).unwrap();

        // WHEN
        let md = parse_voyager_profile(&profile);

        // THEN: About section not emitted for blank summary
        assert!(!md.contains("### About"));
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn parse_voyager_activity_with_posts() {
        // GIVEN: activity feed with two posts
        let json = r#"{
            "elements": [
                {
                    "value": {
                        "commentary": {
                            "text": { "text": "First post content here." }
                        }
                    }
                },
                {
                    "value": {
                        "commentary": {
                            "text": { "text": "Second post content here." }
                        }
                    }
                }
            ]
        }"#;
        let activity: VoyagerActivityResponse = serde_json::from_str(json).unwrap();

        // WHEN
        let md = parse_voyager_activity(&activity);

        // THEN
        assert!(md.contains("First post content here."));
        assert!(md.contains("Second post content here."));
        assert_eq!(md.matches("---").count(), 2);
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn parse_voyager_activity_skips_elements_without_commentary() {
        // GIVEN: mix of posts with and without commentary (e.g., share-only items)
        let json = r#"{
            "elements": [
                { "value": null },
                {
                    "value": {
                        "commentary": {
                            "text": { "text": "Real post text." }
                        }
                    }
                }
            ]
        }"#;
        let activity: VoyagerActivityResponse = serde_json::from_str(json).unwrap();

        // WHEN
        let md = parse_voyager_activity(&activity);

        // THEN: only the real post is rendered
        assert!(md.contains("Real post text."));
        assert_eq!(md.matches("---").count(), 1);
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn parse_voyager_activity_empty_feed() {
        // GIVEN: empty elements array
        let json = r#"{"elements": []}"#;
        let activity: VoyagerActivityResponse = serde_json::from_str(json).unwrap();

        // WHEN
        let md = parse_voyager_activity(&activity);

        // THEN: empty string, no separator
        assert!(md.trim().is_empty());
    }

    // ── HTML Parser (impersonate feature) ───────────────────────────────

    #[cfg(feature = "impersonate")]
    #[test]
    fn parses_json_ld_profile() {
        let html = r#"
        <html>
        <head>
            <script type="application/ld+json">
            {
                "@type": "Person",
                "name": "Mikko Parkkola",
                "description": "Building things with Rust and AI",
                "image": "https://media.linkedin.com/photo.jpg"
            }
            </script>
        </head>
        <body></body>
        </html>
        "#;

        let content = parse_linkedin_html(
            html,
            "https://linkedin.com/in/mikko",
            LinkedInUrlKind::Profile,
        )
        .unwrap();
        assert!(content.markdown.contains("## Mikko Parkkola"));
        assert!(
            content
                .markdown
                .contains("Building things with Rust and AI")
        );
        assert_eq!(content.metadata.platform, "LinkedIn (Profile)");
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn falls_back_to_selectors() {
        let html = r#"
        <html>
        <head>
            <title>Mikko Parkkola | LinkedIn</title>
            <meta property="og:description" content="Rust developer and AI enthusiast">
            <meta property="og:image" content="https://media.linkedin.com/photo.jpg">
        </head>
        <body>
            <h1>Mikko Parkkola</h1>
            <div class="text-body-medium">Senior Engineer at Some Company</div>
        </body>
        </html>
        "#;

        let content = parse_linkedin_html(
            html,
            "https://linkedin.com/in/mikko",
            LinkedInUrlKind::Profile,
        )
        .unwrap();
        assert!(content.markdown.contains("## Mikko Parkkola"));
        assert!(content.markdown.contains("Senior Engineer at Some Company"));
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn og_description_fallback() {
        let html = r#"
        <html>
        <head>
            <meta property="og:description" content="This is the only content available">
        </head>
        <body></body>
        </html>
        "#;

        let content = parse_linkedin_html(
            html,
            "https://linkedin.com/in/user",
            LinkedInUrlKind::Profile,
        )
        .unwrap();
        assert!(
            content
                .markdown
                .contains("This is the only content available")
        );
    }

    // ── strip_html_comment ──────────────────────────────────────────────

    #[test]
    fn strip_comment_removes_html_comment_wrapper() {
        // GIVEN: string wrapped in HTML comment (LinkedIn's <code> element format)
        let input = r#"<!--{"firstName":"Jane"}-->"#;
        // WHEN
        let result = strip_html_comment(input);
        // THEN: JSON is exposed without wrappers
        assert_eq!(result, r#"{"firstName":"Jane"}"#);
    }

    #[test]
    fn strip_comment_trims_whitespace_inside_comment() {
        // GIVEN: comment wrapper with surrounding whitespace
        let input = "<!--  {\"key\":\"value\"}  -->";
        let result = strip_html_comment(input);
        assert_eq!(result, r#"{"key":"value"}"#);
    }

    #[test]
    fn strip_comment_passthrough_when_no_comment_wrapper() {
        // GIVEN: plain JSON (no comment wrapper)
        let input = r#"{"firstName":"Jane"}"#;
        let result = strip_html_comment(input);
        assert_eq!(result, input);
    }

    #[test]
    fn strip_comment_passthrough_empty_string() {
        assert_eq!(strip_html_comment(""), "");
    }

    // ── extract_post_text ───────────────────────────────────────────────

    #[cfg(feature = "impersonate")]
    #[test]
    fn extract_post_text_voyager_nested_shape() {
        // GIVEN: {"commentary": {"text": {"text": "actual text"}}} (Voyager activity shape)
        let mut map = serde_json::Map::new();
        let mut commentary = serde_json::Map::new();
        let mut text_inner = serde_json::Map::new();
        text_inner.insert("text".into(), serde_json::json!("Voyager nested post text"));
        commentary.insert("text".into(), serde_json::Value::Object(text_inner));
        map.insert("commentary".into(), serde_json::Value::Object(commentary));

        // WHEN
        let result = extract_post_text(&map);
        // THEN
        assert_eq!(result.as_deref(), Some("Voyager nested post text"));
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn extract_post_text_flat_commentary_text() {
        // GIVEN: {"commentary": {"text": "flat text"}}
        let mut map = serde_json::Map::new();
        let mut commentary = serde_json::Map::new();
        commentary.insert("text".into(), serde_json::json!("Flat commentary text"));
        map.insert("commentary".into(), serde_json::Value::Object(commentary));

        let result = extract_post_text(&map);
        assert_eq!(result.as_deref(), Some("Flat commentary text"));
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn extract_post_text_string_commentary() {
        // GIVEN: {"commentary": "direct string"}
        let mut map = serde_json::Map::new();
        map.insert("commentary".into(), serde_json::json!("Direct string post"));

        let result = extract_post_text(&map);
        assert_eq!(result.as_deref(), Some("Direct string post"));
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn extract_post_text_returns_none_when_absent() {
        // GIVEN: object with no commentary field
        let mut map = serde_json::Map::new();
        map.insert("firstName".into(), serde_json::json!("Jane"));

        let result = extract_post_text(&map);
        assert!(result.is_none());
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn extract_post_text_skips_blank_commentary() {
        // GIVEN: commentary with only whitespace
        let mut map = serde_json::Map::new();
        map.insert("commentary".into(), serde_json::json!("   "));

        let result = extract_post_text(&map);
        assert!(result.is_none());
    }

    // ── looks_like_profile ──────────────────────────────────────────────

    #[cfg(feature = "impersonate")]
    #[test]
    fn looks_like_profile_with_two_profile_keys() {
        // GIVEN: object with firstName + headline (2 profile keys — minimum threshold)
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"firstName":"Jane","headline":"Staff Engineer"}"#).unwrap();
        assert!(looks_like_profile(&map));
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn looks_like_profile_rejects_single_profile_key() {
        // GIVEN: only one profile key present — insufficient signal
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"firstName":"Jane","unrelated":"data"}"#).unwrap();
        assert!(!looks_like_profile(&map));
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn looks_like_profile_rejects_non_profile_object() {
        // GIVEN: unrelated JSON object
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"color":"blue","size":42}"#).unwrap();
        assert!(!looks_like_profile(&map));
    }

    // ── <code> tag JSON extraction (integration) ────────────────────────

    #[cfg(feature = "impersonate")]
    #[test]
    fn code_tag_extraction_profile_data() {
        // GIVEN: HTML with LinkedIn-style <code> element containing profile JSON
        let profile_json = r#"{"firstName":"Jane","lastName":"Engineer","headline":"Staff Engineer at Acme","summary":"Building systems in Rust.","geoLocationName":"Helsinki, Finland","industryName":"Computer Software"}"#;
        let html = format!(
            r#"<!DOCTYPE html><html><head></head><body>
            <code style="display:none" id="bpr-guid-1"><!—{profile_json}—></code>
            <code style="display:none" id="bpr-guid-2"><!--{profile_json}--></code>
            </body></html>"#
        );

        // WHEN
        let content = parse_linkedin_html(
            &html,
            "https://linkedin.com/in/janeengineer",
            LinkedInUrlKind::Profile,
        )
        .unwrap();

        // THEN: profile fields appear in markdown
        assert!(
            content.markdown.contains("## Jane Engineer"),
            "Missing name: {}",
            content.markdown
        );
        assert!(
            content.markdown.contains("Staff Engineer at Acme"),
            "Missing headline: {}",
            content.markdown
        );
        assert!(
            content.markdown.contains("Helsinki, Finland"),
            "Missing location: {}",
            content.markdown
        );
        assert!(
            content.markdown.contains("Computer Software"),
            "Missing industry: {}",
            content.markdown
        );
        assert!(
            content.markdown.contains("Building systems in Rust."),
            "Missing summary: {}",
            content.markdown
        );
        assert_eq!(content.metadata.platform, "LinkedIn (Profile)");
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn code_tag_extraction_post_commentary() {
        // GIVEN: HTML with <code> elements containing post commentary JSON
        let post_json = r#"{"commentary":{"text":{"text":"Just shipped Rust HTTP/3 client."}},"actor":{"name":"Jane Engineer"}}"#;
        let html = format!(
            r#"<!DOCTYPE html><html><head></head><body>
            <code style="display:none" id="bpr-guid-1"><!--{post_json}--></code>
            </body></html>"#
        );

        // WHEN
        let content = parse_linkedin_html(
            &html,
            "https://linkedin.com/posts/jane_rust-123",
            LinkedInUrlKind::Post,
        )
        .unwrap();

        // THEN
        assert!(
            content
                .markdown
                .contains("Just shipped Rust HTTP/3 client."),
            "Missing post text: {}",
            content.markdown
        );
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn code_tag_extraction_deduplicates_posts() {
        // GIVEN: same post JSON appears in two different <code> elements
        let post_json = r#"{"commentary":"Unique post text."}"#;
        let html = format!(
            r"<!DOCTYPE html><html><head></head><body>
            <code><!--{post_json}--></code>
            <code><!--{post_json}--></code>
            </body></html>"
        );

        // WHEN
        let content = parse_linkedin_html(
            &html,
            "https://linkedin.com/posts/user_post-123",
            LinkedInUrlKind::Post,
        )
        .unwrap();

        // THEN: appears exactly once
        assert_eq!(
            content.markdown.matches("Unique post text.").count(),
            1,
            "Expected 1 occurrence but got more: {}",
            content.markdown
        );
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn code_tag_extraction_falls_through_to_json_ld_when_empty() {
        // GIVEN: <code> elements with no useful data, but JSON-LD present
        let html = r#"<!DOCTYPE html><html><head>
            <script type="application/ld+json">
            {"@type":"Person","name":"Fallback Person","description":"JSON-LD description"}
            </script>
        </head><body>
            <code><!--{"irrelevant":"noise"}--></code>
        </body></html>"#;

        // WHEN
        let content = parse_linkedin_html(
            html,
            "https://linkedin.com/in/fallback",
            LinkedInUrlKind::Profile,
        )
        .unwrap();

        // THEN: JSON-LD fallback used
        assert!(
            content.markdown.contains("## Fallback Person"),
            "Expected JSON-LD fallback: {}",
            content.markdown
        );
        assert!(
            content.markdown.contains("JSON-LD description"),
            "Expected JSON-LD desc: {}",
            content.markdown
        );
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn code_tag_extraction_handles_malformed_json_gracefully() {
        // GIVEN: <code> elements with broken JSON mixed with valid JSON-LD
        let html = r#"<!DOCTYPE html><html><head>
            <meta property="og:description" content="og fallback works">
        </head><body>
            <code><!--{broken json--></code>
            <code><!--not json at all--></code>
        </body></html>"#;

        // WHEN — should not panic, falls through to og:description
        let content = parse_linkedin_html(
            html,
            "https://linkedin.com/in/user",
            LinkedInUrlKind::Profile,
        )
        .unwrap();

        // THEN: og fallback used
        assert!(
            content.markdown.contains("og fallback works"),
            "Expected og fallback: {}",
            content.markdown
        );
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn code_tag_extraction_nested_profile_in_object() {
        // GIVEN: profile data nested inside a larger wrapper object
        let html = r#"<!DOCTYPE html><html><head></head><body>
            <code><!--{"data":{"profile":{"firstName":"Nested","lastName":"Profile","headline":"CTO at Example","summary":"Led engineering teams for a decade."}}}--></code>
        </body></html>"#;

        // WHEN
        let content = parse_linkedin_html(
            html,
            "https://linkedin.com/in/nested",
            LinkedInUrlKind::Profile,
        )
        .unwrap();

        // THEN: recursive scan finds the nested profile
        assert!(
            content.markdown.contains("## Nested Profile"),
            "Missing nested profile: {}",
            content.markdown
        );
        assert!(
            content.markdown.contains("CTO at Example"),
            "Missing headline: {}",
            content.markdown
        );
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn code_tag_profile_without_industry_still_renders() {
        // GIVEN: profile missing industryName
        let html = r#"<!DOCTYPE html><html><head></head><body>
            <code><!--{"firstName":"Alice","lastName":"Smith","headline":"Engineer"}--></code>
        </body></html>"#;

        let content = parse_linkedin_html(
            html,
            "https://linkedin.com/in/alice",
            LinkedInUrlKind::Profile,
        )
        .unwrap();

        assert!(content.markdown.contains("## Alice Smith"));
        assert!(content.markdown.contains("Engineer"));
        assert!(!content.markdown.contains("Industry:"));
    }
}
