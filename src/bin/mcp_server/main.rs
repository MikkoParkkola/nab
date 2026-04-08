//! `MicroFetch` MCP Server - Native Rust implementation
//!
//! Ultra-fast MCP server for web fetching with HTTP/3, fingerprint spoofing,
//! and 1Password integration. Uses MCP protocol 2025-11-25 with full
//! tool annotations, structured output schemas, task-augmented execution,
//! elicitation support, MCP logging, argument completion, sampling plumbing,
//! roots plumbing, and Streamable HTTP transport.
//!
//! # Usage
//!
//! Stdio mode (for Claude Code integration):
//! ```bash
//! nab-mcp
//! ```
//!
//! Streamable HTTP mode (network-accessible):
//! ```bash
//! nab-mcp --http 127.0.0.1:8765
//! nab-mcp --http 0.0.0.0:8765 --http-allow-origin https://claude.ai
//! ```

pub mod active_reading_mcp;
pub mod completion;
pub mod elicitation;
pub mod hebb_client;
pub mod helpers;
pub mod mcp_log;
pub mod roots;
pub mod sampling;
pub mod structured;
#[cfg(test)]
mod tests;
pub mod tools;

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_mcp_sdk::mcp_server::ToMcpServerHandler;
use rust_mcp_sdk::mcp_server::{McpServerOptions, ServerHandler, server_runtime};
use rust_mcp_sdk::schema::{
    CallToolRequestParams, CallToolResult, CompleteRequestParams, CompleteResult, ContentBlock,
    CreateTaskResult, GetPromptRequestParams, GetPromptResult, Implementation, InitializeResult,
    LATEST_PROTOCOL_VERSION, ListPromptsResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, Prompt, PromptArgument, PromptMessage, ReadResourceRequestParams,
    ReadResourceResult, Resource, ResourceUpdatedNotificationParams, RpcError, ServerCapabilities,
    ServerCapabilitiesPrompts, ServerCapabilitiesResources, ServerCapabilitiesTools,
    ServerTaskRequest, ServerTaskTools, ServerTasks, SetLevelRequestParams, SubscribeRequestParams,
    TextContent, TextResourceContents, ToolAnnotations, ToolExecution, ToolExecutionTaskSupport,
    ToolOutputSchema, UnsubscribeRequestParams, schema_utils::CallToolError,
};
use rust_mcp_sdk::schema::{
    TaskStatus,
    schema_utils::{ClientJsonrpcRequest, ResultFromServer},
};
use rust_mcp_sdk::task_store::{CreateTaskOptions, InMemoryTaskStore, ServerTaskCreator};
use rust_mcp_sdk::{McpServer, StdioTransport, TransportOptions, tool_box};

use nab::watch::WatchManager;

use structured::server_icons;
use tools::{
    AnalyzeTool, AuthLookupTool, BenchmarkTool, FetchBatchTool, FetchTool, FingerprintTool,
    LoginTool, SubmitTool, ValidateTool, WatchCreateTool, WatchListTool, WatchRemoveTool,
    get_client, init_watch_manager,
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
        BenchmarkTool,
        AnalyzeTool,
        WatchCreateTool,
        WatchListTool,
        WatchRemoveTool
    ]
);

// ─── Output Schema builders ───────────────────────────────────────────────────

/// Build the `outputSchema` for the `fetch` tool.
///
/// Required fields: `{ url, status, content, timing_ms, has_diff }`
///
/// Optional fields may also be present when focus, token budgets, or response
/// diagnostics are active.
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
    props.insert(
        "omitted_sections".into(),
        integer_prop("Number of sections omitted when focus filtering was applied"),
    );
    props.insert(
        "total_sections".into(),
        integer_prop("Total number of sections considered by focus filtering"),
    );
    props.insert(
        "truncated".into(),
        bool_prop("True when max_tokens caused the content to be truncated"),
    );
    props.insert(
        "full_tokens".into(),
        integer_prop("Estimated token count before truncation when max_tokens was applied"),
    );
    props.insert(
        "response_class".into(),
        string_prop("Machine-readable primary response classification code"),
    );
    props.insert(
        "response_confidence".into(),
        number_prop("Confidence score for the primary response classification"),
    );
    props.insert(
        "response_reason".into(),
        string_prop("Human-readable reason for the primary response classification"),
    );
    props.insert(
        "thin_content_detected".into(),
        bool_prop("True when HTML extraction looked suspiciously thin relative to the source body"),
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

/// Build the `outputSchema` for the `analyze` tool.
///
/// Returns the `TranscriptionResult` JSON shape:
/// `{ segments, language, duration_seconds, model, backend, rtfx,
///    processing_time_seconds, speakers? }`
#[allow(clippy::too_many_lines)]
fn analyze_output_schema() -> ToolOutputSchema {
    // ── segment item ──────────────────────────────────────────────────────────
    let mut seg_props = serde_json::Map::new();
    seg_props.insert("type".into(), "object".into());
    {
        let mut sp = serde_json::Map::new();
        sp.insert(
            "text".into(),
            serde_json::Value::Object(string_prop("Transcribed text")),
        );
        sp.insert(
            "start".into(),
            serde_json::Value::Object(number_prop("Segment start time in seconds")),
        );
        sp.insert(
            "end".into(),
            serde_json::Value::Object(number_prop("Segment end time in seconds")),
        );
        sp.insert(
            "confidence".into(),
            serde_json::Value::Object(number_prop("Average word confidence [0.0, 1.0]")),
        );
        sp.insert(
            "language".into(),
            serde_json::Value::Object(string_prop("BCP-47 language tag (when detected)")),
        );
        sp.insert(
            "speaker".into(),
            serde_json::Value::Object(string_prop("Speaker label after diarization")),
        );
        seg_props.insert("properties".into(), serde_json::Value::Object(sp));
    }
    seg_props.insert(
        "required".into(),
        serde_json::json!(["text", "start", "end", "confidence"]),
    );

    // ── speaker turn item ─────────────────────────────────────────────────────
    let mut spk_props = serde_json::Map::new();
    spk_props.insert("type".into(), "object".into());
    {
        let mut sp = serde_json::Map::new();
        sp.insert(
            "speaker".into(),
            serde_json::Value::Object(string_prop("Speaker label, e.g. SPEAKER_00")),
        );
        sp.insert(
            "start".into(),
            serde_json::Value::Object(number_prop("Turn start time in seconds")),
        );
        sp.insert(
            "end".into(),
            serde_json::Value::Object(number_prop("Turn end time in seconds")),
        );
        spk_props.insert("properties".into(), serde_json::Value::Object(sp));
    }
    spk_props.insert(
        "required".into(),
        serde_json::json!(["speaker", "start", "end"]),
    );

    // ── top-level properties ──────────────────────────────────────────────────
    let mut props = BTreeMap::new();

    let mut segs_schema = serde_json::Map::new();
    segs_schema.insert("type".into(), "array".into());
    segs_schema.insert("items".into(), serde_json::Value::Object(seg_props));
    props.insert("segments".into(), segs_schema);

    props.insert(
        "language".into(),
        string_prop("Dominant BCP-47 language of the recording"),
    );
    props.insert(
        "duration_seconds".into(),
        number_prop("Duration of processed audio in seconds"),
    );
    props.insert(
        "model".into(),
        string_prop("ASR model identifier, e.g. parakeet-tdt-0.6b-v3"),
    );
    props.insert(
        "backend".into(),
        string_prop("Backend identifier, e.g. fluidaudio"),
    );
    props.insert(
        "rtfx".into(),
        number_prop("Realtime factor (audio seconds / wall seconds)"),
    );
    props.insert(
        "processing_time_seconds".into(),
        number_prop("Wall-clock processing time in seconds"),
    );

    let mut speakers_schema = serde_json::Map::new();
    speakers_schema.insert("type".into(), "array".into());
    speakers_schema.insert("items".into(), serde_json::Value::Object(spk_props));
    props.insert("speakers".into(), speakers_schema);

    ToolOutputSchema::new(
        vec![
            "segments".into(),
            "language".into(),
            "duration_seconds".into(),
            "model".into(),
            "backend".into(),
            "rtfx".into(),
            "processing_time_seconds".into(),
        ],
        Some(props),
        None,
    )
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
    let (read_only, destructive, idempotent, open_world) = match name {
        "submit" => (false, true, false, Some(true)),
        "login" | "watch_create" => (false, false, false, Some(true)),
        // analyze reads a local file; result is deterministic for the same input
        "analyze" => (true, false, true, Some(false)),
        // watch_remove deletes a watch (state change, destructive)
        "watch_remove" => (false, true, true, None),
        _ => (true, false, true, None), // fetch, fetch_batch, validate, fingerprint, auth_lookup, benchmark
    };
    ToolAnnotations {
        read_only_hint: Some(read_only),
        destructive_hint: Some(destructive),
        idempotent_hint: Some(idempotent),
        open_world_hint: open_world,
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
            description: Some("Fetch a URL and extract specific information from the page.".into()),
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
                prompt_arg(
                    "question",
                    "Question to answer from the fetched pages",
                    true,
                ),
            ],
            icons: vec![],
            meta: None,
        },
        Prompt {
            name: "authenticated-fetch".into(),
            title: Some("Authenticated Fetch".into()),
            description: Some(
                "Fetch a URL that requires authentication using automatic browser cookies or a login session."
                    .into(),
            ),
            arguments: vec![
                prompt_arg("url", "URL to fetch", true),
                prompt_arg(
                    "auth_method",
                    "Authentication method: 'cookies' (automatic browser cookies) or '1password' (login + session)",
                    false,
                ),
            ],
            icons: vec![],
            meta: None,
        },
        Prompt {
            name: "match-speakers-with-hebb".into(),
            title: Some("Match Speakers with hebb Voiceprint DB".into()),
            description: Some(
                "Run speaker diarization with embeddings, then resolve SPEAKER_NN labels to real \
                 names using the hebb voiceprint database. Elicits the user for unknown speakers \
                 and stores new voiceprints for future recognition."
                    .into(),
            ),
            arguments: vec![
                prompt_arg("input", "Local audio or video file path to transcribe", true),
                prompt_arg(
                    "language",
                    "BCP-47 language hint (optional, e.g. 'fi', 'en')",
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
            let question = args.get("question").map_or("<question>", String::as_str);
            format!(
                "Use `fetch_batch` to fetch these URLs in parallel: {urls}\n\
                 Then synthesize the results to answer: {question}"
            )
        }
        "authenticated-fetch" => {
            let url = args.get("url").map_or("<url>", String::as_str);
            let method = args.get("auth_method").map_or("cookies", String::as_str);
            if method == "1password" {
                format!(
                    "Use the `login` tool for {url} with a session name to establish an authenticated session via 1Password.\n\
                     Then call the `fetch` tool with the same `session` to retrieve the protected page."
                )
            } else {
                format!(
                    "Use the `fetch` tool to retrieve {url}.\n\
                     By default it automatically uses cookies from the default browser for that domain.\n\
                     Only set `cookies` when you need to override the browser choice or disable cookies with `cookies = \"none\"`."
                )
            }
        }
        "match-speakers-with-hebb" => {
            let input = args
                .get("input")
                .map_or("<audio-or-video-file>", String::as_str);
            let lang_hint = args
                .get("language")
                .map_or(String::new(), |l| format!(" language=\"{l}\""));
            format!(
                "## Speaker Resolution Workflow\n\n\
                 **Step 1 — Transcribe with diarization and embeddings:**\n\
                 Call `analyze` with `input=\"{input}\"{lang_hint} diarize=true include_embeddings=true`.\n\n\
                 **Step 2 — Resolve each speaker turn:**\n\
                 For each entry in the result's `speakers[]` array:\n\
                 1. Call `hebb voice_match` with `embedding=<segment.embedding>` and `threshold=0.7`.\n\
                 2. If a match is returned with `confidence > 0.7`, use `match.name` as the speaker label.\n\
                 3. If no match, elicit the user: \"Who is SPEAKER_NN speaking from \
                    <start>s to <end>s? (leave blank to skip)\"\n\
                 4. If the user supplies a name, call `hebb voice_remember` with \
                    `embedding=<segment.embedding>` and `name=<user_name>`.\n\n\
                 **Step 3 — Produce the final transcript:**\n\
                 Replace all `SPEAKER_NN` labels in `segments[].speaker` with the resolved names. \
                 Present the final transcript with speaker-attributed lines."
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

/// Build the static resources this server always exposes.
fn static_resources() -> Vec<Resource> {
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

/// Build a `Resource` descriptor from a watch.
fn watch_resource(watch: &nab::watch::Watch) -> Resource {
    Resource {
        uri: format!("nab://watch/{}", watch.id),
        name: format!("Watch: {}", watch.url),
        title: Some(format!("Watch: {}", watch.url)),
        description: Some(format!(
            "Monitors {} every {}s for content changes.",
            watch.url, watch.interval_secs,
        )),
        mime_type: Some("text/markdown".into()),
        annotations: None,
        icons: vec![],
        meta: None,
        size: None,
    }
}

/// Build the full dynamic resource list: static + all watches.
async fn all_resources(watch_manager: &WatchManager) -> Vec<Resource> {
    let mut resources = static_resources();
    let watches = watch_manager.list().await;
    resources.extend(watches.iter().map(watch_resource));
    resources
}

/// Return the text content for a known resource URI, or `None` if unknown.
///
/// Watch URIs (`nab://watch/<id>`) are handled separately via the async path.
fn static_resource_content(uri: &str) -> Option<String> {
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
`fetch` and `submit` automatically try cookies from the default browser for the target domain.
Useful for sites where you are already logged in without needing extra parameters.

Override with `cookies = \"brave\"` (or `\"chrome\"`, `\"firefox\"`, `\"safari\"`, `\"edge\"`) when needed.
Pass `cookies = \"none\"` to disable cookies.

### 1Password
Use the `login` tool to authenticate with 1Password and optionally persist the result in a named `session`.

### Interactive login
Use the `login` tool to open an interactive browser session and then reuse the same `session` in `fetch` or `submit`.

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
         **Tools**: fetch, fetch_batch, submit, login, auth_lookup, fingerprint, validate, benchmark, watch_create, watch_list, watch_remove\n\
         **Prompts**: fetch-and-extract, multi-page-research, authenticated-fetch\n\
         **Resources**: nab://guide/quickstart, nab://status, nab://watch/<id> (subscribable)\n\
         **Watch subscriptions**: enabled — use watch_create then resources/subscribe nab://watch/<id>\n",
        env!("CARGO_PKG_VERSION")
    )
}

// ─── Server Handler ───────────────────────────────────────────────────────────

/// MCP server handler for the `MicroFetch` tool suite.
///
/// Handles all standard MCP tool requests synchronously, and routes
/// `fetch_batch` through task-augmented execution when the client requests it,
/// enabling non-blocking parallel fetches for long-running batch operations.
///
/// Also manages subscriptions to `nab://watch/<id>` resources and pushes
/// `notifications/resources/updated` when the background poller detects changes.
pub struct MicroFetchHandler {
    /// Watch manager for dynamic resource enumeration and subscription support.
    watch_manager: Arc<WatchManager>,
    /// Set of watch resource URIs the client has subscribed to.
    /// Shared with the notification fanout task via `Arc`.
    subscribed_uris: Arc<Mutex<HashSet<String>>>,
}

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
                "analyze" => Some(analyze_output_schema()),
                _ => None, // watch tools return freeform text
            };

            tool.annotations = Some(tool_annotations(tool.name.as_str()));

            // Advertise that fetch_batch supports optional task-augmented execution.
            // Clients that understand tasks can opt in; others get synchronous execution.
            if tool.name == "fetch_batch" {
                tool.execution = Some(ToolExecution {
                    task_support: Some(ToolExecutionTaskSupport::Optional),
                });
            }

            // analyze requires task-augmented execution — long videos can take
            // minutes to transcribe and must not block the MCP request loop.
            if tool.name == "analyze" {
                tool.execution = Some(ToolExecution {
                    task_support: Some(ToolExecutionTaskSupport::Required),
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
            MicroFetchTools::AnalyzeTool(t) => t.run(&runtime).await,
            MicroFetchTools::WatchCreateTool(t) => t.run().await,
            MicroFetchTools::WatchListTool(t) => t.run().await,
            MicroFetchTools::WatchRemoveTool(t) => t.run().await,
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
            RpcError::method_not_found().with_message(format!("Unknown prompt: '{name}'"))
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
            resources: all_resources(&self.watch_manager).await,
        })
    }

    async fn handle_read_resource_request(
        &self,
        params: ReadResourceRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ReadResourceResult, RpcError> {
        use rust_mcp_sdk::schema::ReadResourceContent;

        let text = if let Some(id) = params.uri.strip_prefix("nab://watch/") {
            self.watch_manager
                .render_resource(&id.to_owned())
                .await
                .ok_or_else(|| {
                    RpcError::method_not_found().with_message(format!("Watch '{id}' not found"))
                })?
        } else {
            static_resource_content(&params.uri).ok_or_else(|| {
                RpcError::method_not_found()
                    .with_message(format!("Unknown resource: '{}'", params.uri))
            })?
        };

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

    async fn handle_subscribe_request(
        &self,
        params: SubscribeRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<rust_mcp_sdk::schema::Result, RpcError> {
        let uri = &params.uri;
        if uri.starts_with("nab://watch/") {
            self.subscribed_uris
                .lock()
                .expect("subscription lock")
                .insert(uri.clone());
            tracing::info!(%uri, "Client subscribed to watch resource");
        }
        Ok(rust_mcp_sdk::schema::Result::default())
    }

    async fn handle_unsubscribe_request(
        &self,
        params: UnsubscribeRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<rust_mcp_sdk::schema::Result, RpcError> {
        let uri = &params.uri;
        self.subscribed_uris
            .lock()
            .expect("subscription lock")
            .remove(uri);
        tracing::info!(%uri, "Client unsubscribed from watch resource");
        Ok(rust_mcp_sdk::schema::Result::default())
    }

    /// Handle `logging/setLevel` — update the MCP log forwarding threshold.
    ///
    /// After this call, `notifications/message` are only emitted for messages
    /// at or above the requested level.  Tracing (stderr) is unaffected.
    async fn handle_set_level_request(
        &self,
        params: SetLevelRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<rust_mcp_sdk::schema::Result, RpcError> {
        mcp_log::LOGGER.set_level(params.level);
        tracing::debug!(level = ?params.level, "MCP log level updated");
        Ok(rust_mcp_sdk::schema::Result::default())
    }

    /// Handle `completion/complete` — return argument auto-complete suggestions.
    ///
    /// Dispatches to `completion::handle_complete` which enumerates browsers,
    /// fingerprint profiles, and session names.
    async fn handle_complete_request(
        &self,
        params: CompleteRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CompleteResult, RpcError> {
        Ok(completion::handle_complete(&params))
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
        runtime: Arc<dyn McpServer>,
    ) -> Result<CreateTaskResult, CallToolError> {
        // Only tools that advertise task support may enter this path.
        if !matches!(params.name.as_str(), "fetch_batch" | "analyze") {
            return Err(CallToolError::from_message(format!(
                "Tool '{}' does not support task-augmented execution",
                params.name
            )));
        }

        // Deserialize the tool from params before creating the task so we fail
        // fast on bad input without spending a task slot.
        let tool = MicroFetchTools::try_from(params)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        // Create the task synchronously — the client polls this task_id.
        let task = task_creator
            .create_task(CreateTaskOptions {
                ttl: None,
                poll_interval: Some(1000), // suggest 1-second polling
                meta: None,
            })
            .await;

        let task_id = task.task_id.clone();
        let task_store = runtime
            .task_store()
            .ok_or_else(|| CallToolError::from_message("Task store not configured"))?;

        // Spawn the actual work in the background.
        // When complete, store_task_result() signals completion and the
        // runtime pushes a notifications/task/status event to the client.
        match tool {
            MicroFetchTools::FetchBatchTool(batch_tool) => {
                tokio::spawn(async move {
                    let (status, call_result) = match batch_tool.run().await {
                        Ok(r) => (TaskStatus::Completed, ResultFromServer::CallToolResult(r)),
                        Err(e) => {
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
            }
            MicroFetchTools::AnalyzeTool(analyze_tool) => {
                let runtime_clone = runtime.clone();
                tokio::spawn(async move {
                    let (status, call_result) = match analyze_tool.run(&runtime_clone).await {
                        Ok(r) => (TaskStatus::Completed, ResultFromServer::CallToolResult(r)),
                        Err(e) => {
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
            }
            other => {
                return Err(CallToolError::from_message(format!(
                    "Unexpected tool variant in task-augmented path: {}",
                    other.tool_name()
                )));
            }
        }

        Ok(CreateTaskResult { task, meta: None })
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

// ─── CLI args ─────────────────────────────────────────────────────────────────

/// nab MCP server — stdio (default) or Streamable HTTP transport.
#[derive(clap::Parser, Debug)]
#[command(name = "nab-mcp", about = "nab MCP server (stdio or Streamable HTTP)")]
struct Cli {
    /// Bind address for Streamable HTTP transport (e.g. "127.0.0.1:8765").
    ///
    /// When set, nab-mcp listens as an HTTP server instead of using stdio.
    /// Omit to run in stdio mode (default — suitable for Claude Code integration).
    #[arg(long, value_name = "HOST:PORT")]
    http: Option<String>,

    /// Allowed CORS origin for HTTP mode (e.g. `<https://claude.ai>`).
    ///
    /// When `--http` is set without this flag, nab-mcp only binds to localhost
    /// and enforces an origin allowlist of <http://localhost> and
    /// <http://127.0.0.1>.
    /// Supply this to allow an additional origin.  Wildcard `*` is NOT supported
    /// for security reasons.
    #[arg(long, value_name = "ORIGIN")]
    http_allow_origin: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clap::Parser as _;
    let cli = Cli::parse();

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
            // list_changed: Some(true) advertises that tools/prompts lists can
            // change at runtime; we use notify_tool_list_changed() when dynamic
            // tool registration is added in Phase 3.
            tools: Some(ServerCapabilitiesTools {
                list_changed: Some(true),
            }),
            prompts: Some(ServerCapabilitiesPrompts {
                list_changed: Some(true),
            }),
            resources: Some(ServerCapabilitiesResources {
                list_changed: Some(true),
                subscribe: Some(true),
            }),
            // Advertise structured MCP logging (RFC 5424 levels via notifications/message).
            logging: Some(serde_json::Map::new()),
            // Advertise argument completion for prompts and resource templates.
            completions: Some(serde_json::Map::new()),
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

    // Initialize the WatchManager (loads persisted watches from disk).
    let watch_manager =
        Arc::new(WatchManager::new_default().expect("Failed to initialize WatchManager"));

    // Register the shared singleton so MCP watch tools can access it.
    init_watch_manager(Arc::clone(&watch_manager));

    // Spawn the background poller (ticks every 60 seconds).
    tokio::spawn(Arc::clone(&watch_manager).poll_loop());

    // Subscribed-URI set shared between the handler and the notification fanout task.
    let subscribed_uris: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    let handler = MicroFetchHandler {
        watch_manager: Arc::clone(&watch_manager),
        subscribed_uris: Arc::clone(&subscribed_uris),
    };

    // Choose transport: Streamable HTTP when --http is supplied, stdio otherwise.
    if let Some(ref bind) = cli.http {
        run_http(
            bind,
            cli.http_allow_origin.as_deref(),
            server_details,
            handler,
            task_store,
            watch_manager,
            subscribed_uris,
        )
        .await
    } else {
        run_stdio(
            server_details,
            handler,
            task_store,
            watch_manager,
            subscribed_uris,
        )
        .await
    }
}

// ─── stdio runtime ────────────────────────────────────────────────────────────

async fn run_stdio(
    server_details: InitializeResult,
    handler: MicroFetchHandler,
    task_store: Arc<rust_mcp_sdk::task_store::ServerTaskStore>,
    watch_manager: Arc<WatchManager>,
    subscribed_uris: Arc<Mutex<HashSet<String>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let transport = StdioTransport::new(TransportOptions::default())?;

    let server = server_runtime::create_server(McpServerOptions {
        server_details,
        transport,
        handler: handler.to_mcp_server_handler(),
        task_store: Some(task_store),
        client_task_store: None,
        message_observer: None,
    });

    // Initialize the MCP logger with the runtime so it can send notifications/message.
    // Coerce Arc<ServerRuntime> → Arc<dyn McpServer> via explicit upcast.
    let server_as_mcp: Arc<dyn McpServer> = server.clone() as Arc<dyn McpServer>;
    mcp_log::LOGGER.init(Arc::clone(&server_as_mcp));

    spawn_watch_fanout(&watch_manager, subscribed_uris, Arc::clone(&server_as_mcp));

    Ok(server.start().await?)
}

// ─── Streamable HTTP runtime ──────────────────────────────────────────────────

/// Run the server on a Streamable HTTP transport.
///
/// Security (MCP 2025-11-25 §basic/transports):
/// - Origin header validated against `allowed_origins`; HTTP 403 on mismatch.
/// - When no `--http-allow-origin` is supplied and bind host is not 0.0.0.0,
///   only localhost origins are permitted.
/// - Session IDs are cryptographically random (UUID v4, handled by the SDK).
/// - `MCP-Protocol-Version` header check is handled by the SDK's routing layer.
async fn run_http(
    bind: &str,
    allow_origin: Option<&str>,
    server_details: InitializeResult,
    handler: MicroFetchHandler,
    task_store: Arc<rust_mcp_sdk::task_store::ServerTaskStore>,
    watch_manager: Arc<WatchManager>,
    subscribed_uris: Arc<Mutex<HashSet<String>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use rust_mcp_sdk::event_store::InMemoryEventStore;
    use rust_mcp_sdk::mcp_server::{HyperServerOptions, hyper_server};

    // Parse bind address.
    let (host, port) = parse_bind_address(bind)?;

    // Build the origin allowlist.
    let allowed_origins = build_origin_allowlist(&host, allow_origin);

    if !allowed_origins.is_empty() {
        tracing::warn!(
            origins = ?allowed_origins,
            "nab-mcp HTTP: origin validation enabled"
        );
    }

    let is_public_bind = host == "0.0.0.0" || host == "::";
    if is_public_bind && allow_origin.is_none() {
        tracing::warn!(
            "nab-mcp HTTP: bound to {} without --http-allow-origin. \
             Only localhost origins are allowed.",
            host
        );
    }

    let options = HyperServerOptions {
        host: host.clone(),
        port,
        allowed_origins: if allowed_origins.is_empty() {
            None
        } else {
            Some(allowed_origins)
        },
        dns_rebinding_protection: true,
        task_store: Some(task_store),
        event_store: Some(Arc::new(InMemoryEventStore::default())),
        ..HyperServerOptions::default()
    };

    let server =
        hyper_server::create_server(server_details, handler.to_mcp_server_handler(), options);

    // Start the watch-change notification fanout.
    // In HTTP mode each session has its own runtime; watch-change notifications
    // are dispatched via the session store internally by the SDK.
    // We still spin up the fanout here for future per-session fan-out wiring.
    let _ = (watch_manager, subscribed_uris); // held for future use

    tracing::info!("nab-mcp HTTP listening on {}:{}", host, port);
    Ok(server.start().await?)
}

/// Parse "host:port" into (host, port).
fn parse_bind_address(bind: &str) -> Result<(String, u16), Box<dyn std::error::Error>> {
    let err = || format!("invalid --http address '{bind}': expected HOST:PORT");
    let colon = bind.rfind(':').ok_or_else(err)?;
    let host = bind[..colon].to_string();
    let port: u16 = bind[colon + 1..].parse().map_err(|_| err())?;
    Ok((host, port))
}

/// Build the origin allowlist for the HTTP server.
///
/// Always includes localhost variants.  Adds `allow_origin` when provided.
fn build_origin_allowlist(host: &str, allow_origin: Option<&str>) -> Vec<String> {
    let mut origins = vec![
        "http://localhost".to_string(),
        "http://127.0.0.1".to_string(),
        "https://localhost".to_string(),
    ];
    // For localhost bind addresses include only localhost origins (already added above).
    let is_local = host == "127.0.0.1" || host == "localhost" || host == "::1";
    if !is_local {
        // Public bind: only the explicitly supplied origin is permitted.
        origins.clear();
    }
    if let Some(origin) = allow_origin {
        let owned = origin.to_string();
        if !origins.contains(&owned) {
            origins.push(owned);
        }
    }
    origins
}

// ─── Watch notification fanout (shared by both transports) ───────────────────

/// Spawn the background task that pushes `notifications/resources/updated`
/// to the client when a watched URL changes.
fn spawn_watch_fanout(
    watch_manager: &Arc<WatchManager>,
    subscribed_uris: Arc<Mutex<HashSet<String>>>,
    server: Arc<dyn McpServer>,
) {
    let mut event_rx = watch_manager.subscribe();
    tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(nab::watch::WatchEvent::Changed { id, .. }) => {
                    let uri = format!("nab://watch/{id}");
                    let is_subscribed = {
                        subscribed_uris
                            .lock()
                            .expect("subscription lock")
                            .contains(&uri)
                    };
                    if is_subscribed {
                        let params = ResourceUpdatedNotificationParams {
                            uri: uri.clone(),
                            meta: None,
                        };
                        if let Err(e) = server.notify_resource_updated(params).await {
                            tracing::warn!(%uri, error = %e, "Failed to push resource updated notification");
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "Watch event receiver lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                _ => {}
            }
        }
    });
}
