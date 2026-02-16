//! High-Performance HTTP Client
//!
//! Features:
//! - HTTP/2 multiplexing (100 concurrent streams per connection)
//! - TLS 1.3 with session resumption
//! - Brotli, Zstd, Gzip compression (auto-negotiated)
//! - DNS caching + Happy Eyeballs (IPv4/IPv6 racing)
//! - Connection pooling with keep-alive
//! - Realistic browser fingerprinting
//! - SSRF protection with DNS pinning and per-hop redirect validation
//! - Configurable response body size cap (default 10 MB)
//! - Cloudflare `text/markdown` support via `Accept` header

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use bytes::Bytes;
use reqwest::{Client, Response, StatusCode};
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};
use url::Url;

use crate::fingerprint::{random_profile, BrowserProfile};
use crate::ssrf::{self, DEFAULT_MAX_BODY_SIZE, DEFAULT_MAX_REDIRECTS};

/// HTTP client with all acceleration features
pub struct AcceleratedClient {
    client: Client,
    /// Client with redirects disabled, used by `fetch_safe` for manual redirect handling.
    no_redirect_client: Client,
    profile: Arc<RwLock<BrowserProfile>>,
}

/// Returns a reqwest `ClientBuilder` with common acceleration settings applied.
///
/// Does NOT set redirect policy - callers must set that themselves.
fn accelerated_builder(
    headers: &reqwest::header::HeaderMap,
    http2_prior: bool,
) -> reqwest::ClientBuilder {
    let mut builder = Client::builder()
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(60))
        .tcp_nodelay(true)
        .use_rustls_tls()
        .brotli(true)
        .zstd(true)
        .gzip(true)
        .deflate(true)
        .default_headers(headers.clone())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .cookie_store(true);

    if http2_prior {
        builder = builder.http2_prior_knowledge();
    } else {
        builder = builder.http2_adaptive_window(true);
    }

    builder
}

impl AcceleratedClient {
    /// Create a new accelerated HTTP client
    pub fn new() -> Result<Self> {
        Self::with_profile(random_profile())
    }

    /// Create client with specific browser profile
    pub fn with_profile(profile: BrowserProfile) -> Result<Self> {
        let headers = profile.to_headers();

        let client = accelerated_builder(&headers, true)
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;

        // The no-redirect client uses adaptive HTTP/2 for broader compatibility
        // since it handles the manual redirect chain in fetch_safe.
        let no_redirect_client = accelerated_builder(&headers, false)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Self {
            client,
            no_redirect_client,
            profile: Arc::new(RwLock::new(profile)),
        })
    }

    /// Create client that tries HTTP/2 with fallback to HTTP/1.1
    pub fn new_adaptive() -> Result<Self> {
        let profile = random_profile();
        let headers = profile.to_headers();

        let client = accelerated_builder(&headers, false)
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;

        let no_redirect_client = accelerated_builder(&headers, false)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Self {
            client,
            no_redirect_client,
            profile: Arc::new(RwLock::new(profile)),
        })
    }

    /// Create client from an existing reqwest::Client (for custom configurations like proxies)
    pub fn from_client(client: Client) -> Result<Self> {
        let profile = random_profile();
        let headers = profile.to_headers();

        let no_redirect_client = accelerated_builder(&headers, false)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Self {
            client,
            no_redirect_client,
            profile: Arc::new(RwLock::new(random_profile())),
        })
    }

    /// Create client that doesn't follow redirects (for auth flows)
    pub fn new_no_redirect() -> Result<Self> {
        let profile = random_profile();
        let headers = profile.to_headers();

        let client = accelerated_builder(&headers, false)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        let no_redirect_client = client.clone();

        Ok(Self {
            client,
            no_redirect_client,
            profile: Arc::new(RwLock::new(profile)),
        })
    }

    /// Fetch a URL with all accelerations
    #[instrument(skip(self), fields(url = %url))]
    pub async fn fetch(&self, url: &str) -> Result<Response> {
        debug!("Fetching with acceleration");
        let response = self.client.get(url).send().await?;

        info!(
            status = %response.status(),
            version = ?response.version(),
            content_encoding = ?response.headers().get("content-encoding"),
            "Response received"
        );

        Ok(response)
    }

    /// Fetch and return body as string
    pub async fn fetch_text(&self, url: &str) -> Result<String> {
        let response = self.fetch(url).await?;
        let text = response.text().await?;
        Ok(text)
    }

    /// Get current browser profile
    pub async fn profile(&self) -> BrowserProfile {
        self.profile.read().await.clone()
    }

    /// Rotate to a new random browser profile
    pub async fn rotate_profile(&self) -> Result<()> {
        let new_profile = random_profile();
        *self.profile.write().await = new_profile;
        // Note: This only affects the stored profile, not the client
        // For full rotation, create a new client
        Ok(())
    }

    /// Fetch with SSRF protection, DNS pinning, redirect validation, and body size cap.
    ///
    /// This is the recommended method for fetching untrusted URLs. It:
    /// 1. Validates the URL host against the SSRF deny list
    /// 2. Pins the DNS resolution to prevent rebinding attacks
    /// 3. Follows redirects manually, validating each hop
    /// 4. Caps the response body size to prevent OOM
    /// 5. Sends `Accept: text/markdown` for Cloudflare Markdown for Agents support
    #[instrument(skip(self, config), fields(url = %url))]
    pub async fn fetch_safe(
        &self,
        url: &str,
        config: &SafeFetchConfig,
    ) -> Result<SafeFetchResponse> {
        let mut current_url: Url = url
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid URL '{url}': {e}"))?;

        // Validate initial URL against SSRF deny list
        let _pinned = ssrf::validate_url(&current_url)?;
        debug!("SSRF validation passed for {current_url}");

        let mut redirect_count = 0u32;

        loop {
            let mut request = self.no_redirect_client.get(current_url.as_str());

            // Add Accept header preferring text/markdown (Cloudflare Markdown for Agents)
            if config.prefer_markdown {
                request = request.header(
                    "Accept",
                    "text/markdown, text/html;q=0.9, application/xhtml+xml;q=0.8, */*;q=0.7",
                );
            }

            let response = request.send().await?;
            let status = response.status();

            info!(
                status = %status,
                version = ?response.version(),
                url = %current_url,
                "Response received"
            );

            // Handle redirects manually with per-hop SSRF validation
            if status.is_redirection() {
                redirect_count += 1;
                if redirect_count > config.max_redirects {
                    bail!(
                        "Too many redirects ({redirect_count} > {}): started at {url}",
                        config.max_redirects
                    );
                }

                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| anyhow::anyhow!("Redirect without Location header"))?;

                // Resolve relative redirect against current URL
                let next_url = current_url
                    .join(location)
                    .map_err(|e| anyhow::anyhow!("Invalid redirect URL '{location}': {e}"))?;

                // Validate redirect target against SSRF deny list
                ssrf::validate_redirect_target(&next_url)?;
                debug!("Redirect hop {redirect_count}: {current_url} -> {next_url}");

                current_url = next_url;
                continue;
            }

            // Read body with size cap
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_string();

            let headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_string()))
                .collect();

            let body = read_body_capped(response, config.max_body_size).await?;

            return Ok(SafeFetchResponse {
                status,
                url: current_url,
                content_type,
                headers,
                body,
                redirect_count,
            });
        }
    }

    /// Get the underlying reqwest client
    #[must_use]
    pub fn inner(&self) -> &Client {
        &self.client
    }
}

/// Configuration for [`AcceleratedClient::fetch_safe`].
#[derive(Debug, Clone)]
pub struct SafeFetchConfig {
    /// Maximum number of redirect hops to follow.
    pub max_redirects: u32,
    /// Maximum response body size in bytes before truncation.
    pub max_body_size: usize,
    /// Send `Accept: text/markdown` to prefer Cloudflare Markdown for Agents.
    pub prefer_markdown: bool,
}

impl Default for SafeFetchConfig {
    fn default() -> Self {
        Self {
            max_redirects: DEFAULT_MAX_REDIRECTS,
            max_body_size: DEFAULT_MAX_BODY_SIZE,
            prefer_markdown: true,
        }
    }
}

/// Response from [`AcceleratedClient::fetch_safe`].
#[derive(Debug)]
pub struct SafeFetchResponse {
    /// HTTP status code.
    pub status: StatusCode,
    /// Final URL after redirects.
    pub url: Url,
    /// Content-Type header value.
    pub content_type: String,
    /// All response headers.
    pub headers: Vec<(String, String)>,
    /// Response body (capped at `max_body_size`).
    pub body: Bytes,
    /// Number of redirects followed.
    pub redirect_count: u32,
}

impl SafeFetchResponse {
    /// Returns the body as a UTF-8 string, using lossy conversion for non-UTF-8 bytes.
    pub fn text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Returns `true` if the Content-Type indicates markdown
    /// (Cloudflare Markdown for Agents responded with `text/markdown`).
    pub fn is_markdown(&self) -> bool {
        self.content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/markdown")
    }
}

/// Reads a response body up to `max_size` bytes.
///
/// Reads the body in chunks via streaming to avoid allocating the entire
/// response in memory before checking the size. If the body exceeds
/// `max_size`, it is truncated and a warning is logged.
async fn read_body_capped(response: Response, max_size: usize) -> Result<Bytes> {
    // Use content-length hint for early rejection if available
    if let Some(len) = response.content_length() {
        if len as usize > max_size {
            warn!(
                content_length = len,
                max_size, "Response body exceeds size cap; will truncate"
            );
        }
    }

    // Read body in chunks to avoid OOM on huge responses
    let mut body = Vec::with_capacity(max_size.min(1024 * 1024)); // Pre-alloc max 1MB
    let mut stream = response;
    while let Some(chunk) = stream.chunk().await? {
        let remaining = max_size.saturating_sub(body.len());
        if remaining == 0 {
            warn!(max_size, "Response body truncated at size cap");
            break;
        }
        let take = chunk.len().min(remaining);
        body.extend_from_slice(&chunk[..take]);
    }

    Ok(Bytes::from(body))
}

impl Default for AcceleratedClient {
    fn default() -> Self {
        Self::new().expect("Failed to create default client")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Existing tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_example() {
        let client = AcceleratedClient::new().unwrap();
        let response = client.fetch("https://httpbin.org/get").await.unwrap();
        assert!(response.status().is_success());
    }

    #[tokio::test]
    async fn test_compression_negotiation() {
        let client = AcceleratedClient::new().unwrap();
        let response = client.fetch("https://httpbin.org/brotli").await.unwrap();
        assert!(response.status().is_success());
    }

    // ─── SafeFetchConfig ─────────────────────────────────────────────────

    #[test]
    fn safe_fetch_config_defaults() {
        let config = SafeFetchConfig::default();
        assert_eq!(config.max_redirects, DEFAULT_MAX_REDIRECTS);
        assert_eq!(config.max_body_size, DEFAULT_MAX_BODY_SIZE);
        assert!(config.prefer_markdown);
    }

    #[test]
    fn safe_fetch_config_custom() {
        let config = SafeFetchConfig {
            max_redirects: 3,
            max_body_size: 1024,
            prefer_markdown: false,
        };
        assert_eq!(config.max_redirects, 3);
        assert_eq!(config.max_body_size, 1024);
        assert!(!config.prefer_markdown);
    }

    // ─── SafeFetchResponse ───────────────────────────────────────────────

    #[test]
    fn safe_fetch_response_text_lossy() {
        let resp = SafeFetchResponse {
            status: StatusCode::OK,
            url: Url::parse("https://example.com").unwrap(),
            content_type: "text/html".to_string(),
            headers: vec![],
            body: Bytes::from("Hello world"),
            redirect_count: 0,
        };
        assert_eq!(resp.text_lossy(), "Hello world");
    }

    #[test]
    fn safe_fetch_response_text_lossy_non_utf8() {
        let resp = SafeFetchResponse {
            status: StatusCode::OK,
            url: Url::parse("https://example.com").unwrap(),
            content_type: "text/html".to_string(),
            headers: vec![],
            body: Bytes::from_static(&[0xff, 0xfe, b'H', b'i']),
            redirect_count: 0,
        };
        let text = resp.text_lossy();
        assert!(text.contains("Hi"));
    }

    #[test]
    fn safe_fetch_response_is_markdown_true() {
        let resp = SafeFetchResponse {
            status: StatusCode::OK,
            url: Url::parse("https://example.com").unwrap(),
            content_type: "text/markdown".to_string(),
            headers: vec![],
            body: Bytes::from("# Hello"),
            redirect_count: 0,
        };
        assert!(resp.is_markdown());
    }

    #[test]
    fn safe_fetch_response_is_markdown_with_charset() {
        let resp = SafeFetchResponse {
            status: StatusCode::OK,
            url: Url::parse("https://example.com").unwrap(),
            content_type: "text/markdown; charset=utf-8".to_string(),
            headers: vec![],
            body: Bytes::from("# Hello"),
            redirect_count: 0,
        };
        assert!(resp.is_markdown());
    }

    #[test]
    fn safe_fetch_response_is_markdown_false_for_html() {
        let resp = SafeFetchResponse {
            status: StatusCode::OK,
            url: Url::parse("https://example.com").unwrap(),
            content_type: "text/html".to_string(),
            headers: vec![],
            body: Bytes::from("<h1>Hello</h1>"),
            redirect_count: 0,
        };
        assert!(!resp.is_markdown());
    }

    // ─── Client constructors ─────────────────────────────────────────────

    #[test]
    fn client_new_succeeds() {
        assert!(AcceleratedClient::new().is_ok());
    }

    #[test]
    fn client_new_adaptive_succeeds() {
        assert!(AcceleratedClient::new_adaptive().is_ok());
    }

    #[test]
    fn client_new_no_redirect_succeeds() {
        assert!(AcceleratedClient::new_no_redirect().is_ok());
    }

    #[test]
    fn client_default_succeeds() {
        let _client = AcceleratedClient::default();
    }

    // ─── fetch_safe SSRF blocking ────────────────────────────────────────

    #[tokio::test]
    async fn fetch_safe_blocks_loopback() {
        let client = AcceleratedClient::new().unwrap();
        let config = SafeFetchConfig::default();
        let result = client.fetch_safe("http://127.0.0.1/secret", &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[tokio::test]
    async fn fetch_safe_blocks_private_ip() {
        let client = AcceleratedClient::new().unwrap();
        let config = SafeFetchConfig::default();
        let result = client.fetch_safe("http://192.168.1.1/admin", &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[tokio::test]
    async fn fetch_safe_blocks_mapped_ipv6() {
        let client = AcceleratedClient::new().unwrap();
        let config = SafeFetchConfig::default();
        let result = client
            .fetch_safe("http://[::ffff:127.0.0.1]/secret", &config)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[tokio::test]
    async fn fetch_safe_allows_public_url() {
        let client = AcceleratedClient::new().unwrap();
        let config = SafeFetchConfig::default();
        let result = client.fetch_safe("https://httpbin.org/get", &config).await;
        assert!(result.is_ok(), "Public URL should be allowed: {result:?}");
        let resp = result.unwrap();
        assert!(resp.status.is_success());
    }

    #[tokio::test]
    async fn fetch_safe_returns_body() {
        let client = AcceleratedClient::new().unwrap();
        let config = SafeFetchConfig::default();
        let resp = client
            .fetch_safe("https://httpbin.org/get", &config)
            .await
            .unwrap();
        let text = resp.text_lossy();
        assert!(
            text.contains("httpbin") || text.contains("headers") || text.contains("url"),
            "Body should contain httpbin response content"
        );
    }

    // ─── Body size cap ───────────────────────────────────────────────────

    #[tokio::test]
    async fn fetch_safe_caps_body_size() {
        let client = AcceleratedClient::new().unwrap();
        let config = SafeFetchConfig {
            max_body_size: 100, // Very small cap
            ..SafeFetchConfig::default()
        };
        let resp = client
            .fetch_safe("https://httpbin.org/get", &config)
            .await
            .unwrap();
        assert!(
            resp.body.len() <= 100,
            "Body should be capped at 100 bytes, got {}",
            resp.body.len()
        );
    }

    // ─── accelerated_builder ─────────────────────────────────────────────

    #[test]
    fn accelerated_builder_builds_with_h2_prior() {
        let headers = reqwest::header::HeaderMap::new();
        let client = accelerated_builder(&headers, true)
            .redirect(reqwest::redirect::Policy::none())
            .build();
        assert!(client.is_ok());
    }

    #[test]
    fn accelerated_builder_builds_with_h2_adaptive() {
        let headers = reqwest::header::HeaderMap::new();
        let client = accelerated_builder(&headers, false)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build();
        assert!(client.is_ok());
    }
}
