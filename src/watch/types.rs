//! Core types for the URL watch subsystem.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─── Type aliases ─────────────────────────────────────────────────────────────

/// 8-hex-char identifier derived from `sha256(url + selector + timestamp)[..8]`.
pub type WatchId = String;

// ─── Watch ────────────────────────────────────────────────────────────────────

/// A registered URL watch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watch {
    /// Stable 8-hex identifier.
    pub id: WatchId,
    /// URL being watched.
    pub url: String,
    /// Optional CSS selector — only the matched subtree is hashed.
    pub selector: Option<String>,
    /// Polling interval in seconds.
    pub interval_secs: u64,
    /// When this watch was created.
    pub created_at: DateTime<Utc>,
    /// When the URL was last polled (regardless of change).
    pub last_check_at: Option<DateTime<Utc>>,
    /// When content last differed from the previous snapshot.
    pub last_change_at: Option<DateTime<Utc>>,
    /// `ETag` from the last `200`/`304` response (for conditional `GET`).
    pub last_etag: Option<String>,
    /// Last-Modified from the last 200/304 response (for conditional GET).
    pub last_last_modified: Option<String>,
    /// Light metadata about stored snapshots (body bytes are separate files).
    pub snapshots: Vec<WatchSnapshot>,
    /// Consecutive error count (reset on 200; mutes at 5).
    pub consecutive_errors: u32,
    /// Configuration options.
    pub options: WatchOptions,
}

impl Watch {
    /// Returns `true` when this watch is due for a poll right now.
    ///
    /// A watch with `interval_secs == 0` is muted and never due.
    pub fn is_due(&self) -> bool {
        if self.interval_secs == 0 {
            return false;
        }
        match self.last_check_at {
            None => true,
            Some(last) => {
                let elapsed =
                    u64::try_from(Utc::now().signed_duration_since(last).num_seconds().max(0))
                        .unwrap_or(0);
                elapsed >= self.interval_secs
            }
        }
    }
}

// ─── WatchSnapshot ────────────────────────────────────────────────────────────

/// Lightweight snapshot metadata (body bytes stored separately by hash).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchSnapshot {
    /// SHA-256 hex digest of the captured body (content-addressed storage key).
    pub sha256: String,
    /// When this snapshot was captured.
    pub captured_at: DateTime<Utc>,
    /// Body size in bytes.
    pub size: usize,
}

// ─── WatchOptions ─────────────────────────────────────────────────────────────

/// Per-watch configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchOptions {
    /// Which changes trigger a notification.
    #[serde(default = "default_notify_on")]
    pub notify_on: NotifyOn,
    /// Diff algorithm to use when comparing snapshots.
    #[serde(default)]
    pub diff_kind: DiffKind,
    /// Maximum snapshots to retain per watch.
    #[serde(default = "default_max_snapshots")]
    pub max_snapshots: usize,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            notify_on: default_notify_on(),
            diff_kind: DiffKind::default(),
            max_snapshots: default_max_snapshots(),
        }
    }
}

fn default_notify_on() -> NotifyOn {
    NotifyOn::Any
}

fn default_max_snapshots() -> usize {
    10
}

// ─── NotifyOn ─────────────────────────────────────────────────────────────────

/// Condition under which a `Changed` event is emitted.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NotifyOn {
    /// Any content change (default).
    #[default]
    Any,
    /// Only when content reverts toward a previous state (regression detection).
    Regression,
    /// Only when semantically meaningful sections change (ignores noise).
    Semantic,
}

// ─── DiffKind ─────────────────────────────────────────────────────────────────

/// Strategy for comparing two snapshots.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    /// Plain text comparison after HTML→markdown (default).
    #[default]
    Text,
    /// Semantic section comparison (strips nav/footer noise).
    Semantic,
    /// CSS-selector-scoped DOM subtree comparison.
    Dom,
}

// ─── WatchEvent ───────────────────────────────────────────────────────────────

/// Events emitted by [`WatchManager`] on its broadcast channel.
///
/// [`WatchManager`]: super::WatchManager
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// A new watch was successfully registered (initial snapshot captured).
    Added(WatchId),
    /// A watch was removed.
    Removed(WatchId),
    /// A watch was polled; `changed` indicates whether content differed.
    Checked { id: WatchId, changed: bool },
    /// Content changed since the last snapshot.
    Changed { id: WatchId, summary: String },
    /// A poll attempt failed.
    Error { id: WatchId, error: String },
}

// ─── AddOptions ───────────────────────────────────────────────────────────────

/// Parameters for [`WatchManager::add`].
///
/// [`WatchManager::add`]: super::WatchManager::add
#[derive(Debug, Clone, Default)]
pub struct AddOptions {
    /// CSS selector — restricts hashing to a matched subtree.
    pub selector: Option<String>,
    /// Polling interval in seconds (default: 3600).
    pub interval_secs: u64,
    /// Advanced options.
    pub options: WatchOptions,
}

impl AddOptions {
    /// Create options with the given interval in seconds.
    pub fn with_interval(interval_secs: u64) -> Self {
        Self {
            interval_secs,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_watch(interval_secs: u64, last_check_secs_ago: Option<i64>) -> Watch {
        let last_check_at = last_check_secs_ago.map(|s| Utc::now() - chrono::Duration::seconds(s));
        Watch {
            id: "test0001".into(),
            url: "https://example.com".into(),
            selector: None,
            interval_secs,
            created_at: Utc::now(),
            last_check_at,
            last_change_at: None,
            last_etag: None,
            last_last_modified: None,
            snapshots: vec![],
            consecutive_errors: 0,
            options: WatchOptions::default(),
        }
    }

    #[test]
    fn interval_due_logic_never_checked_is_always_due() {
        // GIVEN: watch with interval=3600 that has never been checked
        let w = make_watch(3600, None);
        // WHEN/THEN: it's immediately due
        assert!(w.is_due());
    }

    #[test]
    fn interval_due_logic_checked_recently_is_not_due() {
        // GIVEN: watch with 1h interval, checked 30s ago
        let w = make_watch(3600, Some(30));
        // WHEN/THEN: not yet due
        assert!(!w.is_due());
    }

    #[test]
    fn interval_due_logic_checked_long_ago_is_due() {
        // GIVEN: watch with 1h interval, checked 2h ago
        let w = make_watch(3600, Some(7200));
        // WHEN/THEN: overdue
        assert!(w.is_due());
    }

    #[test]
    fn muted_watch_never_due() {
        // GIVEN: watch with interval_secs=0 (muted)
        let w = make_watch(0, None);
        // WHEN/THEN: never due
        assert!(!w.is_due());
    }
}
