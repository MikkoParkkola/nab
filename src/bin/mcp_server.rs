//! `MicroFetch` MCP Server - Native Rust implementation
//!
//! Ultra-fast MCP server for web fetching with HTTP/3, fingerprint spoofing,
//! and 1Password integration. Uses latest MCP protocol (2025-06-18).
//!
//! # Usage
//!
//! Stdio mode (for Claude Code integration):
//! ```bash
//! microfetch-mcp
//! ```

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use rust_mcp_sdk::macros::{mcp_tool, JsonSchema};
use rust_mcp_sdk::mcp_server::{server_runtime, ServerHandler};
use rust_mcp_sdk::schema::{
    schema_utils::CallToolError, CallToolRequest, CallToolResult, Implementation, InitializeResult,
    ListToolsRequest, ListToolsResult, RpcError, ServerCapabilities, ServerCapabilitiesTools,
    TextContent, LATEST_PROTOCOL_VERSION,
};
use rust_mcp_sdk::{tool_box, McpServer, StdioTransport, TransportOptions};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use microfetch::{
    chrome_profile, firefox_profile, random_profile, safari_profile, AcceleratedClient,
    CookieSource, CredentialRetriever, OnePasswordAuth,
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
    description = "Fetch a URL with HTTP/3, fingerprint spoofing, and compression.

Features:
- HTTP/2 multiplexing, HTTP/3 (QUIC) with 0-RTT
- TLS 1.3 with session resumption
- Brotli/Zstd/Gzip auto-decompression
- Realistic browser fingerprints (Chrome/Firefox/Safari)
- Happy Eyeballs (IPv4/IPv6 racing), DNS caching

Returns: Response body with timing info.",
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
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let start = Instant::now();
        let client = get_client().await;
        let profile = client.profile().await;

        let mut output = format!("🌐 Fetching: {}\n", self.url);
        output.push_str(&format!(
            "🎭 Profile: {}\n",
            profile.user_agent.split('/').next().unwrap_or("Unknown")
        ));

        // Get cookies if requested
        let cookie_header = if let Some(browser) = &self.cookies {
            let source = match browser.to_lowercase().as_str() {
                "brave" => CookieSource::Brave,
                "chrome" => CookieSource::Chrome,
                "firefox" => CookieSource::Firefox,
                "safari" => CookieSource::Safari,
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

        // Fetch with or without cookies
        let response = if cookie_header.is_empty() {
            client.fetch(&self.url).await
        } else {
            client
                .inner()
                .get(&self.url)
                .header("Cookie", &cookie_header)
                .headers(profile.to_headers())
                .send()
                .await
                .map_err(anyhow::Error::from)
        };

        let response = response.map_err(|e| CallToolError::from_message(e.to_string()))?;

        let elapsed = start.elapsed();
        let status = response.status();
        let version = format!("{:?}", response.version());

        output.push_str("\n📊 Response:\n");
        output.push_str(&format!("   Status: {status}\n"));
        output.push_str(&format!("   Version: {version}\n"));
        output.push_str(&format!(
            "   Time: {:.2}ms\n",
            elapsed.as_secs_f64() * 1000.0
        ));

        if self.headers {
            output.push_str("\n📋 Headers:\n");
            for (name, value) in response.headers() {
                output.push_str(&format!(
                    "   {}: {}\n",
                    name,
                    value.to_str().unwrap_or("<binary>")
                ));
            }
        }

        let body_text = response
            .text()
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        output.push_str(&format!("\n📄 Body: {} bytes\n", body_text.len()));

        if self.body {
            let truncated = if body_text.len() > 4000 {
                format!("{}\n\n... [truncated]", &body_text[..4000])
            } else {
                body_text
            };
            output.push_str(&format!("\n{truncated}"));
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
            output.push_str(&format!("=== {url} ===\n"));
            match result {
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    let preview = if body.len() > 500 {
                        format!("{}...", &body[..500])
                    } else {
                        body.clone()
                    };
                    output.push_str(&format!(
                        "Status: {status} | {:.0}ms | {} bytes\n{preview}\n\n",
                        elapsed.as_secs_f64() * 1000.0,
                        body.len()
                    ));
                }
                Err(e) => {
                    output.push_str(&format!("Error: {e}\n\n"));
                }
            }
        }

        output.push_str(&format!(
            "\n[Total: {:.2}s for {} URLs]",
            total_elapsed.as_secs_f64(),
            self.urls.len()
        ));

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
                output.push_str(&format!("   Title: {}\n", cred.title));
                if let Some(ref username) = cred.username {
                    output.push_str(&format!("   Username: {username}\n"));
                }
                if cred.password.is_some() {
                    output.push_str("   Password: [present]\n");
                }
                if cred.has_totp {
                    output.push_str("   TOTP: available\n");
                }
                if let Some(ref passkey) = cred.passkey_credential_id {
                    output.push_str(&format!("   Passkey: {passkey}\n"));
                }
            }
            Ok(None) => {
                output.push_str("❌ No credential found for this URL\n");
            }
            Err(e) => {
                output.push_str(&format!("⚠️ Error: {e}\n"));
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

            output.push_str(&format!("Profile {}:\n", i + 1));
            output.push_str(&format!("   UA: {}\n", profile.user_agent));
            output.push_str(&format!(
                "   Accept-Language: {}\n",
                profile.accept_language
            ));
            if !profile.sec_ch_ua.is_empty() {
                output.push_str(&format!("   Sec-CH-UA: {}\n", profile.sec_ch_ua));
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
                    output.push_str(&format!(
                        "✅ {:.0}ms, {} bytes\n",
                        test_start.elapsed().as_secs_f64() * 1000.0,
                        body.len()
                    ));
                } else {
                    output.push_str("⚠️ Unexpected content\n");
                }
            }
            Err(e) => output.push_str(&format!("❌ {e}\n")),
        }

        // Test 2: Compression
        output.push_str("2️⃣  Brotli compression (httpbin.org)... ");
        let test_start = Instant::now();
        match client.fetch("https://httpbin.org/brotli").await {
            Ok(response) => {
                let body = response.text().await.unwrap_or_default();
                if body.contains("brotli") {
                    output.push_str(&format!(
                        "✅ {:.0}ms\n",
                        test_start.elapsed().as_secs_f64() * 1000.0
                    ));
                } else {
                    output.push_str("⚠️ Compression may not be working\n");
                }
            }
            Err(e) => output.push_str(&format!("❌ {e}\n")),
        }

        // Test 3: TLS 1.3
        output.push_str("3️⃣  TLS 1.3 (cloudflare.com)... ");
        let test_start = Instant::now();
        match client.fetch("https://www.cloudflare.com").await {
            Ok(response) => {
                if response.status().is_success() {
                    output.push_str(&format!(
                        "✅ {:.0}ms\n",
                        test_start.elapsed().as_secs_f64() * 1000.0
                    ));
                } else {
                    output.push_str(&format!("⚠️ Status: {}\n", response.status()));
                }
            }
            Err(e) => output.push_str(&format!("❌ {e}\n")),
        }

        // Test 4: 1Password
        output.push_str("4️⃣  1Password CLI... ");
        if OnePasswordAuth::is_available() {
            output.push_str("✅ Available\n");
        } else {
            output.push_str("⚠️ Not available (run: op signin)\n");
        }

        output.push_str(&format!(
            "\n✨ Validation complete in {:.2}s\n",
            start.elapsed().as_secs_f64()
        ));

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
                let avg = times.iter().sum::<f64>() / times.len() as f64;
                let min = times.iter().copied().fold(f64::INFINITY, f64::min);
                let max = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);

                output.push_str(&format!("📊 {url}\n"));
                output.push_str(&format!(
                    "   Avg: {avg:.2}ms | Min: {min:.2}ms | Max: {max:.2}ms\n\n"
                ));
            }
        }

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
            name: "microfetch".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: Some("MicroFetch Browser Engine".into()),
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            ..Default::default()
        },
        meta: None,
        instructions: Some(
            "MicroFetch provides ultra-fast web fetching with HTTP/3, browser fingerprinting, and 1Password authentication support.".into(),
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
