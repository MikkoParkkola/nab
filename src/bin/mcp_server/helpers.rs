//! HTTP fetch helpers for `nab-mcp` tool implementations.
//!
//! Low-level async helpers used by the tool `run` methods:
//! cookie resolution, safe/cookie-injected fetch, body conversion,
//! response formatting, and validation test runners.

use std::fmt::Write as FmtWrite;
use std::time::Instant;

use rust_mcp_sdk::schema::schema_utils::CallToolError;

use nab::content::ContentRouter;
use nab::{AcceleratedClient, CookieSource, SafeFetchConfig};

// ─── Cookie helpers ───────────────────────────────────────────────────────────

/// Resolve cookie header for a URL from the requested browser.
pub(crate) fn resolve_cookie_header(url: &str, browser: Option<&str>) -> String {
    let Some(browser) = browser else {
        return String::new();
    };
    let source = match browser.to_lowercase().as_str() {
        "chrome" => CookieSource::Chrome,
        "firefox" => CookieSource::Firefox,
        "safari" => CookieSource::Safari,
        _ => CookieSource::Brave,
    };
    let domain = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_default();
    source.get_cookie_header(&domain).unwrap_or_default()
}

// ─── Fetch helpers ────────────────────────────────────────────────────────────

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
    let response = client
        .inner()
        .get(url)
        .header("Cookie", cookie_header)
        .headers(profile.to_headers())
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
    let bytes = response
        .bytes()
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;
    Ok((status, ct, hdrs, bytes, elapsed))
}

/// Fetch a URL using a session-owned `reqwest::Client` whose cookie jar
/// already contains the session's cookies.
///
/// The caller is responsible for any URL-level SSRF validation before invoking
/// this helper.  The session client follows redirects via its own policy (up to
/// 10 hops); body bytes are returned without a size cap (same as the
/// `fetch_with_cookies` path).
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
    let bytes = response
        .bytes()
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;
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
    use nab::content::spa_extract;

    if !spa_extract::is_nextjs_metadata_only(html) {
        return None;
    }

    let script_urls = spa_extract::discover_nextjs_content_chunks(html, page_url);
    if script_urls.len() < 2 {
        return None;
    }

    tracing::debug!("Attempting Next.js content chunk recovery");

    let (webpack_resp, page_resp) = tokio::join!(
        client.fetch(&script_urls[0]),
        client.fetch(&script_urls[1]),
    );

    let webpack_js = webpack_resp.ok()?.text().await.ok()?;
    let page_js = page_resp.ok()?.text().await.ok()?;

    let origin = url::Url::parse(page_url)
        .ok()
        .map(|u| u.origin().unicode_serialization())?;

    // Extract slug from URL for targeted chunk resolution
    let slug = url::Url::parse(page_url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|segs| segs.last().map(String::from)));
    let chunk_urls = spa_extract::resolve_content_chunk_urls_for_slug(
        &webpack_js, &page_js, &origin, slug.as_deref(),
    );

    for chunk_url in &chunk_urls {
        tracing::debug!("Fetching content chunk: {chunk_url}");
        if let Ok(resp) = client.fetch(chunk_url).await {
            if let Ok(chunk_js) = resp.text().await {
                if let Some(content) = spa_extract::extract_jsx_text_content(&chunk_js) {
                    tracing::info!(
                        "Recovered {} chars from Next.js content chunk",
                        content.len()
                    );
                    return Some(content);
                }
            }
        }
    }

    None
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
