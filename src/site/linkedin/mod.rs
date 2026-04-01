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

#[cfg(feature = "impersonate")]
pub(super) mod auth;
pub mod helpers;
pub(super) mod oembed;
pub(super) mod types;
pub mod url;

pub use helpers::{extract_csrf_token, extract_username_from_url};
#[cfg(feature = "impersonate")]
pub use helpers::{parse_voyager_activity, parse_voyager_profile};
#[cfg(feature = "impersonate")]
pub use types::VoyagerProfileResponse;
#[cfg(feature = "impersonate")]
pub use types::{
    VoyagerActivityResponse, VoyagerCommentary, VoyagerFeedElement, VoyagerText, VoyagerUpdateValue,
};
pub use url::{LinkedInUrlKind, classify_linkedin_url};

use anyhow::{Result, bail};
use async_trait::async_trait;

use super::{SiteContent, SiteProvider};
use crate::http_client::AcceleratedClient;

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
        #[cfg(not(feature = "impersonate"))]
        let _ = cookies;

        let kind = classify_linkedin_url(url)
            .ok_or_else(|| anyhow::anyhow!("URL does not match any LinkedIn pattern"))?;

        // Try authenticated extraction first (requires impersonate feature + cookies)
        #[cfg(feature = "impersonate")]
        {
            if let Some(cookie_header) = cookies
                && !cookie_header.is_empty()
            {
                match auth::fetch_authenticated(url, cookie_header, kind).await {
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
        oembed::fetch_oembed(url, client).await
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
