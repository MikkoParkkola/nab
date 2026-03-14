//! `LinkedIn` data types: Voyager API response shapes and oEmbed.

use serde::Deserialize;

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
pub(super) struct LinkedInOEmbed {
    pub title: Option<String>,
    pub author_name: Option<String>,
    pub author_url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub html: Option<String>,
}
