//! `fetch_batch` tool — parallel multi-URL fetch with task-augmented support.

use std::fmt::Write as FmtWrite;
use std::time::Instant;

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use crate::structured::{build_structured, truncate_markdown};
use crate::tools::client::get_client;

// ─── Tool definition ─────────────────────────────────────────────────────────

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
        let client = get_client().await;

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
