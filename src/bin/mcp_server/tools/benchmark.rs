//! `benchmark` tool — multi-iteration URL timing statistics.

use std::fmt::Write as FmtWrite;
use std::time::Instant;

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use crate::structured::build_structured;
use crate::tools::client::get_client;

// ─── Tool definition ─────────────────────────────────────────────────────────

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
        let client = get_client().await;

        let mut output = format!(
            "🚀 Benchmarking {} URLs, {} iterations each\n\n",
            url_list.len(),
            iterations
        );
        let mut structured_items: Vec<serde_json::Value> = Vec::new();

        for url in url_list {
            let mut times = Vec::with_capacity(iterations);
            let mut errors = 0u32;
            for _ in 0..iterations {
                let start = Instant::now();
                match client.fetch(url).await {
                    Ok(response) => {
                        let _ = response.text().await;
                        times.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    Err(_) => {
                        errors += 1;
                    }
                }
            }
            if !times.is_empty() {
                #[allow(clippy::cast_precision_loss)]
                let avg = times.iter().sum::<f64>() / times.len() as f64;
                let min = times.iter().copied().fold(f64::INFINITY, f64::min);
                let max = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let _ = writeln!(output, "📊 {url}");
                if errors > 0 {
                    let _ = writeln!(
                        output,
                        "   Avg: {avg:.2}ms | Min: {min:.2}ms | Max: {max:.2}ms | Errors: {errors}\n"
                    );
                } else {
                    let _ = writeln!(
                        output,
                        "   Avg: {avg:.2}ms | Min: {min:.2}ms | Max: {max:.2}ms\n"
                    );
                }
                structured_items.push(serde_json::json!({
                    "url": url,
                    "min_ms": min,
                    "avg_ms": avg,
                    "max_ms": max,
                    "iterations": times.len(),
                    "errors": errors,
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
