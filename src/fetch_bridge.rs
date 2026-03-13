//! Fetch API Bridge - Bridges JavaScript `fetch()` to Rust reqwest
//!
//! This module provides a `fetch()` implementation for `QuickJS` that bridges
//! to Rust's reqwest HTTP client.
//!
//! Architecture:
//! ```text
//! JavaScript:  fetch("/api/data")
//!      ↓       Native function call
//! Rust:        reqwest::blocking::get(url).text()
//!      ↓       HTTP/2 client with cookies
//! HTTP:        GET /api/data Cookie: ...
//!      ↓
//! JavaScript:  Returns response text
//! ```

use anyhow::{Result, bail};
use reqwest::blocking::Client;
use reqwest::header::LOCATION;
use rquickjs::{Context, Function};
use std::sync::{Arc, Mutex};
use url::Url;

use crate::ssrf::{self, DEFAULT_MAX_REDIRECTS};

/// HTTP client wrapper for fetch bridge
#[derive(Clone)]
pub struct FetchClient {
    client: Client,
    cookie_header: String,
    base_url: String,
    /// Log of all fetched URLs (for debugging/discovery)
    fetch_log: Arc<Mutex<Vec<String>>>,
}

impl FetchClient {
    /// Create a new fetch client with optional cookies and base URL
    #[must_use]
    pub fn new(cookies: Option<String>, base_url: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .user_agent("nab/1.0")
                .redirect(reqwest::redirect::Policy::none()) // SSRF: Disable automatic redirects
                .build()
                .expect("HTTP client builder should succeed with default config"),
            cookie_header: cookies.unwrap_or_default(),
            base_url: base_url.unwrap_or_default(),
            fetch_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get the list of all fetched URLs
    #[must_use]
    pub fn get_fetch_log(&self) -> Vec<String> {
        self.fetch_log
            .lock()
            .expect("Fetch log mutex should not be poisoned")
            .clone()
    }

    /// Fetch a URL and return the response body as text
    /// This is a blocking call that executes the HTTP request synchronously
    ///
    /// # SSRF Protection
    /// - Validates URL against private/loopback/link-local ranges
    /// - Follows redirects manually with per-hop validation
    /// - Caps redirect chain at 5 hops
    pub fn fetch_sync(&self, url: String) -> Result<String> {
        // Resolve relative URLs against base_url
        let full_url = if url.starts_with("http://") || url.starts_with("https://") {
            url
        } else if url.starts_with('/') && !self.base_url.is_empty() {
            format!("{}{}", self.base_url, url)
        } else {
            url
        };

        // Parse URL for SSRF validation
        let mut current_url: Url = full_url
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid URL '{full_url}': {e}"))?;

        // SSRF validation: Validate initial URL
        ssrf::validate_url(&current_url)?;

        // Log the fetch for discovery
        if let Ok(mut log) = self.fetch_log.lock() {
            log.push(current_url.to_string());
        }

        // Manual redirect loop with per-hop SSRF validation
        let mut redirect_count = 0u32;

        loop {
            let mut request = self.client.get(current_url.as_str());

            // Add cookies
            if !self.cookie_header.is_empty() {
                request = request.header("Cookie", &self.cookie_header);
            }

            // Execute request (blocking)
            let response = request.send()?;
            let status = response.status();

            // Handle redirects manually
            if status.is_redirection() {
                redirect_count += 1;
                if redirect_count > DEFAULT_MAX_REDIRECTS {
                    bail!(
                        "Too many redirects ({redirect_count} > {DEFAULT_MAX_REDIRECTS}): started at {full_url}"
                    );
                }

                // Extract Location header
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| anyhow::anyhow!("Redirect without Location header"))?;

                // Resolve relative redirect against current URL
                let next_url = current_url
                    .join(location)
                    .map_err(|e| anyhow::anyhow!("Invalid redirect URL '{location}': {e}"))?;

                // SSRF validation: Validate redirect target
                ssrf::validate_url(&next_url)?;

                current_url = next_url;
                continue;
            }

            // Non-redirect response: return body
            let body = response.text()?;
            return Ok(body);
        }
    }
}

/// Inject `fetch()` global into `QuickJS` context
/// This creates a synchronous `fetch()` that blocks on HTTP requests
#[allow(clippy::too_many_lines)] // Complex JS bridge function; splitting would reduce clarity
pub fn inject_fetch_sync(ctx: &Context, client: FetchClient) -> Result<()> {
    ctx.with(|ctx| {
        // Create fetch function
        let fetch_fn = Function::new(ctx.clone(), {
            move |url: String| {
                client
                    .fetch_sync(url)
                    .unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
            }
        })?;

        // Set global fetch
        ctx.globals().set("fetch", fetch_fn)?;

        // Create a minimal Response + Promise polyfill for fetch() compatibility
        // QuickJS has no event loop, so we use synchronous "fake" Promises
        let response_code = r#"
            // Minimal Promise polyfill that resolves immediately (no event loop)
            class SyncPromise {
                constructor(executor) {
                    this._state = 'pending';
                    this._value = undefined;
                    this._handlers = [];

                    const resolve = (value) => {
                        if (this._state !== 'pending') return;
                        this._state = 'fulfilled';
                        this._value = value;
                        this._handlers.forEach(h => h.onFulfilled && h.onFulfilled(value));
                    };

                    const reject = (reason) => {
                        if (this._state !== 'pending') return;
                        this._state = 'rejected';
                        this._value = reason;
                        this._handlers.forEach(h => h.onRejected && h.onRejected(reason));
                    };

                    try {
                        executor(resolve, reject);
                    } catch (e) {
                        reject(e);
                    }
                }

                then(onFulfilled, onRejected) {
                    return new SyncPromise((resolve, reject) => {
                        const handle = () => {
                            try {
                                if (this._state === 'fulfilled') {
                                    const result = onFulfilled ? onFulfilled(this._value) : this._value;
                                    resolve(result);
                                } else if (this._state === 'rejected') {
                                    if (onRejected) {
                                        const result = onRejected(this._value);
                                        resolve(result);
                                    } else {
                                        reject(this._value);
                                    }
                                }
                            } catch (e) {
                                reject(e);
                            }
                        };

                        if (this._state !== 'pending') {
                            handle();
                        } else {
                            this._handlers.push({ onFulfilled: () => handle(), onRejected: () => handle() });
                        }
                    });
                }

                catch(onRejected) {
                    return this.then(null, onRejected);
                }

                finally(onFinally) {
                    return this.then(
                        value => { onFinally && onFinally(); return value; },
                        reason => { onFinally && onFinally(); throw reason; }
                    );
                }

                static resolve(value) {
                    return new SyncPromise(resolve => resolve(value));
                }

                static reject(reason) {
                    return new SyncPromise((_, reject) => reject(reason));
                }
            }

            // Use SyncPromise as global Promise if not available
            if (typeof Promise === 'undefined') {
                globalThis.Promise = SyncPromise;
            }

            class Response {
                constructor(body, init = {}) {
                    this.body = body;
                    this.ok = init.ok !== false;
                    this.status = init.status || 200;
                    this.statusText = init.statusText || 'OK';
                    this.headers = init.headers || {};
                    this._bodyUsed = false;
                }

                text() {
                    if (this._bodyUsed) return SyncPromise.reject(new Error('Body already read'));
                    this._bodyUsed = true;
                    return SyncPromise.resolve(this.body);
                }

                json() {
                    return this.text().then(text => JSON.parse(text));
                }

                clone() {
                    return new Response(this.body, {
                        ok: this.ok,
                        status: this.status,
                        statusText: this.statusText,
                        headers: this.headers
                    });
                }
            }

            // Override native fetch to return Promise<Response>
            const _nativeFetch = fetch;
            globalThis.fetch = function(url, options = {}) {
                return new SyncPromise((resolve, reject) => {
                    try {
                        const body = _nativeFetch(url);
                        // Check for error response
                        if (body && body.startsWith('{"error":')) {
                            const err = JSON.parse(body);
                            reject(new Error(err.error));
                        } else {
                            resolve(new Response(body, { ok: true, status: 200 }));
                        }
                    } catch (e) {
                        reject(e);
                    }
                });
            };
        "#;

        ctx.eval::<(), _>(response_code)?;

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Client construction tests ───────────────────────────────────────

    #[test]
    fn test_fetch_client_new() {
        let client = FetchClient::new(None, None);
        assert!(client.cookie_header.is_empty());
        assert!(client.base_url.is_empty());
    }

    #[test]
    fn test_fetch_client_with_cookies() {
        let client = FetchClient::new(Some("session=abc123".to_string()), None);
        assert_eq!(client.cookie_header, "session=abc123");
    }

    #[test]
    fn test_fetch_client_with_base_url() {
        let client = FetchClient::new(None, Some("https://example.com".to_string()));
        assert_eq!(client.base_url, "https://example.com");
    }

    #[test]
    fn test_fetch_log_empty_initially() {
        let client = FetchClient::new(None, None);
        let log = client.get_fetch_log();
        assert!(log.is_empty());
    }

    // ─── SSRF validation tests ───────────────────────────────────────────

    #[test]
    fn fetch_sync_blocks_loopback() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("http://127.0.0.1/secret".to_string());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[test]
    fn fetch_sync_blocks_localhost() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("http://localhost/admin".to_string());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[test]
    fn fetch_sync_blocks_private_10() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("http://10.0.0.1/internal".to_string());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[test]
    fn fetch_sync_blocks_private_192() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("http://192.168.1.1/router".to_string());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[test]
    fn fetch_sync_blocks_private_172() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("http://172.16.0.1/admin".to_string());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[test]
    fn fetch_sync_blocks_link_local() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("http://169.254.169.254/latest/meta-data".to_string());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[test]
    fn fetch_sync_blocks_ipv6_loopback() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("http://[::1]/secret".to_string());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[test]
    fn fetch_sync_blocks_ipv4_mapped_ipv6_loopback() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("http://[::ffff:127.0.0.1]/secret".to_string());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[test]
    fn fetch_sync_blocks_ipv4_mapped_ipv6_private() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("http://[::ffff:192.168.1.1]/admin".to_string());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("SSRF"), "Error should mention SSRF: {err}");
    }

    #[test]
    fn fetch_sync_blocks_file_scheme() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("file:///etc/passwd".to_string());
        assert!(result.is_err());
        // Should fail at URL parsing or scheme validation
    }

    #[test]
    fn fetch_sync_blocks_ftp_scheme() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("ftp://internal.server/data".to_string());
        assert!(result.is_err());
        // Should fail at SSRF validation (no host_str) or connection
    }

    // ─── Relative URL resolution tests ───────────────────────────────────

    #[test]
    fn fetch_sync_resolves_relative_url_with_base() {
        let client = FetchClient::new(None, Some("https://example.com".to_string()));
        // This will fail at network level (example.com), but should pass URL resolution
        let result = client.fetch_sync("/api/data".to_string());
        // Check that the error is NOT about invalid URL format
        if let Err(e) = result {
            let err_str = e.to_string();
            assert!(
                !err_str.contains("Invalid URL"),
                "Should not fail on URL parsing for relative URLs with base_url"
            );
        }
    }

    #[test]
    fn fetch_sync_accepts_absolute_url() {
        let client = FetchClient::new(None, None);
        // This will fail at network level, but URL should be valid
        let result = client.fetch_sync("https://example.com".to_string());
        if let Err(e) = result {
            let err_str = e.to_string();
            assert!(
                !err_str.contains("Invalid URL"),
                "Should not fail on URL parsing for absolute URLs"
            );
        }
    }

    // ─── Integration tests (require network) ─────────────────────────────
    // These tests are marked with #[ignore] by default to avoid network
    // dependency in CI. Run with `cargo test -- --ignored` to execute.

    #[test]
    #[ignore = "requires network access"]
    fn fetch_sync_allows_public_url() {
        let client = FetchClient::new(None, None);
        let result = client.fetch_sync("https://httpbin.org/get".to_string());
        assert!(result.is_ok(), "Public URL should be allowed: {result:?}");
        let body = result.unwrap();
        assert!(
            body.contains("httpbin") || body.contains("headers"),
            "Body should contain httpbin response"
        );
    }

    #[test]
    #[ignore = "requires network access"]
    fn fetch_sync_follows_redirects_with_validation() {
        let client = FetchClient::new(None, None);
        // httpbin.org/redirect/1 redirects once to /get
        let result = client.fetch_sync("https://httpbin.org/redirect/1".to_string());
        assert!(result.is_ok(), "Should follow valid redirects: {result:?}");
        let body = result.unwrap();
        assert!(body.contains("httpbin"), "Should reach final destination");
    }

    #[test]
    #[ignore = "requires network access"]
    fn fetch_sync_enforces_redirect_limit() {
        let client = FetchClient::new(None, None);
        // httpbin.org/redirect/10 redirects 10 times (exceeds DEFAULT_MAX_REDIRECTS=5)
        let result = client.fetch_sync("https://httpbin.org/redirect/10".to_string());
        assert!(result.is_err(), "Should reject excessive redirects");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Too many redirects"),
            "Error should mention redirect limit: {err}"
        );
    }
}
