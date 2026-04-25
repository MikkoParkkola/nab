//! TLS fingerprint impersonation for anti-bot bypass.
//!
//! Uses `rquest` (reqwest fork with `BoringSSL`) to produce Chrome/Safari/Firefox
//! TLS fingerprints that pass edge-level JA3/JA4 checks. This is the only
//! reliable way to fetch content from sites like `LinkedIn` that reject non-browser
//! TLS handshakes with HTTP 999.
//!
//! Key design: do NOT mix nab's `BrowserProfile` headers with rquest's emulation.
//! rquest sets User-Agent, Sec-CH-UA, and other headers automatically to match
//! the emulated TLS fingerprint. Overriding them would create a detectable mismatch.

use anyhow::{Context, Result};
use rquest::StatusCode;
use rquest_util::{Emulation, EmulationOption};
use std::time::Duration;

/// Domains that require TLS fingerprint impersonation.
///
/// These sites check TLS fingerprints at the CDN edge and reject requests
/// from non-browser TLS stacks (rustls, etc.) with bot-detection responses.
const IMPERSONATION_DOMAINS: &[&str] = &[
    "linkedin.com",
    "www.linkedin.com",
    // Future: instagram.com, facebook.com
];

/// Check whether a URL targets a domain that requires TLS impersonation.
pub fn needs_impersonation(url: &str) -> bool {
    let lower = url.to_lowercase();
    IMPERSONATION_DOMAINS
        .iter()
        .any(|domain| lower.contains(domain))
}

/// Select the browser emulation profile for the given URL.
///
/// Chrome 137 is chosen as the default: highest version available in
/// `wreq-util` 2.2.6 = highest probability of matching `LinkedIn`'s browser
/// whitelist. Older profiles (e.g. Chrome 136) get rejected with HTTP 999.
fn select_emulation(_url: &str) -> Emulation {
    Emulation::Chrome137
}

/// Response from an impersonated fetch.
pub struct ImpersonatedResponse {
    pub status: StatusCode,
    pub content_type: String,
    pub body: String,
}

/// Fetch a URL using TLS fingerprint impersonation.
///
/// Builds a fresh `rquest` client with Chrome 136 TLS fingerprint.
/// The client sets its own User-Agent and Sec-CH-UA headers matching
/// the TLS profile — do NOT override these.
///
/// `cookies` is the browser cookie header value (e.g., `"li_at=AQ...; JSESSIONID=..."`)
pub async fn fetch_impersonated(
    url: &str,
    cookies: Option<&str>,
    extra_headers: Option<&[(String, String)]>,
) -> Result<ImpersonatedResponse> {
    let emulation = select_emulation(url);

    let emulation_option = EmulationOption::builder().emulation(emulation).build();

    let client = rquest::Client::builder()
        .emulation(emulation_option)
        .cookie_store(true)
        .redirect(rquest::redirect::Policy::limited(10))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to build impersonating HTTP client")?;

    // IMPORTANT: do NOT override Accept / Accept-Language / sec-ch-ua / User-Agent.
    // wreq's Chrome emulation already sets the canonical Chrome 137 versions of
    // these. Overriding them here creates a header/TLS-fingerprint mismatch that
    // LinkedIn (and others) reject with HTTP 999.
    //
    // We only ADD top-level-navigation hints that wreq does not set:
    //   - Upgrade-Insecure-Requests: every Chrome top-level nav
    //   - Sec-Fetch-User: ?1                — user-initiated nav
    //   - Cache-Control / Pragma           — cold-load nav (typed URL)
    let mut request = client
        .get(url)
        .header("upgrade-insecure-requests", "1")
        .header("sec-fetch-user", "?1")
        .header("cache-control", "max-age=0");

    if let Some(cookie_val) = cookies {
        request = request.header("Cookie", cookie_val);
    }

    if let Some(headers) = extra_headers {
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
    }

    let response = request
        .send()
        .await
        .context("Impersonated request failed")?;

    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html")
        .to_string();

    let body = response
        .text()
        .await
        .context("Failed to read impersonated response body")?;

    Ok(ImpersonatedResponse {
        status,
        content_type,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_linkedin_domains() {
        assert!(needs_impersonation("https://www.linkedin.com/in/someuser"));
        assert!(needs_impersonation(
            "https://linkedin.com/company/somecompany"
        ));
        assert!(needs_impersonation(
            "https://www.linkedin.com/posts/user_topic-123"
        ));
    }

    #[test]
    fn ignores_non_impersonation_domains() {
        assert!(!needs_impersonation("https://example.com"));
        assert!(!needs_impersonation("https://twitter.com/user/status/123"));
        assert!(!needs_impersonation("https://github.com/user/repo"));
    }

    #[test]
    fn selects_chrome_137() {
        let emulation = select_emulation("https://www.linkedin.com/in/user");
        assert_eq!(emulation, Emulation::Chrome137);
    }
}
