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

use anyhow::Result;
use reqwest::blocking::Client;
use rquickjs::{Context, Function};
use std::sync::{Arc, Mutex};

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
                .build()
                .unwrap(),
            cookie_header: cookies.unwrap_or_default(),
            base_url: base_url.unwrap_or_default(),
            fetch_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get the list of all fetched URLs
    #[must_use]
    pub fn get_fetch_log(&self) -> Vec<String> {
        self.fetch_log.lock().unwrap().clone()
    }

    /// Fetch a URL and return the response body as text
    /// This is a blocking call that executes the HTTP request synchronously
    pub fn fetch_sync(&self, url: String) -> Result<String> {
        // Resolve relative URLs against base_url
        let full_url = if url.starts_with("http://") || url.starts_with("https://") {
            url
        } else if url.starts_with('/') && !self.base_url.is_empty() {
            format!("{}{}", self.base_url, url)
        } else {
            url
        };

        // Log the fetch for discovery
        if let Ok(mut log) = self.fetch_log.lock() {
            log.push(full_url.clone());
        }

        let mut request = self.client.get(&full_url);

        // Add cookies
        if !self.cookie_header.is_empty() {
            request = request.header("Cookie", &self.cookie_header);
        }

        // Execute request (blocking)
        let response = request.send()?;
        let body = response.text()?;

        Ok(body)
    }
}

/// Inject `fetch()` global into `QuickJS` context
/// This creates a synchronous `fetch()` that blocks on HTTP requests
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

        // Also create a minimal Response object constructor for compatibility
        let response_code = r"
            class Response {
                constructor(body, init = {}) {
                    this.body = body;
                    this.ok = init.ok !== false;
                    this.status = init.status || 200;
                    this._bodyUsed = false;
                }

                async text() {
                    if (this._bodyUsed) throw new Error('Body already read');
                    this._bodyUsed = true;
                    return this.body;
                }

                async json() {
                    const text = await this.text();
                    return JSON.parse(text);
                }
            }

            // Override native fetch to return Response object
            const _nativeFetch = fetch;
            globalThis.fetch = function(url, options) {
                const body = _nativeFetch(url);
                try {
                    // Try to parse as JSON to validate
                    JSON.parse(body);
                    return new Response(body, { ok: true, status: 200 });
                } catch {
                    // Not JSON, but still valid
                    return new Response(body, { ok: true, status: 200 });
                }
            };
        ";

        ctx.eval::<(), _>(response_code)?;

        Ok(())
    })
}
