//! One pre-dispatch callback for every ingress.
//!
//! Design event, recorded here because the ratified design file stays frozen.
//! Nab classifies and builds the bytes. This module stores the hook and nothing
//! else: no tools, no sessions, no elicitation. An unset hook is `Continue`,
//! so a 0.9 build that never installs one keeps today's path.
//! The hook is async because a `tools/call` awaits `run`. A function pointer
//! cannot.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use crate::schema::schema_utils::ClientMessage;

#[cfg(any(feature = "hyper-server", feature = "streamable-http", feature = "sse"))]
use http::{HeaderMap, Method};

/// What the 0.9 call site does with one message.
pub enum Answer {
    /// The existing 0.9 path runs, including its HTTP 400s and its stdio silence.
    Continue,
    /// Write this status and these bytes. The 0.9 handler does not run.
    /// Present whenever this crate can see an HTTP request.
    #[cfg(any(feature = "hyper-server", feature = "streamable-http", feature = "sse"))]
    Http { status: u16, body: Vec<u8> },
    /// Write this JSON-RPC message. On HTTP the status is 200.
    JsonRpc(Vec<u8>),
    /// Write nothing. The 0.9 handler does not run.
    Silent,
}

/// What the call site can see. The body and the message stay borrowed.
pub enum Ingress<'a> {
    #[cfg(any(feature = "hyper-server", feature = "streamable-http", feature = "sse"))]
    Http {
        method: &'a Method,
        headers: &'a HeaderMap,
        body: &'a str,
    },
    Stdio {
        message: &'a ClientMessage,
        /// `McpServer::is_initialized`. Read only for the unknown-revision row.
        initialized: bool,
    },
}

/// Nab's classifier. One implementation serves every call site.
pub trait IngressHook: Send + Sync {
    fn on_ingress<'a>(
        &'a self,
        ingress: Ingress<'a>,
    ) -> Pin<Box<dyn Future<Output = Answer> + Send + 'a>>;
}

static HOOK: OnceLock<Arc<dyn IngressHook>> = OnceLock::new();

/// Install the process hook. A second call keeps the first hook.
/// Each `nab-mcp` process installs once, before `start`.
pub fn install_ingress(hook: Arc<dyn IngressHook>) {
    let _ = HOOK.set(hook);
}

/// Ask the hook. No hook means `Continue`.
pub(crate) async fn consult(ingress: Ingress<'_>) -> Answer {
    match HOOK.get() {
        Some(hook) => hook.on_ingress(ingress).await,
        None => Answer::Continue,
    }
}
