// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#![allow(clippy::doc_markdown, clippy::map_unwrap_or)]

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
//! The typed `VoyagerActivityResponse` parse is the legacy happy path. When
//! `LinkedIn` wraps the response in `{data:…, included:[…]}` (decorator
//! envelope, the modern shape) we run a typed-pass walker over `included[]`
//! that selects strictly `$type == "com.linkedin.voyager.dash.feed.Update"`
//! entries — Comment / Profile / SocialDetail entries are excluded by
//! construction. Reshares are detected via the `resharedUpdate` field and
//! tagged accordingly so the original poster's commentary is never mis-
//! attributed to the user. Engagement counters (likes / comments / shares /
//! impressions) are joined from `SocialActivityCounts` entries via the
//! activity URN.

use anyhow::{Context, Result, bail};
use serde_json::Value;

use std::fmt::Write as _;

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
        (
            "x-restli-protocol-version".to_string(),
            RESTLI_VERSION.to_string(),
        ),
        ("x-li-lang".to_string(), "en_US".to_string()),
        (
            "referer".to_string(),
            "https://www.linkedin.com/".to_string(),
        ),
    ]
}

/// Resolve a `LinkedIn` username to its profile URN via the dash endpoint.
///
/// Returns `(profile_urn, display_name)` for a `LinkedIn` username.
///
/// `profile_urn` is the `urn:li:fsd_profile:ACoAA…` shape the feed endpoint
/// accepts as `profileUrn`. `display_name` is the user's full name as
/// rendered on the profile (e.g. "Mikko Parkkola"); used downstream to
/// filter reshare envelopes whose commentary text belongs to other authors.
async fn resolve_profile_urn(
    username: &str,
    cookies: &str,
    csrf: &str,
) -> Result<(String, String)> {
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
    let json: Value =
        serde_json::from_str(&resp.body).context("Voyager dash/profiles response was not JSON")?;

    let display_name = {
        let first = json
            .pointer("/elements/0/firstName")
            .and_then(Value::as_str)
            .or_else(|| {
                json.pointer("/data/elements/0/firstName")
                    .and_then(Value::as_str)
            })
            .unwrap_or("");
        let last = json
            .pointer("/elements/0/lastName")
            .and_then(Value::as_str)
            .or_else(|| {
                json.pointer("/data/elements/0/lastName")
                    .and_then(Value::as_str)
            })
            .unwrap_or("");
        format!("{first} {last}").trim().to_string()
    };

    if let Some(urn) = json
        .pointer("/elements/0/entityUrn")
        .and_then(Value::as_str)
        .or_else(|| {
            json.pointer("/data/elements/0/entityUrn")
                .and_then(Value::as_str)
        })
    {
        return Ok((urn.to_string(), display_name));
    }

    // Fallback: walk the entire body for the first profile URN we recognise.
    for needle in ["urn:li:fsd_profile:", "urn:li:fs_profile:"] {
        if let Some(start) = resp.body.find(needle) {
            let tail = &resp.body[start..];
            let end = tail.find(['"', ',', '}', ')', '&']).unwrap_or(tail.len());
            return Ok((tail[..end].to_string(), display_name));
        }
    }

    bail!("could not extract profile URN for username `{username}`")
}

/// `voyagerFeedDashProfileUpdates` GraphQL queryId hashes discovered in the
/// `/in/{handle}/recent-activity/all/` SPA bundle on 2026-04-26. `LinkedIn`
/// ships a separate hash per activity-tab (Posts / Comments / Reactions /
/// Reshares); the order below puts the user's own original posts first so
/// `/recent-activity/all/` URLs return Posts-tab content by default. The
/// other hashes remain as fallbacks if the primary rotates first.
///
/// Mapping verified 2026-04-26 by inspecting `entityUrn` shape per hash:
/// - `7f16…` → `MEMBER_FEED` (user's original posts) ← default
/// - `8f05…` → `PROFILE_COMMENTS` (user's comments on others' posts)
/// - `3a42…` → `PROFILE_REACTIONS` (posts the user reacted to)
/// - `4af0…` → `MEMBER_SHARES` (user's reshares of others' posts)
/// - `1159…` → small / less-tested variant
///
/// **Refresh**: when all start returning HTTP 400 ("query not found"),
/// re-discover by visiting the activity URL, extracting the bundle script
/// URLs from `<script src="https://static.licdn.com/aero-v1/sc/h/…">`, and
/// grepping each chunk for `voyagerFeedDashProfileUpdates\.[a-f0-9]+`.
const FEED_QUERY_IDS: &[&str] = &[
    "7f16f6612fc18a3623688ca7a74d7696",
    "8f05a4e5ad12d9cb2b56eaa22afbcab9",
    "3a42619bc23360ce8c29e737277e2ea9",
    "4af00b28d60ed0f1488018948daad822",
    "11595bab074f70dab009cecc3a585768",
];

/// Fetch the member share feed for a given profile URN.
async fn fetch_member_share_feed(profile_urn: &str, cookies: &str, csrf: &str) -> Result<String> {
    let encoded_urn = urlencoding::encode(profile_urn);

    // Try the modern GraphQL endpoint first (works as of 2026-04-26), then
    // fall back to historical REST endpoints. Returns the first 2xx with a
    // non-empty body; render_voyager_body handles all shapes.
    let mut candidates: Vec<String> = FEED_QUERY_IDS
        .iter()
        .map(|hash| {
            format!(
                "https://www.linkedin.com/voyager/api/graphql\
                 ?variables=(profileUrn:{encoded_urn},count:{ACTIVITY_COUNT},start:0)\
                 &queryId=voyagerFeedDashProfileUpdates.{hash}"
            )
        })
        .collect();
    candidates.extend([
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
    ]);

    let headers = voyager_headers(csrf);
    let mut last_err: Option<String> = None;
    for endpoint in &candidates {
        let resp =
            impersonate_client::fetch_impersonated(endpoint, Some(cookies), Some(&headers)).await?;
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

/// Engagement counters for a single activity URN, sourced from
/// `com.linkedin.voyager.dash.feed.SocialActivityCounts` entries in `included[]`.
#[derive(Debug, Default, Clone)]
struct SocialCounts {
    likes: u64,
    comments: u64,
    shares: u64,
    impressions: u64,
}

/// One rendered post extracted from a Voyager `included[]` `Update` entity.
///
/// Designed so the renderer never needs to recurse back into the JSON: the
/// activity URN, author, body, reshare provenance, and engagement are all
/// captured in this struct.
#[derive(Debug, Clone)]
struct PostRecord {
    /// Canonical post id, e.g. `urn:li:activity:7451014806356230146`. Used to
    /// build the per-post URL and to join against `SocialCounts`.
    activity_urn: String,
    /// Author display name as rendered in `actor.name.text`.
    actor_name: String,
    /// Post body text. May be empty for share-with-no-comment.
    body: String,
    /// `Some(original_author)` when this is a reshare. The body field then
    /// holds the user's reshare commentary (often empty); the original post's
    /// commentary is intentionally NOT included to prevent mis-attribution.
    reshare_of: Option<String>,
    /// Engagement counters joined via `activity_urn`.
    counts: SocialCounts,
}

impl PostRecord {
    /// `https://www.linkedin.com/feed/update/urn:li:activity:N/`
    fn url(&self) -> String {
        format!(
            "https://www.linkedin.com/feed/update/{}/",
            self.activity_urn
        )
    }
}

/// Bits the snowflake id is shifted right by to recover Unix-millisecond time.
/// LinkedIn activity ids are Twitter-style snowflakes: the high 41 bits encode
/// Unix milliseconds, the low 22 bits are worker id + sequence.
const SNOWFLAKE_TIMESTAMP_SHIFT: u32 = 22;

/// Lower sanity bound (2010-01-01T00:00:00Z) in Unix milliseconds.
const SNOWFLAKE_MIN_MS: u64 = 1_262_304_000_000;

/// Upper sanity bound (2100-01-01T00:00:00Z) in Unix milliseconds.
const SNOWFLAKE_MAX_MS: u64 = 4_102_444_800_000;

/// Derive the post creation time (RFC-3339 UTC, second precision) from a
/// `urn:li:activity:NUMBER` URN.
///
/// LinkedIn activity ids are Twitter-style snowflakes whose high 41 bits encode
/// Unix milliseconds. The timestamp is therefore intrinsic to the URN — no
/// separate `createdAt` field is required, which is why this works on both the
/// XHR and embedded-`<code>` paths.
///
/// Returns `None` when the URN is malformed, the numeric id does not parse, or
/// the derived instant falls outside a sane window — guarding against non-
/// snowflake ids leaking a 1970 / far-future date.
///
/// ```
/// # use nab::site::linkedin::timestamp_from_activity_urn;
/// let ts = timestamp_from_activity_urn("urn:li:activity:7451014806356230146");
/// assert_eq!(ts.as_deref(), Some("2026-04-17T21:12:42Z"));
/// assert!(timestamp_from_activity_urn("urn:li:activity:not-a-number").is_none());
/// ```
#[must_use]
pub fn timestamp_from_activity_urn(activity_urn: &str) -> Option<String> {
    let id: u64 = activity_urn
        .strip_prefix("urn:li:activity:")?
        .parse()
        .ok()?;
    let unix_ms = id >> SNOWFLAKE_TIMESTAMP_SHIFT;
    if !(SNOWFLAKE_MIN_MS..SNOWFLAKE_MAX_MS).contains(&unix_ms) {
        return None;
    }
    #[allow(clippy::cast_possible_wrap)]
    let secs = (unix_ms / 1000) as i64;
    chrono::DateTime::<chrono::Utc>::from_timestamp_secs(secs)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// Extract `urn:li:activity:NUMBER` from `urn:li:fsd_update:(urn:li:activity:NUMBER,...)`.
///
/// LinkedIn's compound URN syntax is `(child_urn,tag1,tag2,...)`; the activity
/// URN is always the first comma-delimited piece.
fn activity_urn_from_update_urn(update_urn: &str) -> Option<String> {
    let inner = update_urn.strip_prefix("urn:li:fsd_update:(")?;
    let first = inner.split(',').next()?;
    if first.starts_with("urn:li:activity:") {
        Some(first.to_string())
    } else {
        None
    }
}

/// Extract `urn:li:activity:NUMBER` directly from a `SocialActivityCounts.urn`
/// field. Filters out comment-level counts (`urn:li:comment:(...)`).
fn activity_urn_from_counts_urn(urn: &str) -> Option<String> {
    if urn.starts_with("urn:li:activity:") {
        Some(urn.to_string())
    } else {
        None
    }
}

/// Walk `included[]` and collect engagement counters keyed by activity URN.
fn collect_social_counts(included: &[Value]) -> std::collections::HashMap<String, SocialCounts> {
    let mut map = std::collections::HashMap::new();
    for entry in included {
        let Some(t) = entry.get("$type").and_then(Value::as_str) else {
            continue;
        };
        if t != "com.linkedin.voyager.dash.feed.SocialActivityCounts" {
            continue;
        }
        let Some(urn) = entry.get("urn").and_then(Value::as_str) else {
            continue;
        };
        let Some(activity_urn) = activity_urn_from_counts_urn(urn) else {
            // Comment-level counts: urn:li:comment:(...) — skip.
            continue;
        };
        let counts = SocialCounts {
            likes: entry.get("numLikes").and_then(Value::as_u64).unwrap_or(0),
            comments: entry
                .get("numComments")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            shares: entry.get("numShares").and_then(Value::as_u64).unwrap_or(0),
            impressions: entry
                .get("numImpressions")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        };
        map.insert(activity_urn, counts);
    }
    map
}

/// Walk `included[]` and collect Update entities as PostRecords.
///
/// Skips Comment, SocialDetail, Profile, etc. — only `feed.Update` becomes a
/// post. This is the structural fix for the prior recursive walker that mis-
/// attributed comment text and inline reshared commentary to the user.
fn collect_posts(
    included: &[Value],
    counts_map: &std::collections::HashMap<String, SocialCounts>,
) -> Vec<PostRecord> {
    let mut posts = Vec::new();
    for entry in included {
        let Some(t) = entry.get("$type").and_then(Value::as_str) else {
            continue;
        };
        if t != "com.linkedin.voyager.dash.feed.Update" {
            continue;
        }
        let Some(update_urn) = entry.get("entityUrn").and_then(Value::as_str) else {
            continue;
        };
        let Some(activity_urn) = activity_urn_from_update_urn(update_urn) else {
            continue;
        };

        let actor_name = entry
            .pointer("/actor/name/text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        // The user's own commentary on this update. For reshares, this is the
        // user's optional reshare comment — the original post's commentary
        // lives inside resharedUpdate.* and is intentionally NOT pulled here.
        let body = entry
            .pointer("/commentary/text/text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        // Reshare detection: presence of resharedUpdate marks a reshare. We
        // try to surface the original author's name when it is inlined; when
        // it is only a URN reference we still tag the post as a reshare so
        // downstream readers know not to attribute the body to the user.
        let reshare_of = entry.get("resharedUpdate").and_then(|rs| {
            rs.pointer("/actor/name/text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    // Naked URN reference — surface a placeholder so we still
                    // tag the post as a reshare downstream.
                    if rs.is_object() || rs.is_string() {
                        Some("(original author)".to_string())
                    } else {
                        None
                    }
                })
        });

        let counts = counts_map.get(&activity_urn).cloned().unwrap_or_default();

        posts.push(PostRecord {
            activity_urn,
            actor_name,
            body,
            reshare_of,
            counts,
        });
    }
    // Newest first: activity URN is a snowflake-style monotonic id.
    posts.sort_by(|a, b| b.activity_urn.cmp(&a.activity_urn));
    posts
}

/// Render an engagement summary line. Returns empty string when all zero.
fn fmt_engagement(c: &SocialCounts) -> String {
    if c.likes == 0 && c.comments == 0 && c.shares == 0 && c.impressions == 0 {
        return String::new();
    }
    let mut parts = Vec::new();
    if c.impressions > 0 {
        parts.push(format!("{} impressions", c.impressions));
    }
    if c.likes > 0 {
        parts.push(format!("{} reactions", c.likes));
    }
    if c.comments > 0 {
        parts.push(format!("{} comments", c.comments));
    }
    if c.shares > 0 {
        parts.push(format!("{} reposts", c.shares));
    }
    parts.join(" · ")
}

/// Render a Voyager response body as markdown.
///
/// Two-stage strategy:
///
/// 1. Try the typed `VoyagerActivityResponse` parser (matches the legacy v1
///    `/feed/updates` shape). Kept as a happy-path fast lane for endpoints
///    that have not migrated to GraphQL.
/// 2. Fall back to the typed-pass `included[]` walker which filters strictly
///    on `$type == "com.linkedin.voyager.dash.feed.Update"`. This is the
///    structural fix that prevents Comment / Profile / SocialDetail entries
///    from leaking into the rendered output.
///
/// `expected_actor_name` flags posts whose author does not match the resolved
/// profile name as reshares (defence in depth — `resharedUpdate` is the
/// primary signal but is occasionally absent on quote-shares).
///
/// Exposed to the `<code>`-JSON path (`auth.rs`): when `/recent-activity/all/`
/// embeds a pre-fetched Voyager feed envelope inside a hidden `<code>` element,
/// that path routes the envelope body through this same walker so the embedded
/// and XHR paths produce byte-identical structured output (author, urn,
/// timestamp, engagement) instead of the text-only commentary fallback.
pub(super) fn render_voyager_body(body: &str, expected_actor_name: &str) -> String {
    if let Ok(typed) = serde_json::from_str::<VoyagerActivityResponse>(body) {
        let md = parse_voyager_activity(&typed);
        if !md.trim().is_empty() {
            return md;
        }
    }

    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    let included: &[Value] = value
        .get("included")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if included.is_empty() {
        return String::new();
    }

    let counts_map = collect_social_counts(included);
    let posts = collect_posts(included, &counts_map);
    if posts.is_empty() {
        return String::new();
    }

    let mut md = String::new();
    for post in posts.iter().take(ACTIVITY_COUNT as usize) {
        // Tag: POST (own original) | RESHARE of X | RESHARE? (foreign actor,
        // no resharedUpdate field — defensive label).
        let tag = if let Some(orig) = &post.reshare_of {
            format!("RESHARE of {orig}")
        } else if !expected_actor_name.is_empty()
            && !post.actor_name.is_empty()
            && post.actor_name != expected_actor_name
        {
            format!("RESHARE? (actor: {})", post.actor_name)
        } else {
            "POST".to_string()
        };

        let _ = writeln!(md, "---");
        let _ = writeln!(md, "**[{tag}]** · <{}>", post.url());
        if let Some(ts) = timestamp_from_activity_urn(&post.activity_urn) {
            let _ = writeln!(md, "_{ts}_");
        }
        let engagement = fmt_engagement(&post.counts);
        if !engagement.is_empty() {
            let _ = writeln!(md, "_{engagement}_");
        }
        let _ = writeln!(md);
        if post.body.is_empty() {
            let _ = writeln!(md, "_(no commentary)_");
        } else {
            let _ = writeln!(md, "{}", post.body);
        }
        let _ = writeln!(md);
    }
    md
}

/// Fetch `LinkedIn` activity-feed content for `/in/{username}/recent-activity/`
/// URLs via the Voyager API.
///
/// Returns `Ok(Some(content))` on success, `Ok(None)` when Voyager is
/// reachable but the response carries no posts (caller falls back to SSR
/// HTML scraping for at least the profile chrome), and `Err(_)` on hard
/// failures (no JSESSIONID cookie, network error, decryption failure, etc.).
pub async fn fetch_activity_via_voyager(url: &str, cookies: &str) -> Result<Option<SiteContent>> {
    let username =
        extract_username_from_url(url).context("activity URL did not contain /in/{username}")?;
    let csrf = extract_csrf_token(cookies)
        .context("no JSESSIONID cookie — cannot derive Voyager csrf-token")?;

    let (profile_urn, display_name) = resolve_profile_urn(&username, cookies, &csrf).await?;
    let body = fetch_member_share_feed(&profile_urn, cookies, &csrf).await?;

    // Debug helper: dump the raw Voyager body when NAB_DUMP_VOYAGER is set.
    // Useful when LinkedIn rotates queryId hashes or envelope shapes — capture
    // a known-working response and diff against the new one.
    if let Ok(path) = std::env::var("NAB_DUMP_VOYAGER")
        && let Err(e) = std::fs::write(&path, &body)
    {
        tracing::warn!("NAB_DUMP_VOYAGER write to {path} failed: {e}");
    }

    let posts_md = render_voyager_body(&body, &display_name);

    if posts_md.trim().is_empty() {
        return Ok(None);
    }

    let mut md = String::new();
    md.push_str("## Recent Activity\n\n");
    md.push_str(&posts_md);
    let _ = writeln!(md, "[View on LinkedIn]({url})");

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
        // Typed parser path doesn't filter — first wins.
        let body = r#"{"elements":[
            {"value":{"commentary":{"text":{"text":"first post"}}}},
            {"value":{"commentary":{"text":{"text":"second post"}}}}
        ]}"#;
        let md = render_voyager_body(body, "");
        assert!(md.contains("first post"), "missing first post: {md}");
        assert!(md.contains("second post"), "missing second post: {md}");
    }

    #[test]
    fn render_decorator_envelope_only_includes_update_entities() {
        // included[] mixes Update + Comment + Profile — only Update should
        // surface as a post in the rendered markdown.
        let body = r#"{"data":{},"included":[
            {"$type":"com.linkedin.voyager.dash.feed.Update",
             "entityUrn":"urn:li:fsd_update:(urn:li:activity:7000000000000000001,MEMBER_FEED,DEBUG_REASON,DEFAULT,false)",
             "actor":{"name":{"text":"Mikko Parkkola"}},
             "commentary":{"text":{"text":"my actual post"}}},
            {"$type":"com.linkedin.voyager.dash.social.Comment",
             "entityUrn":"urn:li:fsd_comment:(activity:7000000000000000001,1)",
             "commentary":{"text":{"text":"a one-line comment that should NOT appear as a post"}}},
            {"$type":"com.linkedin.voyager.dash.identity.profile.Profile",
             "firstName":"Other","lastName":"Person"}
        ]}"#;
        let md = render_voyager_body(body, "Mikko Parkkola");
        assert!(md.contains("my actual post"), "missing post: {md}");
        assert!(
            !md.contains("one-line comment"),
            "leaked Comment entity into post output: {md}"
        );
        assert!(
            !md.contains("Other Person"),
            "leaked Profile entity into post output: {md}"
        );
        assert!(md.contains("[POST]"), "missing POST tag: {md}");
    }

    #[test]
    fn render_reshare_does_not_attribute_inner_text_to_user() {
        // Outer Update is Mikko's reshare wrapper (no commentary). The
        // resharedUpdate field carries the original post by Mitko Vasilev
        // whose body must NOT appear under Mikko's name.
        let body = r#"{"data":{},"included":[
            {"$type":"com.linkedin.voyager.dash.feed.Update",
             "entityUrn":"urn:li:fsd_update:(urn:li:activity:7000000000000000002,MEMBER_FEED,DEBUG_REASON,DEFAULT,false)",
             "actor":{"name":{"text":"Mikko Parkkola"}},
             "commentary":{"text":{"text":""}},
             "resharedUpdate":{
                "actor":{"name":{"text":"Mitko Vasilev"}},
                "commentary":{"text":{"text":"I just deleted all my MCPs"}}
             }}
        ]}"#;
        let md = render_voyager_body(body, "Mikko Parkkola");
        assert!(
            md.contains("RESHARE of Mitko Vasilev"),
            "missing reshare tag: {md}"
        );
        assert!(
            !md.contains("deleted all my MCPs"),
            "leaked reshared content into Mikko's post body: {md}"
        );
    }

    #[test]
    fn render_attaches_engagement_counts_via_activity_join() {
        let body = r#"{"data":{},"included":[
            {"$type":"com.linkedin.voyager.dash.feed.Update",
             "entityUrn":"urn:li:fsd_update:(urn:li:activity:7000000000000000003,MEMBER_FEED,DEBUG_REASON,DEFAULT,false)",
             "actor":{"name":{"text":"Mikko Parkkola"}},
             "commentary":{"text":{"text":"a post with engagement"}}},
            {"$type":"com.linkedin.voyager.dash.feed.SocialActivityCounts",
             "urn":"urn:li:activity:7000000000000000003",
             "numLikes":42,"numComments":5,"numShares":1,"numImpressions":900}
        ]}"#;
        let md = render_voyager_body(body, "Mikko Parkkola");
        assert!(md.contains("900 impressions"), "missing impressions: {md}");
        assert!(md.contains("42 reactions"), "missing reactions: {md}");
        assert!(md.contains("5 comments"), "missing comments: {md}");
        assert!(md.contains("1 reposts"), "missing reposts: {md}");
    }

    #[test]
    fn render_includes_post_url_for_each_post() {
        let body = r#"{"data":{},"included":[
            {"$type":"com.linkedin.voyager.dash.feed.Update",
             "entityUrn":"urn:li:fsd_update:(urn:li:activity:7000000000000000004,MEMBER_FEED,DEBUG_REASON,DEFAULT,false)",
             "actor":{"name":{"text":"Mikko Parkkola"}},
             "commentary":{"text":{"text":"link me"}}}
        ]}"#;
        let md = render_voyager_body(body, "Mikko Parkkola");
        assert!(
            md.contains(
                "https://www.linkedin.com/feed/update/urn:li:activity:7000000000000000004/"
            ),
            "missing per-post URL: {md}"
        );
    }

    #[test]
    fn render_sorts_newest_first_by_activity_urn() {
        let body = r#"{"data":{},"included":[
            {"$type":"com.linkedin.voyager.dash.feed.Update",
             "entityUrn":"urn:li:fsd_update:(urn:li:activity:7000000000000000010,MEMBER_FEED,DEBUG_REASON,DEFAULT,false)",
             "actor":{"name":{"text":"Mikko Parkkola"}},
             "commentary":{"text":{"text":"older post"}}},
            {"$type":"com.linkedin.voyager.dash.feed.Update",
             "entityUrn":"urn:li:fsd_update:(urn:li:activity:7000000000000000099,MEMBER_FEED,DEBUG_REASON,DEFAULT,false)",
             "actor":{"name":{"text":"Mikko Parkkola"}},
             "commentary":{"text":{"text":"newer post"}}}
        ]}"#;
        let md = render_voyager_body(body, "Mikko Parkkola");
        let newer = md.find("newer post").expect("newer post missing");
        let older = md.find("older post").expect("older post missing");
        assert!(newer < older, "newer should render first; md: {md}");
    }

    #[test]
    fn render_empty_response() {
        let body = r#"{"elements":[]}"#;
        let md = render_voyager_body(body, "Mikko Parkkola");
        assert!(md.trim().is_empty());
    }

    #[test]
    fn timestamp_from_known_activity_urn_decodes_snowflake() {
        // GIVEN: a real-shaped activity URN whose snowflake decodes to 2026-04-17.
        // WHEN / THEN: the high 41 bits yield the correct UTC instant.
        assert_eq!(
            timestamp_from_activity_urn("urn:li:activity:7451014806356230146").as_deref(),
            Some("2026-04-17T21:12:42Z")
        );
    }

    #[test]
    fn timestamp_rejects_non_numeric_and_out_of_range_ids() {
        // Malformed numeric component → None (no panic).
        assert!(timestamp_from_activity_urn("urn:li:activity:not-a-number").is_none());
        // Wrong prefix → None.
        assert!(timestamp_from_activity_urn("urn:li:comment:7451014806356230146").is_none());
        // A tiny id decodes to ~1970 (below the sanity window) → None rather
        // than inventing a 1970 timestamp.
        assert!(timestamp_from_activity_urn("urn:li:activity:1").is_none());
    }

    #[test]
    fn render_includes_snowflake_timestamp_line() {
        // GIVEN: a feed envelope with one Update whose URN decodes to 2026-04-17.
        let body = r#"{"data":{},"included":[
            {"$type":"com.linkedin.voyager.dash.feed.Update",
             "entityUrn":"urn:li:fsd_update:(urn:li:activity:7451014806356230146,MEMBER_FEED,DEBUG_REASON,DEFAULT,false)",
             "actor":{"name":{"text":"Mikko Parkkola"}},
             "commentary":{"text":{"text":"timestamped post"}}}
        ]}"#;
        // WHEN
        let md = render_voyager_body(body, "Mikko Parkkola");
        // THEN: the derived timestamp is rendered.
        assert!(
            md.contains("2026-04-17T21:12:42Z"),
            "missing snowflake timestamp: {md}"
        );
    }

    #[test]
    fn activity_urn_extraction_from_compound_update_urn() {
        let urn = "urn:li:fsd_update:(urn:li:activity:7451014806356230146,MEMBER_FEED,DEBUG_REASON,DEFAULT,false)";
        assert_eq!(
            activity_urn_from_update_urn(urn).as_deref(),
            Some("urn:li:activity:7451014806356230146")
        );
    }

    #[test]
    fn voyager_headers_includes_csrf() {
        let h = voyager_headers("ajax:1234");
        assert!(h.iter().any(|(k, v)| k == "csrf-token" && v == "ajax:1234"));
        assert!(h.iter().any(|(k, _)| k == "x-restli-protocol-version"));
        assert!(h.iter().any(|(k, _)| k == "accept"));
    }
}
