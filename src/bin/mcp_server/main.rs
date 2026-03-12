//! `MicroFetch` MCP Server - Native Rust implementation
//!
//! Ultra-fast MCP server for web fetching with HTTP/3, fingerprint spoofing,
//! and 1Password integration. Uses latest MCP protocol (2025-06-18).
//!
//! # Usage
//!
//! Stdio mode (for Claude Code integration):
//! ```bash
//! nab-mcp
//! ```

pub mod tools;

use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::mcp_server::{ServerHandler, server_runtime};
use rust_mcp_sdk::schema::{
    CallToolRequest, CallToolResult, Implementation, InitializeResult, LATEST_PROTOCOL_VERSION,
    ListToolsRequest, ListToolsResult, RpcError, ServerCapabilities, ServerCapabilitiesTools,
    schema_utils::CallToolError,
};
use rust_mcp_sdk::{McpServer, StdioTransport, TransportOptions, tool_box};

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

// ─── Server Handler ───────────────────────────────────────────────────────────

pub struct MicroFetchHandler;

#[async_trait]
impl ServerHandler for MicroFetchHandler {
    async fn handle_list_tools_request(
        &self,
        _request: ListToolsRequest,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            meta: None,
            next_cursor: None,
            tools: MicroFetchTools::tools(),
        })
    }

    async fn handle_call_tool_request(
        &self,
        request: CallToolRequest,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        let tool = MicroFetchTools::try_from(request.params)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;

        match tool {
            MicroFetchTools::FetchTool(t) => t.run().await,
            MicroFetchTools::FetchBatchTool(t) => t.run().await,
            MicroFetchTools::SubmitTool(t) => t.run().await,
            MicroFetchTools::LoginTool(t) => t.run().await,
            MicroFetchTools::AuthLookupTool(t) => t.run(),
            MicroFetchTools::FingerprintTool(t) => t.run(),
            MicroFetchTools::ValidateTool(t) => t.run().await,
            MicroFetchTools::BenchmarkTool(t) => t.run().await,
        }
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

    let server_details = InitializeResult {
        server_info: Implementation {
            name: "nab".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: Some("MicroFetch Browser Engine".into()),
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            ..Default::default()
        },
        meta: None,
        instructions: Some(
            "nab provides ultra-fast web fetching with automatic content conversion (HTML/PDF→markdown), \
             SPA data extraction, form submission with CSRF handling, auto-login via 1Password, \
             HTTP/3, and browser fingerprinting.".into(),
        ),
        protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
    };

    let transport = StdioTransport::new(TransportOptions::default())?;
    let handler = MicroFetchHandler;
    let server = server_runtime::create_server(server_details, transport, handler);

    Ok(server.start().await?)
}
