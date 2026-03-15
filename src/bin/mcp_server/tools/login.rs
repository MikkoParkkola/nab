//! `login` tool — automated website login via 1Password with OAuth/SSO detection.

use std::fmt::Write as FmtWrite;
use std::sync::Arc;

use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{
    CallToolResult, ElicitResultAction, TextContent, schema_utils::CallToolError,
};
use serde::{Deserialize, Serialize};

use nab::content::ContentRouter;
use nab::{AcceleratedClient, OnePasswordAuth};

use crate::elicitation::{
    elicit_credential_choice, elicit_credentials, elicit_oauth_url, is_oauth_redirect,
    oauth_service_name, resolve_login_cookies, run_login_with_credentials,
};
use crate::structured::{TOOL_TRUNCATION_LIMIT, truncate_markdown};
use crate::tools::client::resolve_session_client;

// ─── Tool definition ─────────────────────────────────────────────────────────

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
    /// Named session to store authenticated cookies in after login.
    ///
    /// When set, the login flow uses the session's isolated cookie jar.
    /// All `Set-Cookie` headers from the login flow are automatically stored
    /// in the jar and will be sent on subsequent `fetch` or `submit` calls
    /// that use the same session name — no manual cookie extraction needed.
    ///
    /// Session names: 1-64 chars, alphanumeric + hyphens + underscores.
    #[serde(default)]
    session: Option<String>,
}

impl LoginTool {
    #[allow(clippy::too_many_lines)]
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

        // Build the AcceleratedClient for the login flow.
        // When a session is named, wrap the session's dedicated reqwest::Client
        // so that all Set-Cookie headers from the login flow are stored in the
        // session's cookie jar and available to subsequent fetch/submit calls.
        let client = if let Some(ref session_name) = self.session {
            let session_client =
                resolve_session_client(session_name, resolved_cookies.as_deref(), &self.url)
                    .await?;
            let _ = writeln!(output, "   Session: {session_name}");
            AcceleratedClient::from_client(session_client)
                .map_err(|e| CallToolError::from_message(e.to_string()))?
        } else {
            AcceleratedClient::new().map_err(|e| CallToolError::from_message(e.to_string()))?
        };
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

        output.push_str(&truncate_markdown(
            &conversion.markdown,
            TOOL_TRUNCATION_LIMIT,
        ));

        let structured = crate::structured::build_structured([
            ("url", serde_json::Value::String(self.url.clone())),
            (
                "final_url",
                serde_json::Value::String(result.final_url.clone()),
            ),
            ("status", serde_json::Value::String("success".to_string())),
            (
                "content",
                serde_json::Value::String(truncate_markdown(
                    &conversion.markdown,
                    TOOL_TRUNCATION_LIMIT,
                )),
            ),
        ]);
        let mut call_result = CallToolResult::text_content(vec![TextContent::from(output)]);
        call_result.structured_content = Some(structured);
        Ok(call_result)
    }
}
