//! `LinkedIn` content extraction.
//!
//! Supports two extraction paths:
//! 1. **Authenticated** (default with `impersonate` feature): Uses TLS fingerprint
//!    impersonation via `rquest` to bypass `LinkedIn`'s JA3/JA4 bot detection.
//!    Required for profiles, companies, and activity pages. Falls back to oEmbed
//!    for posts/pulse when authentication fails.
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
    ) -> Result<SiteContent> {
        let kind = classify_linkedin_url(url)
            .context("URL does not match any LinkedIn pattern")?;

        // Try authenticated extraction first (requires impersonate feature + cookies)
        #[cfg(feature = "impersonate")]
        {
            if let Some(cookie_header) = cookies
                && !cookie_header.is_empty()
            {
                match fetch_authenticated(url, cookie_header, kind).await {
                    Ok(content) => return Ok(content),
                    Err(e) => {
                        tracing::warn!(
                            "LinkedIn authenticated fetch failed for {}: {}",
                            url,
                            e
                        );
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

/// Top-level authenticated extraction: Voyager API first, HTML parsing fallback.
#[cfg(feature = "impersonate")]
async fn fetch_authenticated(
    url: &str,
    cookies: &str,
    kind: LinkedInUrlKind,
) -> Result<SiteContent> {
    // Voyager API is only applicable to Profile and Activity URL kinds.
    // For other kinds (Company, Post, Pulse, FeedUpdate) fall straight through to HTML.
    if matches!(kind, LinkedInUrlKind::Profile | LinkedInUrlKind::Activity) {
        match fetch_voyager_api(url, cookies, kind).await {
            Ok(content) => return Ok(content),
            Err(e) => tracing::debug!("Voyager API failed ({}), falling back to HTML: {}", url, e),
        }
    }

    fetch_authenticated_html(url, cookies, kind).await
}

/// Fetch via Voyager REST API (`LinkedIn`'s internal SPA backend).
///
/// `LinkedIn`'s HTML pages are JavaScript-rendered shells — the server returns
/// no profile/post data in the initial HTML. All content is loaded via the
/// Voyager API, which is what this function targets.
#[cfg(feature = "impersonate")]
async fn fetch_voyager_api(
    url: &str,
    cookies: &str,
    kind: LinkedInUrlKind,
) -> Result<SiteContent> {
    let csrf_token = extract_csrf_token(cookies)
        .context("No JSESSIONID cookie found; cannot derive csrf-token for Voyager API")?;

    let username = extract_username_from_url(url)
        .context("Could not extract LinkedIn username from URL")?;

    let voyager_headers = build_voyager_headers(cookies, &csrf_token);

    let profile = fetch_voyager_profile(&username, &voyager_headers).await?;
    let activities = fetch_voyager_activity(&username, &voyager_headers).await.ok();

    Ok(format_voyager_content(url, kind, &profile, activities))
}

/// Fetch the `LinkedIn` Voyager profile endpoint for `username`.
#[cfg(feature = "impersonate")]
async fn fetch_voyager_profile(
    username: &str,
    headers: &[(String, String)],
) -> Result<VoyagerProfileResponse> {
    let api_url = format!(
        "https://www.linkedin.com/voyager/api/identity/profiles/{username}"
    );

    let response =
        crate::impersonate_client::fetch_impersonated(&api_url, None, Some(headers)).await?;

    check_voyager_status(response.status.as_u16(), &api_url)?;

    serde_json::from_str::<VoyagerProfileResponse>(&response.body)
        .context("Failed to parse Voyager profile JSON")
}

/// Fetch recent activity/posts for `username` via the Voyager feed endpoint.
///
/// Returns `Err` on HTTP 401/403/404, which the caller silently ignores.
#[cfg(feature = "impersonate")]
async fn fetch_voyager_activity(
    username: &str,
    headers: &[(String, String)],
) -> Result<VoyagerActivityResponse> {
    // Feed updates filtered by profile URN
    let api_url = format!(
        "https://www.linkedin.com/voyager/api/feed/updates\
         ?profileUrn=urn%3Ali%3Afsd_profile%3A{username}&count=10"
    );

    let response =
        crate::impersonate_client::fetch_impersonated(&api_url, None, Some(headers)).await?;

    check_voyager_status(response.status.as_u16(), &api_url)?;

    serde_json::from_str::<VoyagerActivityResponse>(&response.body)
        .context("Failed to parse Voyager activity JSON")
}

/// Build the header set required by every Voyager API call.
///
/// The `csrf-token` is derived from the `JSESSIONID` cookie value: `LinkedIn`
/// stores it as `"ajax:NNNN"` (with quotes), and the token is the bare
/// `ajax:NNNN` value (without quotes).
#[cfg(feature = "impersonate")]
fn build_voyager_headers(cookies: &str, csrf_token: &str) -> Vec<(String, String)> {
    vec![
        ("Cookie".to_string(), cookies.to_string()),
        ("csrf-token".to_string(), csrf_token.to_string()),
        ("x-restli-protocol-version".to_string(), "2.0.0".to_string()),
        (
            "Accept".to_string(),
            "application/vnd.linkedin.normalized+json+2.1".to_string(),
        ),
        ("x-li-lang".to_string(), "en_US".to_string()),
        ("x-li-track".to_string(),
         r#"{"clientVersion":"1.13","mpVersion":"1.13","osName":"web","timezoneOffset":0,"timezone":"UTC","deviceFormFactor":"DESKTOP","mpName":"voyager-web","displayDensity":1,"displayWidth":1920,"displayHeight":1080}"#.to_string()),
    ]
}

/// Return `Err` for non-success Voyager HTTP status codes.
#[cfg(feature = "impersonate")]
fn check_voyager_status(status: u16, url: &str) -> Result<()> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => anyhow::bail!("Voyager API returned HTTP {status} (auth) for {url}"),
        404 => anyhow::bail!("Voyager API returned HTTP 404 (not found) for {url}"),
        _ => anyhow::bail!("Voyager API returned HTTP {status} for {url}"),
    }
}

/// Format Voyager profile and optional activity into `SiteContent`.
#[cfg(feature = "impersonate")]
fn format_voyager_content(
    url: &str,
    kind: LinkedInUrlKind,
    profile: &VoyagerProfileResponse,
    activities: Option<VoyagerActivityResponse>,
) -> SiteContent {
    let mut md = String::new();
    let profile_md = parse_voyager_profile(profile);
    md.push_str(&profile_md);

    if let Some(activity) = activities {
        let activity_md = parse_voyager_activity(&activity);
        if !activity_md.trim().is_empty() {
            let _ = writeln!(md, "\n### Recent Activity\n");
            md.push_str(&activity_md);
        }
    }

    let _ = writeln!(md, "\n[View on LinkedIn]({url})");

    let full_name = build_full_name(profile.first_name.as_deref(), profile.last_name.as_deref());

    let metadata = SiteMetadata {
        author: full_name.clone(),
        title: full_name,
        published: None,
        platform: format!("LinkedIn ({})", kind_label(kind)),
        canonical_url: url.to_string(),
        media_urls: vec![],
        engagement: None,
    };

    SiteContent { markdown: md, metadata }
}

/// Render a Voyager profile response as markdown text.
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

/// Render Voyager activity/feed response as markdown text.
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

/// Extract the `csrf-token` value from the raw cookie header string.
///
/// `JSESSIONID` is stored as `"ajax:NNNN"` (with surrounding double quotes).
/// The Voyager `csrf-token` header requires just `ajax:NNNN` (no quotes).
///
/// Returns `None` if no `JSESSIONID` cookie is present.
#[must_use]
pub fn extract_csrf_token(cookies: &str) -> Option<String> {
    for part in cookies.split(';') {
        let kv = part.trim();
        let (key, value) = kv.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("jsessionid") {
            // Strip surrounding quotes if present: "ajax:NNN" → ajax:NNN
            let raw = value.trim();
            let token = raw.trim_matches('"');
            return Some(token.to_string());
        }
    }
    None
}

/// Extract the `LinkedIn` username from a `/in/{username}` URL.
///
/// Returns `None` for non-profile URLs or malformed input.
#[must_use]
pub fn extract_username_from_url(url: &str) -> Option<String> {
    // Strip query string; preserve original casing — Voyager API is case-sensitive.
    let without_query = url.split('?').next().unwrap_or(url);

    // Locate /in/ using case-insensitive search via lowercase copy
    let lower = without_query.to_lowercase();
    let in_offset = lower.find("/in/")?;
    let after_in = &without_query[in_offset + 4..]; // 4 == len("/in/")

    let username = after_in.split('/').next()?;
    if username.is_empty() {
        return None;
    }

    Some(username.to_string())
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
#[cfg(feature = "impersonate")]
fn parse_linkedin_html(html: &str, url: &str, kind: LinkedInUrlKind) -> Result<SiteContent> {
    let document = Html::parse_document(html);

    // Priority 1: Try JSON-LD structured data
    if let Some(content) = extract_json_ld(&document, url, kind) {
        return Ok(content);
    }

    // Priority 2: CSS selector extraction
    extract_from_selectors(&document, url, kind)
}

/// Extract content from JSON-LD (`<script type="application/ld+json">`).
#[cfg(feature = "impersonate")]
fn extract_json_ld(document: &Html, url: &str, kind: LinkedInUrlKind) -> Option<SiteContent> {
    let selector = Selector::parse(r#"script[type="application/ld+json"]"#).ok()?;

    for element in document.select(&selector) {
        let json_text = element.text().collect::<String>();
        if let Ok(ld) = serde_json::from_str::<serde_json::Value>(&json_text) {
            let name = ld.get("name")
                .or_else(|| ld.get("headline"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let description = ld.get("description")
                .or_else(|| ld.get("articleBody"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let author = ld.get("author")
                .and_then(|a| {
                    a.get("name").and_then(|n| n.as_str()).map(String::from)
                        .or_else(|| a.as_str().map(String::from))
                });

            let image = ld.get("image")
                .and_then(|i| {
                    i.as_str().map(String::from)
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
                    published: ld.get("datePublished")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    platform: format!("LinkedIn ({})", kind_label(kind)),
                    canonical_url: url.to_string(),
                    media_urls: image.into_iter().collect(),
                    engagement: None,
                };

                return Some(SiteContent { markdown: md, metadata });
            }
        }
    }
    None
}

/// Extract content from HTML using CSS selectors.
#[cfg(feature = "impersonate")]
fn extract_from_selectors(document: &Html, url: &str, kind: LinkedInUrlKind) -> Result<SiteContent> {
    let mut md = String::new();
    let mut title = None;
    let mut author = None;

    // Profile name
    if let Ok(sel) = Selector::parse("h1") {
        if let Some(el) = document.select(&sel).next() {
            let text = el.text().collect::<String>().trim().to_string();
            if !text.is_empty() {
                title = Some(text.clone());
                let _ = writeln!(md, "## {text}\n");
            }
        }
    }

    // Profile headline / tagline
    for selector_str in &[
        ".text-body-medium",              // Profile headline
        ".top-card-layout__headline",     // Public profile
        ".break-words",                   // Various content
    ] {
        if let Ok(sel) = Selector::parse(selector_str) {
            if let Some(el) = document.select(&sel).next() {
                let text = el.text().collect::<String>().trim().to_string();
                if !text.is_empty() && Some(&text) != title.as_ref() {
                    let _ = writeln!(md, "{text}\n");
                    break;
                }
            }
        }
    }

    // About / description section
    for selector_str in &[
        "#about ~ .display-flex .pv-shared-text-with-see-more span[aria-hidden=true]",
        ".pv-about__summary-text",
        "section.summary .description",
    ] {
        if let Ok(sel) = Selector::parse(selector_str) {
            if let Some(el) = document.select(&sel).next() {
                let text = el.text().collect::<String>().trim().to_string();
                if !text.is_empty() {
                    let _ = writeln!(md, "### About\n\n{text}\n");
                    break;
                }
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
                let clean: String = text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
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
    if author.is_none() {
        if let Ok(sel) = Selector::parse(r#"meta[name="author"]"#) {
            if let Some(el) = document.select(&sel).next() {
                author = el.attr("content").map(String::from);
            }
        }
    }

    // Page title from <title> or og:title as fallback
    if title.is_none() {
        if let Ok(sel) = Selector::parse("title") {
            if let Some(el) = document.select(&sel).next() {
                let text = el.text().collect::<String>().trim().to_string();
                // LinkedIn titles often end with " | LinkedIn"
                title = Some(
                    text.strip_suffix(" | LinkedIn")
                        .unwrap_or(&text)
                        .to_string(),
                );
            }
        }
    }

    if md.trim().is_empty() {
        // Last resort: try og:description
        if let Ok(sel) = Selector::parse(r#"meta[property="og:description"]"#) {
            if let Some(el) = document.select(&sel).next() {
                if let Some(desc) = el.attr("content") {
                    let _ = writeln!(md, "{desc}\n");
                }
            }
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

    Ok(SiteContent { markdown: md, metadata })
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
    tracing::debug!("Fetching from LinkedIn oEmbed: {}", oembed_url);

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

/// Format LinkedIn oEmbed data as markdown.
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
            classify_linkedin_url("https://www.linkedin.com/posts/someuser_topic-activity-123456789"),
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
            classify_linkedin_url("https://www.linkedin.com/feed/update/urn:li:activity:7654321098765432109"),
            Some(LinkedInUrlKind::FeedUpdate)
        );
    }

    #[test]
    fn classifies_activity_urls() {
        assert_eq!(
            classify_linkedin_url("https://www.linkedin.com/in/mikko-parkkola/recent-activity/all/"),
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
        assert_eq!(classify_linkedin_url("https://youtube.com/watch?v=abc"), None);
        assert_eq!(classify_linkedin_url("https://twitter.com/user/status/123"), None);
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

        let content = parse_linkedin_html(html, "https://linkedin.com/in/mikko", LinkedInUrlKind::Profile).unwrap();
        assert!(content.markdown.contains("## Mikko Parkkola"));
        assert!(content.markdown.contains("Building things with Rust and AI"));
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

        let content = parse_linkedin_html(html, "https://linkedin.com/in/mikko", LinkedInUrlKind::Profile).unwrap();
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

        let content = parse_linkedin_html(html, "https://linkedin.com/in/user", LinkedInUrlKind::Profile).unwrap();
        assert!(content.markdown.contains("This is the only content available"));
    }
}
