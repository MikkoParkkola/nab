// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Authenticated `LinkedIn` extraction via TLS fingerprint impersonation.
//!
//! All functions in this module are gated behind `#[cfg(feature = "impersonate")]`.
//!
//! Primary parsing strategy: `<code>` tag JSON extraction. `LinkedIn` serves a 1.3 MB
//! SPA shell with no server-rendered CSS-selectable content. All profile and feed data
//! is embedded as JSON inside hidden `<code>` elements:
//! `<code style="display:none" id="bpr-guid-XXXX"><!--{...}--></code>`.
//! JSON-LD and CSS selectors are tried as fallbacks only.

use std::fmt::Write as _;

use anyhow::{Result, bail};
use scraper::{Html, Selector};

use super::helpers::{build_full_name, decode_html_entities, kind_label, strip_html_comment};
use super::types::VoyagerProfileResponse;
use super::url::LinkedInUrlKind;
use crate::site::{SiteContent, SiteMetadata};

// ── Top-level entry ──────────────────────────────────────────────────────────

/// Top-level authenticated extraction: fetch HTML, parse `<code>` JSON first.
///
/// The Voyager REST API (`/voyager/api/identity/profiles/{id}`) was deprecated
/// and returns HTTP 410 Gone. `LinkedIn` now embeds all profile/feed data as JSON
/// inside hidden `<code>` elements in the initial HTML response — that is the
/// only reliable server-side data source.
pub(super) async fn fetch_authenticated(
    url: &str,
    cookies: &str,
    kind: LinkedInUrlKind,
) -> Result<SiteContent> {
    // Activity feeds are not in the SSR HTML — `LinkedIn` loads them
    // client-side from `/voyager/api/feed/updatesV2`. Try that first; fall
    // back to SSR scraping (which still surfaces profile chrome) only if
    // Voyager declines to return posts.
    if matches!(kind, LinkedInUrlKind::Activity) {
        match super::voyager::fetch_activity_via_voyager(url, cookies).await {
            Ok(Some(content)) => return Ok(content),
            Ok(None) => {
                tracing::debug!("Voyager returned no posts for {}; falling back to SSR", url);
            }
            Err(e) => {
                tracing::warn!("Voyager activity fetch failed for {}: {}", url, e);
            }
        }
    }

    fetch_authenticated_html(url, cookies, kind).await
}

/// Fetch a `LinkedIn` URL via impersonated HTTP, then parse the HTML.
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

// ── HTML parsing ─────────────────────────────────────────────────────────────

/// Parse `LinkedIn` HTML into structured markdown content.
///
/// Extraction priority:
/// 1. `<code>` tag JSON — primary data source on 2026 `LinkedIn` SPA pages.
/// 2. JSON-LD (`<script type="application/ld+json">`) — present on some pages.
/// 3. CSS selectors — last resort; unreliable on the fully JS-rendered shell.
pub(super) fn parse_linkedin_html(
    html: &str,
    url: &str,
    kind: LinkedInUrlKind,
) -> Result<SiteContent> {
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

// ── <code> tag JSON extraction ────────────────────────────────────────────────

/// Extract `LinkedIn` profile and post data from hidden `<code>` elements.
///
/// `LinkedIn`'s 2026 SPA architecture embeds all server-side rendered data as JSON
/// inside `<code style="display:none"><!--{...}--></code>` elements. This is the
/// only reliable extraction path for authenticated pages — the rest of the DOM is
/// a skeleton shell with no meaningful content.
///
/// The JSON comment wrapper (`<!--` / `-->`) must be stripped before parsing.
/// Returns `None` when no useful data is found across all `<code>` elements.
fn extract_code_json(document: &Html, url: &str, kind: LinkedInUrlKind) -> Option<SiteContent> {
    let selector = Selector::parse("code").ok()?;

    let mut profile: Option<VoyagerProfileResponse> = None;
    let mut posts: Vec<String> = Vec::new();
    // Rich structured activity-feed markdown extracted from an embedded Voyager
    // feed envelope (`{data, included:[…]}`). Preferred over the text-only
    // `posts` fallback because it carries author, urn, timestamp, and
    // engagement per post. See `extract_activity_envelope`.
    let mut activity_md: Option<String> = None;
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

        // Priority for activity pages: a hidden <code> may carry the pre-fetched
        // Voyager feed envelope LinkedIn would otherwise load over XHR. Route it
        // through the shared rich walker so embedded and XHR paths agree.
        if activity_md.is_none()
            && let Some(md) = extract_activity_envelope(&value, json_str, profile.as_ref())
        {
            activity_md = Some(md);
        }

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
            if activity_md.is_none()
                && let Some(md) = extract_activity_envelope(&body_json, body_str, profile.as_ref())
            {
                activity_md = Some(md);
            }
            scan_json_value(&body_json, &mut profile, &mut posts);
        }
    }

    build_code_json_content(url, kind, profile.as_ref(), &posts, activity_md.as_deref())
}

/// Detect and render a pre-fetched Voyager activity-feed envelope embedded in a
/// hidden `<code>` element on `/recent-activity/all/`.
///
/// `LinkedIn`'s SPA inlines the first feed XHR response (`{data, included:[…]}`)
/// into the SSR HTML to avoid a client round-trip. When that envelope is
/// present, this routes the raw JSON through the shared
/// [`super::voyager::render_voyager_body`] walker — the same code the live XHR
/// path uses — so the embedded path yields fully structured posts (author,
/// activity URN, snowflake-derived timestamp, engagement counts) instead of the
/// text-only commentary fallback.
///
/// `value` is the parsed JSON; `raw` is the original string passed straight to
/// the walker (which re-parses internally). `profile` supplies the expected
/// author name used to flag reshares. Returns `None` when no `included[]` feed
/// array with `feed.Update` entities is present.
fn extract_activity_envelope(
    value: &serde_json::Value,
    raw: &str,
    profile: Option<&VoyagerProfileResponse>,
) -> Option<String> {
    // Cheap structural gate before the expensive walker: must have an
    // `included` array carrying at least one feed Update entity.
    let included = value.get("included")?.as_array()?;
    let has_update = included.iter().any(|e| {
        e.get("$type").and_then(serde_json::Value::as_str)
            == Some("com.linkedin.voyager.dash.feed.Update")
    });
    if !has_update {
        return None;
    }

    let expected_actor = profile
        .and_then(|p| build_full_name(p.first_name.as_deref(), p.last_name.as_deref()))
        .unwrap_or_default();

    let md = super::voyager::render_voyager_body(raw, &expected_actor);
    if md.trim().is_empty() { None } else { Some(md) }
}

/// Recursively walk a JSON value tree looking for `LinkedIn` data objects.
///
/// `LinkedIn` embeds many small JSON blobs; relevant objects can appear at any
/// nesting depth. We search until we find a profile object (one with at least
/// `firstName` or `headline`) and collect post commentary strings.
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
                let new_field_count = count_profile_fields(&p);
                let old_field_count = profile.as_ref().map_or(0, count_profile_fields);
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

/// Count how many optional profile fields are populated (for richness comparison).
fn count_profile_fields(p: &VoyagerProfileResponse) -> usize {
    [
        &p.first_name,
        &p.last_name,
        &p.headline,
        &p.summary,
        &p.location_name,
        &p.industry_name,
    ]
    .iter()
    .filter(|f| f.is_some())
    .count()
}

/// Manually extract profile fields from a `LinkedIn` JSON object.
///
/// `LinkedIn`'s `<code>` JSON uses several naming conventions:
/// - Simple: `firstName`, `headline` (string values)
/// - Multi-locale: `multiLocaleHeadline` (object with locale keys)
/// - Nested geo: `geoLocation` → lookup in `included` array
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
pub(super) fn looks_like_profile(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    let profile_keys = ["firstName", "lastName", "headline", "summary"];
    profile_keys
        .iter()
        .filter(|k| map.contains_key(**k))
        .count()
        >= 2
}

/// Extract post text from a commentary JSON object in one of three shapes:
/// - `{"commentary": {"text": {"text": "..."}}}` — Voyager activity feed format
/// - `{"commentary": {"text": "..."}}` — flat string commentary
/// - `{"commentary": "..."}` — direct string
pub(super) fn extract_post_text(
    map: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
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

/// Build `SiteContent` from extracted `<code>` JSON data.
///
/// `activity_md`, when present, is the richly structured activity-feed markdown
/// rendered from an embedded Voyager envelope (see `extract_activity_envelope`).
/// It is preferred over the text-only `posts` fallback so activity pages surface
/// author, urn, timestamp, and engagement per post.
///
/// Returns `None` when no profile, structured activity, or post text was found.
fn build_code_json_content(
    url: &str,
    kind: LinkedInUrlKind,
    profile: Option<&VoyagerProfileResponse>,
    posts: &[String],
    activity_md: Option<&str>,
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

    // Prefer the structured activity envelope when present; otherwise fall back
    // to the text-only commentary collection.
    if let Some(activity) = activity_md.map(str::trim).filter(|s| !s.is_empty()) {
        let _ = writeln!(md, "### Recent Activity\n");
        let _ = writeln!(md, "{activity}");
    } else if !posts.is_empty() {
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

// ── JSON-LD extraction ────────────────────────────────────────────────────────

/// Extract content from JSON-LD (`<script type="application/ld+json">`).
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

// ── CSS selector extraction ───────────────────────────────────────────────────

/// Extract content from HTML using CSS selectors (last-resort fallback).
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

fn extract_og_image(document: &Html) -> Vec<String> {
    Selector::parse(r#"meta[property="og:image"]"#)
        .ok()
        .and_then(|sel| document.select(&sel).next())
        .and_then(|el| el.attr("content"))
        .map(|url| vec![url.to_string()])
        .unwrap_or_default()
}
