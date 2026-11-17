//! Shared internal utility helpers for CLI and MCP surfaces.

use crate::{AcceleratedClient, CookieSource};

/// Extract the host/domain from a URL, returning an empty string on failure.
#[must_use]
pub fn extract_domain(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Build a `Referer` header value from a URL (scheme + host + `/`).
///
/// Returns `None` if the URL cannot be parsed.
#[must_use]
pub fn build_referer(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .map(|parsed| format!("{}://{}/", parsed.scheme(), parsed.host_str().unwrap_or("")))
}

/// Resolve a cookie header for a known domain from an optional browser name.
#[must_use]
pub fn resolve_cookie_header_for_domain(domain: &str, browser: Option<&str>) -> String {
    let Some(browser) = browser else {
        return String::new();
    };

    CookieSource::from_browser_name(browser)
        .get_cookie_header(domain)
        .unwrap_or_default()
}

/// Resolve a cookie header for a URL from an optional browser name.
#[must_use]
pub fn resolve_cookie_header_for_url(url: &str, browser: Option<&str>) -> String {
    let domain = extract_domain(url);
    resolve_cookie_header_for_domain(&domain, browser)
}

/// Attempt to recover article content from Next.js content chunks.
///
/// Called when the initial extraction yields thin content on a Next.js page
/// with `__NEXT_DATA__` containing only metadata. Makes up to 3 secondary HTTP
/// requests: webpack runtime, page component, and content chunk.
///
/// Returns `Some(markdown)` on success, `None` if recovery fails.
pub async fn recover_nextjs_chunks(
    client: &AcceleratedClient,
    html: &str,
    page_url: &str,
) -> Option<String> {
    use crate::content::spa_extract;

    if !spa_extract::is_nextjs_metadata_only(html) {
        return None;
    }

    let script_urls = spa_extract::discover_nextjs_content_chunks(html, page_url);
    if script_urls.len() < 2 {
        return None;
    }

    tracing::debug!("Attempting Next.js content chunk recovery");

    let (webpack_resp, page_resp) =
        tokio::join!(client.fetch(&script_urls[0]), client.fetch(&script_urls[1]),);

    let webpack_js = webpack_resp.ok()?.text().await.ok()?;
    let page_js = page_resp.ok()?.text().await.ok()?;

    let origin = url::Url::parse(page_url)
        .ok()
        .map(|parsed| parsed.origin().unicode_serialization())?;

    let slug = url::Url::parse(page_url).ok().and_then(|parsed| {
        parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back().map(String::from))
    });
    let chunk_urls = spa_extract::resolve_content_chunk_urls_for_slug(
        &webpack_js,
        &page_js,
        &origin,
        slug.as_deref(),
    );
    if chunk_urls.is_empty() {
        tracing::debug!("No content chunk URLs resolved from webpack runtime");
        return None;
    }

    for chunk_url in &chunk_urls {
        tracing::debug!("Fetching content chunk: {chunk_url}");
        if let Ok(resp) = client.fetch(chunk_url).await
            && let Ok(chunk_js) = resp.text().await
            && let Some(content) = spa_extract::extract_jsx_text_content(&chunk_js)
        {
            tracing::info!(
                "Recovered {} chars from Next.js content chunk",
                content.len()
            );
            return Some(content);
        }
    }

    None
}

/// `2026-11-17T11:03:18Z`: whole seconds in UTC, the form nab prints and
/// stores stamps in.
pub const UTC_SECONDS: &[time::format_description::BorrowedFormatItem<'static>] =
    time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");

/// `t` as [`UTC_SECONDS`], converted to UTC first so the `Z` is true.
#[must_use]
pub fn format_utc_seconds(t: time::OffsetDateTime) -> String {
    t.to_offset(time::UtcOffset::UTC)
        .format(UTC_SECONDS)
        .unwrap_or_default()
}

/// Serde for timestamps stored as RFC 3339 and always read back in UTC, as
/// chrono's `DateTime<Utc>` did, so files written by an older nab still load
/// and a hand-edited offset cannot be printed under a `Z`.
pub mod utc_rfc3339 {
    use serde::{Deserializer, Serializer};
    use time::{OffsetDateTime, UtcOffset};

    /// # Errors
    /// When the instant cannot be written as RFC 3339.
    pub fn serialize<S: Serializer>(t: &OffsetDateTime, s: S) -> Result<S::Ok, S::Error> {
        time::serde::rfc3339::serialize(&t.to_offset(UtcOffset::UTC), s)
    }

    /// # Errors
    /// When the value is not an RFC 3339 timestamp.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<OffsetDateTime, D::Error> {
        time::serde::rfc3339::deserialize(d).map(|t| t.to_offset(UtcOffset::UTC))
    }

    /// The same for `Option<OffsetDateTime>`.
    pub mod option {
        use serde::{Deserializer, Serializer};
        use time::{OffsetDateTime, UtcOffset};

        /// # Errors
        /// When the instant cannot be written as RFC 3339.
        #[allow(clippy::ref_option)]
        pub fn serialize<S: Serializer>(
            t: &Option<OffsetDateTime>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            time::serde::rfc3339::option::serialize(&t.map(|t| t.to_offset(UtcOffset::UTC)), s)
        }

        /// # Errors
        /// When the value is neither null nor an RFC 3339 timestamp.
        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<OffsetDateTime>, D::Error> {
            time::serde::rfc3339::option::deserialize(d)
                .map(|t| t.map(|t| t.to_offset(UtcOffset::UTC)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_referer, extract_domain, format_utc_seconds, resolve_cookie_header_for_domain,
        resolve_cookie_header_for_url,
    };

    #[test]
    fn utc_rfc3339_reads_offsets_as_utc() {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Saved {
            #[serde(with = "super::utc_rfc3339")]
            at: time::OffsetDateTime,
            #[serde(default, with = "super::utc_rfc3339::option")]
            maybe: Option<time::OffsetDateTime>,
        }
        let saved: Saved = serde_json::from_str(
            r#"{"at":"2026-11-17T13:03:18+02:00","maybe":"2026-11-17T08:03:18-03:00"}"#,
        )
        .unwrap();
        assert_eq!(saved.at.offset(), time::UtcOffset::UTC);
        assert_eq!(format_utc_seconds(saved.at), "2026-11-17T11:03:18Z");
        assert_eq!(
            saved.maybe.map(format_utc_seconds).as_deref(),
            Some("2026-11-17T11:03:18Z")
        );
        let absent: Saved = serde_json::from_str(r#"{"at":"2026-11-17T11:03:18Z"}"#).unwrap();
        assert!(absent.maybe.is_none());
        // chrono stored an empty optional timestamp as null.
        let null: Saved =
            serde_json::from_str(r#"{"at":"2026-11-17T11:03:18Z","maybe":null}"#).unwrap();
        assert!(null.maybe.is_none());
        // Written back in UTC with a Z, whatever offset the value carried.
        let shifted = Saved {
            at: saved.at.to_offset(time::macros::offset!(+2)),
            maybe: None,
        };
        let written = serde_json::to_string(&shifted).unwrap();
        assert_eq!(written, r#"{"at":"2026-11-17T11:03:18Z","maybe":null}"#);
    }

    #[test]
    fn extract_domain_returns_host_or_empty_string() {
        assert_eq!(extract_domain("https://example.com/path"), "example.com");
        assert_eq!(extract_domain("not a url"), "");
    }

    #[test]
    fn build_referer_preserves_scheme_and_host() {
        assert_eq!(
            build_referer("https://example.com/path?x=1").as_deref(),
            Some("https://example.com/"),
        );
        assert_eq!(build_referer("not a url"), None);
    }

    #[test]
    fn cookie_helpers_return_empty_string_when_browser_is_absent() {
        assert_eq!(resolve_cookie_header_for_domain("example.com", None), "");
        assert_eq!(
            resolve_cookie_header_for_url("https://example.com/path", None),
            "",
        );
    }
}
