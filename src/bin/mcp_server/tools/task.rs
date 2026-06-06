//! `task` MCP tool — API-first web-task entry (the single contact point).
//!
//! Slice-4 step 4 (host-driven seed): fetch the seed URL through the moat,
//! YARA-screen it, surface rung-1 API leads, and return a
//! [`nab::task::TaskOutcome`] as JSON. The self-contained MCP-sampling loop
//! (`nab::task::run_task_loop` over an `McpSampler` + `McpFetcher`) lands in a
//! follow-up; this exposes `nab task` as an MCP tool at rung 0 + rung-1
//! discovery so a host LLM has the single contact point today.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use nab::content::html::HtmlConversionOptions;
use nab::task::{
    FetchRequest, LoopBounds, Sampler, TaskFetcher, TaskOutcome, TaskStatus, discover_apis,
    run_task_loop,
};
use nab::{AcceleratedClient, SafeFetchConfig, SafeRequestOptions, SsrfPolicy};
use reqwest::Method;
use reqwest::header::{COOKIE, HeaderName, HeaderValue};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use crate::helpers::{convert_body_async_with_options, fetch_with_cookies, resolve_cookie_header};
use crate::sampling;
use crate::tools::client::get_client;

/// `nab-mcp`'s fetch backend for the self-contained loop: executes a rung-1
/// `api_call` (method + headers + body) through the moat via the library
/// `AcceleratedClient`, then YARA-screens + shapes the response. Mirrors the
/// CLI's `CmdFetcher` but on lib primitives (the binary boundary, design §12.2).
struct McpFetcher;

impl TaskFetcher for McpFetcher {
    async fn fetch(&self, req: FetchRequest) -> anyhow::Result<String> {
        let client: &AcceleratedClient = get_client().await;
        let profile = client.profile().await;
        let mut headers = profile.to_headers();
        let cookie_header = resolve_cookie_header(&req.url, None);
        if !cookie_header.is_empty() {
            headers.insert(COOKIE, HeaderValue::from_str(&cookie_header)?);
        }
        for (name, value) in &req.headers {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(value)?,
            );
        }
        let method = Method::from_bytes(req.method.as_bytes())?;
        let resp = client
            .request_safe(
                &req.url,
                SafeRequestOptions {
                    method,
                    headers,
                    body: req.body.map(Bytes::from),
                    config: SafeFetchConfig::default(),
                    ssrf_policy: SsrfPolicy::from_env(),
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let conversion = convert_body_async_with_options(
            &resp.body,
            &resp.content_type,
            &req.url,
            HtmlConversionOptions::default(),
        )
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let screened =
            nab::security::guard_fetch_output(&conversion.markdown, "mcp_task_api", &req.url)?;
        Ok(screened)
    }

    async fn fetch_raw(&self, url: &str) -> anyhow::Result<String> {
        // The submit rung needs the form page as raw `<form>` HTML — markdown
        // conversion (the `fetch` path) would strip it. Same moat (client,
        // fingerprint, cookies, SSRF, YARA screen), but no markdown conversion.
        let client: &AcceleratedClient = get_client().await;
        let profile = client.profile().await;
        let mut headers = profile.to_headers();
        let cookie_header = resolve_cookie_header(url, None);
        if !cookie_header.is_empty() {
            headers.insert(COOKIE, HeaderValue::from_str(&cookie_header)?);
        }
        let resp = client
            .request_safe(
                url,
                SafeRequestOptions {
                    method: Method::GET,
                    headers,
                    body: None,
                    config: SafeFetchConfig::default(),
                    ssrf_policy: SsrfPolicy::from_env(),
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let raw = String::from_utf8_lossy(&resp.body);
        let screened = nab::security::guard_fetch_output(&raw, "mcp_task_submit", url)?;
        Ok(screened)
    }
}

/// The loop's brain over MCP sampling: forwards the prompt to the connected
/// client's LLM via `sampling/createMessage` and returns its reply.
struct McpSampler<'a> {
    runtime: &'a Arc<dyn McpServer>,
}

impl Sampler for McpSampler<'_> {
    async fn next_action(&self, prompt: &str) -> anyhow::Result<String> {
        sampling::create_message(self.runtime, prompt, 1024, None).await
    }
}

#[mcp_tool(
    name = "task",
    description = "Run a web task: fetch a seed URL through the moat (browser
cookies, fingerprint, HTTP/3), YARA-screen it, and return LLM-shaped markdown
plus the rung-1 API endpoints discovered on the page that you can call directly.

This is nab's single-contact-point web-task entry. Today it returns rung-0 (the
screened seed) plus rung-1 leads (discovered_apis); to act on a lead, call the
`fetch` tool with the chosen endpoint. The self-contained agentic loop (nab
drives the steps itself via MCP sampling) is being wired in a follow-up.

Returns: JSON TaskOutcome { goal, url, rung, status, content, discovered_apis }.",
    read_only_hint = true,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TaskTool {
    /// Natural-language goal for the task.
    pub goal: String,
    /// Seed URL to start from.
    pub url: String,
    /// When true AND the client supports sampling, nab runs the bounded
    /// self-contained loop (sample → execute → observe → repeat) and returns the
    /// full trajectory. Defaults to false (host-driven: seed + `discovered_apis`).
    #[serde(default)]
    pub autonomous: bool,
}

impl TaskTool {
    /// Run the task tool (rung-0 seed + rung-1 discovery).
    ///
    /// `runtime` is used to report whether the connected client supports
    /// `sampling/createMessage` (the self-contained loop's prerequisite).
    pub async fn run(&self, runtime: &Arc<dyn McpServer>) -> Result<CallToolResult, CallToolError> {
        let start = Instant::now();
        let client: &AcceleratedClient = get_client().await;
        let profile = client.profile().await;
        let cookie_header = resolve_cookie_header(&self.url, None);
        let ssrf_policy = nab::SsrfPolicy::from_env();

        let (_status, content_type, _headers, body_bytes, _elapsed) = fetch_with_cookies(
            client,
            &self.url,
            &cookie_header,
            &profile,
            &ssrf_policy,
            start,
        )
        .await?;

        let conversion = convert_body_async_with_options(
            &body_bytes,
            &content_type,
            &self.url,
            HtmlConversionOptions::default(),
        )
        .await?;
        let markdown =
            nab::security::guard_fetch_output(&conversion.markdown, "mcp_task", &self.url)
                .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let raw = String::from_utf8_lossy(&body_bytes);
        let discovered_apis = discover_apis(&raw);

        // Self-contained mode (§9.1): when the caller opts in and the client
        // supports sampling, nab drives the whole bounded loop itself — the host
        // LLM is the brain (McpSampler), nab supplies execution (McpFetcher).
        if self.autonomous && sampling::is_supported(runtime) {
            let sampler = McpSampler { runtime };
            let fetcher = McpFetcher;
            let loop_outcome = run_task_loop(
                &self.goal,
                &markdown,
                &discovered_apis,
                &sampler,
                &fetcher,
                &LoopBounds::default(),
            )
            .await;
            let json = serde_json::to_string_pretty(&loop_outcome)
                .map_err(|e| CallToolError::from_message(e.to_string()))?;
            return Ok(CallToolResult::text_content(vec![TextContent::from(json)]));
        }

        let outcome = TaskOutcome {
            goal: self.goal.clone(),
            url: self.url.clone(),
            rung: 0,
            status: TaskStatus::Done,
            content: markdown,
            discovered_apis,
        };
        let json = serde_json::to_string_pretty(&outcome)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let mode = if sampling::is_supported(runtime) {
            "[task] client supports sampling — self-contained agentic loop is being wired; \
             for now read discovered_apis and call the `fetch` tool with a chosen endpoint."
        } else {
            "[task] host-driven — read discovered_apis and call the `fetch` tool with a \
             chosen endpoint to continue."
        };

        Ok(CallToolResult::text_content(vec![TextContent::from(
            format!("{json}\n\n{mode}"),
        )]))
    }
}
