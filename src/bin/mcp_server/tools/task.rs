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

use nab::AcceleratedClient;
use nab::content::html::HtmlConversionOptions;
use nab::task::{TaskOutcome, TaskStatus, discover_apis};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use crate::helpers::{convert_body_async_with_options, fetch_with_cookies, resolve_cookie_header};
use crate::sampling;
use crate::tools::client::get_client;

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
