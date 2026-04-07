//! MCP tool definitions and implementations for `nab-mcp`.
//!
//! Each submodule corresponds to one MCP tool exposed by the server.
//! Shared client singletons live in [`client`].
//!
//! # Module layout
//!
//! | Module | Tool |
//! |---|---|
//! | [`analyze`] | `analyze` |
//! | [`fetch`] | `fetch` |
//! | [`fetch_batch`] | `fetch_batch` |
//! | [`auth`] | `auth_lookup` |
//! | [`fingerprint`] | `fingerprint` |
//! | [`validate`] | `validate` |
//! | [`benchmark`] | `benchmark` |
//! | [`submit`] | `submit` |
//! | [`login`] | `login` |
//! | [`client`] | shared HTTP client + session store |

pub(crate) mod analyze;
pub(crate) mod auth;
pub(crate) mod benchmark;
pub(crate) mod client;
pub(crate) mod fetch;
pub(crate) mod fetch_batch;
pub(crate) mod fingerprint;
pub(crate) mod login;
pub(crate) mod submit;
pub(crate) mod validate;
pub(crate) mod watch;

// ─── Re-exports ───────────────────────────────────────────────────────────────
//
// Keep the same public surface that `main.rs` and `tests.rs` depend on.

pub use analyze::AnalyzeTool;
pub use auth::AuthLookupTool;
pub use benchmark::BenchmarkTool;
pub use client::get_client;
pub use fetch::FetchTool;
#[cfg(test)]
pub(crate) use fetch::apply_diff_with_store;
pub use fetch_batch::FetchBatchTool;
pub use fingerprint::FingerprintTool;
pub use login::LoginTool;
pub use submit::SubmitTool;
pub use validate::ValidateTool;
pub use watch::{WatchCreateTool, WatchListTool, WatchRemoveTool, get_watch_manager, init_watch_manager};
