//! Roots helper — server-side plumbing to query `roots/list`.
//!
//! "Roots" is a CLIENT capability per MCP 2025-11-25: the client advertises
//! which `file://` URIs it has access to, and the server SHOULD respect them
//! (e.g., not write files outside these roots).
//!
//! This module provides:
//! - [`list_roots`]: query `roots/list` from the client.
//! - [`validate_path_in_roots`]: check whether a filesystem path falls under
//!   at least one advertised root.
//! - [`RootsCache`]: a session-lifetime in-memory cache that can be refreshed
//!   when the client sends `notifications/roots/list_changed`.
//!
//! Actual integration with `fetch --save-to` and `analyze file://` paths is
//! deferred to Phase 3.  This module is wired in but not yet called from tool
//! handlers.
//!
//! # Caching
//!
//! Roots change rarely (typically once at session start).  We cache the last
//! result in an `Arc<RwLock<...>>` and expose [`RootsCache::refresh`] so the
//! `notifications/roots/list_changed` handler can invalidate it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rust_mcp_sdk::schema::Root;
use rust_mcp_sdk::McpServer;
use tokio::sync::RwLock;

// ─── Public helpers ───────────────────────────────────────────────────────────

/// Query the client for its current roots list.
///
/// Returns an empty `Vec` when the client does not support roots or when the
/// request fails, so callers can always treat "no roots" as "unconstrained".
pub(crate) async fn list_roots(runtime: &Arc<dyn McpServer>) -> anyhow::Result<Vec<Root>> {
    if !runtime.client_supports_root_list().unwrap_or(false) {
        return Ok(vec![]);
    }
    let result = runtime
        .request_root_list(None)
        .await
        .map_err(|e| anyhow::anyhow!("roots/list request failed: {e}"))?;
    Ok(result.roots)
}

/// Return `true` when `path` is under at least one of the advertised roots,
/// or when `roots` is empty (meaning the client is unconstrained).
///
/// Root URIs are expected to be `file://` URIs per the MCP spec.
pub(crate) fn validate_path_in_roots(path: &Path, roots: &[Root]) -> bool {
    if roots.is_empty() {
        // No roots advertised → server is unconstrained (backward compat).
        return true;
    }
    roots.iter().any(|root| path_is_under_root(path, &root.uri))
}

// ─── RootsCache ───────────────────────────────────────────────────────────────

/// Session-scoped cache for `roots/list` results.
///
/// Thread-safe via `Arc<RwLock>`.  Refresh on `notifications/roots/list_changed`.
#[derive(Clone)]
pub(crate) struct RootsCache {
    inner: Arc<RwLock<Vec<Root>>>,
}

impl RootsCache {
    /// Create an empty cache.
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(vec![])),
        }
    }

    /// Fetch the current roots from the client and store them.
    ///
    /// Called once at session start and again on `notifications/roots/list_changed`.
    pub(crate) async fn refresh(&self, runtime: &Arc<dyn McpServer>) {
        match list_roots(runtime).await {
            Ok(roots) => {
                *self.inner.write().await = roots;
            }
            Err(e) => {
                tracing::warn!("Failed to refresh roots cache: {e}");
            }
        }
    }

    /// Return a snapshot of the cached roots.
    pub(crate) async fn get(&self) -> Vec<Root> {
        self.inner.read().await.clone()
    }

    /// Check whether `path` is valid under the cached roots.
    ///
    /// Convenience wrapper around [`validate_path_in_roots`].
    pub(crate) async fn is_path_valid(&self, path: &Path) -> bool {
        let roots = self.get().await;
        validate_path_in_roots(path, &roots)
    }
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Return `true` when `path` falls under the root identified by `root_uri`.
///
/// `root_uri` is expected to be a `file://` URI.  Paths are compared after
/// stripping the `file://` prefix and normalising to a [`PathBuf`].
fn path_is_under_root(path: &Path, root_uri: &str) -> bool {
    file_uri_to_path(root_uri).is_some_and(|root_path| path.starts_with(&root_path))
}

/// Parse a `file://` URI into a `PathBuf`.
///
/// Returns `None` for non-`file://` URIs or malformed input.
fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://").map(PathBuf::from)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rust_mcp_sdk::schema::Root;

    fn root(uri: &str) -> Root {
        Root {
            uri: uri.to_string(),
            name: None,
            meta: None,
        }
    }

    // ── file_uri_to_path ──────────────────────────────────────────────────────

    #[test]
    fn file_uri_to_path_strips_prefix() {
        // GIVEN a file:// URI
        let path = file_uri_to_path("file:///home/user/projects");
        // THEN it produces the correct PathBuf
        assert_eq!(path, Some(PathBuf::from("/home/user/projects")));
    }

    #[test]
    fn file_uri_to_path_returns_none_for_non_file() {
        // GIVEN a non-file URI
        let path = file_uri_to_path("https://example.com");
        // THEN None is returned
        assert!(path.is_none());
    }

    // ── validate_path_in_roots ────────────────────────────────────────────────

    #[test]
    fn validate_path_true_when_roots_empty() {
        // GIVEN no roots (unconstrained)
        let roots: Vec<Root> = vec![];
        // THEN any path is valid
        assert!(validate_path_in_roots(Path::new("/anywhere"), &roots));
    }

    #[test]
    fn validate_path_true_when_path_under_root() {
        // GIVEN a root at /home/user/projects
        let roots = vec![root("file:///home/user/projects")];
        // WHEN checking a path under that root
        let path = Path::new("/home/user/projects/my-app/src/main.rs");
        // THEN valid
        assert!(validate_path_in_roots(path, &roots));
    }

    #[test]
    fn validate_path_false_when_path_outside_all_roots() {
        // GIVEN roots that do not include /tmp
        let roots = vec![
            root("file:///home/user/projects"),
            root("file:///home/user/docs"),
        ];
        // WHEN checking /tmp/evil
        let path = Path::new("/tmp/evil");
        // THEN invalid
        assert!(!validate_path_in_roots(path, &roots));
    }

    #[test]
    fn validate_path_true_when_any_root_matches() {
        // GIVEN two roots, second one matches
        let roots = vec![
            root("file:///home/user/projects"),
            root("file:///home/user/docs"),
        ];
        let path = Path::new("/home/user/docs/report.pdf");
        assert!(validate_path_in_roots(path, &roots));
    }

    #[test]
    fn validate_path_false_for_prefix_that_is_not_ancestor() {
        // GIVEN root /home/user/project (without trailing slash)
        // Path /home/user/project-evil should NOT match
        let roots = vec![root("file:///home/user/project")];
        let path = Path::new("/home/user/project-evil/file.rs");
        // PathBuf::starts_with checks component boundaries, so this is false
        assert!(!validate_path_in_roots(path, &roots));
    }

    // ── RootsCache ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn roots_cache_starts_empty() {
        // GIVEN a fresh cache
        let cache = RootsCache::new();
        // WHEN queried before any refresh
        let roots = cache.get().await;
        // THEN empty
        assert!(roots.is_empty());
    }

    #[tokio::test]
    async fn roots_cache_is_path_valid_true_when_empty() {
        // GIVEN an empty cache (no roots constraint)
        let cache = RootsCache::new();
        // THEN any path is valid
        assert!(cache.is_path_valid(Path::new("/anywhere")).await);
    }
}
