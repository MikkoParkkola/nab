//! `auth_lookup` tool — 1Password credential lookup for a URL.

use std::fmt::Write as FmtWrite;

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use nab::{CredentialRetriever, OnePasswordAuth};

use crate::structured::build_structured;

// ─── Tool definition ─────────────────────────────────────────────────────────

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
