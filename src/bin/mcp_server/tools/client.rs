//! Shared HTTP client and session-store singletons for `nab-mcp` tools.
//!
//! Both `OnceCell` globals are initialised lazily on first use and live for the
//! process lifetime, so all tool invocations share the same connection pool.
//! Named sessions additionally persist their cookie jars to encrypted files
//! under `~/.nab/sessions/` on non-Windows platforms.

use rust_mcp_sdk::schema::schema_utils::CallToolError;
use tokio::sync::OnceCell;

use nab::{AcceleratedClient, SessionStore};

// ─── Global singletons ────────────────────────────────────────────────────────

/// Process-wide `AcceleratedClient` — connection pool shared by all tools.
static CLIENT: OnceCell<AcceleratedClient> = OnceCell::const_new();

/// Return (or lazily create) the global `AcceleratedClient`.
pub async fn get_client() -> &'static AcceleratedClient {
    CLIENT
        .get_or_init(|| async { AcceleratedClient::new().expect("Failed to create HTTP client") })
        .await
}

/// Process-wide `SessionStore` — cookie jars keyed by session name.
static SESSION_STORE: OnceCell<SessionStore> = OnceCell::const_new();

pub(crate) async fn get_session_store() -> &'static SessionStore {
    SESSION_STORE
        .get_or_init(|| async { SessionStore::new() })
        .await
}

// ─── Session helpers ──────────────────────────────────────────────────────────

/// Look up (or create) the named session, seeding it with browser cookies when
/// `seed_cookies` is non-empty.  Returns the session's dedicated `reqwest::Client`.
pub(crate) async fn resolve_session_client(
    name: &str,
    seed_cookies: Option<&str>,
    seed_url: &str,
) -> Result<reqwest::Client, CallToolError> {
    let store = get_session_store().await;
    let seed = seed_cookies.filter(|s| !s.is_empty());
    let entry = store
        .get_or_create(name, seed, Some(seed_url))
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))?;
    Ok(entry.client)
}

pub(crate) async fn persist_session(name: &str) -> Result<(), CallToolError> {
    let store = get_session_store().await;
    store
        .persist(name)
        .await
        .map_err(|e| CallToolError::from_message(e.to_string()))
}

/// Build a temporary per-call client with its own cookie jar.
///
/// Used by non-session MCP flows that still need cookie seeding and `Set-Cookie`
/// persistence across multiple requests inside a single tool invocation.
pub(crate) async fn build_transient_client(
    seed_cookies: Option<&str>,
    seed_url: &str,
) -> Result<reqwest::Client, CallToolError> {
    let seed = seed_cookies.filter(|s| !s.is_empty());
    let entry = nab::session::build_transient_entry(seed, Some(seed_url))
        .map_err(|e| CallToolError::from_message(e.to_string()))?;
    Ok(entry.client)
}
