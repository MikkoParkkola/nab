//! Sampling helper — server-side plumbing to call `sampling/createMessage`.
//!
//! Sampling is a CLIENT capability per MCP 2025-11-25: the *server* (nab)
//! calls `sampling/createMessage` on the *client* (Claude Code, etc.) to
//! request an LLM inference step.
//!
//! This module provides thin helpers that:
//! 1. Check whether the connected client advertises sampling support.
//! 2. Build and send a `CreateMessageRequest` with sensible defaults.
//!
//! The actual integration with nab tools (e.g., `analyze`, active reading)
//! is deferred to Phase 1.5b.  For now the module is wired in but not called
//! from any tool path.
//!
//! # Usage (future)
//!
//! ```rust,ignore
//! if sampling::is_supported(&runtime) {
//!     let text = sampling::create_message(&runtime, "Summarize this page", 512, None).await?;
//!     // use text ...
//! }
//! ```

use std::sync::Arc;

use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{
    CreateMessageContent, CreateMessageRequestParams, ModelPreferences, Role, SamplingMessage,
    SamplingMessageContent, TextContent,
};

// ─── Public helpers ───────────────────────────────────────────────────────────

/// Return `true` when the connected client supports `sampling/createMessage`.
///
/// Returns `false` (not `None`) when client capabilities are not yet available,
/// so callers can safely skip sampling without special-casing the uninitialized
/// case.
pub(crate) fn is_supported(runtime: &Arc<dyn McpServer>) -> bool {
    runtime.client_supports_sampling().unwrap_or(false)
}

/// Ask the client to run an LLM inference step and return the text response.
///
/// Builds a `CreateMessageRequest` with a single `user` turn containing
/// `prompt` as plain text.  Delegates model selection to the client — nab
/// passes `model_preferences` as a hint only.
///
/// # Errors
///
/// Returns an error if:
/// - The client does not support sampling (call [`is_supported`] first).
/// - The network request fails.
/// - The response does not contain a text block.
pub(crate) async fn create_message(
    runtime: &Arc<dyn McpServer>,
    prompt: &str,
    max_tokens: u32,
    model_preferences: Option<ModelPreferences>,
) -> anyhow::Result<String> {
    if !is_supported(runtime) {
        anyhow::bail!("client does not support sampling");
    }

    let params = CreateMessageRequestParams {
        max_tokens: i64::from(max_tokens),
        messages: vec![SamplingMessage {
            role: Role::User,
            content: SamplingMessageContent::TextContent(TextContent::new(
                prompt.to_string(),
                None,
                None,
            )),
            meta: None,
        }],
        model_preferences,
        system_prompt: None,
        include_context: None,
        temperature: None,
        stop_sequences: vec![],
        metadata: None,
        task: None,
        tool_choice: None,
        tools: vec![],
        meta: None,
    };

    let result = runtime
        .request_message_creation(params)
        .await
        .map_err(|e| anyhow::anyhow!("sampling request failed: {e}"))?;

    // Extract the text content from the response.
    match result.content {
        CreateMessageContent::TextContent(text) => Ok(text.text),
        other => anyhow::bail!("sampling returned unexpected content type: {other:?}"),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use rust_mcp_sdk::error::SdkResult;
    use rust_mcp_sdk::schema::schema_utils::{ClientMessage, MessageFromServer, ServerMessage};
    use rust_mcp_sdk::schema::{ClientSampling, InitializeRequestParams};
    use rust_mcp_sdk::task_store::{ClientTaskStore, ServerTaskStore};
    use rust_mcp_sdk::{McpServer, SessionId};
    use std::time::Duration;

    /// Minimal mock that reports whether sampling is supported.
    struct MockRuntime {
        supports_sampling: bool,
    }

    #[async_trait::async_trait]
    impl McpServer for MockRuntime {
        async fn start(self: Arc<Self>) -> SdkResult<()> {
            unimplemented!()
        }
        async fn set_client_details(&self, _: InitializeRequestParams) -> SdkResult<()> {
            unimplemented!()
        }
        fn server_info(&self) -> &rust_mcp_sdk::schema::InitializeResult {
            unimplemented!()
        }
        fn client_info(&self) -> Option<InitializeRequestParams> {
            if self.supports_sampling {
                let mut caps = rust_mcp_sdk::schema::ClientCapabilities::default();
                caps.sampling = Some(ClientSampling {
                    context: None,
                    tools: None,
                });
                Some(InitializeRequestParams {
                    client_info: rust_mcp_sdk::schema::Implementation {
                        name: "test".into(),
                        version: "0.0.0".into(),
                        title: None,
                        description: None,
                        icons: vec![],
                        website_url: None,
                    },
                    capabilities: caps,
                    protocol_version: rust_mcp_sdk::schema::LATEST_PROTOCOL_VERSION.to_string(),
                    meta: None,
                })
            } else {
                None
            }
        }
        async fn auth_info(
            &self,
        ) -> tokio::sync::RwLockReadGuard<'_, Option<rust_mcp_sdk::auth::AuthInfo>> {
            unimplemented!()
        }
        async fn auth_info_cloned(&self) -> Option<rust_mcp_sdk::auth::AuthInfo> {
            unimplemented!()
        }
        async fn update_auth_info(&self, _: Option<rust_mcp_sdk::auth::AuthInfo>) {
            unimplemented!()
        }
        async fn wait_for_initialization(&self) {}
        fn task_store(&self) -> Option<Arc<ServerTaskStore>> {
            None
        }
        fn client_task_store(&self) -> Option<Arc<ClientTaskStore>> {
            None
        }
        async fn stderr_message(&self, _: String) -> SdkResult<()> {
            Ok(())
        }
        fn session_id(&self) -> Option<SessionId> {
            None
        }
        async fn send(
            &self,
            _: MessageFromServer,
            _: Option<rust_mcp_sdk::schema::RequestId>,
            _: Option<Duration>,
        ) -> SdkResult<Option<ClientMessage>> {
            unimplemented!()
        }
        async fn send_batch(
            &self,
            _: Vec<ServerMessage>,
            _: Option<Duration>,
        ) -> SdkResult<Option<Vec<ClientMessage>>> {
            unimplemented!()
        }
    }

    #[test]
    fn is_supported_true_when_client_has_sampling() {
        // GIVEN a runtime whose client advertises sampling
        let rt: Arc<dyn McpServer> = Arc::new(MockRuntime {
            supports_sampling: true,
        });
        // WHEN checked
        // THEN returns true
        assert!(is_supported(&rt));
    }

    #[test]
    fn is_supported_false_when_client_has_no_info() {
        // GIVEN a runtime with no client info (supports_sampling=false → client_info=None)
        let rt: Arc<dyn McpServer> = Arc::new(MockRuntime {
            supports_sampling: false,
        });
        // WHEN checked
        // THEN returns false (not panics)
        assert!(!is_supported(&rt));
    }

    #[tokio::test]
    async fn create_message_errors_when_not_supported() {
        // GIVEN a runtime without sampling capability
        let rt: Arc<dyn McpServer> = Arc::new(MockRuntime {
            supports_sampling: false,
        });
        // WHEN create_message is called
        let result = create_message(&rt, "hello", 128, None).await;
        // THEN it returns an error (not panics)
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("sampling"));
    }
}
