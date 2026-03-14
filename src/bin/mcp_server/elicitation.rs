//! Elicitation helpers for `nab-mcp`.
//!
//! Contains all interactive prompts sent to the MCP client:
//! - Credential form (username + password)
//! - Credential choice (when multiple 1Password entries match)
//! - OAuth/SSO URL elicitation
//! - Browser cookie-source multi-select
//! - Cookie resolution logic that stitches the above together

use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::sync::Arc;

use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{
    CallToolResult, ElicitFormSchema, ElicitRequestFormParams, ElicitRequestParams,
    ElicitRequestUrlParams, ElicitResultAction, ElicitResultContent, ElicitResultContentPrimitive,
    LegacyTitledEnumSchema, PrimitiveSchemaDefinition, StringSchema, TextContent,
    TitledMultiSelectEnumSchema, TitledMultiSelectEnumSchemaItems,
    TitledMultiSelectEnumSchemaItemsAnyOfItem, schema_utils::CallToolError,
};

use nab::CookieSource;
use nab::content::ContentRouter;

use crate::structured::truncate_markdown;
use crate::tools::get_client;

// ─── Elicitation helpers ──────────────────────────────────────────────────────

/// Ask the user to provide a username and password when no stored credential exists.
pub(crate) async fn elicit_credentials(
    runtime: &Arc<dyn McpServer>,
    url: &str,
) -> Result<(String, String), CallToolError> {
    let mut properties = BTreeMap::new();
    properties.insert(
        "username".into(),
        PrimitiveSchemaDefinition::StringSchema(StringSchema::new(
            None,
            Some("Your username or email address".into()),
            None,
            None,
            None,
            Some("Username".into()),
        )),
    );
    properties.insert(
        "password".into(),
        PrimitiveSchemaDefinition::StringSchema(StringSchema::new(
            None,
            Some("Your password".into()),
            None,
            None,
            None,
            Some("Password".into()),
        )),
    );

    let schema =
        ElicitFormSchema::new(properties, vec!["username".into(), "password".into()], None);

    let result = runtime
        .request_elicitation(ElicitRequestParams::FormParams(
            ElicitRequestFormParams::new(
                format!("No stored credentials found for {url}. Please enter your login details:"),
                schema,
                None,
                None,
            ),
        ))
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

    match result.action {
        ElicitResultAction::Accept => {
            let content = result.content.unwrap_or_default();
            let username = extract_string_field(&content, "username")?;
            let password = extract_string_field(&content, "password")?;
            Ok((username, password))
        }
        ElicitResultAction::Decline | ElicitResultAction::Cancel => {
            Err(CallToolError::from_message("Login cancelled by user"))
        }
    }
}

/// Ask the user to choose one credential when multiple match the domain.
pub(crate) async fn elicit_credential_choice(
    runtime: &Arc<dyn McpServer>,
    url: &str,
    credentials: &[nab::auth::Credential],
) -> Result<String, CallToolError> {
    let titles: Vec<String> = credentials.iter().map(|c| c.title.clone()).collect();
    let title_labels: Vec<String> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let username = credentials[i]
                .username
                .as_deref()
                .map(|u| format!(" ({u})"))
                .unwrap_or_default();
            format!("{t}{username}")
        })
        .collect();

    let mut properties = BTreeMap::new();
    properties.insert(
        "credential".into(),
        PrimitiveSchemaDefinition::LegacyTitledEnumSchema(LegacyTitledEnumSchema::new(
            titles.clone(),
            title_labels,
            None,
            Some("Select the credential to use for login".into()),
            Some("Credential".into()),
        )),
    );

    let schema = ElicitFormSchema::new(properties, vec!["credential".into()], None);

    let result = runtime
        .request_elicitation(ElicitRequestParams::FormParams(
            ElicitRequestFormParams::new(
                format!("Multiple credentials found for {url}. Which one would you like to use?"),
                schema,
                None,
                None,
            ),
        ))
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

    match result.action {
        ElicitResultAction::Accept => {
            let content = result.content.unwrap_or_default();
            extract_string_field(&content, "credential")
        }
        ElicitResultAction::Decline | ElicitResultAction::Cancel => {
            Err(CallToolError::from_message("Login cancelled by user"))
        }
    }
}

/// Perform a credential-based login using a manually-provided username + password.
///
/// This path is used when no 1Password entry exists and the user supplies
/// credentials via elicitation.  The `LoginFlow` cannot be used here because
/// it pulls credentials from 1Password internally, so we fall back to the
/// form-submission path (`SubmitTool`-style).
pub(crate) async fn run_login_with_credentials(
    url: &str,
    username: &str,
    password: &str,
    mut output: String,
) -> Result<CallToolResult, CallToolError> {
    let client = get_client().await;

    let page_html = client
        .fetch_text(url)
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

    let mut forms =
        nab::Form::parse_all(&page_html).map_err(|e| CallToolError::from_message(e.to_string()))?;

    if forms.is_empty() {
        return Err(CallToolError::from_message("No login form found on page"));
    }

    let mut form = forms.remove(0);
    let _ = writeln!(output, "   Form: {} {}", form.method, form.action);

    // Best-effort field injection: typical login forms use username/email + password.
    form.fields
        .entry("username".into())
        .or_insert_with(|| username.to_string());
    form.fields
        .entry("email".into())
        .or_insert_with(|| username.to_string());
    form.fields.insert("password".into(), password.to_string());

    let action_url = form
        .resolve_action(url)
        .map_err(|e| CallToolError::from_message(e.to_string()))?;
    let form_data = form.encode_urlencoded();

    let response = client
        .inner()
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

    output.push_str(&truncate_markdown(&conversion.markdown, 4000));
    Ok(CallToolResult::text_content(vec![TextContent::from(
        output,
    )]))
}

/// Extract a string value from the elicitation response content map.
pub(crate) fn extract_string_field(
    content: &BTreeMap<String, ElicitResultContent>,
    field: &str,
) -> Result<String, CallToolError> {
    match content.get(field) {
        Some(ElicitResultContent::Primitive(ElicitResultContentPrimitive::String(s))) => {
            Ok(s.clone())
        }
        Some(_) => Err(CallToolError::from_message(format!(
            "Unexpected type for elicitation field '{field}'"
        ))),
        None => Err(CallToolError::from_message(format!(
            "Missing required elicitation field '{field}'"
        ))),
    }
}

// ─── OAuth URL elicitation ────────────────────────────────────────────────────

/// Known OAuth/SSO redirect hostname patterns.
const OAUTH_HOSTS: &[&str] = &[
    "accounts.google.com",
    "github.com/login/oauth",
    "login.microsoftonline.com",
    "appleid.apple.com",
    "facebook.com/login",
    "twitter.com/i/oauth",
    "x.com/i/oauth",
    "linkedin.com/oauth",
    "auth0.com",
    "okta.com",
    "pingidentity.com",
    "onelogin.com",
    "login.live.com",
];

/// Returns `true` when `url` looks like an OAuth/SSO provider redirect.
pub(crate) fn is_oauth_redirect(url: &str) -> bool {
    let lower = url.to_lowercase();
    OAUTH_HOSTS.iter().any(|&host| lower.contains(host))
}

/// Extract a human-readable service name from an OAuth URL for display in
/// the elicitation message.
pub(crate) fn oauth_service_name(url: &str) -> String {
    let lower = url.to_lowercase();
    if lower.contains("google") {
        "Google"
    } else if lower.contains("github") {
        "GitHub"
    } else if lower.contains("microsoft") || lower.contains("live.com") {
        "Microsoft"
    } else if lower.contains("apple") {
        "Apple"
    } else if lower.contains("facebook") {
        "Facebook"
    } else if lower.contains("twitter") || lower.contains("x.com") {
        "X (Twitter)"
    } else if lower.contains("linkedin") {
        "LinkedIn"
    } else if lower.contains("auth0") {
        "Auth0"
    } else if lower.contains("okta") {
        "Okta"
    } else {
        "the OAuth provider"
    }
    .to_string()
}

/// Send a URL elicitation to guide the user through an OAuth/SSO flow.
///
/// The 2025-11-25 protocol's `ElicitRequestUrlParams` lets the client open a
/// URL directly in the user's browser rather than showing a form.  After the
/// user completes the OAuth flow the server can capture the resulting cookies
/// via a follow-up form elicitation.
///
/// Returns the elicitation result action so the caller can branch on
/// accept/cancel.
pub(crate) async fn elicit_oauth_url(
    runtime: &Arc<dyn McpServer>,
    oauth_url: &str,
    service_name: &str,
) -> Result<ElicitResultAction, CallToolError> {
    // elicitation_id must be unique per request; use a short random suffix.
    let elicitation_id = format!(
        "oauth-{}-{}",
        service_name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_millis())
            .unwrap_or(0)
    );

    let result = runtime
        .request_elicitation(ElicitRequestParams::UrlParams(ElicitRequestUrlParams::new(
            elicitation_id,
            format!(
                "OAuth login required for {service_name}. \
                     Please complete the sign-in in your browser. \
                     The page will reload once authentication is complete."
            ),
            oauth_url.to_string(),
            None,
            None,
        )))
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

    Ok(result.action)
}

// ─── Cookie resolution ───────────────────────────────────────────────────────

/// Resolve the cookie header to use for login.
///
/// When `explicit_cookies` is provided (the tool's `cookies` parameter), use it
/// directly.  Otherwise, offer multi-select elicitation so the user can choose
/// one or more browser cookie stores; cookies from all selected stores are
/// concatenated with `"; "` as separator.
///
/// # Errors
///
/// Returns `CallToolError` if the elicitation RPC fails.
pub(crate) async fn resolve_login_cookies(
    url: &str,
    explicit_cookies: Option<&str>,
    runtime: &Arc<dyn McpServer>,
) -> Result<Option<String>, CallToolError> {
    if let Some(cookie) = explicit_cookies {
        return Ok(Some(cookie.to_string()));
    }

    let sources = elicit_cookie_sources(runtime, url).await?;
    if sources.is_empty() {
        return Ok(None);
    }

    let domain = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_default();

    let combined = sources
        .iter()
        .filter_map(|s| {
            let source = match s.as_str() {
                "chrome" => CookieSource::Chrome,
                "firefox" => CookieSource::Firefox,
                "safari" => CookieSource::Safari,
                _ => CookieSource::Brave,
            };
            source.get_cookie_header(&domain).ok()
        })
        .collect::<Vec<_>>()
        .join("; ");

    Ok(if combined.is_empty() {
        None
    } else {
        Some(combined)
    })
}

// ─── Multi-select cookie source elicitation ───────────────────────────────────

/// Ask the user which browser cookie stores to use for the login, allowing
/// multiple sources to be selected simultaneously.
///
/// Uses `TitledMultiSelectEnumSchema` from the 2025-11-25 protocol spec.
/// Returns the selected browser names (e.g. `["brave", "chrome"]`).
pub(crate) async fn elicit_cookie_sources(
    runtime: &Arc<dyn McpServer>,
    url: &str,
) -> Result<Vec<String>, CallToolError> {
    let options: &[(&str, &str)] = &[
        ("brave", "Brave Browser"),
        ("chrome", "Google Chrome"),
        ("firefox", "Mozilla Firefox"),
        ("safari", "Apple Safari"),
    ];

    let items = TitledMultiSelectEnumSchemaItems {
        any_of: options
            .iter()
            .map(
                |&(value, label)| TitledMultiSelectEnumSchemaItemsAnyOfItem {
                    const_: value.to_string(),
                    title: label.to_string(),
                },
            )
            .collect(),
    };

    let multi_select = TitledMultiSelectEnumSchema::new(
        vec!["brave".to_string()], // default: Brave
        items,
        Some("Cookie stores to import for authentication".into()),
        None, // max_items
        None, // min_items
        Some("Cookie Sources".into()),
    );

    let mut properties = BTreeMap::new();
    properties.insert(
        "sources".into(),
        PrimitiveSchemaDefinition::TitledMultiSelectEnumSchema(multi_select),
    );

    let schema = ElicitFormSchema::new(properties, vec!["sources".into()], None);

    let result = runtime
        .request_elicitation(ElicitRequestParams::FormParams(
            ElicitRequestFormParams::new(
                format!(
                    "Which browser cookie stores should be used for login to {url}? \
                 Select all that apply."
                ),
                schema,
                None,
                None,
            ),
        ))
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;

    match result.action {
        ElicitResultAction::Accept => {
            // The multi-select result comes back as a JSON array of selected values.
            // The SDK encodes it as ElicitResultContent::Primitive::String (JSON-serialised
            // array) or as individual entries — extract and decode defensively.
            let content = result.content.unwrap_or_default();
            let sources = extract_multiselect_field(&content, "sources");
            Ok(sources)
        }
        ElicitResultAction::Decline | ElicitResultAction::Cancel => {
            // User skipped — return empty list so caller can fall back to no cookies.
            Ok(vec![])
        }
    }
}

/// Extract a multi-select field (array of strings) from elicitation result content.
///
/// Clients MAY encode the array as a JSON string `"[\"a\",\"b\"]"` or as a
/// comma-separated string `"a,b"`.  Both forms are handled here.  Returns an
/// empty `Vec` when the field is absent or has an unexpected type.
pub(crate) fn extract_multiselect_field(
    content: &BTreeMap<String, ElicitResultContent>,
    field: &str,
) -> Vec<String> {
    match content.get(field) {
        Some(ElicitResultContent::Primitive(ElicitResultContentPrimitive::String(s))) => {
            // Try JSON array first.
            if let Ok(vals) = serde_json::from_str::<Vec<String>>(s) {
                return vals;
            }
            // Fall back to comma-separated.
            s.split(',')
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect()
        }
        Some(_) | None => vec![],
    }
}
