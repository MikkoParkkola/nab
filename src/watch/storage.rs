//! JSON-file persistence for [`Watch`] metadata.
//!
//! Each watch is stored as `<storage_dir>/<id>.json`.
//! Snapshot body bytes are stored content-addressed at `<snapshot_dir>/<sha256>`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::types::{Watch, WatchId, WatchSnapshot};

// ─── Watch persistence ────────────────────────────────────────────────────────

/// Persist a watch's metadata to `<dir>/<id>.json`.
pub fn save_watch(dir: &Path, watch: &Watch) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("create watch storage dir {}", dir.display()))?;
    let path = watch_path(dir, &watch.id);
    let bytes = serde_json::to_vec_pretty(watch).context("serialize watch")?;
    atomic_write(&path, &bytes)
}

/// Load a watch from `<dir>/<id>.json`, returning `None` if it does not exist.
pub fn load_watch(dir: &Path, id: &WatchId) -> Option<Watch> {
    let bytes = fs::read(watch_path(dir, id)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Delete a watch file from `<dir>/<id>.json`.
pub fn delete_watch(dir: &Path, id: &WatchId) -> Result<()> {
    let path = watch_path(dir, id);
    fs::remove_file(&path).with_context(|| format!("remove watch file {}", path.display()))
}

/// Load all watches from `dir`, silently skipping corrupt files.
pub fn load_all_watches(dir: &Path) -> Vec<Watch> {
    let Ok(entries) = fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .filter_map(|e| {
            let bytes = fs::read(e.path()).ok()?;
            serde_json::from_slice(&bytes).ok()
        })
        .collect()
}

fn watch_path(dir: &Path, id: &WatchId) -> PathBuf {
    dir.join(format!("{id}.json"))
}

// ─── Snapshot body storage ────────────────────────────────────────────────────

/// Save body bytes to `<snapshot_dir>/<sha256>` (content-addressed, idempotent).
pub fn save_snapshot_body(snapshot_dir: &Path, sha256: &str, body: &[u8]) -> Result<()> {
    fs::create_dir_all(snapshot_dir)
        .with_context(|| format!("create snapshot dir {}", snapshot_dir.display()))?;
    let path = snapshot_dir.join(sha256);
    if path.exists() {
        // Deduplicated — identical content already stored.
        return Ok(());
    }
    atomic_write(&path, body)
}

/// Load snapshot body bytes from content-addressed storage.
pub fn load_snapshot_body(snapshot_dir: &Path, sha256: &str) -> Option<Vec<u8>> {
    fs::read(snapshot_dir.join(sha256)).ok()
}

/// Garbage-collect snapshot files that are no longer referenced by any watch.
///
/// Called after a watch is removed or its snapshot list is pruned.
pub fn gc_snapshots<S: std::hash::BuildHasher>(
    snapshot_dir: &Path,
    referenced: &HashSet<String, S>,
) {
    let Ok(entries) = fs::read_dir(snapshot_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && !referenced.contains(name)
        {
            let _ = fs::remove_file(entry.path()); // best-effort
        }
    }
}

/// Prune snapshot metadata on a watch to at most `max` entries (newest retained).
/// Returns the SHA-256 hashes that were removed.
pub fn prune_snapshots(snapshots: &mut Vec<WatchSnapshot>, max: usize) -> Vec<String> {
    if snapshots.len() <= max {
        return vec![];
    }
    // Sort newest-first so we can drain the tail.
    snapshots.sort_by_key(|s| std::cmp::Reverse(s.captured_at));
    let removed: Vec<String> = snapshots.drain(max..).map(|s| s.sha256).collect();
    removed
}

// ─── Atomic write ─────────────────────────────────────────────────────────────

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data).with_context(|| format!("write tmp file {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::types::WatchOptions;
    use chrono::Utc;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().expect("tmp dir")
    }

    fn make_watch(id: &str, url: &str) -> Watch {
        Watch {
            id: id.into(),
            url: url.into(),
            selector: None,
            interval_secs: 3600,
            created_at: Utc::now(),
            last_check_at: None,
            last_change_at: None,
            last_etag: None,
            last_last_modified: None,
            snapshots: vec![],
            consecutive_errors: 0,
            options: WatchOptions::default(),
        }
    }

    #[test]
    fn save_and_load_roundtrips() {
        // GIVEN
        let dir = tmp();
        let w = make_watch("abc12345", "https://example.com");
        // WHEN
        save_watch(dir.path(), &w).unwrap();
        let loaded = load_watch(dir.path(), &w.id).unwrap();
        // THEN
        assert_eq!(loaded.url, w.url);
        assert_eq!(loaded.id, w.id);
    }

    #[test]
    fn delete_removes_file() {
        // GIVEN
        let dir = tmp();
        let w = make_watch("del00001", "https://del.com");
        save_watch(dir.path(), &w).unwrap();
        // WHEN
        delete_watch(dir.path(), &w.id).unwrap();
        // THEN
        assert!(load_watch(dir.path(), &w.id).is_none());
    }

    #[test]
    fn load_all_watches_finds_saved() {
        // GIVEN
        let dir = tmp();
        for i in 0..3usize {
            save_watch(
                dir.path(),
                &make_watch(&format!("watch{i:05}"), &format!("https://{i}.com")),
            )
            .unwrap();
        }
        // WHEN
        let watches = load_all_watches(dir.path());
        // THEN
        assert_eq!(watches.len(), 3);
    }

    #[test]
    fn snapshot_body_dedup_shares_file() {
        // GIVEN
        let dir = tmp();
        let body = b"identical content";
        let sha = "deadbeef";
        save_snapshot_body(dir.path(), sha, body).unwrap();
        // WHEN: save same hash again
        save_snapshot_body(dir.path(), sha, body).unwrap();
        // THEN: only one file exists
        let count = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(count, 1);
    }

    #[test]
    fn prune_snapshots_keeps_newest() {
        // GIVEN
        let mut snaps: Vec<WatchSnapshot> = (0..5u64)
            .map(|i| WatchSnapshot {
                sha256: format!("hash{i}"),
                captured_at: chrono::DateTime::from_timestamp(i as i64, 0).unwrap(),
                size: 100,
            })
            .collect();
        // WHEN
        let removed = prune_snapshots(&mut snaps, 3);
        // THEN: 3 retained, 2 removed
        assert_eq!(snaps.len(), 3);
        assert_eq!(removed.len(), 2);
        // Newest (ts=4,3,2) retained
        assert!(snaps.iter().any(|s| s.sha256 == "hash4"));
    }
}
