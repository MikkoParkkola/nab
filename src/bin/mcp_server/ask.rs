//! Per-call decision for a tool that would wait on the client.
//!
//! `Allow` is the 2025 path and the default outside a scope. `Refuse` is what
//! the 2026 dispatcher sets for one `run`. The value is a task-local, not a
//! process global, so a 2025 session and a 2026 POST do not share it.
//! `tokio::spawn` does not inherit the local. Login, analyze, and task await
//! the wait on the same task that entered `run`.

use std::sync::Arc;

use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::schema_utils::CallToolError;
use tokio::task_local;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    Allow,
    /// `sampling` is this request's advertisement, not the stored initialize.
    Refuse {
        sampling: bool,
    },
}

task_local! {
    static CALL_ASK: Ask;
}

pub(crate) fn current() -> Ask {
    CALL_ASK.try_with(|ask| *ask).unwrap_or(Ask::Allow)
}

pub(crate) async fn scope<T>(ask: Ask, future: impl std::future::Future<Output = T>) -> T {
    CALL_ASK.scope(ask, future).await
}

/// The 2026 dispatcher turns this into JSON-RPC `-32600`.
#[derive(Debug)]
pub(crate) struct WaitRefusal;

impl std::fmt::Display for WaitRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this call would wait for the client")
    }
}

impl std::error::Error for WaitRefusal {}

/// One guard for every elicitation send. A `Refuse` call returns before the
/// server-to-client request. `Allow` does nothing.
pub(crate) fn refuse_wait() -> Result<(), CallToolError> {
    match current() {
        Ask::Refuse { .. } => Err(CallToolError::new(WaitRefusal)),
        Ask::Allow => Ok(()),
    }
}

pub(crate) fn is_refuse() -> bool {
    matches!(current(), Ask::Refuse { .. })
}

/// Whether this call would send `sampling/createMessage`.
///
/// `Allow` keeps `sampling::is_supported` (false when there is no runtime).
/// `Refuse` uses the advertisement on this request and ignores the runtime.
pub(crate) fn sampling_would(runtime: Option<&Arc<dyn McpServer>>) -> bool {
    match current() {
        Ask::Allow => runtime.is_some_and(super::sampling::is_supported),
        Ask::Refuse { sampling } => sampling,
    }
}
