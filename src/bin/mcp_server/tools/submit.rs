//! `submit` tool — HTML form extraction and submission with CSRF handling.

use std::fmt::Write as FmtWrite;

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use nab::AcceleratedClient;
use nab::content::ContentRouter;

use crate::helpers::resolve_cookie_header;
use crate::structured::{TOOL_TRUNCATION_LIMIT, truncate_markdown};
use crate::tools::client::{get_client, resolve_session_client};

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
    #[serde(default)]
    cookies: Option<String>,
    /// Named session for cookie persistence.  When set, the form page fetch
    /// and the POST submission both use the session's cookie jar, preserving
    /// authentication state.  See `fetch` `session` for full documentation.
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
        let form_data = form.encode_urlencoded();

        let response = inner_client
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
    /// otherwise falls back to the global `AcceleratedClient`.
    async fn fetch_page(
        &self,
        output: &mut String,
    ) -> Result<(String, reqwest::Client), CallToolError> {
        if let Some(ref session_name) = self.session {
            let cookie_header = resolve_cookie_header(&self.url, self.cookies.as_deref());
            let session_client =
                resolve_session_client(session_name, Some(&cookie_header), &self.url).await?;
            let _ = writeln!(output, "   Session: {session_name}");
            let resp = session_client
                .get(&self.url)
                .send()
                .await
                .map_err(|e| CallToolError::from_message(e.to_string()))?;
            let html = resp
                .text()
                .await
                .map_err(|e| CallToolError::from_message(e.to_string()))?;
            Ok((html, session_client))
        } else {
            let client: &AcceleratedClient = get_client().await;
            let html = client
                .fetch_text(&self.url)
                .await
                .map_err(|e| CallToolError::from_message(e.to_string()))?;
            Ok((html, client.inner().clone()))
        }
    }
}
