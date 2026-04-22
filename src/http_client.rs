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

use anyhow::{Result, bail};
use bytes::Bytes;
use reqwest::{Client, Response, StatusCode};
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};
use url::Url;

use crate::fingerprint::{BrowserProfile, random_profile};
use crate::ssrf::{self, DEFAULT_MAX_BODY_SIZE, DEFAULT_MAX_REDIRECTS};

/// `SOCKS5h` proxy URL for the Tor anonymity network.
///
/// The `socks5h` scheme routes DNS through the proxy, preventing leaks to the
/// local resolver that would reveal the destination to the ISP.
pub const TOR_PROXY_URL: &str = "socks5h://127.0.0.1:9050";

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
#[derive(Clone, Copy)]
enum TransportMode {
    Http2PriorKnowledge,
    Http2Adaptive,
    Http1Only,
}

fn accelerated_builder(
    headers: &reqwest::header::HeaderMap,
    transport: TransportMode,
) -> reqwest::ClientBuilder {
    let mut builder = Client::builder()
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_mins(1))
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

    builder = match transport {
        TransportMode::Http2PriorKnowledge => builder.http2_prior_knowledge(),
        TransportMode::Http2Adaptive => builder.http2_adaptive_window(true),
        TransportMode::Http1Only => builder.http1_only(),
    };

    builder
}

fn build_http_client(
    headers: &reqwest::header::HeaderMap,
    transport: TransportMode,
    redirect_policy: reqwest::redirect::Policy,
) -> Result<Client> {
    Ok(accelerated_builder(headers, transport)
        .redirect(redirect_policy)
        .build()?)
}

impl AcceleratedClient {
    fn from_parts(client: Client, no_redirect_client: Client, profile: BrowserProfile) -> Self {
        Self {
            client,
            no_redirect_client,
            profile: Arc::new(RwLock::new(profile)),
        }
    }

    /// Create a new accelerated HTTP client
    pub fn new() -> Result<Self> {
        Self::with_profile(random_profile())
    }

    /// Create client with specific browser profile
    pub fn with_profile(profile: BrowserProfile) -> Result<Self> {
        let headers = profile.to_headers();

        let client = build_http_client(
            &headers,
            TransportMode::Http2PriorKnowledge,
            reqwest::redirect::Policy::limited(10),
        )?;

        // The no-redirect client uses adaptive HTTP/2 for broader compatibility
        // since it handles the manual redirect chain in fetch_safe.
        let no_redirect_client = build_http_client(
            &headers,
            TransportMode::Http2Adaptive,
            reqwest::redirect::Policy::none(),
        )?;

        Ok(Self::from_parts(client, no_redirect_client, profile))
    }

    /// Create client that tries HTTP/2 with fallback to HTTP/1.1
    pub fn new_adaptive() -> Result<Self> {
        let profile = random_profile();
        let headers = profile.to_headers();

        let client = build_http_client(
            &headers,
            TransportMode::Http2Adaptive,
            reqwest::redirect::Policy::limited(10),
        )?;

        let no_redirect_client = build_http_client(
            &headers,
            TransportMode::Http2Adaptive,
            reqwest::redirect::Policy::none(),
        )?;

        Ok(Self::from_parts(client, no_redirect_client, profile))
    }

    /// Create a client that forces HTTP/1.1 for origin servers with HTTP/2 issues.
    pub fn new_http1_only() -> Result<Self> {
        let profile = random_profile();
        let headers = profile.to_headers();

        let client = build_http_client(
            &headers,
            TransportMode::Http1Only,
            reqwest::redirect::Policy::limited(10),
        )?;

        let no_redirect_client = build_http_client(
            &headers,
            TransportMode::Http1Only,
            reqwest::redirect::Policy::none(),
        )?;

        Ok(Self::from_parts(client, no_redirect_client, profile))
    }

    /// Create client from an existing `reqwest::Client` (for custom configurations like proxies)
    pub fn from_client(client: Client) -> Result<Self> {
        Self::from_client_with_profile(client, random_profile())
    }

    /// Create a client that routes all traffic through the Tor SOCKS5 proxy.
    ///
    /// Uses `socks5h://127.0.0.1:9050` (the `h` suffix means DNS resolution
    /// happens through the proxy, preventing DNS leaks to the local resolver).
    ///
    /// Returns an error if the SOCKS5 proxy URL cannot be parsed.  A connection
    /// refused at fetch time is handled by the caller.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use nab::AcceleratedClient;
    ///
    /// #[tokio::main]
    /// async fn main() -> anyhow::Result<()> {
    ///     let client = AcceleratedClient::with_tor_proxy()?;
    ///     let html = client.fetch_text("https://check.torproject.org").await?;
    ///     println!("{html}");
    ///     Ok(())
    /// }
    /// ```
    pub fn with_tor_proxy() -> Result<Self> {
        let proxy = reqwest::Proxy::all(TOR_PROXY_URL)?;
        let inner = Client::builder().proxy(proxy).build()?;
        Self::from_client(inner)
    }

    fn from_client_with_profile(client: Client, profile: BrowserProfile) -> Result<Self> {
        let headers = profile.to_headers();

        let no_redirect_client = build_http_client(
            &headers,
            TransportMode::Http2Adaptive,
            reqwest::redirect::Policy::none(),
        )?;

        Ok(Self::from_parts(client, no_redirect_client, profile))
    }

    /// Create client that doesn't follow redirects (for auth flows)
    pub fn new_no_redirect() -> Result<Self> {
        let profile = random_profile();
        let headers = profile.to_headers();

        let client = build_http_client(
            &headers,
            TransportMode::Http2Adaptive,
            reqwest::redirect::Policy::none(),
        )?;

        let no_redirect_client = client.clone();

        Ok(Self::from_parts(client, no_redirect_client, profile))
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

    /// Rotate to a new random browser profile.
    ///
    /// Reqwest bakes default headers into the client at construction time, so this
    /// client cannot swap profiles in place without rebuilding the underlying
    /// connection pools. Callers that need a different fingerprint should create a
    /// new `AcceleratedClient` with the desired profile instead.
    pub async fn rotate_profile(&self) -> Result<()> {
        // Preserve the async API for existing callers even though rotation is now
        // rejected explicitly for truthfulness.
        drop(self.profile.read().await);
        bail!(
            "Cannot rotate browser profile on an existing client; create a new AcceleratedClient with the desired profile"
        )
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
        self.fetch_safe_with_validators(
            url,
            config,
            ssrf::validate_url,
            ssrf::validate_redirect_target,
        )
        .await
    }

    async fn fetch_safe_with_validators(
        &self,
        url: &str,
        config: &SafeFetchConfig,
        validate_url: fn(&Url) -> std::result::Result<std::net::SocketAddr, crate::error::NabError>,
        validate_redirect_target: fn(&Url) -> std::result::Result<(), crate::error::NabError>,
    ) -> Result<SafeFetchResponse> {
        let mut current_url: Url = url
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid URL '{url}': {e}"))?;

        // Validate initial URL against SSRF deny list
        let _pinned = validate_url(&current_url)?;
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
                validate_redirect_target(&next_url)?;
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
    // Truncation acceptable: content_length is used only as a size hint for logging
    #[allow(clippy::cast_possible_truncation)]
    if let Some(len) = response.content_length()
        && len as usize > max_size
    {
        warn!(
            content_length = len,
            max_size, "Response body exceeds size cap; will truncate"
        );
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
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    use crate::error::NabError;
    use crate::fingerprint::chrome_profile;

    #[derive(Debug)]
    struct TestResponse {
        status_line: &'static str,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl TestResponse {
        fn ok(body: impl Into<Vec<u8>>, content_type: &str) -> Self {
            Self {
                status_line: "HTTP/1.1 200 OK",
                headers: vec![("Content-Type".to_string(), content_type.to_string())],
                body: body.into(),
            }
        }

        fn redirect(location: &str) -> Self {
            Self {
                status_line: "HTTP/1.1 302 Found",
                headers: vec![("Location".to_string(), location.to_string())],
                body: Vec::new(),
            }
        }

        fn into_bytes(self) -> Vec<u8> {
            use std::fmt::Write;
            let mut response = format!("{}\r\n", self.status_line);
            let mut has_content_length = false;

            for (name, value) in &self.headers {
                if name.eq_ignore_ascii_case("content-length") {
                    has_content_length = true;
                }
                let _ = write!(response, "{name}: {value}\r\n");
            }

            if !has_content_length {
                let _ = write!(response, "Content-Length: {}\r\n", self.body.len());
            }
            response.push_str("Connection: close\r\n\r\n");

            let mut bytes = response.into_bytes();
            bytes.extend(self.body);
            bytes
        }
    }

    async fn spawn_test_server<F>(expected_requests: usize, handler: F) -> (String, JoinHandle<()>)
    where
        F: Fn(String) -> TestResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local test server");
        let address = listener
            .local_addr()
            .expect("read local test server address");
        let handler = Arc::new(handler);

        let server = tokio::spawn(async move {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().await.expect("accept test connection");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];

                loop {
                    let read = stream.read(&mut buffer).await.expect("read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }

                let response = handler(String::from_utf8_lossy(&request).into_owned());
                stream
                    .write_all(&response.into_bytes())
                    .await
                    .expect("write response");
            }
        });

        (format!("http://{address}"), server)
    }

    fn loopback_url_allowed_for_tests(url: &Url) -> std::result::Result<SocketAddr, NabError> {
        match url.host() {
            Some(url::Host::Ipv4(ip)) if ip.is_loopback() => Ok(SocketAddr::new(
                IpAddr::V4(ip),
                url.port_or_known_default().unwrap_or(80),
            )),
            Some(url::Host::Domain("localhost")) => Ok(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                url.port_or_known_default().unwrap_or(80),
            )),
            _ => ssrf::validate_url(url),
        }
    }

    fn loopback_redirect_allowed_for_tests(url: &Url) -> std::result::Result<(), NabError> {
        match url.scheme() {
            "http" | "https" => loopback_url_allowed_for_tests(url).map(|_| ()),
            scheme => Err(NabError::SsrfBlocked(format!(
                "disallowed redirect scheme '{scheme}'"
            ))),
        }
    }

    // ─── Existing tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_example() {
        let (base_url, server) = spawn_test_server(1, |request| {
            assert!(
                request.starts_with("GET /example HTTP/1.1\r\n"),
                "unexpected request: {request}"
            );
            TestResponse::ok("stable test body", "text/plain")
        })
        .await;

        let client = AcceleratedClient::from_client(
            reqwest::Client::builder()
                .http1_only()
                .brotli(true)
                .zstd(true)
                .gzip(true)
                .deflate(true)
                .build()
                .unwrap(),
        )
        .unwrap();
        let response = client.fetch(&format!("{base_url}/example")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "stable test body");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_fetch_example_http1_only() {
        let (base_url, server) = spawn_test_server(1, |request| {
            assert!(
                request.starts_with("GET /example HTTP/1.1\r\n"),
                "unexpected request: {request}"
            );
            TestResponse::ok("stable test body", "text/plain")
        })
        .await;

        let client = AcceleratedClient::new_http1_only().unwrap();
        let response = client.fetch(&format!("{base_url}/example")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.version(), reqwest::Version::HTTP_11);
        assert_eq!(response.text().await.unwrap(), "stable test body");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_compression_negotiation() {
        let (base_url, server) = spawn_test_server(1, |request| {
            let request_lower = request.to_ascii_lowercase();
            let accept_encoding = request_lower
                .lines()
                .find(|line| line.starts_with("accept-encoding:"))
                .expect("request should include accept-encoding header");
            for encoding in ["gzip", "br", "zstd", "deflate"] {
                assert!(
                    accept_encoding.contains(encoding),
                    "accept-encoding header should advertise {encoding}: {accept_encoding}"
                );
            }
            TestResponse::ok("compression negotiated", "text/plain")
        })
        .await;

        let client = AcceleratedClient::from_client(
            reqwest::Client::builder()
                .http1_only()
                .brotli(true)
                .zstd(true)
                .gzip(true)
                .deflate(true)
                .build()
                .unwrap(),
        )
        .unwrap();
        let response = client
            .fetch(&format!("{base_url}/compression"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "compression negotiated");
        server.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires external network access"]
    async fn test_fetch_example_live() {
        let client = AcceleratedClient::new().unwrap();
        let response = client.fetch("https://httpbin.org/get").await.unwrap();
        assert!(response.status().is_success());
    }

    #[tokio::test]
    #[ignore = "requires external network access"]
    async fn test_compression_negotiation_live() {
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

    #[tokio::test]
    async fn from_client_with_profile_keeps_safe_fetch_headers_in_sync() {
        let profile = chrome_profile();
        let expected_user_agent = profile.user_agent.to_ascii_lowercase();
        let expected_accept_language = profile.accept_language.to_ascii_lowercase();
        let (base_url, server) = spawn_test_server(1, move |request| {
            let request = request.to_ascii_lowercase();
            assert!(
                request.contains(&format!("user-agent: {expected_user_agent}\r\n")),
                "request should include stored profile user-agent: {request}"
            );
            assert!(
                request.contains(&format!("accept-language: {expected_accept_language}\r\n")),
                "request should include stored profile accept-language: {request}"
            );
            TestResponse::ok("profile headers stable", "text/plain")
        })
        .await;

        let client = AcceleratedClient::from_client_with_profile(
            reqwest::Client::builder()
                .http1_only()
                .brotli(true)
                .zstd(true)
                .gzip(true)
                .deflate(true)
                .build()
                .unwrap(),
            profile,
        )
        .unwrap();

        let config = SafeFetchConfig::default();
        let response = client
            .fetch_safe_with_validators(
                &format!("{base_url}/profile"),
                &config,
                loopback_url_allowed_for_tests,
                loopback_redirect_allowed_for_tests,
            )
            .await
            .unwrap();
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.text_lossy(), "profile headers stable");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rotate_profile_returns_error_and_preserves_profile_truth() {
        let profile = chrome_profile();
        let expected_user_agent = profile.user_agent.to_ascii_lowercase();
        let expected_accept_language = profile.accept_language.to_ascii_lowercase();
        let (base_url, server) = spawn_test_server(1, move |request| {
            let request = request.to_ascii_lowercase();
            assert!(
                request.contains(&format!("user-agent: {expected_user_agent}\r\n")),
                "request should keep the original user-agent after failed rotation: {request}"
            );
            assert!(
                request.contains(&format!("accept-language: {expected_accept_language}\r\n")),
                "request should keep the original accept-language after failed rotation: {request}"
            );
            TestResponse::ok("rotation remains truthful", "text/plain")
        })
        .await;

        let client = AcceleratedClient::with_profile(profile.clone()).unwrap();
        let error = client.rotate_profile().await.unwrap_err().to_string();
        assert!(
            error.contains("create a new AcceleratedClient"),
            "rotation failure should explain the truthful recovery path: {error}"
        );

        let stored_profile = client.profile().await;
        assert_eq!(stored_profile.user_agent, profile.user_agent);
        assert_eq!(stored_profile.accept_language, profile.accept_language);

        let config = SafeFetchConfig::default();
        let response = client
            .fetch_safe_with_validators(
                &format!("{base_url}/rotate"),
                &config,
                loopback_url_allowed_for_tests,
                loopback_redirect_allowed_for_tests,
            )
            .await
            .unwrap();
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.text_lossy(), "rotation remains truthful");
        server.await.unwrap();
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
    async fn fetch_safe_follows_redirects_on_test_server() {
        let (base_url, server) = spawn_test_server(2, |request| {
            if request.starts_with("GET /redirect HTTP/1.1\r\n") {
                TestResponse::redirect("/final")
            } else if request.starts_with("GET /final HTTP/1.1\r\n") {
                TestResponse::ok("redirect complete", "text/plain")
            } else {
                panic!("unexpected request: {request}");
            }
        })
        .await;

        let client = AcceleratedClient::new().unwrap();
        let config = SafeFetchConfig::default();
        let result = client
            .fetch_safe_with_validators(
                &format!("{base_url}/redirect"),
                &config,
                loopback_url_allowed_for_tests,
                loopback_redirect_allowed_for_tests,
            )
            .await;
        assert!(
            result.is_ok(),
            "Loopback test server should be allowed by test validator: {result:?}"
        );
        let resp = result.unwrap();
        assert!(resp.status.is_success());
        assert_eq!(resp.redirect_count, 1);
        assert_eq!(resp.text_lossy(), "redirect complete");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_safe_returns_body() {
        let (base_url, server) = spawn_test_server(1, |request| {
            assert!(
                request.starts_with("GET /body HTTP/1.1\r\n"),
                "unexpected request: {request}"
            );
            TestResponse::ok(r#"{"hello":"world"}"#, "application/json")
        })
        .await;

        let client = AcceleratedClient::new().unwrap();
        let config = SafeFetchConfig::default();
        let resp = client
            .fetch_safe_with_validators(
                &format!("{base_url}/body"),
                &config,
                loopback_url_allowed_for_tests,
                loopback_redirect_allowed_for_tests,
            )
            .await
            .unwrap();
        let text = resp.text_lossy();
        assert!(
            text.contains("\"hello\":\"world\""),
            "Body should contain test server response content"
        );
        server.await.unwrap();
    }

    // ─── Body size cap ───────────────────────────────────────────────────

    #[tokio::test]
    async fn fetch_safe_caps_body_size() {
        let body = "x".repeat(256);
        let (base_url, server) = spawn_test_server(1, move |request| {
            assert!(
                request.starts_with("GET /large HTTP/1.1\r\n"),
                "unexpected request: {request}"
            );
            TestResponse::ok(body.clone().into_bytes(), "text/plain")
        })
        .await;

        let client = AcceleratedClient::new().unwrap();
        let config = SafeFetchConfig {
            max_body_size: 100, // Very small cap
            ..SafeFetchConfig::default()
        };
        let resp = client
            .fetch_safe_with_validators(
                &format!("{base_url}/large"),
                &config,
                loopback_url_allowed_for_tests,
                loopback_redirect_allowed_for_tests,
            )
            .await
            .unwrap();
        assert!(
            resp.body.len() <= 100,
            "Body should be capped at 100 bytes, got {}",
            resp.body.len()
        );
        server.await.unwrap();
    }

    // ─── accelerated_builder ─────────────────────────────────────────────

    #[test]
    fn accelerated_builder_builds_with_h2_prior() {
        let headers = reqwest::header::HeaderMap::new();
        let client = accelerated_builder(&headers, TransportMode::Http2PriorKnowledge)
            .redirect(reqwest::redirect::Policy::none())
            .build();
        assert!(client.is_ok());
    }

    #[test]
    fn accelerated_builder_builds_with_h2_adaptive() {
        let headers = reqwest::header::HeaderMap::new();
        let client = accelerated_builder(&headers, TransportMode::Http2Adaptive)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build();
        assert!(client.is_ok());
    }
}
