//! `submit` tool — HTML form extraction and submission with CSRF handling.

use std::fmt::Write as FmtWrite;

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use crate::helpers::{
    convert_body_async, read_response_capped, resolve_cookie_header, validate_tool_url,
};
use crate::structured::{TOOL_TRUNCATION_LIMIT, truncate_markdown};
use crate::tools::client::{build_transient_client, persist_session, resolve_session_client};

// ─── Tool definition ─────────────────────────────────────────────────────────

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
    /// Browser cookie source.
    ///
    /// Omit or use `"auto"` to seed from the default browser for this domain.
    /// Use `"none"` to disable cookie seeding, or pass an explicit browser name
    /// such as `"brave"`, `"chrome"`, `"firefox"`, `"safari"`, or `"edge"`.
    #[serde(default)]
    cookies: Option<String>,
    /// Named session for cookie persistence.  When set, the form page fetch
    /// and the POST submission both use the session's cookie jar, preserving
    /// authentication state. Session jars are AES-256-GCM encrypted at rest in
    /// `~/.nab/sessions/` on non-Windows platforms. See `fetch` `session` for
    /// full documentation.
    #[serde(default)]
    session: Option<String>,
}

impl SubmitTool {
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let mut output = format!("📝 Submitting form on: {}\n", self.url);

        // Resolve inner reqwest::Client: session-owned or global.
        let (page_html, inner_client) = self.fetch_page(&mut output).await?;

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
        validate_tool_url(&action_url)?;
        let form_data = form.encode_urlencoded();

        let response = inner_client
            .post(&action_url)
            .header("Content-Type", form.content_type())
            .body(form_data)
            .send()
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        let status = response.status();
        let body_bytes = read_response_capped(response).await?;
        if let Some(ref session_name) = self.session {
            persist_session(session_name).await?;
        }

        let _ = writeln!(output, "   Status: {status}\n");

        let conversion = convert_body_async(&body_bytes, "text/html", &action_url).await?;

        output.push_str(&truncate_markdown(
            &conversion.markdown,
            TOOL_TRUNCATION_LIMIT,
        ));

        let structured = crate::structured::build_structured([
            ("url", serde_json::Value::String(self.url.clone())),
            ("status", serde_json::json!(status.as_u16())),
            (
                "content",
                serde_json::Value::String(truncate_markdown(
                    &conversion.markdown,
                    TOOL_TRUNCATION_LIMIT,
                )),
            ),
        ]);
        let mut result = CallToolResult::text_content(vec![TextContent::from(output)]);
        result.structured_content = Some(structured);
        Ok(result)
    }

    /// Fetch the form page and return `(html, reqwest::Client)`.
    ///
    /// Uses the session's cookie-jar client when a session name is set,
    /// otherwise builds a transient per-call client so cookies and `Set-Cookie`
    /// state persist across the initial page fetch and the eventual form submit.
    async fn fetch_page(
        &self,
        output: &mut String,
    ) -> Result<(String, reqwest::Client), CallToolError> {
        validate_tool_url(&self.url)?;
        let cookie_header = resolve_cookie_header(&self.url, self.cookies.as_deref());
        if let Some(ref session_name) = self.session {
            let session_client =
                resolve_session_client(session_name, Some(&cookie_header), &self.url).await?;
            let _ = writeln!(output, "   Session: {session_name}");
            let resp = session_client
                .get(&self.url)
                .send()
                .await
                .map_err(|e| CallToolError::from_message(e.to_string()))?;
            let html_bytes = read_response_capped(resp).await?;
            let html = String::from_utf8_lossy(&html_bytes).into_owned();
            Ok((html, session_client))
        } else {
            let client = build_transient_client(Some(&cookie_header), &self.url).await?;
            let resp = client
                .get(&self.url)
                .send()
                .await
                .map_err(|e| CallToolError::from_message(e.to_string()))?;
            let html_bytes = read_response_capped(resp).await?;
            let html = String::from_utf8_lossy(&html_bytes).into_owned();
            Ok((html, client))
        }
    }
}
