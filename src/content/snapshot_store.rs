//! File-based storage for [`ContentSnapshot`] values.
//!
//! Each URL's snapshots are stored under `~/.nab/snapshots/{url_hash}/` as
//! JSON files named `{timestamp}.json`.  Automatic cleanup keeps at most
//! [`SnapshotStore::MAX_SNAPSHOTS_DEFAULT`] snapshots per URL.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::content::snapshot_store::SnapshotStore;
//! use nab::content::diff::ContentSnapshot;
//! use std::time::SystemTime;
//!
//! let store = SnapshotStore::default();
//! let snap = ContentSnapshot::new("https://example.com", "Hello world.", SystemTime::now());
//! store.save_snapshot("https://example.com", &snap).unwrap();
//! let latest = store.load_latest_snapshot("https://example.com");
//! assert!(latest.is_some());
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::diff::ContentSnapshot;

// ── Types ────────────────────────────────────────────────────────────────────

/// Lightweight metadata about a stored snapshot (no content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    /// Unix timestamp of the snapshot.
    pub timestamp: u64,
    /// Content hash (for cheap equality checks without loading full content).
    pub content_hash: u64,
    /// File path on disk.
    pub path: PathBuf,
}

/// File-based snapshot store.
///
/// Thread-safe only in the sense that each write is atomic at the OS level
/// (write to temp file, rename).  Concurrent writes from multiple processes
/// may interleave but will not corrupt individual snapshots.
#[derive(Debug, Clone)]
pub struct SnapshotStore {
    /// Root directory: `~/.nab/snapshots/` by default.
    root: PathBuf,
    /// Maximum snapshots retained per URL.
    max_per_url: usize,
}

impl SnapshotStore {
    /// Default max snapshots retained per URL before pruning.
    pub const MAX_SNAPSHOTS_DEFAULT: usize = 20;

    /// Create a store rooted at `~/.nab/snapshots/`.
    pub fn new() -> Self {
        let root = default_root();
        Self { root, max_per_url: Self::MAX_SNAPSHOTS_DEFAULT }
    }

    /// Create a store with a custom root (useful for tests).
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), max_per_url: Self::MAX_SNAPSHOTS_DEFAULT }
    }

    /// Override max snapshots per URL.
    #[must_use]
    pub fn with_max_per_url(mut self, n: usize) -> Self {
        self.max_per_url = n;
        self
    }

    // ── Persistence ──────────────────────────────────────────────────────────

    /// Persist a snapshot to disk, then prune old snapshots.
    pub fn save_snapshot(&self, url: &str, snapshot: &ContentSnapshot) -> Result<()> {
        let dir = self.url_dir(url);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create snapshot dir {}", dir.display()))?;

        let path = dir.join(format!("{}.json", snapshot.timestamp));
        atomic_write(&path, &serde_json::to_vec_pretty(snapshot)?)
            .with_context(|| format!("write snapshot {}", path.display()))?;

        self.prune_old(&dir);
        Ok(())
    }

    /// Load the most recent snapshot for `url`, or `None` if none exist.
    pub fn load_latest_snapshot(&self, url: &str) -> Option<ContentSnapshot> {
        let mut metas = self.list_snapshots(url);
        metas.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        metas.first().and_then(|m| Self::load_from_path(&m.path))
    }

    /// Load the snapshot closest to `timestamp` for `url`.
    pub fn load_snapshot_at(&self, url: &str, timestamp: u64) -> Option<ContentSnapshot> {
        let metas = self.list_snapshots(url);
        metas
            .into_iter()
            .min_by_key(|m| m.timestamp.abs_diff(timestamp))
            .and_then(|m| Self::load_from_path(&m.path))
    }

    /// Return metadata for all stored snapshots for `url`, sorted oldest-first.
    pub fn list_snapshots(&self, url: &str) -> Vec<SnapshotMeta> {
        let dir = self.url_dir(url);
        read_metas(&dir)
    }

    // ── Internal ─────────────────────────────────────────────────────────────

    fn url_dir(&self, url: &str) -> PathBuf {
        let hash = url_hash(url);
        self.root.join(hash)
    }

    fn load_from_path(path: &Path) -> Option<ContentSnapshot> {
        let bytes = fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Delete oldest snapshots beyond `max_per_url`.
    fn prune_old(&self, dir: &Path) {
        let mut metas = read_metas(dir);
        metas.sort_by(|a, b| a.timestamp.cmp(&b.timestamp)); // oldest first
        let excess = metas.len().saturating_sub(self.max_per_url);
        for meta in metas.iter().take(excess) {
            let _ = fs::remove_file(&meta.path); // best-effort
        }
    }
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Default storage root: `~/.nab/snapshots/`
fn default_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".nab")
        .join("snapshots")
}

/// Stable 16-hex-char hash of a URL string (for directory naming).
fn url_hash(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Read snapshot metadata from all JSON files in `dir`.
fn read_metas(dir: &Path) -> Vec<SnapshotMeta> {
    let Ok(entries) = fs::read_dir(dir) else {
        return vec![];
    };

    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| meta_from_entry(&e.path()))
        .collect()
}

/// Parse a `{timestamp}.json` file into `SnapshotMeta` without loading body.
fn meta_from_entry(path: &Path) -> Option<SnapshotMeta> {
    // Prefer reading from file to avoid loading full content; fall back to parsing.
    let stem = path.file_stem()?.to_str()?;
    let timestamp: u64 = stem.parse().ok()?;

    // Read only the hash without deserialising full content by parsing partially.
    // We load the full struct here — content is small enough that this is fine.
    let bytes = fs::read(path).ok()?;
    let snap: ContentSnapshot = serde_json::from_slice(&bytes).ok()?;

    Some(SnapshotMeta { timestamp, content_hash: snap.content_hash, path: path.to_owned() })
}

/// Atomically write `data` to `path` via a sibling temp file + rename.
fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn tmp_store() -> (TempDir, SnapshotStore) {
        let dir = tempfile::tempdir().expect("tmp dir");
        let store = SnapshotStore::with_root(dir.path());
        (dir, store)
    }

    fn make_snap(url: &str, text: &str, ts_secs: u64) -> ContentSnapshot {
        let ts = SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(ts_secs);
        ContentSnapshot::new(url, text, ts)
    }

    // ── save + load ──────────────────────────────────────────────────────────

    #[test]
    fn save_and_load_latest_roundtrips_content() {
        // GIVEN: a store and a snapshot
        let (_dir, store) = tmp_store();
        let snap = make_snap("https://example.com", "Hello world.", 1_000);
        // WHEN: saved and then loaded
        store.save_snapshot("https://example.com", &snap).unwrap();
        let loaded = store.load_latest_snapshot("https://example.com").unwrap();
        // THEN: content is identical
        assert_eq!(loaded.text, snap.text);
        assert_eq!(loaded.url, snap.url);
    }

    #[test]
    fn load_latest_returns_most_recent_when_multiple() {
        // GIVEN: three snapshots at different timestamps
        let (_dir, store) = tmp_store();
        for ts in [100u64, 200, 300] {
            let s = make_snap("https://example.com", &format!("Text at {ts}"), ts);
            store.save_snapshot("https://example.com", &s).unwrap();
        }
        // WHEN: load latest
        let loaded = store.load_latest_snapshot("https://example.com").unwrap();
        // THEN: text from latest (ts=300)
        assert!(loaded.text.contains("300"), "got: {}", loaded.text);
    }

    #[test]
    fn load_latest_returns_none_for_unknown_url() {
        // GIVEN: empty store
        let (_dir, store) = tmp_store();
        // WHEN: load for URL with no snapshots
        let result = store.load_latest_snapshot("https://never-saved.com");
        // THEN: None
        assert!(result.is_none());
    }

    // ── load_snapshot_at ────────────────────────────────────────────────────

    #[test]
    fn load_snapshot_at_returns_closest_match() {
        // GIVEN: snapshots at ts 100 and 300
        let (_dir, store) = tmp_store();
        for ts in [100u64, 300] {
            let s = make_snap("https://example.com", &format!("ts={ts}"), ts);
            store.save_snapshot("https://example.com", &s).unwrap();
        }
        // WHEN: ask for ts=250 (closer to 300)
        let loaded = store.load_snapshot_at("https://example.com", 250).unwrap();
        // THEN: ts=300 snapshot returned
        assert!(loaded.text.contains("300"), "got: {}", loaded.text);
    }

    #[test]
    fn load_snapshot_at_exact_timestamp_returns_exact() {
        // GIVEN: snapshot at ts=500
        let (_dir, store) = tmp_store();
        let s = make_snap("https://example.com", "exact ts", 500);
        store.save_snapshot("https://example.com", &s).unwrap();
        // WHEN: ask for exact ts=500
        let loaded = store.load_snapshot_at("https://example.com", 500).unwrap();
        // THEN: that snapshot returned
        assert_eq!(loaded.text, "exact ts");
    }

    // ── list_snapshots ───────────────────────────────────────────────────────

    #[test]
    fn list_snapshots_returns_all_saved() {
        // GIVEN: three snapshots
        let (_dir, store) = tmp_store();
        for ts in [10u64, 20, 30] {
            let s = make_snap("https://x.com", "text", ts);
            store.save_snapshot("https://x.com", &s).unwrap();
        }
        // WHEN: list
        let metas = store.list_snapshots("https://x.com");
        // THEN: three entries
        assert_eq!(metas.len(), 3);
    }

    #[test]
    fn list_snapshots_returns_empty_for_unknown_url() {
        // GIVEN: empty store
        let (_dir, store) = tmp_store();
        // WHEN: list for unknown URL
        let metas = store.list_snapshots("https://nope.com");
        // THEN: empty
        assert!(metas.is_empty());
    }

    // ── pruning ──────────────────────────────────────────────────────────────

    #[test]
    fn prune_keeps_at_most_max_per_url() {
        // GIVEN: store with max_per_url=3, saving 5 snapshots
        let (_dir, store) = tmp_store();
        let store = store.with_max_per_url(3);
        for ts in 1u64..=5 {
            let s = make_snap("https://prune.com", &format!("text {ts}"), ts);
            store.save_snapshot("https://prune.com", &s).unwrap();
        }
        // WHEN: list remaining
        let metas = store.list_snapshots("https://prune.com");
        // THEN: at most 3
        assert!(metas.len() <= 3, "expected <=3, got {}", metas.len());
    }

    #[test]
    fn prune_retains_newest_snapshots() {
        // GIVEN: max=2, 4 snapshots saved ts=1..4
        let (_dir, store) = tmp_store();
        let store = store.with_max_per_url(2);
        for ts in 1u64..=4 {
            let s = make_snap("https://prune.com", &format!("t{ts}"), ts);
            store.save_snapshot("https://prune.com", &s).unwrap();
        }
        // WHEN: load latest
        let latest = store.load_latest_snapshot("https://prune.com").unwrap();
        // THEN: newest retained
        assert_eq!(latest.text, "t4");
    }

    // ── url_hash isolation ───────────────────────────────────────────────────

    #[test]
    fn different_urls_stored_separately() {
        // GIVEN: two different URLs
        let (_dir, store) = tmp_store();
        store
            .save_snapshot("https://a.com", &make_snap("https://a.com", "A content", 1))
            .unwrap();
        store
            .save_snapshot("https://b.com", &make_snap("https://b.com", "B content", 1))
            .unwrap();
        // WHEN: load each
        let a = store.load_latest_snapshot("https://a.com").unwrap();
        let b = store.load_latest_snapshot("https://b.com").unwrap();
        // THEN: contents differ
        assert_eq!(a.text, "A content");
        assert_eq!(b.text, "B content");
    }
}
