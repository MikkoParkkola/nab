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
pub mod tools;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::mcp_server::{McpServerOptions, ServerHandler, server_runtime};
use rust_mcp_sdk::schema::{
    CallToolRequestParams, CallToolResult, CreateTaskResult, Implementation, InitializeResult,
    LATEST_PROTOCOL_VERSION, ListToolsResult, PaginatedRequestParams, RpcError, ServerCapabilities,
    ServerCapabilitiesTools, ServerTaskRequest, ServerTaskTools, ServerTasks, ToolExecution,
    ToolExecutionTaskSupport, ToolOutputSchema, schema_utils::CallToolError,
};
use rust_mcp_sdk::mcp_server::ToMcpServerHandler;
use rust_mcp_sdk::schema::{TaskStatus, schema_utils::{ClientJsonrpcRequest, ResultFromServer}};
use rust_mcp_sdk::task_store::{CreateTaskOptions, InMemoryTaskStore, ServerTaskCreator};
use rust_mcp_sdk::{McpServer, StdioTransport, TransportOptions, tool_box};

use tools::{
    AuthLookupTool, BenchmarkTool, FetchBatchTool, FetchTool, FingerprintTool, LoginTool,
    SubmitTool, ValidateTool, get_client,
};
use structured::server_icons;

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
    let mut props = HashMap::new();
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
    ToolOutputSchema::new(
        vec![
            "url".into(),
            "status".into(),
            "content".into(),
            "timing_ms".into(),
        ],
        Some(props),
        None,
    )
}

/// Build the `outputSchema` for the `fetch_batch` tool.
///
/// Returns: `{ results: [{ url, status, content, timing_ms }] }`
fn fetch_batch_output_schema() -> ToolOutputSchema {
    let mut item_props = HashMap::new();
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

    let mut props = HashMap::new();
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
    let mut props = HashMap::new();
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
    let mut item_props = HashMap::new();
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

    let mut props = HashMap::new();
    let mut profiles_schema = serde_json::Map::new();
    profiles_schema.insert("type".into(), "array".into());
    profiles_schema.insert("items".into(), serde_json::Value::Object(profiles_items));
    props.insert("profiles".into(), profiles_schema);

    ToolOutputSchema::new(vec!["profiles".into()], Some(props), None)
}

/// Build the `outputSchema` for the `benchmark` tool.
///
/// Returns: `{ results: [{ url, min_ms, avg_ms, max_ms, iterations }] }`
fn benchmark_output_schema() -> ToolOutputSchema {
    let mut item_props = HashMap::new();
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
        serde_json::json!(["url", "min_ms", "avg_ms", "max_ms", "iterations"]),
    );

    let mut props = HashMap::new();
    let mut results_schema = serde_json::Map::new();
    results_schema.insert("type".into(), "array".into());
    results_schema.insert("items".into(), serde_json::Value::Object(results_items));
    props.insert("results".into(), results_schema);

    ToolOutputSchema::new(vec!["results".into()], Some(props), None)
}

// ─── Schema property helpers ──────────────────────────────────────────────────

fn string_prop(description: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), "string".into());
    m.insert("description".into(), description.into());
    m
}

fn number_prop(description: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), "number".into());
    m.insert("description".into(), description.into());
    m
}

fn integer_prop(description: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), "integer".into());
    m.insert("description".into(), description.into());
    m
}

fn bool_prop(description: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), "boolean".into());
    m.insert("description".into(), description.into());
    m
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

        // Inject outputSchema and task execution metadata after macro generation.
        // The #[mcp_tool] macro always emits `output_schema: None` and
        // `execution: None`, so we patch both fields here.
        for tool in &mut tools {
            tool.output_schema = match tool.name.as_str() {
                "fetch" => Some(fetch_output_schema()),
                "fetch_batch" => Some(fetch_batch_output_schema()),
                "auth_lookup" => Some(auth_lookup_output_schema()),
                "fingerprint" => Some(fingerprint_output_schema()),
                "benchmark" => Some(benchmark_output_schema()),
                _ => None,
            };

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
                        ResultFromServer::CallToolResult(
                            CallToolError::from_message(msg).into(),
                        ),
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
    let task_store: Arc<rust_mcp_sdk::task_store::ServerTaskStore> = Arc::new(
        InMemoryTaskStore::<ClientJsonrpcRequest, ResultFromServer>::new(None),
    );

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
             fetch_batch supports task-augmented execution for non-blocking parallel fetches."
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
    });

    Ok(server.start().await?)
}
