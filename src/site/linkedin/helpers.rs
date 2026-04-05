//! Shared utility helpers for `LinkedIn` extraction.

#[cfg(feature = "impersonate")]
use super::url::LinkedInUrlKind;

#[cfg(feature = "impersonate")]
use super::types::VoyagerActivityResponse;
#[cfg(feature = "impersonate")]
use super::types::VoyagerProfileResponse;
#[cfg(feature = "impersonate")]
use std::fmt::Write as _;

/// Human-readable label for a URL kind (used in platform metadata).
#[cfg(feature = "impersonate")]
pub(super) fn kind_label(kind: LinkedInUrlKind) -> &'static str {
    match kind {
        LinkedInUrlKind::Profile => "Profile",
        LinkedInUrlKind::Company => "Company",
        LinkedInUrlKind::Post => "Post",
        LinkedInUrlKind::Pulse => "Article",
        LinkedInUrlKind::FeedUpdate => "Feed Update",
        LinkedInUrlKind::Activity => "Activity",
    }
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

/// Build a full name from optional first and last name components.
#[cfg(feature = "impersonate")]
pub(super) fn build_full_name(first: Option<&str>, last: Option<&str>) -> Option<String> {
    match (first, last) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f.to_string()),
        (None, Some(l)) => Some(l.to_string()),
        (None, None) => None,
    }
}

/// Strip HTML comment wrappers (`<!--` ... `-->`) from a string.
///
/// `LinkedIn`'s `<code>` element content is `<!--{...}-->` — the JSON is
/// wrapped in an HTML comment so browsers ignore it until JS reads it.
#[cfg(any(test, feature = "impersonate"))]
pub(super) fn strip_html_comment(s: &str) -> &str {
    s.strip_prefix("<!--")
        .and_then(|inner| inner.strip_suffix("-->"))
        .map_or(s, str::trim)
}

/// Decode common HTML entities in a string.
///
/// Profile fields extracted from `LinkedIn`'s embedded JSON arrive pre-HTML-escaped
/// (e.g. `&amp;` instead of `&`). This helper decodes the five standard XML/HTML
/// entities that appear in practice.
#[cfg(feature = "impersonate")]
pub(super) fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Strip HTML tags for plain text display.
pub(super) fn strip_html(html: &str) -> String {
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
