// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Voyager XHR client for `LinkedIn` activity feeds.
//!
//! `LinkedIn`'s `/in/{user}/recent-activity/all/` page is a pure SPA shell —
//! the actual posts arrive client-side from the Voyager API. The previous
//! design assumed `/voyager/api/feed/updates` returned 410 Gone and switched
//! to scraping `<code>` JSON in the SSR HTML, but the activity-feed posts are
//! not present there at all. As of 2026-04-25 the modern endpoint
//! `/voyager/api/feed/updatesV2?profileUrn=…&q=memberShareFeed` is alive and
//! returns the post list when called with the `csrf-token` derived from
//! `JSESSIONID` plus the canonical Voyager `accept` and
//! `x-restli-protocol-version` headers.
//!
//! This module wires the existing `extract_csrf_token` and
//! `parse_voyager_activity` helpers — already in the codebase but never
//! called from a live fetch path — into a real two-step XHR call:
//!
//! 1. Resolve `/in/{username}` → profile URN via `dash/profiles`. ✅ works.
//! 2. Fetch the share feed via the historical REST endpoints
//!    (`/feed/updates`, `/feed/updatesV2`, `/identity/profileView`).
//!
//! Status as of 2026-04-25: step 1 succeeds; step 2 historical endpoints
//! all return 400/404 — `LinkedIn` migrated activity feeds to a GraphQL
//! shape (`/voyager/api/graphql?queryId=voyagerFeedDashProfileUpdates.HASH`)
//! whose `queryId` hash rotates across releases. The hash is loaded from a
//! lazy JS chunk and is not present in the SSR HTML of the activity URL.
//! A future iteration will discover the hash by fetching the JS chunk and
//! grepping for the queryId; this module ships the auth + URN resolution
//! pieces that path will need.
//!
//! The typed `VoyagerActivityResponse` parse is the happy path. When
//! `LinkedIn` wraps the response in `{data:…, included:[…]}` (decorator
//! envelope), we fall back to a recursive commentary scan so post text is
//! recovered regardless of envelope shape.

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::helpers::{extract_csrf_token, extract_username_from_url, parse_voyager_activity};
use super::types::VoyagerActivityResponse;
use crate::impersonate_client;
use crate::site::SiteContent;

/// How many recent posts to request from the Voyager feed endpoint.
const ACTIVITY_COUNT: u32 = 10;

/// Voyager `accept` header — instructs `LinkedIn` to return the normalized
/// JSON shape (rather than the partial-rendered Pemberly response).
const ACCEPT_VND_LINKEDIN: &str = "application/vnd.linkedin.normalized+json+2.1";

/// Voyager Rest.li protocol version. The endpoint rejects requests without
/// this header.
const RESTLI_VERSION: &str = "2.0.0";

/// Build the standard Voyager request headers (csrf + accept + restli + lang).
fn voyager_headers(csrf: &str) -> Vec<(String, String)> {
    vec![
        ("csrf-token".to_string(), csrf.to_string()),
        ("accept".to_string(), ACCEPT_VND_LINKEDIN.to_string()),
        ("x-restli-protocol-version".to_string(), RESTLI_VERSION.to_string()),
        ("x-li-lang".to_string(), "en_US".to_string()),
        ("referer".to_string(), "https://www.linkedin.com/".to_string()),
    ]
}

/// Resolve a `LinkedIn` username to its profile URN via the dash endpoint.
///
/// Returns `urn:li:fsd_profile:ACoAA…` style URN that the feed endpoint
/// accepts as `profileUrn`. Falls back to extracting any `urn:li:fs_profile:`
/// or `urn:li:fsd_profile:` identifier from the response body when the typed
/// JSON pointer fails (decorator envelopes vary across `LinkedIn` versions).
async fn resolve_profile_urn(username: &str, cookies: &str, csrf: &str) -> Result<String> {
    let url = format!(
        "https://www.linkedin.com/voyager/api/identity/dash/profiles\
         ?q=memberIdentity&memberIdentity={username}\
         &decorationId=com.linkedin.voyager.dash.deco.identity.profile.FullProfileWithEntities-103"
    );
    let headers = voyager_headers(csrf);
    let resp = impersonate_client::fetch_impersonated(&url, Some(cookies), Some(&headers)).await?;
    if !resp.status.is_success() {
        bail!(
            "Voyager dash/profiles returned HTTP {} for username `{}`",
            resp.status.as_u16(),
            username
        );
    }
    let json: Value = serde_json::from_str(&resp.body)
        .context("Voyager dash/profiles response was not JSON")?;

    if let Some(urn) = json
        .pointer("/elements/0/entityUrn")
        .and_then(Value::as_str)
        .or_else(|| json.pointer("/data/elements/0/entityUrn").and_then(Value::as_str))
    {
        return Ok(urn.to_string());
    }

    // Fallback: walk the entire body for the first profile URN we recognise.
    for needle in ["urn:li:fsd_profile:", "urn:li:fs_profile:"] {
        if let Some(start) = resp.body.find(needle) {
            let tail = &resp.body[start..];
            let end = tail
                .find(|c: char| c == '"' || c == ',' || c == '}' || c == ')' || c == '&')
                .unwrap_or(tail.len());
            return Ok(tail[..end].to_string());
        }
    }

    bail!("could not extract profile URN for username `{}`", username)
}

/// Fetch the member share feed for a given profile URN.
async fn fetch_member_share_feed(
    profile_urn: &str,
    cookies: &str,
    csrf: &str,
) -> Result<String> {
    let encoded_urn = urlencoding::encode(profile_urn);

    // `LinkedIn` rotates which endpoint is canonical for the activity feed.
    // Try a small set of historically-shipped paths in order; return the
    // first one that returns 2xx with a non-empty JSON body. The shapes are
    // similar enough that `render_voyager_body` handles all of them via the
    // typed parser → recursive commentary scan fallback.
    let candidates = [
        // Legacy v1 — still answers in 2025/2026 for many handles.
        format!(
            "https://www.linkedin.com/voyager/api/feed/updates\
             ?profileUrn={encoded_urn}&q=memberShareFeed&count={ACTIVITY_COUNT}"
        ),
        // V2 with phone module key.
        format!(
            "https://www.linkedin.com/voyager/api/feed/updatesV2\
             ?profileUrn={encoded_urn}\
             &q=memberShareFeed&moduleKey=member-shares%3Aphone&count={ACTIVITY_COUNT}"
        ),
        // V2 without module key (some accounts).
        format!(
            "https://www.linkedin.com/voyager/api/feed/updatesV2\
             ?profileUrn={encoded_urn}&q=memberShareFeed&count={ACTIVITY_COUNT}"
        ),
        // Identity profile view — sometimes carries recent activity inline.
        format!(
            "https://www.linkedin.com/voyager/api/identity/profileView\
             ?id={encoded_urn}"
        ),
    ];

    let headers = voyager_headers(csrf);
    let mut last_err: Option<String> = None;
    for endpoint in &candidates {
        let resp =
            impersonate_client::fetch_impersonated(endpoint, Some(cookies), Some(&headers))
                .await?;
        if resp.status.is_success() && !resp.body.trim().is_empty() {
            tracing::debug!(
                "Voyager feed call succeeded via {} (body {} bytes)",
                endpoint,
                resp.body.len()
            );
            return Ok(resp.body);
        }
        let preview: String = resp.body.chars().take(160).collect();
        last_err = Some(format!(
            "HTTP {} from {} (body[0..160]: {})",
            resp.status.as_u16(),
            endpoint,
            preview
        ));
        tracing::debug!(
            "Voyager candidate {} returned HTTP {}",
            endpoint,
            resp.status.as_u16()
        );
    }

    bail!(
        "all Voyager activity endpoints failed for `{}`: {}",
        profile_urn,
        last_err.unwrap_or_else(|| "(no responses)".to_string())
    );
}

/// Walk an arbitrary JSON value collecting commentary text from any
/// `{"commentary": {"text": {"text": "..."}}}` shape, regardless of nesting
/// depth or envelope. Used as a fallback when the typed parse misses
/// posts because `LinkedIn` wrapped them in a `data` / `included` envelope.
fn scan_commentary(value: &Value, posts: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(commentary) = map.get("commentary").and_then(Value::as_object) {
                if let Some(text) = commentary
                    .get("text")
                    .and_then(Value::as_object)
                    .and_then(|t| t.get("text"))
                    .and_then(Value::as_str)
                {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && !posts.contains(&trimmed.to_string()) {
                        posts.push(trimmed.to_string());
                    }
                } else if let Some(text) = commentary.get("text").and_then(Value::as_str) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && !posts.contains(&trimmed.to_string()) {
                        posts.push(trimmed.to_string());
                    }
                }
            }
            for v in map.values() {
                scan_commentary(v, posts);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                scan_commentary(v, posts);
            }
        }
        _ => {}
    }
}

/// Render a Voyager response body as markdown.
///
/// Tries the typed `VoyagerActivityResponse` parser first (matches the shape
/// used by the v1 `/feed/updates` endpoint plus naked v2 responses), then
/// falls back to a recursive commentary-walker for the
/// `{data:…, included:[…]}` decorator envelope.
fn render_voyager_body(body: &str) -> String {
    if let Ok(typed) = serde_json::from_str::<VoyagerActivityResponse>(body) {
        let md = parse_voyager_activity(&typed);
        if !md.trim().is_empty() {
            return md;
        }
    }

    if let Ok(value) = serde_json::from_str::<Value>(body) {
        let mut posts = Vec::new();
        scan_commentary(&value, &mut posts);
        if !posts.is_empty() {
            let mut md = String::new();
            for post in posts.iter().take(ACTIVITY_COUNT as usize) {
                md.push_str("---\n\n");
                md.push_str(post);
                md.push_str("\n\n");
            }
            return md;
        }
    }

    String::new()
}

/// Fetch `LinkedIn` activity-feed content for `/in/{username}/recent-activity/`
/// URLs via the Voyager API.
///
/// Returns `Ok(Some(content))` on success, `Ok(None)` when Voyager is
/// reachable but the response carries no posts (caller falls back to SSR
/// HTML scraping for at least the profile chrome), and `Err(_)` on hard
/// failures (no JSESSIONID cookie, network error, decryption failure, etc.).
pub async fn fetch_activity_via_voyager(
    url: &str,
    cookies: &str,
) -> Result<Option<SiteContent>> {
    let username = extract_username_from_url(url)
        .context("activity URL did not contain /in/{username}")?;
    let csrf = extract_csrf_token(cookies)
        .context("no JSESSIONID cookie — cannot derive Voyager csrf-token")?;

    let profile_urn = resolve_profile_urn(&username, cookies, &csrf).await?;
    let body = fetch_member_share_feed(&profile_urn, cookies, &csrf).await?;
    let posts_md = render_voyager_body(&body);

    if posts_md.trim().is_empty() {
        return Ok(None);
    }

    let mut md = String::new();
    md.push_str("## Recent Activity\n\n");
    md.push_str(&posts_md);
    md.push_str(&format!("[View on LinkedIn]({url})\n"));

    Ok(Some(SiteContent {
        markdown: md,
        metadata: super::super::SiteMetadata {
            author: Some(username),
            title: Some("LinkedIn Activity".to_string()),
            published: None,
            platform: "linkedin".to_string(),
            canonical_url: url.to_string(),
            media_urls: Vec::new(),
            engagement: None,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_typed_response() {
        let body = r#"{"elements":[
            {"value":{"commentary":{"text":{"text":"first post"}}}},
            {"value":{"commentary":{"text":{"text":"second post"}}}}
        ]}"#;
        let md = render_voyager_body(body);
        assert!(md.contains("first post"), "missing first post: {md}");
        assert!(md.contains("second post"), "missing second post: {md}");
    }

    #[test]
    fn render_decorator_envelope_fallback() {
        let body = r#"{"data":{"elements":[
            {"*value":"urn:li:fsd_update:abc"}
        ]},"included":[
            {"$type":"com.linkedin.voyager.feed.Update",
             "commentary":{"text":{"text":"envelope post"}}}
        ]}"#;
        let md = render_voyager_body(body);
        assert!(md.contains("envelope post"), "missing envelope post: {md}");
    }

    #[test]
    fn render_empty_response() {
        let body = r#"{"elements":[]}"#;
        let md = render_voyager_body(body);
        assert!(md.trim().is_empty());
    }

    #[test]
    fn voyager_headers_includes_csrf() {
        let h = voyager_headers("ajax:1234");
        assert!(h.iter().any(|(k, v)| k == "csrf-token" && v == "ajax:1234"));
        assert!(h.iter().any(|(k, _)| k == "x-restli-protocol-version"));
        assert!(h.iter().any(|(k, _)| k == "accept"));
    }
}
