//! MCP tool definitions and implementations for `nab-mcp`.
//!
//! Each struct corresponds to one MCP tool exposed by the server.

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::sync::Arc;
use std::time::Instant;

use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{
    CallToolResult, ElicitFormSchema, ElicitRequestFormParams, ElicitRequestParams,
    ElicitRequestUrlParams, ElicitResultAction, ElicitResultContent,
    ElicitResultContentPrimitive, LegacyTitledEnumSchema, PrimitiveSchemaDefinition,
    StringSchema, TextContent, TitledMultiSelectEnumSchema, TitledMultiSelectEnumSchemaItems,
    TitledMultiSelectEnumSchemaItemsAnyOfItem, schema_utils::CallToolError,
};
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
        if let Some(site_content) = site_router.try_extract(&self.url, client, cookie_opt).await {
            output.push_str("\n📄 Content (from specialized provider):\n\n");
            output.push_str(&site_content.markdown);
            return Ok(CallToolResult::text_content(vec![TextContent::from(
                output,
            )]));
        }

        let config = SafeFetchConfig::default();

        let (status, content_type, response_headers, body_bytes, elapsed) =
            if cookie_header.is_empty() {
                fetch_safe_response(client, &self.url, &config, start).await?
            } else {
                fetch_with_cookies(client, &self.url, &cookie_header, &profile, start).await?
            };

        write_response_summary(
            &mut output,
            status,
            elapsed,
            self.headers,
            &response_headers,
        );
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

        let structured = build_fetch_structured(
            &self.url,
            status.as_u16(),
            &content_type,
            &conversion.markdown,
            elapsed.as_secs_f64() * 1000.0,
        );

        let mut result = CallToolResult::text_content(vec![TextContent::from(output)]);
        result.structured_content = Some(structured);
        Ok(result)
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
        let mut structured_items: Vec<serde_json::Value> = Vec::new();

        for (url, result, elapsed) in results {
            let _ = writeln!(output, "=== {url} ===");
            let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
            match result {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let body = response.text().await.unwrap_or_default();
                    let preview = truncate_markdown(&body, 500);
                    let _ = writeln!(
                        output,
                        "Status: {status} | {elapsed_ms:.0}ms | {} bytes\n{preview}\n",
                        body.len()
                    );
                    structured_items.push(serde_json::json!({
                        "url": url,
                        "status": status,
                        "content": preview,
                        "timing_ms": elapsed_ms,
                    }));
                }
                Err(e) => {
                    let msg = e.to_string();
                    let _ = writeln!(output, "Error: {msg}\n");
                    structured_items.push(serde_json::json!({
                        "url": url,
                        "status": null,
                        "content": msg,
                        "timing_ms": elapsed_ms,
                    }));
                }
            }
        }

        let _ = write!(
            output,
            "\n[Total: {:.2}s for {} URLs]",
            total_elapsed.as_secs_f64(),
            self.urls.len()
        );

        let structured = build_structured([("results", serde_json::Value::Array(structured_items))]);
        let mut result = CallToolResult::text_content(vec![TextContent::from(output)]);
        result.structured_content = Some(structured);
        Ok(result)
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
            let structured = build_structured([
                ("domain", serde_json::Value::String(self.url.clone())),
                ("username", serde_json::Value::Null),
                ("has_totp", serde_json::Value::Bool(false)),
            ]);
            let mut result = CallToolResult::text_content(vec![TextContent::from(output)]);
            result.structured_content = Some(structured);
            return Ok(result);
        }

        let (username, has_totp) = match CredentialRetriever::get_credential_for_url(&self.url) {
            Ok(Some(cred)) => {
                output.push_str("✅ Found credential:\n");
                let _ = writeln!(output, "   Title: {}", cred.title);
                if let Some(ref u) = cred.username {
                    let _ = writeln!(output, "   Username: {u}");
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
                (cred.username, cred.has_totp)
            }
            Ok(None) => {
                output.push_str("❌ No credential found for this URL\n");
                (None, false)
            }
            Err(e) => {
                let _ = writeln!(output, "⚠️ Error: {e}");
                (None, false)
            }
        };

        let structured = build_structured([
            ("domain", serde_json::Value::String(self.url.clone())),
            (
                "username",
                username.map_or(serde_json::Value::Null, serde_json::Value::String),
            ),
            ("has_totp", serde_json::Value::Bool(has_totp)),
        ]);
        let mut result = CallToolResult::text_content(vec![TextContent::from(output)]);
        result.structured_content = Some(structured);
        Ok(result)
    }
}

// ─── fingerprint ─────────────────────────────────────────────────────────────

#[mcp_tool(
    name = "fingerprint",
    description = "Generate realistic browser fingerprints.

Creates browser profiles for Chrome, Firefox, or Safari.
Includes User-Agent, Sec-CH-UA headers, Accept-Language, platform info.

Returns: Generated fingerprint profiles.",
    read_only_hint = true,
    destructive_hint = false,
    open_world_hint = false
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
        let mut profiles: Vec<serde_json::Value> = Vec::with_capacity(count);

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
            profiles.push(serde_json::json!({
                "user_agent": profile.user_agent,
                "accept_language": profile.accept_language,
                "sec_ch_ua": profile.sec_ch_ua,
            }));
        }

        let structured = build_structured([("profiles", serde_json::Value::Array(profiles))]);
        let mut result = CallToolResult::text_content(vec![TextContent::from(output)]);
        result.structured_content = Some(structured);
        Ok(result)
    }
}

// ─── validate ─────────────────────────────────────────────────────────────────

#[mcp_tool(
    name = "validate",
    description = "Run validation tests against real websites.

Tests: HTTP/2, HTTP/3, compression, fingerprinting, TLS 1.3, 1Password.

Returns: Validation results with timing.",
    read_only_hint = true,
    destructive_hint = false,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema, Default)]
pub struct ValidateTool {}

impl ValidateTool {
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let start = Instant::now();
        let client: &AcceleratedClient = get_client().await;
        let mut output = String::from("🧪 MicroFetch Validation Suite\n\n");

        run_validation_test(
            client,
            &mut output,
            "1️⃣  Basic fetch (example.com)... ",
            "https://example.com",
            "Example Domain",
        )
        .await;
        run_validation_test(
            client,
            &mut output,
            "2️⃣  Brotli compression (httpbin.org)... ",
            "https://httpbin.org/brotli",
            "brotli",
        )
        .await;
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

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
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
        let mut structured_items: Vec<serde_json::Value> = Vec::new();

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
                structured_items.push(serde_json::json!({
                    "url": url,
                    "min_ms": min,
                    "avg_ms": avg,
                    "max_ms": max,
                    "iterations": times.len(),
                }));
            }
        }

        let structured = build_structured([("results", serde_json::Value::Array(structured_items))]);
        let mut result = CallToolResult::text_content(vec![TextContent::from(output)]);
        result.structured_content = Some(structured);
        Ok(result)
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

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
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
    pub async fn run(&self, runtime: Arc<dyn McpServer>) -> Result<CallToolResult, CallToolError> {
        use nab::LoginFlow;

        let mut output = format!("🔐 Auto-login: {}\n", self.url);

        // P0: Detect OAuth/SSO redirect URLs and use URL elicitation so the
        // user can complete the flow in their browser rather than via a form.
        if is_oauth_redirect(&self.url) {
            let service = oauth_service_name(&self.url);
            output.push_str("   Detected OAuth/SSO flow — directing to browser\n");
            let action = elicit_oauth_url(&runtime, &self.url, &service).await?;
            match action {
                ElicitResultAction::Accept => {
                    output.push_str("   ✅ OAuth flow completed by user\n");
                    output.push_str(
                        "   Note: Use the `fetch` tool with `cookies: \"brave\"` (or your browser) \
                         to access the authenticated session.\n",
                    );
                }
                ElicitResultAction::Decline | ElicitResultAction::Cancel => {
                    output.push_str("   ⚠️ OAuth flow cancelled by user\n");
                }
            }
            return Ok(CallToolResult::text_content(vec![TextContent::from(output)]));
        }

        if !OnePasswordAuth::is_available() {
            // Elicit manual credentials when 1Password is unavailable.
            let (username, password) = elicit_credentials(&runtime, &self.url).await?;
            return run_login_with_credentials(&self.url, &username, &password, output).await;
        }

        // Collect all matching credentials to detect ambiguity.
        let op_auth = OnePasswordAuth::new(None);
        let all_creds = op_auth
            .get_all_credentials_for_url(&self.url)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let credential = match all_creds.len() {
            0 => {
                // No stored credentials — elicit from user.
                let (username, password) = elicit_credentials(&runtime, &self.url).await?;
                return run_login_with_credentials(&self.url, &username, &password, output).await;
            }
            1 => {
                let cred = &all_creds[0];
                let _ = writeln!(output, "   Credential: {}", cred.title);
                cred.clone()
            }
            _ => {
                // Multiple matches — let the user choose via elicitation.
                let chosen_title =
                    elicit_credential_choice(&runtime, &self.url, &all_creds).await?;
                let cred = all_creds
                    .into_iter()
                    .find(|c| c.title == chosen_title)
                    .ok_or_else(|| CallToolError::from_message("Selected credential not found"))?;
                let _ = writeln!(output, "   Credential: {}", cred.title);
                cred
            }
        };

        // Verify we have a usable password before attempting the login flow.
        if credential.password.is_none() {
            return Err(CallToolError::from_message(format!(
                "No password found in credential '{}' for {}",
                credential.title, self.url
            )));
        }

        // P2: When no explicit cookie source was supplied, offer multi-select
        // so the user can choose one or more browser cookie stores to inject
        // into the login request.  An empty selection means no cookies.
        let resolved_cookies = resolve_login_cookies(&self.url, self.cookies.as_deref(), &runtime).await?;

        let client =
            AcceleratedClient::new().map_err(|e| CallToolError::from_message(e.to_string()))?;
        let login_flow = LoginFlow::new(client, true, resolved_cookies);

        let result = login_flow
            .login(&self.url)
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let _ = writeln!(output, "   Final URL: {}", result.final_url);
        output.push_str("   Status: ✅ Login successful\n\n");

        let router = ContentRouter::new();
        let content_type = if result.body.starts_with('<') {
            "text/html"
        } else {
            "text/plain"
        };
        let conversion = router
            .convert(result.body.as_bytes(), content_type)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        output.push_str(&truncate_markdown(&conversion.markdown, 4000));

        Ok(CallToolResult::text_content(vec![TextContent::from(
            output,
        )]))
    }
}

// ─── Elicitation helpers ──────────────────────────────────────────────────────

/// Ask the user to provide a username and password when no stored credential exists.
async fn elicit_credentials(
    runtime: &Arc<dyn McpServer>,
    url: &str,
) -> Result<(String, String), CallToolError> {
    let mut properties = HashMap::new();
    properties.insert(
        "username".into(),
        PrimitiveSchemaDefinition::StringSchema(StringSchema::new(
            None,
            Some("Your username or email address".into()),
            None,
            None,
            None,
            Some("Username".into()),
        )),
    );
    properties.insert(
        "password".into(),
        PrimitiveSchemaDefinition::StringSchema(StringSchema::new(
            None,
            Some("Your password".into()),
            None,
            None,
            None,
            Some("Password".into()),
        )),
    );

    let schema = ElicitFormSchema::new(properties, vec!["username".into(), "password".into()], None);

    let result = runtime
        .request_elicitation(ElicitRequestParams::FormParams(ElicitRequestFormParams::new(
            format!("No stored credentials found for {url}. Please enter your login details:"),
            schema,
            None,
            None,
        )))
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

    match result.action {
        ElicitResultAction::Accept => {
            let content = result.content.unwrap_or_default();
            let username = extract_string_field(&content, "username")?;
            let password = extract_string_field(&content, "password")?;
            Ok((username, password))
        }
        ElicitResultAction::Decline | ElicitResultAction::Cancel => {
            Err(CallToolError::from_message("Login cancelled by user"))
        }
    }
}

/// Ask the user to choose one credential when multiple match the domain.
async fn elicit_credential_choice(
    runtime: &Arc<dyn McpServer>,
    url: &str,
    credentials: &[nab::auth::Credential],
) -> Result<String, CallToolError> {
    let titles: Vec<String> = credentials.iter().map(|c| c.title.clone()).collect();
    let title_labels: Vec<String> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let username = credentials[i]
                .username
                .as_deref()
                .map(|u| format!(" ({u})"))
                .unwrap_or_default();
            format!("{t}{username}")
        })
        .collect();

    let mut properties = HashMap::new();
    properties.insert(
        "credential".into(),
        PrimitiveSchemaDefinition::LegacyTitledEnumSchema(LegacyTitledEnumSchema::new(
            titles.clone(),
            title_labels,
            None,
            Some("Select the credential to use for login".into()),
            Some("Credential".into()),
        )),
    );

    let schema = ElicitFormSchema::new(properties, vec!["credential".into()], None);

    let result = runtime
        .request_elicitation(ElicitRequestParams::FormParams(ElicitRequestFormParams::new(
            format!("Multiple credentials found for {url}. Which one would you like to use?"),
            schema,
            None,
            None,
        )))
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

    match result.action {
        ElicitResultAction::Accept => {
            let content = result.content.unwrap_or_default();
            extract_string_field(&content, "credential")
        }
        ElicitResultAction::Decline | ElicitResultAction::Cancel => {
            Err(CallToolError::from_message("Login cancelled by user"))
        }
    }
}

/// Perform a credential-based login using a manually-provided username + password.
///
/// This path is used when no 1Password entry exists and the user supplies
/// credentials via elicitation.  The `LoginFlow` cannot be used here because
/// it pulls credentials from 1Password internally, so we fall back to the
/// form-submission path (`SubmitTool`-style).
async fn run_login_with_credentials(
    url: &str,
    username: &str,
    password: &str,
    mut output: String,
) -> Result<CallToolResult, CallToolError> {
    let client = get_client().await;

    let page_html = client
        .fetch_text(url)
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

    let mut forms =
        nab::Form::parse_all(&page_html).map_err(|e| CallToolError::from_message(e.to_string()))?;

    if forms.is_empty() {
        return Err(CallToolError::from_message("No login form found on page"));
    }

    let mut form = forms.remove(0);
    let _ = writeln!(output, "   Form: {} {}", form.method, form.action);

    // Best-effort field injection: typical login forms use username/email + password.
    form.fields
        .entry("username".into())
        .or_insert_with(|| username.to_string());
    form.fields
        .entry("email".into())
        .or_insert_with(|| username.to_string());
    form.fields.insert("password".into(), password.to_string());

    let action_url = form
        .resolve_action(url)
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
    Ok(CallToolResult::text_content(vec![TextContent::from(
        output,
    )]))
}

/// Extract a string value from the elicitation response content map.
fn extract_string_field(
    content: &HashMap<String, ElicitResultContent>,
    field: &str,
) -> Result<String, CallToolError> {
    match content.get(field) {
        Some(ElicitResultContent::Primitive(ElicitResultContentPrimitive::String(s))) => {
            Ok(s.clone())
        }
        Some(_) => Err(CallToolError::from_message(format!(
            "Unexpected type for elicitation field '{field}'"
        ))),
        None => Err(CallToolError::from_message(format!(
            "Missing required elicitation field '{field}'"
        ))),
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

/// Convert body bytes to markdown asynchronously via `spawn_blocking`.
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

// ─── Server icon ─────────────────────────────────────────────────────────────

/// Inline SVG globe icon for light backgrounds (~200 bytes).
///
/// Embedded as a `data:` URI — no external URL required.
/// The SVG renders a simple wireframe globe (circle + meridian ellipse + equator).
pub(crate) const GLOBE_SVG_LIGHT: &str = concat!(
    "data:image/svg+xml;base64,",
    "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAzMiAzMiI+",
    "PGNpcmNsZSBjeD0iMTYiIGN5PSIxNiIgcj0iMTQiIGZpbGw9Im5vbmUiIHN0cm9rZT0iIzMzMyIgc3",
    "Ryb2tlLXdpZHRoPSIxLjUiLz48ZWxsaXBzZSBjeD0iMTYiIGN5PSIxNiIgcng9IjYiIHJ5PSIxNCIg",
    "ZmlsbD0ibm9uZSIgc3Ryb2tlPSIjMzMzIiBzdHJva2Utd2lkdGg9IjEuNSIvPjxsaW5lIHgxPSIyIiB",
    "5MT0iMTYiIHgyPSIzMCIgeTI9IjE2IiBzdHJva2U9IiMzMzMiIHN0cm9rZS13aWR0aD0iMS41Ii8+PC",
    "9zdmc+"
);

/// Inline SVG globe icon for dark backgrounds (~200 bytes).
pub(crate) const GLOBE_SVG_DARK: &str = concat!(
    "data:image/svg+xml;base64,",
    "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAzMiAzMiI+",
    "PGNpcmNsZSBjeD0iMTYiIGN5PSIxNiIgcj0iMTQiIGZpbGw9Im5vbmUiIHN0cm9rZT0iI2VlZSIgc3",
    "Ryb2tlLXdpZHRoPSIxLjUiLz48ZWxsaXBzZSBjeD0iMTYiIGN5PSIxNiIgcng9IjYiIHJ5PSIxNCIg",
    "ZmlsbD0ibm9uZSIgc3Ryb2tlPSIjZWVlIiBzdHJva2Utd2lkdGg9IjEuNSIvPjxsaW5lIHgxPSIyIiB",
    "5MT0iMTYiIHgyPSIzMCIgeTI9IjE2IiBzdHJva2U9IiNlZWUiIHN0cm9rZS13aWR0aD0iMS41Ii8+PC",
    "9zdmc+"
);

/// Build the server icon list: one light-theme and one dark-theme globe SVG.
///
/// Both icons use scalable SVG with `sizes: ["any"]` so clients can render them
/// at any resolution.  The data URIs embed the image inline — no external
/// requests are needed.
pub(crate) fn server_icons() -> Vec<rust_mcp_sdk::schema::Icon> {
    use rust_mcp_sdk::schema::{Icon, IconTheme};
    vec![
        Icon {
            src: GLOBE_SVG_LIGHT.to_string(),
            mime_type: Some("image/svg+xml".to_string()),
            sizes: vec!["any".to_string()],
            theme: Some(IconTheme::Light),
        },
        Icon {
            src: GLOBE_SVG_DARK.to_string(),
            mime_type: Some("image/svg+xml".to_string()),
            sizes: vec!["any".to_string()],
            theme: Some(IconTheme::Dark),
        },
    ]
}

// ─── structured_content helpers ───────────────────────────────────────────────

/// Build a `structuredContent` map from a fixed-size array of `(key, value)` pairs.
///
/// This is a zero-allocation helper for the common case of building a flat JSON
/// object with a known set of fields at compile time.
fn build_structured<const N: usize>(
    fields: [(&'static str, serde_json::Value); N],
) -> serde_json::Map<String, serde_json::Value> {
    fields
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// Build the `structuredContent` map for the `fetch` tool response.
fn build_fetch_structured(
    url: &str,
    status: u16,
    content_type: &str,
    markdown: &str,
    timing_ms: f64,
) -> serde_json::Map<String, serde_json::Value> {
    build_structured([
        ("url", serde_json::Value::String(url.to_string())),
        ("status", serde_json::Value::Number(status.into())),
        (
            "content_type",
            serde_json::Value::String(content_type.to_string()),
        ),
        (
            "content",
            serde_json::Value::String(truncate_markdown(markdown, 4000)),
        ),
        (
            "timing_ms",
            serde_json::Value::Number(
                serde_json::Number::from_f64(timing_ms).unwrap_or(serde_json::Number::from(0)),
            ),
        ),
    ])
}

// ─── OAuth URL elicitation ────────────────────────────────────────────────────

/// Known OAuth/SSO redirect hostname patterns.
const OAUTH_HOSTS: &[&str] = &[
    "accounts.google.com",
    "github.com/login/oauth",
    "login.microsoftonline.com",
    "appleid.apple.com",
    "facebook.com/login",
    "twitter.com/i/oauth",
    "x.com/i/oauth",
    "linkedin.com/oauth",
    "auth0.com",
    "okta.com",
    "pingidentity.com",
    "onelogin.com",
    "login.live.com",
];

/// Returns `true` when `url` looks like an OAuth/SSO provider redirect.
pub(crate) fn is_oauth_redirect(url: &str) -> bool {
    let lower = url.to_lowercase();
    OAUTH_HOSTS.iter().any(|&host| lower.contains(host))
}

/// Extract a human-readable service name from an OAuth URL for display in
/// the elicitation message.
fn oauth_service_name(url: &str) -> String {
    let lower = url.to_lowercase();
    if lower.contains("google") {
        "Google"
    } else if lower.contains("github") {
        "GitHub"
    } else if lower.contains("microsoft") || lower.contains("live.com") {
        "Microsoft"
    } else if lower.contains("apple") {
        "Apple"
    } else if lower.contains("facebook") {
        "Facebook"
    } else if lower.contains("twitter") || lower.contains("x.com") {
        "X (Twitter)"
    } else if lower.contains("linkedin") {
        "LinkedIn"
    } else if lower.contains("auth0") {
        "Auth0"
    } else if lower.contains("okta") {
        "Okta"
    } else {
        "the OAuth provider"
    }
    .to_string()
}

/// Send a URL elicitation to guide the user through an OAuth/SSO flow.
///
/// The 2025-11-25 protocol's `ElicitRequestUrlParams` lets the client open a
/// URL directly in the user's browser rather than showing a form.  After the
/// user completes the OAuth flow the server can capture the resulting cookies
/// via a follow-up form elicitation.
///
/// Returns the elicitation result action so the caller can branch on
/// accept/cancel.
pub(crate) async fn elicit_oauth_url(
    runtime: &Arc<dyn McpServer>,
    oauth_url: &str,
    service_name: &str,
) -> Result<ElicitResultAction, CallToolError> {
    // elicitation_id must be unique per request; use a short random suffix.
    let elicitation_id = format!("oauth-{}-{}", service_name, std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis())
        .unwrap_or(0));

    let result = runtime
        .request_elicitation(ElicitRequestParams::UrlParams(
            ElicitRequestUrlParams::new(
                elicitation_id,
                format!(
                    "OAuth login required for {service_name}. \
                     Please complete the sign-in in your browser. \
                     The page will reload once authentication is complete."
                ),
                oauth_url.to_string(),
                None,
                None,
            ),
        ))
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

    Ok(result.action)
}

// ─── Cookie resolution ───────────────────────────────────────────────────────

/// Resolve the cookie header to use for login.
///
/// When `explicit_cookies` is provided (the tool's `cookies` parameter), use it
/// directly.  Otherwise, offer multi-select elicitation so the user can choose
/// one or more browser cookie stores; cookies from all selected stores are
/// concatenated with `"; "` as separator.
///
/// # Errors
///
/// Returns `CallToolError` if the elicitation RPC fails.
async fn resolve_login_cookies(
    url: &str,
    explicit_cookies: Option<&str>,
    runtime: &Arc<dyn McpServer>,
) -> Result<Option<String>, CallToolError> {
    if let Some(cookie) = explicit_cookies {
        return Ok(Some(cookie.to_string()));
    }

    let sources = elicit_cookie_sources(runtime, url).await?;
    if sources.is_empty() {
        return Ok(None);
    }

    let domain = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_default();

    let combined = sources
        .iter()
        .filter_map(|s| {
            let source = match s.as_str() {
                "chrome" => CookieSource::Chrome,
                "firefox" => CookieSource::Firefox,
                "safari" => CookieSource::Safari,
                _ => CookieSource::Brave,
            };
            source.get_cookie_header(&domain).ok()
        })
        .collect::<Vec<_>>()
        .join("; ");

    Ok(if combined.is_empty() { None } else { Some(combined) })
}

// ─── Multi-select cookie source elicitation ───────────────────────────────────

/// Ask the user which browser cookie stores to use for the login, allowing
/// multiple sources to be selected simultaneously.
///
/// Uses `TitledMultiSelectEnumSchema` from the 2025-11-25 protocol spec.
/// Returns the selected browser names (e.g. `["brave", "chrome"]`).
pub(crate) async fn elicit_cookie_sources(
    runtime: &Arc<dyn McpServer>,
    url: &str,
) -> Result<Vec<String>, CallToolError> {
    let options: &[(&str, &str)] = &[
        ("brave", "Brave Browser"),
        ("chrome", "Google Chrome"),
        ("firefox", "Mozilla Firefox"),
        ("safari", "Apple Safari"),
    ];

    let items = TitledMultiSelectEnumSchemaItems {
        any_of: options
            .iter()
            .map(|&(value, label)| TitledMultiSelectEnumSchemaItemsAnyOfItem {
                const_: value.to_string(),
                title: label.to_string(),
            })
            .collect(),
    };

    let multi_select = TitledMultiSelectEnumSchema::new(
        vec!["brave".to_string()], // default: Brave
        items,
        Some("Cookie stores to import for authentication".into()),
        None, // max_items
        None, // min_items
        Some("Cookie Sources".into()),
    );

    let mut properties = HashMap::new();
    properties.insert(
        "sources".into(),
        PrimitiveSchemaDefinition::TitledMultiSelectEnumSchema(multi_select),
    );

    let schema = ElicitFormSchema::new(properties, vec!["sources".into()], None);

    let result = runtime
        .request_elicitation(ElicitRequestParams::FormParams(ElicitRequestFormParams::new(
            format!(
                "Which browser cookie stores should be used for login to {url}? \
                 Select all that apply."
            ),
            schema,
            None,
            None,
        )))
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

    match result.action {
        ElicitResultAction::Accept => {
            // The multi-select result comes back as a JSON array of selected values.
            // The SDK encodes it as ElicitResultContent::Primitive::String (JSON-serialised
            // array) or as individual entries — extract and decode defensively.
            let content = result.content.unwrap_or_default();
            let sources = extract_multiselect_field(&content, "sources");
            Ok(sources)
        }
        ElicitResultAction::Decline | ElicitResultAction::Cancel => {
            // User skipped — return empty list so caller can fall back to no cookies.
            Ok(vec![])
        }
    }
}

/// Extract a multi-select field (array of strings) from elicitation result content.
///
/// Clients MAY encode the array as a JSON string `"[\"a\",\"b\"]"` or as a
/// comma-separated string `"a,b"`.  Both forms are handled here.  Returns an
/// empty `Vec` when the field is absent or has an unexpected type.
fn extract_multiselect_field(
    content: &HashMap<String, ElicitResultContent>,
    field: &str,
) -> Vec<String> {
    match content.get(field) {
        Some(ElicitResultContent::Primitive(ElicitResultContentPrimitive::String(s))) => {
            // Try JSON array first.
            if let Ok(vals) = serde_json::from_str::<Vec<String>>(s) {
                return vals;
            }
            // Fall back to comma-separated.
            s.split(',')
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect()
        }
        Some(_) | None => vec![],
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_oauth_redirect ────────────────────────────────────────────────────

    #[test]
    fn oauth_redirect_detects_google() {
        // GIVEN a Google OAuth URL
        let url = "https://accounts.google.com/o/oauth2/auth?client_id=xxx";
        // WHEN checked for OAuth redirect
        // THEN it is detected
        assert!(is_oauth_redirect(url));
    }

    #[test]
    fn oauth_redirect_detects_github() {
        assert!(is_oauth_redirect("https://github.com/login/oauth/authorize?client_id=abc"));
    }

    #[test]
    fn oauth_redirect_detects_microsoft() {
        assert!(is_oauth_redirect(
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
        ));
    }

    #[test]
    fn oauth_redirect_rejects_normal_site() {
        // GIVEN a regular website URL
        let url = "https://example.com/login";
        // WHEN checked for OAuth redirect
        // THEN it is NOT detected
        assert!(!is_oauth_redirect(url));
    }

    #[test]
    fn oauth_redirect_case_insensitive() {
        assert!(is_oauth_redirect("https://ACCOUNTS.GOOGLE.COM/o/oauth2/auth"));
    }

    // ── extract_multiselect_field ────────────────────────────────────────────

    #[test]
    fn multiselect_parses_json_array() {
        // GIVEN a JSON-encoded array string in the content map
        let mut content = HashMap::new();
        content.insert(
            "sources".to_string(),
            ElicitResultContent::Primitive(ElicitResultContentPrimitive::String(
                r#"["brave","chrome"]"#.to_string(),
            )),
        );
        // WHEN extracted
        let result = extract_multiselect_field(&content, "sources");
        // THEN the values are returned as a Vec
        assert_eq!(result, vec!["brave", "chrome"]);
    }

    #[test]
    fn multiselect_parses_comma_separated() {
        // GIVEN a comma-separated string (fallback encoding)
        let mut content = HashMap::new();
        content.insert(
            "sources".to_string(),
            ElicitResultContent::Primitive(ElicitResultContentPrimitive::String(
                "brave, firefox".to_string(),
            )),
        );
        // WHEN extracted
        let result = extract_multiselect_field(&content, "sources");
        // THEN whitespace is trimmed and values are split
        assert_eq!(result, vec!["brave", "firefox"]);
    }

    #[test]
    fn multiselect_returns_empty_on_missing_field() {
        // GIVEN content without the requested field
        let content: HashMap<String, ElicitResultContent> = HashMap::new();
        // WHEN extracted
        let result = extract_multiselect_field(&content, "sources");
        // THEN empty vec is returned
        assert!(result.is_empty());
    }

    // ── build_structured ────────────────────────────────────────────────────

    #[test]
    fn build_structured_produces_correct_keys() {
        // GIVEN a set of key-value pairs
        // WHEN built into a structured map
        let map = build_structured([
            ("url", serde_json::Value::String("https://example.com".into())),
            ("status", serde_json::Value::Number(200.into())),
        ]);
        // THEN all keys are present with correct values
        assert_eq!(map["url"], serde_json::Value::String("https://example.com".into()));
        assert_eq!(map["status"], serde_json::Value::Number(200.into()));
    }

    // ── build_fetch_structured ───────────────────────────────────────────────

    #[test]
    fn fetch_structured_has_all_required_fields() {
        // GIVEN a complete fetch result
        let map = build_fetch_structured(
            "https://example.com",
            200,
            "text/html",
            "# Hello\n\nworld",
            42.5,
        );
        // WHEN inspected
        // THEN all outputSchema fields are present
        assert!(map.contains_key("url"));
        assert!(map.contains_key("status"));
        assert!(map.contains_key("content_type"));
        assert!(map.contains_key("content"));
        assert!(map.contains_key("timing_ms"));
        assert_eq!(map["status"], serde_json::Value::Number(200.into()));
    }

    #[test]
    fn fetch_structured_truncates_long_content() {
        // GIVEN content longer than 4000 chars
        let long_content = "x".repeat(5000);
        let map = build_fetch_structured("https://example.com", 200, "text/plain", &long_content, 10.0);
        // WHEN inspected
        // THEN content is truncated
        let content = map["content"].as_str().unwrap();
        assert!(content.len() < 5000);
        assert!(content.contains("truncated"));
    }

    // ── server_icons ─────────────────────────────────────────────────────────

    #[test]
    fn server_icons_returns_light_and_dark() {
        use rust_mcp_sdk::schema::IconTheme;
        // GIVEN the server icon list
        let icons = server_icons();
        // WHEN inspected
        // THEN both light and dark variants are present
        assert_eq!(icons.len(), 2);
        assert!(icons.iter().any(|i| i.theme == Some(IconTheme::Light)));
        assert!(icons.iter().any(|i| i.theme == Some(IconTheme::Dark)));
    }

    #[test]
    fn server_icons_have_svg_mime_type() {
        for icon in server_icons() {
            assert_eq!(icon.mime_type.as_deref(), Some("image/svg+xml"));
            assert_eq!(icon.sizes, vec!["any"]);
            assert!(icon.src.starts_with("data:image/svg+xml;base64,"));
        }
    }
}
