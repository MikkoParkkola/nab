//! `validate` tool — built-in integration smoke tests.

use std::fmt::Write as FmtWrite;
use std::time::Instant;

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use nab::OnePasswordAuth;

use crate::helpers::{run_tls_test, run_validation_test};
use crate::tools::client::get_client;

// ─── Tool definition ─────────────────────────────────────────────────────────

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
        let client = get_client().await;
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

        let structured = crate::structured::build_structured([(
            "duration_s",
            serde_json::json!(start.elapsed().as_secs_f64()),
        )]);
        let mut result = CallToolResult::text_content(vec![TextContent::from(output)]);
        result.structured_content = Some(structured);
        Ok(result)
    }
}
