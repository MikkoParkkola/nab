//! HTTP fetch helpers for `nab-mcp` tool implementations.
//!
//! Low-level async helpers used by the tool `run` methods:
//! cookie resolution, safe/cookie-injected fetch, body conversion,
//! response formatting, and validation test runners.

use std::fmt::Write as FmtWrite;
use std::time::Instant;

use reqwest::header::{COOKIE, HeaderValue};
use rust_mcp_sdk::schema::schema_utils::CallToolError;
use url::Url;

use nab::content::ContentRouter;
use nab::{AcceleratedClient, SafeFetchConfig, SafeRequestOptions};

// ─── Cookie helpers ───────────────────────────────────────────────────────────

fn resolve_cookie_browser_name(browser: Option<&str>) -> Option<String> {
    resolve_cookie_browser_name_with(browser, || {
        nab::detect_default_browser().map_or_else(
            |_| "chrome".to_string(),
            |browser| browser.as_str().to_string(),
        )
    })
}

fn resolve_cookie_browser_name_with<F>(browser: Option<&str>, detect_default: F) -> Option<String>
where
    F: FnOnce() -> String,
{
    match browser.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) if value.eq_ignore_ascii_case("none") => None,
        Some(value) if value.eq_ignore_ascii_case("auto") => Some(detect_default()),
        Some(value) => Some(value.to_string()),
        None => Some(detect_default()),
    }
}

/// Resolve cookie header for a URL from the requested browser.
pub(crate) fn resolve_cookie_header(url: &str, browser: Option<&str>) -> String {
    let browser = resolve_cookie_browser_name(browser);
    nab::util::resolve_cookie_header_for_url(url, browser.as_deref())
}

// ─── Fetch helpers ────────────────────────────────────────────────────────────

pub(crate) fn validate_tool_url(url: &str) -> Result<Url, CallToolError> {
    let parsed = url
        .parse::<Url>()
        .map_err(|e| CallToolError::from_message(format!("Invalid URL '{url}': {e}")))?;
    nab::validate_url(&parsed).map_err(|e| CallToolError::from_message(e.to_string()))?;
    Ok(parsed)
}

pub(crate) async fn read_response_capped(
    response: reqwest::Response,
) -> Result<bytes::Bytes, CallToolError> {
    nab::http_client::read_body_capped(response, nab::DEFAULT_MAX_BODY_SIZE)
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))
}

/// Fetch via `fetch_safe` and return the response components.
pub(crate) async fn fetch_safe_response(
    client: &AcceleratedClient,
    url: &str,
    config: &SafeFetchConfig,
    start: Instant,
) -> Result<
    (
        reqwest::StatusCode,
        String,
        Vec<(String, String)>,
        bytes::Bytes,
        std::time::Duration,
    ),
    CallToolError,
> {
    let safe_resp = client
        .fetch_safe(url, config)
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;
    let elapsed = start.elapsed();
    Ok((
        safe_resp.status,
        safe_resp.content_type.clone(),
        safe_resp.headers.clone(),
        safe_resp.body,
        elapsed,
    ))
}

/// Fetch with a cookie header and return the response components.
pub(crate) async fn fetch_with_cookies(
    client: &AcceleratedClient,
    url: &str,
    cookie_header: &str,
    profile: &nab::fingerprint::BrowserProfile,
    start: Instant,
) -> Result<
    (
        reqwest::StatusCode,
        String,
        Vec<(String, String)>,
        bytes::Bytes,
        std::time::Duration,
    ),
    CallToolError,
> {
    let mut headers = profile.to_headers();
    if !cookie_header.is_empty() {
        let cookie_value = HeaderValue::from_str(cookie_header)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        headers.insert(COOKIE, cookie_value);
    }

    let safe_resp = client
        .request_safe(
            url,
            SafeRequestOptions {
                headers,
                config: SafeFetchConfig::default(),
                ..SafeRequestOptions::default()
            },
        )
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;
    let elapsed = start.elapsed();
    Ok((
        safe_resp.status,
        safe_resp.content_type,
        safe_resp.headers,
        safe_resp.body,
        elapsed,
    ))
}

/// Fetch a URL using a session-owned `reqwest::Client` whose cookie jar
/// already contains the session's cookies.
///
/// The URL is SSRF-validated before dispatch. Session clients use a redirect
/// policy that validates every redirect target, and response bodies are capped.
pub(crate) async fn fetch_with_session_response(
    session_client: &reqwest::Client,
    url: &str,
    start: Instant,
) -> Result<
    (
        reqwest::StatusCode,
        String,
        Vec<(String, String)>,
        bytes::Bytes,
        std::time::Duration,
    ),
    CallToolError,
> {
    validate_tool_url(url)?;
    let response = session_client
        .get(url)
        .send()
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;
    let elapsed = start.elapsed();
    let status = response.status();
    let ct = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html")
        .to_string();
    let hdrs: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_string()))
        .collect();
    let bytes = read_response_capped(response).await?;
    Ok((status, ct, hdrs, bytes, elapsed))
}

/// Convert body bytes to markdown asynchronously via `spawn_blocking`.
pub(crate) async fn convert_body_async(
    body_bytes: &bytes::Bytes,
    content_type: &str,
    url: &str,
) -> Result<nab::content::ConversionResult, CallToolError> {
    let bytes_clone = body_bytes.to_vec();
    let ct_clone = content_type.to_string();
    let url_clone = url.to_string();
    let router = ContentRouter::new();
    tokio::task::spawn_blocking(move || {
        router.convert_with_url(&bytes_clone, &ct_clone, Some(&url_clone))
    })
    .await
    .map_err(|e| CallToolError::from_message(e.to_string()))?
    .map_err(|e| CallToolError::from_message(e.to_string()))
}

/// Attempt to recover article content from Next.js content chunks.
///
/// Called when the initial extraction yields thin content on a Next.js page
/// with `__NEXT_DATA__` containing only metadata.  Makes up to 3 secondary
/// HTTP requests: webpack runtime, page component, and content chunk.
///
/// Returns `Some(markdown)` on success, `None` if recovery fails.
pub(crate) async fn recover_nextjs_chunks(
    client: &AcceleratedClient,
    html: &str,
    page_url: &str,
) -> Option<String> {
    nab::util::recover_nextjs_chunks(client, html, page_url).await
}

// ─── Output formatting helpers ────────────────────────────────────────────────

/// Write the response status/timing/header summary to `output`.
pub(crate) fn write_response_summary(
    output: &mut String,
    status: reqwest::StatusCode,
    elapsed: std::time::Duration,
    show_headers: bool,
    response_headers: &[(String, String)],
) {
    output.push_str("\n📊 Response:\n");
    let _ = writeln!(output, "   Status: {status}");
    let _ = writeln!(output, "   Time: {:.2}ms", elapsed.as_secs_f64() * 1000.0);

    if show_headers {
        output.push_str("\n📋 Headers:\n");
        for (name, value) in response_headers {
            let _ = writeln!(output, "   {name}: {value}");
        }
    }
}

/// Write the body size line to `output`.
pub(crate) fn write_body_info(output: &mut String, body_len: usize) {
    let _ = writeln!(output, "\n📄 Body: {body_len} bytes");
}

// ─── Validation test runners ─────────────────────────────────────────────────

/// Run a simple fetch-and-check validation test.
pub(crate) async fn run_validation_test(
    client: &AcceleratedClient,
    output: &mut String,
    label: &str,
    url: &str,
    expected_keyword: &str,
) {
    output.push_str(label);
    let test_start = Instant::now();
    match client.fetch(url).await {
        Ok(response) => {
            let body = response.text().await.unwrap_or_default();
            if body.contains(expected_keyword) {
                let _ = writeln!(
                    output,
                    "✅ {:.0}ms, {} bytes",
                    test_start.elapsed().as_secs_f64() * 1000.0,
                    body.len()
                );
            } else {
                output.push_str("⚠️ Unexpected content\n");
            }
        }
        Err(e) => {
            let _ = writeln!(output, "❌ {e}");
        }
    }
}

/// Run the TLS 1.3 validation test.
pub(crate) async fn run_tls_test(client: &AcceleratedClient, output: &mut String) {
    output.push_str("3️⃣  TLS 1.3 (cloudflare.com)... ");
    let test_start = Instant::now();
    match client.fetch("https://www.cloudflare.com").await {
        Ok(response) => {
            if response.status().is_success() {
                let _ = writeln!(
                    output,
                    "✅ {:.0}ms",
                    test_start.elapsed().as_secs_f64() * 1000.0
                );
            } else {
                let _ = writeln!(output, "⚠️ Status: {}", response.status());
            }
        }
        Err(e) => {
            let _ = writeln!(output, "❌ {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_cookie_browser_name_with;

    #[test]
    fn cookie_browser_defaults_to_auto_detection_when_omitted() {
        let browser = resolve_cookie_browser_name_with(None, || "firefox".to_string());
        assert_eq!(browser.as_deref(), Some("firefox"));
    }

    #[test]
    fn cookie_browser_auto_uses_detected_default() {
        let browser = resolve_cookie_browser_name_with(Some("auto"), || "brave".to_string());
        assert_eq!(browser.as_deref(), Some("brave"));
    }

    #[test]
    fn cookie_browser_none_disables_cookies() {
        let browser = resolve_cookie_browser_name_with(Some("none"), || "chrome".to_string());
        assert_eq!(browser, None);
    }

    #[test]
    fn cookie_browser_preserves_explicit_override() {
        let browser = resolve_cookie_browser_name_with(Some("safari"), || "chrome".to_string());
        assert_eq!(browser.as_deref(), Some("safari"));
    }
}
