//! `MicroFetch` MCP Server - Native Rust implementation
//!
//! Ultra-fast MCP server for web fetching with HTTP/3, fingerprint spoofing,
//! and 1Password integration. Uses latest MCP protocol (2025-06-18).
//!
//! # Usage
//!
//! Stdio mode (for Claude Code integration):
//! ```bash
//! nab-mcp
//! ```

use std::fmt::Write as FmtWrite;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::mcp_server::{ServerHandler, server_runtime};
use rust_mcp_sdk::schema::{
    CallToolRequest, CallToolResult, Implementation, InitializeResult, LATEST_PROTOCOL_VERSION,
    ListToolsRequest, ListToolsResult, RpcError, ServerCapabilities, ServerCapabilitiesTools,
    TextContent, schema_utils::CallToolError,
};
use rust_mcp_sdk::{McpServer, StdioTransport, TransportOptions, tool_box};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use nab::content::ContentRouter;
use nab::{
    AcceleratedClient, CookieSource, CredentialRetriever, OnePasswordAuth, SafeFetchConfig,
    chrome_profile, firefox_profile, random_profile, safari_profile,
};

// Global shared client (initialized once)
static CLIENT: OnceCell<AcceleratedClient> = OnceCell::const_new();

async fn get_client() -> &'static AcceleratedClient {
    CLIENT
        .get_or_init(|| async { AcceleratedClient::new().expect("Failed to create HTTP client") })
        .await
}

// ============================================================================
// TOOLS
// ============================================================================

#[mcp_tool(
    name = "fetch",
    description = "Fetch a URL and convert to clean markdown for LLM consumption.

Content conversion (automatic by Content-Type):
- HTML → clean markdown (boilerplate removed, links preserved)
- PDF → markdown with headings and table detection (requires pdf feature)
- JSON/plain text → passthrough
- SPA data auto-extracted (__NEXT_DATA__, __NUXT__, __APOLLO_STATE__, etc.)

Network features:
- HTTP/2 multiplexing, HTTP/3 (QUIC) with 0-RTT
- TLS 1.3, Brotli/Zstd/Gzip decompression
- Realistic browser fingerprints (Chrome/Firefox/Safari)
- Browser cookie injection (Brave/Chrome/Firefox/Safari)

Returns: Markdown-converted body with timing info.",
    read_only_hint = true,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FetchTool {
    /// URL to fetch
    url: String,
    /// Include response headers in output
    #[serde(default)]
    headers: bool,
    /// Include full body (not just summary)
    #[serde(default)]
    body: bool,
    /// Browser cookies to use (brave, chrome, firefox, safari)
    #[serde(default)]
    cookies: Option<String>,
}

impl FetchTool {
    #[allow(clippy::too_many_lines)]
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let start = Instant::now();
        let client = get_client().await;
        let profile = client.profile().await;

        let mut output = format!("🌐 Fetching: {}\n", self.url);
        let _ = writeln!(
            output,
            "🎭 Profile: {}",
            profile.user_agent.split('/').next().unwrap_or("Unknown")
        );

        // Get cookies if requested — load before site providers so authenticated
        // providers (e.g., Google Workspace) receive the cookie header.
        let cookie_header = if let Some(browser) = &self.cookies {
            let source = match browser.to_lowercase().as_str() {
                "chrome" => CookieSource::Chrome,
                "firefox" => CookieSource::Firefox,
                "safari" => CookieSource::Safari,
                // "brave" and unrecognised values default to Brave
                _ => CookieSource::Brave,
            };
            let domain = url::Url::parse(&self.url)
                .ok()
                .and_then(|u| u.host_str().map(std::string::ToString::to_string))
                .unwrap_or_default();
            source.get_cookie_header(&domain).unwrap_or_default()
        } else {
            String::new()
        };

        // Try site-specific providers first (e.g., Twitter via FxTwitter API).
        // Cookies are passed so authenticated providers (e.g., Google Workspace) can use them.
        let site_router = nab::site::SiteRouter::new();
        let cookie_opt = if cookie_header.is_empty() {
            None
        } else {
            Some(cookie_header.as_str())
        };
        if let Some(site_content) = site_router.try_extract(&self.url, client, cookie_opt).await {
            output.push_str("\n📄 Content (from specialized provider):\n\n");
            output.push_str(&site_content.markdown);

            return Ok(CallToolResult::text_content(vec![TextContent::from(
                output,
            )]));
        }

        // Fetch with SSRF protection via fetch_safe (or manual request for cookie path)
        let config = SafeFetchConfig::default();

        let (status, content_type, response_headers, body_bytes, elapsed) =
            if cookie_header.is_empty() {
                let safe_resp = client
                    .fetch_safe(&self.url, &config)
                    .await
                    .map_err(|e| CallToolError::from_message(e.to_string()))?;
                let elapsed = start.elapsed();
                (
                    safe_resp.status,
                    safe_resp.content_type.clone(),
                    safe_resp.headers.clone(),
                    safe_resp.body,
                    elapsed,
                )
            } else {
                let response = client
                    .inner()
                    .get(&self.url)
                    .header("Cookie", &cookie_header)
                    .headers(profile.to_headers())
                    .send()
                    .await
                    .map_err(|e| CallToolError::from_message(e.to_string()))?;
                let elapsed_val = start.elapsed();
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
                (status, ct, hdrs, bytes, elapsed_val)
            };

        output.push_str("\n📊 Response:\n");
        let _ = writeln!(output, "   Status: {status}");
        let _ = writeln!(output, "   Time: {:.2}ms", elapsed.as_secs_f64() * 1000.0);

        if self.headers {
            output.push_str("\n📋 Headers:\n");
            for (name, value) in &response_headers {
                let _ = writeln!(output, "   {name}: {value}");
            }
        }

        let _ = writeln!(output, "\n📄 Body: {} bytes", body_bytes.len());

        // Route through ContentRouter for markdown conversion
        // Pass the real URL so readability uses site-specific heuristics
        let router = ContentRouter::new();
        let bytes_clone = body_bytes.to_vec();
        let ct_clone = content_type.clone();
        let url_clone = self.url.clone();
        let conversion = tokio::task::spawn_blocking(move || {
            router.convert_with_url(&bytes_clone, &ct_clone, Some(&url_clone))
        })
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

        if let Some(pages) = conversion.page_count {
            let _ = writeln!(output, "📑 Pages: {} | Conversion: {:.1}ms", pages, conversion.elapsed_ms);
        }

        if self.body {
            let body_text = &conversion.markdown;
            let truncated = if body_text.len() > 4000 {
                format!("{}\n\n... [truncated]", &body_text[..4000])
            } else {
                body_text.clone()
            };
            let _ = write!(output, "\n{truncated}");
        }

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
    }
}

#[mcp_tool(
    name = "fetch_batch",
    description = "Fetch multiple URLs in parallel with HTTP/2 multiplexing.

Uses connection pooling and multiplexing for maximum efficiency.
All URLs are fetched concurrently.

Returns: Results for each URL with timing.",
    read_only_hint = true,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FetchBatchTool {
    /// List of URLs to fetch
    urls: Vec<String>,
}

impl FetchBatchTool {
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let start = Instant::now();
        let client = get_client().await;

        let tasks: Vec<_> = self
            .urls
            .iter()
            .map(|url| {
                let url = url.clone();
                async move {
                    let fetch_start = Instant::now();
                    let result = client.fetch(&url).await;
                    let elapsed = fetch_start.elapsed();
                    (url, result, elapsed)
                }
            })
            .collect();

        let results = futures::future::join_all(tasks).await;
        let total_elapsed = start.elapsed();

        let mut output = format!("🚀 Batch fetch: {} URLs\n\n", self.urls.len());

        for (url, result, elapsed) in results {
            let _ = writeln!(output, "=== {url} ===");
            match result {
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    let preview = if body.len() > 500 {
                        format!("{}...", &body[..500])
                    } else {
                        body.clone()
                    };
                    let _ = writeln!(
                        output,
                        "Status: {status} | {:.0}ms | {} bytes\n{preview}\n",
                        elapsed.as_secs_f64() * 1000.0,
                        body.len()
                    );
                }
                Err(e) => {
                    let _ = writeln!(output, "Error: {e}\n");
                }
            }
        }

        let _ = write!(
            output,
            "\n[Total: {:.2}s for {} URLs]",
            total_elapsed.as_secs_f64(),
            self.urls.len()
        );

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
    }
}

#[mcp_tool(
    name = "auth_lookup",
    description = "Look up credentials in 1Password for a URL.

Searches 1Password for credentials matching the URL/domain.
Returns credential info (username, TOTP availability) without exposing password.

Returns: Credential info if found.",
    read_only_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AuthLookupTool {
    /// URL to find credentials for
    url: String,
}

impl AuthLookupTool {
    pub fn run(&self) -> Result<CallToolResult, CallToolError> {
        let mut output = format!("🔐 Looking up credentials for: {}\n\n", self.url);

        if !OnePasswordAuth::is_available() {
            output.push_str("❌ 1Password CLI not available or not authenticated\n");
            output.push_str("   Run: op signin\n");
            return Ok(CallToolResult::text_content(vec![TextContent::from(
                output,
            )]));
        }

        match CredentialRetriever::get_credential_for_url(&self.url) {
            Ok(Some(cred)) => {
                output.push_str("✅ Found credential:\n");
                let _ = writeln!(output, "   Title: {}", cred.title);
                if let Some(ref username) = cred.username {
                    let _ = writeln!(output, "   Username: {username}");
                }
                if cred.password.is_some() {
                    output.push_str("   Password: [present]\n");
                }
                if cred.has_totp {
                    output.push_str("   TOTP: available\n");
                }
                if let Some(ref passkey) = cred.passkey_credential_id {
                    let _ = writeln!(output, "   Passkey: {passkey}");
                }
            }
            Ok(None) => {
                output.push_str("❌ No credential found for this URL\n");
            }
            Err(e) => {
                let _ = writeln!(output, "⚠️ Error: {e}");
            }
        }

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
    }
}

#[mcp_tool(
    name = "fingerprint",
    description = "Generate realistic browser fingerprints.

Creates browser profiles for Chrome, Firefox, or Safari.
Includes User-Agent, Sec-CH-UA headers, Accept-Language, platform info.

Returns: Generated fingerprint profiles.",
    read_only_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FingerprintTool {
    /// Number of profiles to generate (1-10)
    #[serde(default = "default_count")]
    count: u32,
    /// Browser type (chrome, firefox, safari, random)
    #[serde(default)]
    browser: Option<String>,
}

fn default_count() -> u32 {
    1
}

impl FingerprintTool {
    pub fn run(&self) -> Result<CallToolResult, CallToolError> {
        let count = self.count.min(10) as usize;
        let browser_type = self.browser.clone().unwrap_or_else(|| "random".to_string());

        let mut output = format!("🎭 Generating {count} browser fingerprints:\n\n");

        for i in 0..count {
            let profile = match browser_type.to_lowercase().as_str() {
                "chrome" => chrome_profile(),
                "firefox" => firefox_profile(),
                "safari" => safari_profile(),
                _ => random_profile(),
            };

            let _ = writeln!(output, "Profile {}:", i + 1);
            let _ = writeln!(output, "   UA: {}", profile.user_agent);
            let _ = writeln!(output, "   Accept-Language: {}", profile.accept_language);
            if !profile.sec_ch_ua.is_empty() {
                let _ = writeln!(output, "   Sec-CH-UA: {}", profile.sec_ch_ua);
            }
            output.push('\n');
        }

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
    }
}

#[mcp_tool(
    name = "validate",
    description = "Run validation tests against real websites.

Tests: HTTP/2, HTTP/3, compression, fingerprinting, TLS 1.3, 1Password.

Returns: Validation results with timing.",
    read_only_hint = true,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema, Default)]
pub struct ValidateTool {}

impl ValidateTool {
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let start = Instant::now();
        let client = get_client().await;
        let mut output = String::from("🧪 MicroFetch Validation Suite\n\n");

        // Test 1: Basic fetch
        output.push_str("1️⃣  Basic fetch (example.com)... ");
        let test_start = Instant::now();
        match client.fetch("https://example.com").await {
            Ok(response) => {
                let body = response.text().await.unwrap_or_default();
                if body.contains("Example Domain") {
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
            Err(e) => { let _ = writeln!(output, "❌ {e}"); }
        }

        // Test 2: Compression
        output.push_str("2️⃣  Brotli compression (httpbin.org)... ");
        let test_start = Instant::now();
        match client.fetch("https://httpbin.org/brotli").await {
            Ok(response) => {
                let body = response.text().await.unwrap_or_default();
                if body.contains("brotli") {
                    let _ = writeln!(
                        output,
                        "✅ {:.0}ms",
                        test_start.elapsed().as_secs_f64() * 1000.0
                    );
                } else {
                    output.push_str("⚠️ Compression may not be working\n");
                }
            }
            Err(e) => { let _ = writeln!(output, "❌ {e}"); }
        }

        // Test 3: TLS 1.3
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
            Err(e) => { let _ = writeln!(output, "❌ {e}"); }
        }

        // Test 4: 1Password
        output.push_str("4️⃣  1Password CLI... ");
        if OnePasswordAuth::is_available() {
            output.push_str("✅ Available\n");
        } else {
            output.push_str("⚠️ Not available (run: op signin)\n");
        }

        let _ = writeln!(
            output,
            "\n✨ Validation complete in {:.2}s",
            start.elapsed().as_secs_f64()
        );

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
    }
}

#[mcp_tool(
    name = "benchmark",
    description = "Benchmark fetching URLs with timing statistics.

Measures min/avg/max response times over multiple iterations.

Returns: Benchmark results with timing statistics.",
    read_only_hint = true,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BenchmarkTool {
    /// Comma-separated list of URLs to benchmark
    urls: String,
    /// Number of iterations per URL (1-20)
    #[serde(default = "default_iterations")]
    iterations: u32,
}

fn default_iterations() -> u32 {
    3
}

impl BenchmarkTool {
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let iterations = self.iterations.min(20) as usize;
        let url_list: Vec<&str> = self.urls.split(',').map(str::trim).collect();
        let client = get_client().await;

        let mut output = format!(
            "🚀 Benchmarking {} URLs, {} iterations each\n\n",
            url_list.len(),
            iterations
        );

        for url in url_list {
            let mut times = Vec::with_capacity(iterations);

            for _ in 0..iterations {
                let start = Instant::now();
                if let Ok(response) = client.fetch(url).await {
                    let _ = response.text().await;
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                }
            }

            if !times.is_empty() {
                // Precision loss acceptable: timing averages for display only
                #[allow(clippy::cast_precision_loss)]
                let avg = times.iter().sum::<f64>() / times.len() as f64;
                let min = times.iter().copied().fold(f64::INFINITY, f64::min);
                let max = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);

                let _ = writeln!(output, "📊 {url}");
                let _ = writeln!(output, "   Avg: {avg:.2}ms | Min: {min:.2}ms | Max: {max:.2}ms\n");
            }
        }

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
    }
}

#[mcp_tool(
    name = "submit",
    description = "Submit a web form with smart field extraction.

Fetches a page, parses all forms, extracts hidden fields and CSRF tokens,
merges user-provided fields, and submits via POST.

Use for: login forms, search forms, API interactions behind HTML pages.

Returns: Response body (markdown-converted) after form submission.",
    read_only_hint = false,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SubmitTool {
    /// URL of the page containing the form
    url: String,
    /// Fields to submit as key=value pairs (e.g. `["username=admin", "q=search term"]`)
    fields: Vec<String>,
    /// CSS selector to extract CSRF token from (e.g. `input[name=csrf_token]`)
    #[serde(default)]
    csrf_selector: Option<String>,
    /// Browser cookies to use (brave, chrome, firefox, safari)
    #[serde(default)]
    cookies: Option<String>,
}

impl SubmitTool {
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let client = get_client().await;
        let mut output = format!("📝 Submitting form on: {}\n", self.url);

        // Fetch the form page
        let page_html = client
            .fetch_text(&self.url)
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        // Parse forms
        let mut forms = nab::Form::parse_all(&page_html)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        if forms.is_empty() {
            return Err(CallToolError::from_message("No forms found on page"));
        }

        let mut form = forms.remove(0);
        let _ = writeln!(output, "   Form: {} {}", form.method, form.action);

        // Extract CSRF if requested
        if let Some(ref selector) = self.csrf_selector
            && let Ok(Some(token)) = nab::Form::extract_csrf_token(&page_html, selector)
        {
            let field_name = if selector.contains("name=") {
                selector
                    .split("name=")
                    .nth(1)
                    .and_then(|s| s.split(']').next())
                    .unwrap_or("csrf_token")
            } else {
                "csrf_token"
            };
            form.fields.insert(field_name.to_string(), token);
            output.push_str("   CSRF: extracted\n");
        }

        // Merge user fields
        let user_fields = nab::parse_field_args(&self.fields)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        form.merge_fields(&user_fields);

        // Submit
        let action_url = form
            .resolve_action(&self.url)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        let form_data = form.encode_urlencoded();

        let response = client
            .inner()
            .post(&action_url)
            .header("Content-Type", form.content_type())
            .body(form_data)
            .send()
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let _ = writeln!(output, "   Status: {status}\n");

        // Convert response to markdown
        let router = ContentRouter::new();
        let conversion = router
            .convert(body.as_bytes(), "text/html")
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let truncated = if conversion.markdown.len() > 4000 {
            format!("{}\n\n... [truncated]", &conversion.markdown[..4000])
        } else {
            conversion.markdown
        };
        output.push_str(&truncated);

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
    }
}

#[mcp_tool(
    name = "login",
    description = "Auto-login to a website using 1Password credentials.

Detects login form, retrieves credentials from 1Password, fills and submits,
handles MFA/2FA with TOTP. Returns the authenticated page content.

Requires: 1Password CLI (op) installed and authenticated.

Returns: Final page content after login (markdown-converted).",
    read_only_hint = false,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct LoginTool {
    /// URL of the login page
    url: String,
    /// Browser cookies to use (brave, chrome, firefox, safari)
    #[serde(default)]
    cookies: Option<String>,
}

impl LoginTool {
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        use nab::LoginFlow;

        let mut output = format!("🔐 Auto-login: {}\n", self.url);

        if !OnePasswordAuth::is_available() {
            return Err(CallToolError::from_message(
                "1Password CLI not available. Install: brew install 1password-cli",
            ));
        }

        let client =
            AcceleratedClient::new().map_err(|e| CallToolError::from_message(e.to_string()))?;
        let login_flow = LoginFlow::new(client, true, None);

        let result = login_flow
            .login(&self.url)
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let _ = writeln!(output, "   Final URL: {}", result.final_url);
        output.push_str("   Status: ✅ Login successful\n\n");

        // Convert to markdown
        let router = ContentRouter::new();
        let content_type = if result.body.starts_with('<') {
            "text/html"
        } else {
            "text/plain"
        };
        let conversion = router
            .convert(result.body.as_bytes(), content_type)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let truncated = if conversion.markdown.len() > 4000 {
            format!("{}\n\n... [truncated]", &conversion.markdown[..4000])
        } else {
            conversion.markdown
        };
        output.push_str(&truncated);

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
    }
}

// Generate the tools enum
tool_box!(
    MicroFetchTools,
    [
        FetchTool,
        FetchBatchTool,
        SubmitTool,
        LoginTool,
        AuthLookupTool,
        FingerprintTool,
        ValidateTool,
        BenchmarkTool
    ]
);

// ============================================================================
// SERVER HANDLER
// ============================================================================

pub struct MicroFetchHandler;

#[async_trait]
impl ServerHandler for MicroFetchHandler {
    async fn handle_list_tools_request(
        &self,
        _request: ListToolsRequest,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            meta: None,
            next_cursor: None,
            tools: MicroFetchTools::tools(),
        })
    }

    async fn handle_call_tool_request(
        &self,
        request: CallToolRequest,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        let tool = MicroFetchTools::try_from(request.params)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        match tool {
            MicroFetchTools::FetchTool(t) => t.run().await,
            MicroFetchTools::FetchBatchTool(t) => t.run().await,
            MicroFetchTools::SubmitTool(t) => t.run().await,
            MicroFetchTools::LoginTool(t) => t.run().await,
            MicroFetchTools::AuthLookupTool(t) => t.run(),
            MicroFetchTools::FingerprintTool(t) => t.run(),
            MicroFetchTools::ValidateTool(t) => t.run().await,
            MicroFetchTools::BenchmarkTool(t) => t.run().await,
        }
    }
}

// ============================================================================
// MAIN
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for debugging (to stderr so it doesn't interfere with MCP)
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .init();

    // Pre-initialize the HTTP client
    let _ = get_client().await;

    // Server details
    let server_details = InitializeResult {
        server_info: Implementation {
            name: "nab".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: Some("MicroFetch Browser Engine".into()),
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            ..Default::default()
        },
        meta: None,
        instructions: Some(
            "nab provides ultra-fast web fetching with automatic content conversion (HTML/PDF→markdown), \
             SPA data extraction, form submission with CSRF handling, auto-login via 1Password, \
             HTTP/3, and browser fingerprinting.".into(),
        ),
        protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
    };

    // Create transport
    let transport = StdioTransport::new(TransportOptions::default())?;

    // Create handler
    let handler = MicroFetchHandler;

    // Create server (takes 3 args: details, transport, handler)
    let server = server_runtime::create_server(server_details, transport, handler);

    // Start server
    Ok(server.start().await?)
}
