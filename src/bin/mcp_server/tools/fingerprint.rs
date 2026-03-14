//! `fingerprint` tool — realistic browser profile generation.

use std::fmt::Write as FmtWrite;

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use nab::{chrome_profile, firefox_profile, random_profile, safari_profile};

use crate::structured::build_structured;

// ─── Tool definition ─────────────────────────────────────────────────────────

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
