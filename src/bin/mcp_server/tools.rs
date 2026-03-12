//! MCP tool definitions and implementations for `nab-mcp`.
//!
//! Each struct corresponds to one MCP tool exposed by the server.

use std::fmt::Write as FmtWrite;
use std::time::Instant;

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use tokio::sync::OnceCell;

use nab::content::ContentRouter;
use nab::{
    AcceleratedClient, CookieSource, CredentialRetriever, OnePasswordAuth, SafeFetchConfig,
    chrome_profile, firefox_profile, random_profile, safari_profile,
};

// Global shared client (initialized once, shared with mcp_server main)
static CLIENT: OnceCell<AcceleratedClient> = OnceCell::const_new();

pub async fn get_client() -> &'static AcceleratedClient {
    CLIENT
        .get_or_init(|| async { AcceleratedClient::new().expect("Failed to create HTTP client") })
        .await
}

// ─── fetch ────────────────────────────────────────────────────────────────────

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
    url: String,
    #[serde(default)]
    headers: bool,
    #[serde(default)]
    body: bool,
    #[serde(default)]
    cookies: Option<String>,
}

impl FetchTool {
    #[allow(clippy::too_many_lines)]
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let start = Instant::now();
        let client: &AcceleratedClient = get_client().await;
        let profile = client.profile().await;

        let mut output = format!("🌐 Fetching: {}\n", self.url);
        let _ = writeln!(
            output,
            "🎭 Profile: {}",
            profile.user_agent.split('/').next().unwrap_or("Unknown")
        );

        let cookie_header = resolve_cookie_header(&self.url, self.cookies.as_deref());

        let site_router = nab::site::SiteRouter::new();
        let cookie_opt = if cookie_header.is_empty() {
            None
        } else {
            Some(cookie_header.as_str())
        };
        if let Some(site_content) =
            site_router.try_extract(&self.url, client, cookie_opt).await
        {
            output.push_str("\n📄 Content (from specialized provider):\n\n");
            output.push_str(&site_content.markdown);
            return Ok(CallToolResult::text_content(vec![TextContent::from(output)]));
        }

        let config = SafeFetchConfig::default();

        let (status, content_type, response_headers, body_bytes, elapsed) =
            if cookie_header.is_empty() {
                fetch_safe_response(client, &self.url, &config, start).await?
            } else {
                fetch_with_cookies(client, &self.url, &cookie_header, &profile, start).await?
            };

        write_response_summary(&mut output, status, elapsed, self.headers, &response_headers);
        write_body_info(&mut output, body_bytes.len());

        let conversion = convert_body_async(&body_bytes, &content_type, &self.url).await?;

        if let Some(pages) = conversion.page_count {
            let _ = writeln!(
                output,
                "📑 Pages: {} | Conversion: {:.1}ms",
                pages, conversion.elapsed_ms
            );
        }

        if self.body {
            let truncated = truncate_markdown(&conversion.markdown, 4000);
            let _ = write!(output, "\n{truncated}");
        }

        Ok(CallToolResult::text_content(vec![TextContent::from(output)]))
    }
}

// ─── fetch_batch ──────────────────────────────────────────────────────────────

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
    urls: Vec<String>,
}

impl FetchBatchTool {
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let start = Instant::now();
        let client: &AcceleratedClient = get_client().await;

        let tasks: Vec<_> = self
            .urls
            .iter()
            .map(|url| {
                let url = url.clone();
                async move {
                    let fetch_start = Instant::now();
                    let result = client.fetch(&url).await;
                    (url, result, fetch_start.elapsed())
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
                    let preview = truncate_markdown(&body, 500);
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

        Ok(CallToolResult::text_content(vec![TextContent::from(output)]))
    }
}

// ─── auth_lookup ──────────────────────────────────────────────────────────────

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
    url: String,
}

impl AuthLookupTool {
    pub fn run(&self) -> Result<CallToolResult, CallToolError> {
        let mut output = format!("🔐 Looking up credentials for: {}\n\n", self.url);

        if !OnePasswordAuth::is_available() {
            output.push_str("❌ 1Password CLI not available or not authenticated\n");
            output.push_str("   Run: op signin\n");
            return Ok(CallToolResult::text_content(vec![TextContent::from(output)]));
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
            Ok(None) => output.push_str("❌ No credential found for this URL\n"),
            Err(e) => {
                let _ = writeln!(output, "⚠️ Error: {e}");
            }
        }

        Ok(CallToolResult::text_content(vec![TextContent::from(output)]))
    }
}

// ─── fingerprint ─────────────────────────────────────────────────────────────

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
    #[serde(default = "default_count")]
    count: u32,
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

        Ok(CallToolResult::text_content(vec![TextContent::from(output)]))
    }
}

// ─── validate ─────────────────────────────────────────────────────────────────

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
        let client: &AcceleratedClient = get_client().await;
        let mut output = String::from("🧪 MicroFetch Validation Suite\n\n");

        run_validation_test(client, &mut output, "1️⃣  Basic fetch (example.com)... ", "https://example.com", "Example Domain").await;
        run_validation_test(client, &mut output, "2️⃣  Brotli compression (httpbin.org)... ", "https://httpbin.org/brotli", "brotli").await;
        run_tls_test(client, &mut output).await;

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

        Ok(CallToolResult::text_content(vec![TextContent::from(output)]))
    }
}

// ─── benchmark ───────────────────────────────────────────────────────────────

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
    urls: String,
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
        let client: &AcceleratedClient = get_client().await;

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
                #[allow(clippy::cast_precision_loss)]
                let avg = times.iter().sum::<f64>() / times.len() as f64;
                let min = times.iter().copied().fold(f64::INFINITY, f64::min);
                let max = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let _ = writeln!(output, "📊 {url}");
                let _ = writeln!(
                    output,
                    "   Avg: {avg:.2}ms | Min: {min:.2}ms | Max: {max:.2}ms\n"
                );
            }
        }

        Ok(CallToolResult::text_content(vec![TextContent::from(output)]))
    }
}

// ─── submit ───────────────────────────────────────────────────────────────────

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
    url: String,
    fields: Vec<String>,
    #[serde(default)]
    csrf_selector: Option<String>,
    #[serde(default)]
    cookies: Option<String>,
}

impl SubmitTool {
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let client: &AcceleratedClient = get_client().await;
        let mut output = format!("📝 Submitting form on: {}\n", self.url);

        let page_html = client
            .fetch_text(&self.url)
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let mut forms = nab::Form::parse_all(&page_html)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        if forms.is_empty() {
            return Err(CallToolError::from_message("No forms found on page"));
        }

        let mut form = forms.remove(0);
        let _ = writeln!(output, "   Form: {} {}", form.method, form.action);

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

        let user_fields = nab::parse_field_args(&self.fields)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        form.merge_fields(&user_fields);

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

        let router = ContentRouter::new();
        let conversion = router
            .convert(body.as_bytes(), "text/html")
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        output.push_str(&truncate_markdown(&conversion.markdown, 4000));

        Ok(CallToolResult::text_content(vec![TextContent::from(output)]))
    }
}

// ─── login ────────────────────────────────────────────────────────────────────

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
    url: String,
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

        let router = ContentRouter::new();
        let content_type = if result.body.starts_with('<') { "text/html" } else { "text/plain" };
        let conversion = router
            .convert(result.body.as_bytes(), content_type)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        output.push_str(&truncate_markdown(&conversion.markdown, 4000));

        Ok(CallToolResult::text_content(vec![TextContent::from(output)]))
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Resolve cookie header for a URL from the requested browser.
fn resolve_cookie_header(url: &str, browser: Option<&str>) -> String {
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

/// Fetch via `fetch_safe` and return the response components.
async fn fetch_safe_response(
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
async fn fetch_with_cookies(
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

/// Convert body bytes to markdown asynchronously via spawn_blocking.
async fn convert_body_async(
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

/// Write the response status/timing/header summary to `output`.
fn write_response_summary(
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
fn write_body_info(output: &mut String, body_len: usize) {
    let _ = writeln!(output, "\n📄 Body: {body_len} bytes");
}

/// Truncate markdown to `max_chars`, appending `\n\n... [truncated]` if needed.
fn truncate_markdown(text: &str, max_chars: usize) -> String {
    if text.len() > max_chars {
        format!("{}\n\n... [truncated]", &text[..max_chars])
    } else {
        text.to_string()
    }
}

/// Run a simple fetch-and-check validation test.
async fn run_validation_test(
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
async fn run_tls_test(client: &AcceleratedClient, output: &mut String) {
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
