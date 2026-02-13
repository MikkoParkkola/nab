//! HTTP/3 + QUIC Client
//!
//! # Status: Enabled by Default
//!
//! HTTP/3 (RFC 9114) and QUIC (RFC 9000) are finalized standards since 2022.
//! This module is enabled by default for maximum performance.
//!
//! ## Disable HTTP/3 (if needed)
//!
//! ```bash
//! cargo build --no-default-features --features cli
//! ```
//!
//! ## Benefits
//!
//! - **0-RTT**: Resume connections instantly (vs TCP+TLS handshake)
//! - **Multiplexing**: No head-of-line blocking (unlike HTTP/2 over TCP)
//! - **Connection Migration**: Seamless network changes (`WiFi` → cellular)
//! - **UDP-based**: Better performance on lossy networks

/// HTTP/3 stub when the feature is not enabled.
///
/// All methods return errors or defaults indicating the feature is unavailable.
#[cfg(not(feature = "http3"))]
pub struct Http3Client;

#[cfg(not(feature = "http3"))]
impl Http3Client {
    /// HTTP/3 disabled -- rebuild with default features.
    ///
    /// This binary was built without HTTP/3 support.
    /// Rebuild with: `cargo build` (http3 is default)
    pub fn new(_profile: crate::fingerprint::BrowserProfile) -> anyhow::Result<Self> {
        Err(anyhow::anyhow!(
            "HTTP/3 disabled in this build. Rebuild with default features."
        ))
    }

    /// Check if a server advertises HTTP/3 support via Alt-Svc header.
    ///
    /// Performs a HEAD request and inspects the `alt-svc` response header.
    pub async fn supports_h3(url: &str) -> bool {
        let Ok(client) = reqwest::Client::builder().build() else {
            return false;
        };
        let Ok(resp) = client.head(url).send().await else {
            return false;
        };
        resp.headers()
            .get("alt-svc")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("h3"))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// HTTP/3 Implementation (when feature enabled)
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(feature = "http3")]
use std::sync::Arc;
#[cfg(feature = "http3")]
use std::time::Duration;

#[cfg(feature = "http3")]
use anyhow::{Context, Result};
#[cfg(feature = "http3")]
use bytes::Buf;
#[cfg(feature = "http3")]
use bytes::Bytes;
#[cfg(feature = "http3")]
use tracing::{debug, info};

#[cfg(feature = "http3")]
use crate::fingerprint::BrowserProfile;

/// HTTP/3 client with QUIC transport
#[cfg(feature = "http3")]
pub struct Http3Client {
    endpoint: quinn::Endpoint,
    profile: BrowserProfile,
}

#[cfg(feature = "http3")]
impl Http3Client {
    /// Create a new HTTP/3 client
    pub fn new(profile: BrowserProfile) -> Result<Self> {
        // Install crypto provider
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Build TLS config
        let mut roots = rustls::RootCertStore::empty();
        let certs = rustls_native_certs::load_native_certs();
        for cert in certs.certs {
            let _ = roots.add(cert);
        }

        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();

        // Configure QUIC
        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(Some(Duration::from_secs(30).try_into()?));
        transport.keep_alive_interval(Some(Duration::from_secs(5)));

        let mut client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)?,
        ));
        client_config.transport_config(Arc::new(transport));

        // Create endpoint (bind to any available port)
        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(client_config);

        Ok(Self { endpoint, profile })
    }

    /// Fetch a URL using HTTP/3 over QUIC.
    ///
    /// Performs DNS resolution, establishes a QUIC connection, upgrades to
    /// HTTP/3, sends a GET request, and returns the complete response.
    pub async fn fetch(&self, url: &str) -> Result<Http3Response> {
        let uri: http::Uri = url.parse().context("Invalid URL")?;
        let host = uri.host().context("No host in URL")?;
        let port = uri.port_u16().unwrap_or(443);

        info!("HTTP/3 connecting to {}:{}", host, port);

        // DNS resolution
        let addr = tokio::net::lookup_host(format!("{host}:{port}"))
            .await
            .context("DNS lookup failed for host")?
            .next()
            .context("DNS resolution returned no addresses")?;

        // QUIC connection
        let connection = self
            .endpoint
            .connect(addr, host)
            .context("Failed to initiate QUIC connection")?
            .await
            .context("QUIC handshake failed")?;

        debug!(
            "QUIC connected, protocol: {:?}",
            connection.handshake_data()
        );

        // HTTP/3 layer
        let (mut driver, mut send_request) = h3::client::new(h3_quinn::Connection::new(connection))
            .await
            .context("H3 connection setup failed")?;

        // Spawn driver task
        tokio::spawn(async move {
            let err = futures::future::poll_fn(|cx| driver.poll_close(cx)).await;
            debug!("H3 driver closed: {:?}", err);
        });

        // Build request with browser-like headers
        let request = http::Request::builder()
            .method("GET")
            .uri(url)
            .header("Host", host)
            .header("User-Agent", &self.profile.user_agent)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", &self.profile.accept_language)
            .header("Accept-Encoding", "gzip, deflate, br")
            .body(())
            .context("Failed to build HTTP/3 request")?;

        // Send request
        let mut stream = send_request
            .send_request(request)
            .await
            .context("Failed to send HTTP/3 request")?;

        stream
            .finish()
            .await
            .context("Failed to finish HTTP/3 request stream")?;

        // Receive response
        let response = stream
            .recv_response()
            .await
            .context("Failed to receive HTTP/3 response")?;
        let status = response.status();
        let headers = response.headers().clone();

        info!("HTTP/3 response: {} from {}", status, url);

        // Read body chunks
        let mut body = Vec::new();
        while let Some(mut chunk) = stream
            .recv_data()
            .await
            .context("Failed to read HTTP/3 response body")?
        {
            while chunk.has_remaining() {
                body.extend_from_slice(chunk.chunk());
                chunk.advance(chunk.chunk().len());
            }
        }

        Ok(Http3Response {
            status: status.as_u16(),
            headers,
            body: Bytes::from(body),
        })
    }

    /// Check if a server advertises HTTP/3 support via Alt-Svc header.
    ///
    /// Performs a HEAD request and inspects the `alt-svc` response header.
    pub async fn supports_h3(url: &str) -> bool {
        let Ok(client) = reqwest::Client::builder().build() else {
            return false;
        };
        let Ok(resp) = client.head(url).send().await else {
            return false;
        };
        resp.headers()
            .get("alt-svc")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("h3"))
    }
}

/// HTTP/3 response containing status, headers, and body.
#[cfg(feature = "http3")]
#[derive(Debug)]
pub struct Http3Response {
    /// HTTP status code (e.g., 200, 404).
    pub status: u16,
    /// Response headers.
    pub headers: http::HeaderMap,
    /// Response body bytes.
    pub body: Bytes,
}

#[cfg(feature = "http3")]
impl Http3Response {
    /// Decode the body as UTF-8 text.
    pub fn text(&self) -> Result<String> {
        String::from_utf8(self.body.to_vec()).context("Response body is not valid UTF-8")
    }

    /// Returns `true` if the status code is in the 2xx range.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[cfg(all(test, feature = "http3"))]
mod tests {
    use super::*;
    use crate::fingerprint::chrome_profile;

    #[tokio::test]
    async fn test_h3_detection() {
        // Cloudflare always supports H3
        let supports = Http3Client::supports_h3("https://cloudflare.com").await;
        println!("Cloudflare H3 support: {}", supports);
        // Don't assert - depends on network
    }

    #[tokio::test]
    async fn test_h3_fetch() {
        let profile = chrome_profile();
        let client = Http3Client::new(profile).unwrap();

        // Try Cloudflare (known H3 support)
        match client.fetch("https://cloudflare.com").await {
            Ok(resp) => {
                println!("H3 Status: {}", resp.status);
                assert!(resp.is_success());
            }
            Err(e) => {
                println!("H3 fetch failed (may be network): {}", e);
            }
        }
    }

    #[test]
    fn test_response_is_success_2xx() {
        let resp = Http3Response {
            status: 200,
            headers: http::HeaderMap::new(),
            body: Bytes::from("ok"),
        };
        assert!(resp.is_success());

        let resp_204 = Http3Response {
            status: 204,
            headers: http::HeaderMap::new(),
            body: Bytes::new(),
        };
        assert!(resp_204.is_success());
    }

    #[test]
    fn test_response_is_success_non_2xx() {
        let resp_404 = Http3Response {
            status: 404,
            headers: http::HeaderMap::new(),
            body: Bytes::from("not found"),
        };
        assert!(!resp_404.is_success());

        let resp_500 = Http3Response {
            status: 500,
            headers: http::HeaderMap::new(),
            body: Bytes::new(),
        };
        assert!(!resp_500.is_success());

        let resp_301 = Http3Response {
            status: 301,
            headers: http::HeaderMap::new(),
            body: Bytes::new(),
        };
        assert!(!resp_301.is_success());
    }

    #[test]
    fn test_response_text_valid_utf8() {
        let resp = Http3Response {
            status: 200,
            headers: http::HeaderMap::new(),
            body: Bytes::from("Hello, world!"),
        };
        assert_eq!(resp.text().unwrap(), "Hello, world!");
    }

    #[test]
    fn test_response_text_invalid_utf8() {
        let resp = Http3Response {
            status: 200,
            headers: http::HeaderMap::new(),
            body: Bytes::from_static(&[0xff, 0xfe]),
        };
        assert!(resp.text().is_err());
    }

    #[tokio::test]
    async fn test_client_creation() {
        let profile = chrome_profile();
        let client = Http3Client::new(profile);
        assert!(
            client.is_ok(),
            "Http3Client::new failed: {:?}",
            client.err()
        );
    }
}
