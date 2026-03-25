//! `MicroFetch` MCP Server - Native Rust implementation
//!
//! Ultra-fast MCP server for web fetching with HTTP/3, fingerprint spoofing,
//! and 1Password integration. Uses MCP protocol 2025-11-25 with full
//! tool annotations, structured output schemas, task-augmented execution,
//! and elicitation support.
//!
//! # Usage
//!
//! Stdio mode (for Claude Code integration):
//! ```bash
//! nab-mcp
//! ```

pub mod elicitation;
pub mod helpers;
pub mod structured;
#[cfg(test)]
mod tests;
pub mod tools;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::mcp_server::ToMcpServerHandler;
use rust_mcp_sdk::mcp_server::{McpServerOptions, ServerHandler, server_runtime};
use rust_mcp_sdk::schema::{
    CallToolRequestParams, CallToolResult, ContentBlock, CreateTaskResult, GetPromptRequestParams,
    GetPromptResult, Implementation, InitializeResult, LATEST_PROTOCOL_VERSION, ListPromptsResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, Prompt, PromptArgument,
    PromptMessage, ReadResourceRequestParams, ReadResourceResult, Resource,
    ServerCapabilitiesPrompts, ServerCapabilitiesResources, RpcError, ServerCapabilities,
    ServerCapabilitiesTools, ServerTaskRequest, ServerTaskTools, ServerTasks, TextContent,
    TextResourceContents, ToolAnnotations, ToolExecution, ToolExecutionTaskSupport, ToolOutputSchema,
    schema_utils::CallToolError,
};
use rust_mcp_sdk::schema::{
    TaskStatus,
    schema_utils::{ClientJsonrpcRequest, ResultFromServer},
};
use rust_mcp_sdk::task_store::{CreateTaskOptions, InMemoryTaskStore, ServerTaskCreator};
use rust_mcp_sdk::{McpServer, StdioTransport, TransportOptions, tool_box};

use structured::server_icons;
use tools::{
    AuthLookupTool, BenchmarkTool, FetchBatchTool, FetchTool, FingerprintTool, LoginTool,
    SubmitTool, ValidateTool, get_client,
};

// Generate the tools enum
tool_box!(
    MicroFetchTools,
    [
        FetchTool,
        FetchBatchTool,
        SubmitTool,
        LoginTool,
        AuthLookupTool,
        FingerprintTool,
        ValidateTool,
        BenchmarkTool
    ]
);

// ─── Output Schema builders ───────────────────────────────────────────────────

/// Build the `outputSchema` for the `fetch` tool.
///
/// Returns: `{ url, status, content_type, content, timing_ms }`
fn fetch_output_schema() -> ToolOutputSchema {
    let mut props = BTreeMap::new();
    props.insert("url".into(), string_prop("The fetched URL"));
    props.insert("status".into(), integer_prop("HTTP status code"));
    props.insert(
        "content_type".into(),
        string_prop("Response Content-Type header"),
    );
    props.insert(
        "content".into(),
        string_prop("Markdown-converted body content"),
    );
    props.insert(
        "timing_ms".into(),
        number_prop("Round-trip time in milliseconds"),
    );
    props.insert(
        "has_diff".into(),
        bool_prop("True when diff mode was requested and content changed since last snapshot"),
    );
    ToolOutputSchema::new(
        vec![
            "url".into(),
            "status".into(),
            "content".into(),
            "timing_ms".into(),
            "has_diff".into(),
        ],
        Some(props),
        None,
    )
}

/// Build the `outputSchema` for the `fetch_batch` tool.
///
/// Returns: `{ results: [{ url, status, content, timing_ms }] }`
fn fetch_batch_output_schema() -> ToolOutputSchema {
    let mut item_props = BTreeMap::new();
    item_props.insert("url".into(), string_prop("The fetched URL"));
    item_props.insert(
        "status".into(),
        integer_prop("HTTP status code, or null on error"),
    );
    item_props.insert(
        "content".into(),
        string_prop("Body content or error message"),
    );
    item_props.insert(
        "timing_ms".into(),
        number_prop("Per-URL round-trip time in milliseconds"),
    );

    let mut results_items = serde_json::Map::new();
    results_items.insert("type".into(), "object".into());
    results_items.insert(
        "properties".into(),
        serde_json::Value::Object(
            item_props
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::Object(v)))
                .collect(),
        ),
    );
    results_items.insert(
        "required".into(),
        serde_json::json!(["url", "content", "timing_ms"]),
    );

    let mut props = BTreeMap::new();
    let mut results_schema = serde_json::Map::new();
    results_schema.insert("type".into(), "array".into());
    results_schema.insert("items".into(), serde_json::Value::Object(results_items));
    props.insert("results".into(), results_schema);

    ToolOutputSchema::new(vec!["results".into()], Some(props), None)
}

/// Build the `outputSchema` for the `auth_lookup` tool.
///
/// Returns: `{ domain, username, has_totp }`
fn auth_lookup_output_schema() -> ToolOutputSchema {
    let mut props = BTreeMap::new();
    props.insert("domain".into(), string_prop("The queried domain"));
    props.insert("username".into(), string_prop("Account username if found"));
    props.insert(
        "has_totp".into(),
        bool_prop("Whether a TOTP credential is stored"),
    );
    ToolOutputSchema::new(vec!["domain".into(), "has_totp".into()], Some(props), None)
}

/// Build the `outputSchema` for the `fingerprint` tool.
///
/// Returns: `{ profiles: [{ user_agent, accept_language, sec_ch_ua }] }`
fn fingerprint_output_schema() -> ToolOutputSchema {
    let mut item_props = BTreeMap::new();
    item_props.insert(
        "user_agent".into(),
        string_prop("Browser User-Agent string"),
    );
    item_props.insert(
        "accept_language".into(),
        string_prop("Accept-Language header value"),
    );
    item_props.insert(
        "sec_ch_ua".into(),
        string_prop("Sec-CH-UA header value (empty for Firefox/Safari)"),
    );

    let mut profiles_items = serde_json::Map::new();
    profiles_items.insert("type".into(), "object".into());
    profiles_items.insert(
        "properties".into(),
        serde_json::Value::Object(
            item_props
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::Object(v)))
                .collect(),
        ),
    );
    profiles_items.insert(
        "required".into(),
        serde_json::json!(["user_agent", "accept_language", "sec_ch_ua"]),
    );

    let mut props = BTreeMap::new();
    let mut profiles_schema = serde_json::Map::new();
    profiles_schema.insert("type".into(), "array".into());
    profiles_schema.insert("items".into(), serde_json::Value::Object(profiles_items));
    props.insert("profiles".into(), profiles_schema);

    ToolOutputSchema::new(vec!["profiles".into()], Some(props), None)
}

/// Build the `outputSchema` for the `submit` tool.
///
/// Returns: `{ url, status, content }`
fn submit_output_schema() -> ToolOutputSchema {
    let mut props = BTreeMap::new();
    props.insert("url".into(), string_prop("The submitted URL"));
    props.insert("status".into(), integer_prop("HTTP status code"));
    props.insert(
        "content".into(),
        string_prop("Markdown-converted response body"),
    );
    ToolOutputSchema::new(
        vec!["url".into(), "status".into(), "content".into()],
        Some(props),
        None,
    )
}

/// Build the `outputSchema` for the `login` tool.
///
/// Returns: `{ url, final_url, status, content }`
fn login_output_schema() -> ToolOutputSchema {
    let mut props = BTreeMap::new();
    props.insert("url".into(), string_prop("The login URL"));
    props.insert("final_url".into(), string_prop("URL after login redirects"));
    props.insert(
        "status".into(),
        string_prop("Login result status (success/cancelled)"),
    );
    props.insert(
        "content".into(),
        string_prop("Markdown-converted page content after login"),
    );
    ToolOutputSchema::new(vec!["url".into(), "status".into()], Some(props), None)
}

/// Build the `outputSchema` for the `validate` tool.
///
/// Returns: `{ duration_s }`
fn validate_output_schema() -> ToolOutputSchema {
    let mut props = BTreeMap::new();
    props.insert(
        "duration_s".into(),
        number_prop("Total validation duration in seconds"),
    );
    ToolOutputSchema::new(vec!["duration_s".into()], Some(props), None)
}

/// Build the `outputSchema` for the `benchmark` tool.
///
/// Returns: `{ results: [{ url, min_ms, avg_ms, max_ms, iterations, errors }] }`
fn benchmark_output_schema() -> ToolOutputSchema {
    let mut item_props = BTreeMap::new();
    item_props.insert("url".into(), string_prop("Benchmarked URL"));
    item_props.insert(
        "min_ms".into(),
        number_prop("Minimum response time in milliseconds"),
    );
    item_props.insert(
        "avg_ms".into(),
        number_prop("Average response time in milliseconds"),
    );
    item_props.insert(
        "max_ms".into(),
        number_prop("Maximum response time in milliseconds"),
    );
    item_props.insert(
        "iterations".into(),
        integer_prop("Number of successful iterations measured"),
    );
    item_props.insert("errors".into(), integer_prop("Number of failed iterations"));

    let mut results_items = serde_json::Map::new();
    results_items.insert("type".into(), "object".into());
    results_items.insert(
        "properties".into(),
        serde_json::Value::Object(
            item_props
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::Object(v)))
                .collect(),
        ),
    );
    results_items.insert(
        "required".into(),
        serde_json::json!(["url", "min_ms", "avg_ms", "max_ms", "iterations", "errors"]),
    );

    let mut props = BTreeMap::new();
    let mut results_schema = serde_json::Map::new();
    results_schema.insert("type".into(), "array".into());
    results_schema.insert("items".into(), serde_json::Value::Object(results_items));
    props.insert("results".into(), results_schema);

    ToolOutputSchema::new(vec!["results".into()], Some(props), None)
}

// ─── Schema property helpers ──────────────────────────────────────────────────

/// Build a JSON Schema property with a type and description.
fn schema_prop(type_str: &str, description: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), type_str.into());
    m.insert("description".into(), description.into());
    m
}

fn string_prop(description: &str) -> serde_json::Map<String, serde_json::Value> {
    schema_prop("string", description)
}

fn number_prop(description: &str) -> serde_json::Map<String, serde_json::Value> {
    schema_prop("number", description)
}

fn integer_prop(description: &str) -> serde_json::Map<String, serde_json::Value> {
    schema_prop("integer", description)
}

fn bool_prop(description: &str) -> serde_json::Map<String, serde_json::Value> {
    schema_prop("boolean", description)
}

// ─── Tool Annotations ─────────────────────────────────────────────────────────

/// Return `ToolAnnotations` for the named tool.
///
/// Encoding per MCP 2025-11-25 spec:
/// - `read_only_hint`: tool makes no state changes
/// - `destructive_hint`: meaningful only when `read_only_hint == false`; true means
///   the side-effects may be destructive (e.g., form submission)
/// - `idempotent_hint`: meaningful only when `read_only_hint == false`; true means
///   calling again with the same args has no additional effect
fn tool_annotations(name: &str) -> ToolAnnotations {
    let (read_only, destructive, idempotent) = match name {
        "submit" => (false, true, false),
        "login" => (false, false, false),
        _ => (true, false, true), // fetch, fetch_batch, validate, fingerprint, auth_lookup, benchmark
    };
    ToolAnnotations {
        read_only_hint: Some(read_only),
        destructive_hint: Some(destructive),
        idempotent_hint: Some(idempotent),
        open_world_hint: None,
        title: None,
    }
}

// ─── Prompts ──────────────────────────────────────────────────────────────────

/// Build the static list of prompts this server advertises.
fn all_prompts() -> Vec<Prompt> {
    vec![
        Prompt {
            name: "fetch-and-extract".into(),
            title: Some("Fetch and Extract".into()),
            description: Some(
                "Fetch a URL and extract specific information from the page.".into(),
            ),
            arguments: vec![
                prompt_arg("url", "URL to fetch", true),
                prompt_arg("extract_query", "What to extract from the page", true),
            ],
            icons: vec![],
            meta: None,
        },
        Prompt {
            name: "multi-page-research".into(),
            title: Some("Multi-Page Research".into()),
            description: Some(
                "Fetch multiple URLs in parallel and synthesize the results to answer a question."
                    .into(),
            ),
            arguments: vec![
                prompt_arg("urls", "Comma-separated list of URLs to fetch", true),
                prompt_arg("question", "Question to answer from the fetched pages", true),
            ],
            icons: vec![],
            meta: None,
        },
        Prompt {
            name: "authenticated-fetch".into(),
            title: Some("Authenticated Fetch".into()),
            description: Some(
                "Fetch a URL that requires authentication via browser cookies or 1Password.".into(),
            ),
            arguments: vec![
                prompt_arg("url", "URL to fetch", true),
                prompt_arg(
                    "auth_method",
                    "Authentication method: 'cookies' (use saved browser cookies) or '1password'",
                    false,
                ),
            ],
            icons: vec![],
            meta: None,
        },
    ]
}

/// Convenience constructor for a `PromptArgument`.
fn prompt_arg(name: &str, description: &str, required: bool) -> PromptArgument {
    PromptArgument {
        name: name.into(),
        title: None,
        description: Some(description.into()),
        required: Some(required),
    }
}

/// Render a `GetPromptResult` for the named prompt given its arguments.
///
/// Returns `None` if `name` is not a known prompt.
fn build_prompt_result(
    name: &str,
    args: &std::collections::BTreeMap<String, String>,
) -> Option<GetPromptResult> {
    let text = match name {
        "fetch-and-extract" => {
            let url = args.get("url").map_or("<url>", String::as_str);
            let query = args
                .get("extract_query")
                .map_or("<what to extract>", String::as_str);
            format!(
                "Use the `fetch` tool to retrieve {url}.\n\
                 Then extract and return: {query}"
            )
        }
        "multi-page-research" => {
            let urls = args.get("urls").map_or("<urls>", String::as_str);
            let question = args
                .get("question")
                .map_or("<question>", String::as_str);
            format!(
                "Use `fetch_batch` to fetch these URLs in parallel: {urls}\n\
                 Then synthesize the results to answer: {question}"
            )
        }
        "authenticated-fetch" => {
            let url = args.get("url").map_or("<url>", String::as_str);
            let method = args
                .get("auth_method")
                .map_or("cookies", String::as_str);
            let flag = if method == "1password" {
                "--1password"
            } else {
                "--cookies brave"
            };
            format!(
                "Use the `fetch` tool with auth flag `{flag}` to retrieve {url}.\n\
                 This will use {method} authentication to access the protected page."
            )
        }
        _ => return None,
    };

    Some(GetPromptResult {
        description: None,
        meta: None,
        messages: vec![PromptMessage {
            role: rust_mcp_sdk::schema::Role::User,
            content: ContentBlock::TextContent(TextContent::new(text, None, None)),
        }],
    })
}

// ─── Resources ────────────────────────────────────────────────────────────────

/// Build the static list of resources this server exposes.
fn all_resources() -> Vec<Resource> {
    vec![
        Resource {
            uri: "nab://guide/quickstart".into(),
            name: "nab Quickstart Guide".into(),
            title: Some("nab Quickstart Guide".into()),
            description: Some(
                "How to use nab: fetch patterns, authentication, batch mode, and tips.".into(),
            ),
            mime_type: Some("text/markdown".into()),
            annotations: None,
            icons: vec![],
            meta: None,
            size: None,
        },
        Resource {
            uri: "nab://status".into(),
            name: "Server Status".into(),
            title: Some("nab Server Status".into()),
            description: Some("Live server health and capability summary.".into()),
            mime_type: Some("text/markdown".into()),
            annotations: None,
            icons: vec![],
            meta: None,
            size: None,
        },
    ]
}

/// Return the text content for a known resource URI, or `None` if unknown.
fn resource_content(uri: &str) -> Option<String> {
    match uri {
        "nab://guide/quickstart" => Some(QUICKSTART_GUIDE.to_string()),
        "nab://status" => Some(status_content()),
        _ => None,
    }
}

/// Quickstart guide content.
const QUICKSTART_GUIDE: &str = "\
# nab Quickstart Guide

nab is a token-optimized web fetcher for LLMs. It converts any URL to clean markdown.

## Basic Fetch

Use `fetch` for a single URL:
- Plain fetch: `url = \"https://example.com\"`
- With diff tracking: add `diff = true` to see what changed since last fetch

## Batch Fetch

Use `fetch_batch` for multiple URLs in parallel:
- Pass `urls = [\"https://a.com\", \"https://b.com\"]`
- Supports task-augmented execution for non-blocking long batches

## Authentication

### Browser cookies
Pass `cookies = \"brave\"` (or `\"chrome\"`, `\"firefox\"`) to use saved browser cookies.
Useful for sites where you are already logged in.

### 1Password
Pass `use_1password = true` to look up credentials from 1Password and auto-login.

### Interactive login
Use the `login` tool to open an interactive browser session and capture cookies.

## Form Submission

Use `submit` for POST/PUT/PATCH requests:
- `url`, `method`, `body`, optional `content_type`

## Tips

- nab auto-converts HTML, PDF, DOCX, XLSX to markdown
- SPA content is extracted from embedded JSON (no headless browser needed)
- Use `auth_lookup` to check if 1Password has credentials for a domain
- Use `validate` to warm up the connection and verify nab is working
";

/// Generate the live status resource content.
fn status_content() -> String {
    format!(
        "# nab Server Status\n\n\
         **Version**: {}\n\
         **Status**: running\n\
         **Tools**: fetch, fetch_batch, submit, login, auth_lookup, fingerprint, validate, benchmark\n\
         **Prompts**: fetch-and-extract, multi-page-research, authenticated-fetch\n\
         **Resources**: nab://guide/quickstart, nab://status\n",
        env!("CARGO_PKG_VERSION")
    )
}

// ─── Server Handler ───────────────────────────────────────────────────────────

/// MCP server handler for the `MicroFetch` tool suite.
///
/// Handles all standard MCP tool requests synchronously, and routes
/// `fetch_batch` through task-augmented execution when the client requests it,
/// enabling non-blocking parallel fetches for long-running batch operations.
pub struct MicroFetchHandler;

#[async_trait]
impl ServerHandler for MicroFetchHandler {
    async fn handle_list_tools_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        let mut tools = MicroFetchTools::tools();

        // Inject outputSchema, annotations, and task execution metadata after macro
        // generation. The #[mcp_tool] macro always emits these fields as None,
        // so we patch them here.
        for tool in &mut tools {
            tool.output_schema = match tool.name.as_str() {
                "fetch" => Some(fetch_output_schema()),
                "fetch_batch" => Some(fetch_batch_output_schema()),
                "submit" => Some(submit_output_schema()),
                "login" => Some(login_output_schema()),
                "auth_lookup" => Some(auth_lookup_output_schema()),
                "fingerprint" => Some(fingerprint_output_schema()),
                "validate" => Some(validate_output_schema()),
                "benchmark" => Some(benchmark_output_schema()),
                _ => None,
            };

            tool.annotations = Some(tool_annotations(tool.name.as_str()));

            // Advertise that fetch_batch supports optional task-augmented execution.
            // Clients that understand tasks can opt in; others get synchronous execution.
            if tool.name == "fetch_batch" {
                tool.execution = Some(ToolExecution {
                    task_support: Some(ToolExecutionTaskSupport::Optional),
                });
            }
        }

        Ok(ListToolsResult {
            meta: None,
            next_cursor: None,
            tools,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        let tool = MicroFetchTools::try_from(params)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        match tool {
            MicroFetchTools::FetchTool(t) => t.run().await,
            MicroFetchTools::FetchBatchTool(t) => t.run().await,
            MicroFetchTools::SubmitTool(t) => t.run().await,
            MicroFetchTools::LoginTool(t) => t.run(runtime).await,
            MicroFetchTools::AuthLookupTool(t) => t.run(),
            MicroFetchTools::FingerprintTool(t) => t.run(),
            MicroFetchTools::ValidateTool(t) => t.run().await,
            MicroFetchTools::BenchmarkTool(t) => t.run().await,
        }
    }

    async fn handle_list_prompts_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListPromptsResult, RpcError> {
        Ok(ListPromptsResult {
            meta: None,
            next_cursor: None,
            prompts: all_prompts(),
        })
    }

    async fn handle_get_prompt_request(
        &self,
        params: GetPromptRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<GetPromptResult, RpcError> {
        let name = params.name;
        let args = params.arguments.unwrap_or_default();
        build_prompt_result(&name, &args).ok_or_else(|| {
            RpcError::method_not_found()
                .with_message(format!("Unknown prompt: '{name}'"))
        })
    }

    async fn handle_list_resources_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListResourcesResult, RpcError> {
        Ok(ListResourcesResult {
            meta: None,
            next_cursor: None,
            resources: all_resources(),
        })
    }

    async fn handle_read_resource_request(
        &self,
        params: ReadResourceRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ReadResourceResult, RpcError> {
        use rust_mcp_sdk::schema::ReadResourceContent;
        let text = resource_content(&params.uri).ok_or_else(|| {
            RpcError::method_not_found()
                .with_message(format!("Unknown resource: '{}'", params.uri))
        })?;
        Ok(ReadResourceResult {
            meta: None,
            contents: vec![ReadResourceContent::TextResourceContents(
                TextResourceContents {
                    meta: None,
                    mime_type: Some("text/markdown".into()),
                    text,
                    uri: params.uri,
                },
            )],
        })
    }

    /// Handles task-augmented `fetch_batch` calls.
    ///
    /// When a client sends `tools/call` with `_meta: {taskMode: "async"}` for
    /// `fetch_batch`, this handler:
    /// 1. Creates a task immediately and returns `CreateTaskResult` to the client.
    /// 2. Spawns the actual batch fetch in the background.
    /// 3. Stores the result via `task_store.store_task_result()` when done.
    /// 4. The runtime automatically pushes a `notifications/task/status` notification.
    ///
    /// Only `fetch_batch` supports task-augmented execution; all other tools
    /// fall through to `handle_call_tool_request`.
    async fn handle_task_augmented_tool_call(
        &self,
        params: CallToolRequestParams,
        task_creator: ServerTaskCreator,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CreateTaskResult, CallToolError> {
        if params.name != "fetch_batch" {
            return Err(CallToolError::from_message(format!(
                "Tool '{}' does not support task-augmented execution",
                params.name
            )));
        }

        // Deserialize the batch tool from params before creating the task,
        // so we fail fast on bad input before spawning.
        let tool = MicroFetchTools::try_from(params)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        let MicroFetchTools::FetchBatchTool(batch_tool) = tool else {
            return Err(CallToolError::from_message("Expected fetch_batch tool"));
        };

        // Create the task synchronously — this returns the task metadata
        // (task_id, status=pending, timestamps) that the client polls.
        let task = task_creator
            .create_task(CreateTaskOptions {
                ttl: None,
                poll_interval: Some(1000), // suggest 1-second polling
                meta: None,
            })
            .await;

        let task_id = task.task_id.clone();
        let task_store = _runtime
            .task_store()
            .ok_or_else(|| CallToolError::from_message("Task store not configured"))?;

        // Spawn the actual fetch work in the background.
        // When complete, store_task_result() signals completion and the
        // runtime pushes a notifications/task/status event to the client.
        tokio::spawn(async move {
            let (status, call_result) = match batch_tool.run().await {
                Ok(r) => (TaskStatus::Completed, ResultFromServer::CallToolResult(r)),
                Err(e) => {
                    // Convert error to String before any await point to satisfy
                    // the Send bound — CallToolError contains dyn StdError.
                    let msg = e.to_string();
                    (
                        TaskStatus::Failed,
                        ResultFromServer::CallToolResult(CallToolError::from_message(msg).into()),
                    )
                }
            };
            task_store
                .store_task_result(&task_id, status, call_result, None)
                .await;
        });

        Ok(CreateTaskResult { task, meta: None })
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .init();

    let _ = get_client().await;

    // Wire the InMemoryTaskStore so that task-augmented fetch_batch calls work.
    // The runtime subscribes to the store's broadcast channel and sends
    // notifications/task/status to the client whenever a task changes state.
    let task_store: Arc<rust_mcp_sdk::task_store::ServerTaskStore> =
        Arc::new(InMemoryTaskStore::<ClientJsonrpcRequest, ResultFromServer>::new(None));

    let server_details = InitializeResult {
        server_info: Implementation {
            name: "nab".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: Some("MicroFetch Browser Engine".into()),
            description: Some(
                "Token-optimized web fetcher with HTTP/3, browser fingerprinting, \
                 and 1Password integration."
                    .into(),
            ),
            icons: server_icons(),
            website_url: None,
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            prompts: Some(ServerCapabilitiesPrompts { list_changed: None }),
            resources: Some(ServerCapabilitiesResources {
                list_changed: None,
                subscribe: None,
            }),
            // Advertise task support: clients can create tasks, cancel them,
            // list them, and use task-augmented tool calls.
            tasks: Some(ServerTasks {
                cancel: Some(serde_json::Map::new()),
                list: Some(serde_json::Map::new()),
                requests: Some(ServerTaskRequest {
                    tools: Some(ServerTaskTools {
                        call: Some(serde_json::Map::new()),
                    }),
                }),
            }),
            ..Default::default()
        },
        meta: None,
        instructions: Some(
            "nab provides ultra-fast web fetching with automatic content conversion \
             (HTML/PDF→markdown), SPA data extraction, form submission with CSRF handling, \
             auto-login via 1Password with interactive credential selection, \
             HTTP/3, and browser fingerprinting. \
             fetch_batch supports task-augmented execution for non-blocking parallel fetches. \
             Use prompts/list to discover guided workflows (fetch-and-extract, \
             multi-page-research, authenticated-fetch). \
             Use resources/list for the quickstart guide (nab://guide/quickstart) \
             and live server status (nab://status)."
                .into(),
        ),
        protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
    };

    let transport = StdioTransport::new(TransportOptions::default())?;
    let handler = MicroFetchHandler;
    let server = server_runtime::create_server(McpServerOptions {
        server_details,
        transport,
        handler: handler.to_mcp_server_handler(),
        task_store: Some(task_store),
        client_task_store: None,
        message_observer: None,
    });

    Ok(server.start().await?)
}
