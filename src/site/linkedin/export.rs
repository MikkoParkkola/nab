// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! `LinkedIn` data-export (Track F).
//!
//! `LinkedIn` lets every user request a ZIP archive of their own data via
//! Settings → Data Privacy → "Get a copy of your data". The same surface is
//! reachable as an SSR form at `https://www.linkedin.com/psettings/member-data`.
//! This module automates the full lifecycle:
//!
//! 1. **Request** — POST a `csrfToken` + archive-type tuple to the form
//!    endpoint. Server queues the export. Latency: ~10 minutes for FAST,
//!    ~24 hours for FULL.
//! 2. **Poll** — GET the same URL. The page renders a "Download archive" link
//!    once the ZIP is signed and ready.
//! 3. **Download** — fetch the signed URL and stream-write to disk.
//!
//! The form-based path is chosen over the JSON XHR endpoints
//! (`/voyager/api/identity/dataExports`) because the form has been the
//! user-visible flow since 2018; `LinkedIn` rotates `queryIds` and JSON shapes
//! every ~2-3 weeks, but the form action URL is stable.
//!
//! Confidence levels (per nab convention):
//! - V (verified): csrf extraction, cookie resolution, polling state machine.
//! - I (inferred from one source): exact form field names; tracked the
//!   linkedin-api Python package convention. The endpoint accepts at minimum
//!   `csrfToken` + `archiveType` form fields.
//! - A (assumed): exact "ready" marker text in HTML response. Matched
//!   defensively (multiple substrings) so a copy-edit on `LinkedIn`'s side does
//!   not silently break polling.

use anyhow::{Context, Result, bail};
use std::time::Duration;

use crate::impersonate_client::{ImpersonatedMethod, ImpersonatedResponse, request_impersonated};
use crate::site::linkedin::helpers::extract_csrf_token;

/// Default settings page URL — used to GET status only (the SPA shell renders
/// the "download archive" link once the ZIP is signed).
pub const DEFAULT_FORM_URL: &str = "https://www.linkedin.com/psettings/member-data";

/// New JSON-XHR endpoint used to POST archive requests.
/// Discovered 2026-04-25 by grepping `psettings/member-data` HTML for
/// `mysettings-api/settingsApiDataExport` (the SPA settings front-end calls
/// this internal proxy which fans out to Voyager identity/dataExports).
pub const DEFAULT_REQUEST_URL: &str =
    "https://www.linkedin.com/mysettings-api/settingsApiDataExport/";

/// Archive type variants accepted by the form.
#[derive(Debug, Clone, Copy)]
pub enum ArchiveKind {
    /// Fast archive: connections, contacts, account history. ~10 min.
    Fast,
    /// Full archive: posts, articles, messages, all user-generated content. ~24 h.
    Full,
}

impl ArchiveKind {
    pub fn as_form_value(self) -> &'static str {
        match self {
            ArchiveKind::Fast => "FAST_FILE_ONLY",
            ArchiveKind::Full => "ARCHIVE",
        }
    }
}

/// Result of a poll cycle.
#[derive(Debug, Clone)]
pub enum ArchiveStatus {
    /// `LinkedIn` has not finished generating the archive yet.
    Pending {
        /// Free-text status message from the server, when one is rendered.
        message: Option<String>,
    },
    /// Archive is ready; `download_url` points to the signed URL on
    /// licdn / blob storage.
    Ready { download_url: String },
}

/// Build the `Referer` header value the form expects on POST.
fn referer() -> &'static str {
    "https://www.linkedin.com/psettings/member-data"
}

/// Headers shared by the form GET and POST. The `Accept` header tracks
/// `Content-Type`: JSON endpoints need a JSON `Accept`, the SSR status page
/// needs the HTML `Accept`. Mixing them returns HTTP 406.
fn form_headers(csrf: &str, content_type: Option<&str>) -> Vec<(String, String)> {
    let accept = match content_type {
        Some(ct) if ct.contains("json") => "application/json",
        _ => "text/html,application/xhtml+xml,application/xml;q=0.9",
    };
    let mut h = vec![
        ("csrf-token".to_string(), csrf.to_string()),
        ("referer".to_string(), referer().to_string()),
        ("accept".to_string(), accept.to_string()),
    ];
    if let Some(ct) = content_type {
        h.push(("content-type".to_string(), ct.to_string()));
    }
    h
}

/// Initiate an archive request. Server enqueues; returns immediately.
///
/// Returns `Ok(())` on a 2xx or 3xx. Returns `Err` for 4xx/5xx with the
/// response body preview attached.
///
/// Body shape: defaults to JSON `{"archiveType": "FAST_FILE_ONLY"|"ARCHIVE"}`
/// based on the SPA-bundle clues in `/psettings/member-data`. As of 2026-04-25
/// `LinkedIn`'s `/mysettings-api/settingsApiDataExport/` accepts the POST but
/// rejects the default body with HTTP 400 — the exact field name has rotated.
///
/// To discover the live body shape, use Chrome `DevTools` → Network tab → click
/// "Get a copy of your data" → copy the request payload → pass it via
/// `body_override`. The infrastructure (csrf, cookies, headers, polling) is
/// stable; only the body needs `DevTools` capture per release cycle.
pub async fn request_archive(
    cookies: &str,
    csrf: &str,
    kind: ArchiveKind,
    request_url: &str,
    body_override: Option<&str>,
) -> Result<()> {
    let body = body_override.map_or_else(
        || format!(r#"{{"archiveType":"{}"}}"#, kind.as_form_value()),
        std::string::ToString::to_string,
    );

    let headers = form_headers(csrf, Some("application/json"));
    let resp = request_impersonated(
        ImpersonatedMethod::Post,
        request_url,
        Some(cookies),
        Some(&headers),
        Some(body.into_bytes()),
    )
    .await
    .context("data-export request POST failed")?;

    if resp.status.is_success() || resp.status.is_redirection() {
        return Ok(());
    }

    let preview: String = resp.body.chars().take(400).collect();
    bail!(
        "data-export request returned HTTP {} (body preview: {}). \
         Body shape may have rotated. Capture via Chrome DevTools and pass \
         via --body-override.",
        resp.status.as_u16(),
        preview
    )
}

/// One poll cycle. Pure function over an HTML body — extracted for testing
/// without touching the network.
pub fn parse_status_page(html: &str) -> ArchiveStatus {
    // The "ready" marker: `LinkedIn` renders an `<a href="…ambry…">Download
    // archive</a>` link once the ZIP is signed. The hostname rotates between
    // `download.linkedin.com`, `media.licdn.com`, and pre-signed S3-style URLs,
    // so we match by the link text rather than by hostname.
    if let Some(url) = extract_download_url(html) {
        return ArchiveStatus::Ready { download_url: url };
    }

    // Otherwise: pending. Try to surface a status string from the page so the
    // CLI can give the user a sense of progress.
    let pending_markers = [
        "Your archive is being prepared",
        "We're preparing your download",
        "Request a copy of your data",
        "preparing your archive",
    ];
    let lc = html.to_lowercase();
    let message = pending_markers
        .iter()
        .find(|needle| lc.contains(&needle.to_lowercase()))
        .map(|s| (*s).to_string());

    ArchiveStatus::Pending { message }
}

/// Extract the first signed-archive URL from the settings page HTML.
///
/// Strategy: find an `href="https://...linkedin..."`-shaped link whose text
/// or surrounding context contains "Download archive". Falls back to any
/// pre-signed archive hostname when the anchor text shape rotates.
fn extract_download_url(html: &str) -> Option<String> {
    // 1. Look for explicit "Download archive" anchors.
    if let Some(idx) = html.find("Download archive") {
        // Walk backward to find the enclosing href.
        let head = &html[..idx];
        if let Some(href_start) = head.rfind("href=\"") {
            let after = &html[href_start + 6..];
            if let Some(end) = after.find('"') {
                let url = &after[..end];
                if url.starts_with("https://") {
                    return Some(url.to_string());
                }
            }
        }
    }

    // 2. Fall back to any pre-signed archive URL hostname.
    for needle in [
        "https://download.linkedin.com/",
        "https://media.licdn.com/",
        "https://www.linkedin.com/ambry/",
    ] {
        if let Some(start) = html.find(needle) {
            let tail = &html[start..];
            let end = tail.find(['"', '\'', '<', ' ']).unwrap_or(tail.len());
            let candidate = &tail[..end];
            if candidate.contains("archive")
                || candidate.contains("data-export")
                || candidate.contains("ambry")
            {
                return Some(candidate.to_string());
            }
        }
    }

    None
}

/// Fetch the archive status page once.
pub async fn poll_archive_status(
    cookies: &str,
    csrf: &str,
    form_url: &str,
) -> Result<(ArchiveStatus, ImpersonatedResponse)> {
    let headers = form_headers(csrf, None);
    let resp = request_impersonated(
        ImpersonatedMethod::Get,
        form_url,
        Some(cookies),
        Some(&headers),
        None,
    )
    .await
    .context("data-export status GET failed")?;

    if !resp.status.is_success() {
        let preview: String = resp.body.chars().take(400).collect();
        bail!(
            "data-export status returned HTTP {} (body preview: {})",
            resp.status.as_u16(),
            preview
        );
    }

    Ok((parse_status_page(&resp.body), resp))
}

/// Resolve csrf-token from the cookie header. Convenience wrapper.
pub fn csrf_from_cookies(cookies: &str) -> Result<String> {
    extract_csrf_token(cookies)
        .context("no JSESSIONID cookie — cannot derive csrf-token. Use --cookies brave (or chrome) and ensure you are logged into LinkedIn.")
}

/// Compute the next poll delay using exponential backoff with a cap.
///
/// `attempt` is 0-indexed. Caps at the user-supplied `max_secs`.
pub fn next_poll_delay(attempt: u32, base_secs: u64, max_secs: u64) -> Duration {
    let secs = base_secs.saturating_mul(2u64.saturating_pow(attempt.min(6)));
    Duration::from_secs(secs.min(max_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_kind_form_values() {
        assert_eq!(ArchiveKind::Fast.as_form_value(), "FAST_FILE_ONLY");
        assert_eq!(ArchiveKind::Full.as_form_value(), "ARCHIVE");
    }

    #[test]
    fn parse_pending_page() {
        let html = "<html>Your archive is being prepared. Check back soon.</html>";
        match parse_status_page(html) {
            ArchiveStatus::Pending { message } => {
                assert_eq!(message.as_deref(), Some("Your archive is being prepared"));
            }
            ArchiveStatus::Ready { .. } => panic!("should be pending"),
        }
    }

    #[test]
    fn parse_ready_page_explicit_anchor() {
        let html = r#"<html><a href="https://download.linkedin.com/exports/abc.zip">Download archive</a></html>"#;
        match parse_status_page(html) {
            ArchiveStatus::Ready { download_url } => {
                assert_eq!(
                    download_url,
                    "https://download.linkedin.com/exports/abc.zip"
                );
            }
            ArchiveStatus::Pending { .. } => panic!("should be ready"),
        }
    }

    #[test]
    fn parse_ready_page_fallback_hostname() {
        let html = r"<html>Your archive is ready: https://www.linkedin.com/ambry/data-export/abc123.zip</html>";
        match parse_status_page(html) {
            ArchiveStatus::Ready { download_url } => {
                assert!(download_url.contains("ambry"));
            }
            ArchiveStatus::Pending { .. } => panic!("should be ready"),
        }
    }

    #[test]
    fn parse_neutral_page_is_pending_without_message() {
        let html = "<html>some unrelated content</html>";
        match parse_status_page(html) {
            ArchiveStatus::Pending { message } => assert!(message.is_none()),
            ArchiveStatus::Ready { .. } => panic!("should be pending"),
        }
    }

    #[test]
    fn next_poll_delay_grows_then_caps() {
        // base 60, max 600
        assert_eq!(next_poll_delay(0, 60, 600).as_secs(), 60);
        assert_eq!(next_poll_delay(1, 60, 600).as_secs(), 120);
        assert_eq!(next_poll_delay(2, 60, 600).as_secs(), 240);
        assert_eq!(next_poll_delay(3, 60, 600).as_secs(), 480);
        // Capped at 600
        assert_eq!(next_poll_delay(4, 60, 600).as_secs(), 600);
        assert_eq!(next_poll_delay(10, 60, 600).as_secs(), 600);
    }

    #[test]
    fn csrf_extraction_requires_jsessionid() {
        assert!(csrf_from_cookies("li_at=foo").is_err());
        assert!(csrf_from_cookies("JSESSIONID=\"ajax:1234567890\"; li_at=foo").is_ok());
    }
}
