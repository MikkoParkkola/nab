//! MCP tool definitions and implementations for `nab-mcp`.
//!
//! Each struct corresponds to one MCP tool exposed by the server.

use std::fmt::Write as FmtWrite;
use std::sync::Arc;
use std::time::Instant;

use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{
    CallToolResult, ElicitResultAction, TextContent, schema_utils::CallToolError,
};
use serde::{Deserialize, Serialize};

use tokio::sync::OnceCell;

use nab::content::ContentRouter;
use nab::content::budget::truncate_to_budget;
use nab::content::diff::{ContentSnapshot, compute_diff};
use nab::content::diff_format::format_diff_markdown;
use nab::content::focus::extract_focused;
use nab::content::snapshot_store::SnapshotStore;
use nab::{
    AcceleratedClient, CredentialRetriever, OnePasswordAuth, SafeFetchConfig, chrome_profile,
    firefox_profile, random_profile, safari_profile,
};

use crate::elicitation::{
    elicit_credential_choice, elicit_credentials, elicit_oauth_url, is_oauth_redirect,
    oauth_service_name, resolve_login_cookies, run_login_with_credentials,
};
use crate::helpers::{
    convert_body_async, fetch_safe_response, fetch_with_cookies, resolve_cookie_header,
    run_tls_test, run_validation_test, write_body_info, write_response_summary,
};
use crate::structured::{build_fetch_structured_v2, build_structured, truncate_markdown};

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

Diff mode (diff: true):
- Compares current content against the previous snapshot for this URL
- Returns only the changed sections (token-efficient for monitoring tasks)
- First fetch caches the page; subsequent fetches return semantic diffs
- Unchanged content returns a 5-token confirmation instead of full body

Focus mode (focus: query):
- Keeps only sections relevant to the query (BM25 scoring)
- Replaces dropped sections with '[N sections omitted]' markers
- Diff markers are always preserved regardless of relevance

Token budget (max_tokens: N):
- Structure-aware truncation preserving headings, code, and tables
- Priority: title > code/tables > headings (30% cap) > body > blockquotes

Returns: Markdown-converted body with timing info (or diff when diff: true).",
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
    /// When true, return only changed content vs the previous snapshot.
    ///
    /// On first fetch the page is cached and full content is returned.
    /// On subsequent fetches only the semantic diff is returned, saving
    /// tokens for monitoring or change-detection workflows.
    #[serde(default)]
    diff: bool,
    /// Natural-language query to focus extraction on relevant sections.
    ///
    /// When set, uses BM25 scoring to keep only the sections most relevant
    /// to the query, replacing omitted sections with count markers.
    /// Dramatically reduces token count for large documents when you know
    /// what you're looking for.
    #[serde(default)]
    focus: Option<String>,
    /// Maximum token budget for the returned content.
    ///
    /// When set, performs structure-aware truncation that preserves
    /// headings, code blocks, and tables before trimming body text.
    /// Uses priority scoring: title/summary first, then code/tables,
    /// then headings (capped at 30% of budget), then body text.
    #[serde(default)]
    max_tokens: Option<u64>,
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

        // Determine markdown, status, content_type, and elapsed_ms from either
        // a specialized site provider or the standard HTTP fetch path.  Both
        // paths converge below into the single diff + structured_content pipeline.
        let (markdown, status_u16, content_type, elapsed_ms) = if let Some(site_content) =
            site_router.try_extract(&self.url, client, cookie_opt).await
        {
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            output.push_str("\n📄 Content (from specialized provider):\n\n");
            (
                site_content.markdown,
                200u16,
                "text/html".to_owned(),
                elapsed_ms,
            )
        } else {
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

            (
                conversion.markdown,
                status.as_u16(),
                content_type,
                elapsed.as_secs_f64() * 1000.0,
            )
        };

        // Unified post-processing pipeline: diff → focus → budget
        let has_diff = if self.diff {
            let (diff_output, had_diff) = apply_diff(&self.url, &markdown);
            output.push('\n');
            output.push_str(&diff_output);
            had_diff
        } else {
            if self.body {
                let truncated = truncate_markdown(&markdown, 4000);
                let _ = write!(output, "\n{truncated}");
            }
            false
        };

        // Focus: keep only sections relevant to the query (BM25 scoring).
        // Diff markers are automatically exempt from filtering.
        let (processed_markdown, omitted_sections, total_sections) =
            if let Some(ref query) = self.focus {
                let focus_result = extract_focused(&markdown, query);
                (
                    focus_result.markdown,
                    focus_result.omitted_sections,
                    focus_result.total_sections,
                )
            } else {
                (markdown.clone(), 0, 0)
            };

        // Budget: structure-aware truncation with priority scoring.
        let max_tok = self
            .max_tokens
            .map(|t| usize::try_from(t).unwrap_or(usize::MAX));
        let budget_result = truncate_to_budget(&processed_markdown, max_tok);

        let structured = build_fetch_structured_v2(
            &self.url,
            status_u16,
            &content_type,
            &budget_result.markdown,
            elapsed_ms,
            has_diff,
            omitted_sections,
            total_sections,
            budget_result.truncated,
            budget_result.total_tokens,
        );

        let mut result = CallToolResult::text_content(vec![TextContent::from(output)]);
        result.structured_content = Some(structured);
        Ok(result)
    }
}

// ─── diff helper ──────────────────────────────────────────────────────────────

/// Load previous snapshot, compute diff, save new snapshot.
///
/// Returns `(formatted_output, has_diff)` where `has_diff` is `true` when
/// content changed since the last snapshot.  Always saves a fresh snapshot
/// regardless of whether content changed.
fn apply_diff(url: &str, markdown: &str) -> (String, bool) {
    apply_diff_with_store(&SnapshotStore::new(), url, markdown)
}

/// Testable variant: same logic as [`apply_diff`] but uses an explicit store.
pub(crate) fn apply_diff_with_store(
    store: &SnapshotStore,
    url: &str,
    markdown: &str,
) -> (String, bool) {
    let new_snap = ContentSnapshot::new(url, markdown, std::time::SystemTime::now());

    let output = match store.load_latest_snapshot(url) {
        Some(old_snap) if old_snap.content_unchanged(&new_snap) => {
            let _ = store.save_snapshot(url, &new_snap);
            "No changes since last fetch".to_owned()
        }
        Some(old_snap) => {
            let _ = store.save_snapshot(url, &new_snap);
            let diff = compute_diff(&old_snap, &new_snap);
            format!(
                "Changed since last fetch:\n\n{}",
                format_diff_markdown(&diff)
            )
        }
        None => {
            let _ = store.save_snapshot(url, &new_snap);
            format!(
                "First fetch (cached for future diff):\n\n{}",
                truncate_markdown(markdown, 4000)
            )
        }
    };

    let has_diff = !output.starts_with("No changes") && !output.starts_with("First fetch");
    (output, has_diff)
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

        let structured =
            build_structured([("results", serde_json::Value::Array(structured_items))]);
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

        let structured =
            build_structured([("results", serde_json::Value::Array(structured_items))]);
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
            return Ok(CallToolResult::text_content(vec![TextContent::from(
                output,
            )]));
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
        let resolved_cookies =
            resolve_login_cookies(&self.url, self.cookies.as_deref(), &runtime).await?;

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
