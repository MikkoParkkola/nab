//! URL watch subsystem — RSS for the entire web.
//!
//! Allows registering URL watches that are polled in the background.
//! Content changes are broadcast as [`WatchEvent`]s, which the MCP server
//! converts into `notifications/resources/updated` pushes.
//!
//! # Storage
//!
//! Watch metadata lives in `~/.local/share/nab/watches/<id>.json`.
//! Snapshot bodies are content-addressed at `~/.local/share/nab/watches/snapshots/<sha256>`.
//!
//! # Example
//!
//! ```rust,no_run
//! # use std::sync::Arc;
//! # use nab::watch::{WatchManager, AddOptions};
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! let manager = Arc::new(WatchManager::new_default()?);
//! let id = manager.add("https://example.com/pricing", AddOptions::with_interval(3600)).await?;
//! println!("Watch id: {id}");
//! # Ok(())
//! # }
//! ```

pub mod diff;
pub mod poller;
pub mod storage;
pub mod types;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use chrono::Utc;
use tokio::sync::{RwLock, broadcast};
use tracing::info;

pub use types::{
    AddOptions, DiffKind, NotifyOn, Watch, WatchEvent, WatchId, WatchOptions, WatchSnapshot,
};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Default polling interval in seconds (1 hour).
const DEFAULT_INTERVAL_SECS: u64 = 3600;

/// Broadcast channel capacity (events are short-lived; 64 is plenty).
const BROADCAST_CAPACITY: usize = 64;

/// Poller loop tick interval.
const POLL_LOOP_INTERVAL: Duration = Duration::from_secs(60);

// ─── WatchManager ─────────────────────────────────────────────────────────────

/// Central manager for all URL watches.
///
/// Thread-safe; wrap in `Arc` to share across tasks.
pub struct WatchManager {
    /// Directory where `<id>.json` watch files are stored.
    storage_dir: PathBuf,
    /// Content-addressed snapshot body directory.
    snapshot_dir: PathBuf,
    /// In-memory watch table (mirrors disk state).
    watches: Arc<RwLock<HashMap<WatchId, Watch>>>,
    /// Broadcast channel for watch events.
    event_tx: broadcast::Sender<WatchEvent>,
}

impl WatchManager {
    /// Create a new `WatchManager` rooted at the XDG data directory.
    ///
    /// Storage: `~/.local/share/nab/watches/`
    pub fn new_default() -> Result<Self> {
        let base = dirs::data_local_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("nab")
            .join("watches");
        Self::with_storage_dir(base)
    }

    /// Create a `WatchManager` with an explicit storage directory (useful for tests).
    pub fn with_storage_dir(storage_dir: PathBuf) -> Result<Self> {
        let snapshot_dir = storage_dir.join("snapshots");
        let (event_tx, _) = broadcast::channel(BROADCAST_CAPACITY);

        let manager = Self {
            storage_dir,
            snapshot_dir,
            watches: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
        };

        manager.load_from_disk();
        Ok(manager)
    }

    /// Load all persisted watches from disk into the in-memory table.
    fn load_from_disk(&self) {
        // Storage dir may not exist yet — that's fine.
        if !self.storage_dir.exists() {
            return;
        }
        let watches = storage::load_all_watches(&self.storage_dir);
        let mut table = self.watches.try_write().expect("no contention at init");
        for w in watches {
            table.insert(w.id.clone(), w);
        }
        info!("Loaded {} watches from disk", table.len());
    }

    // ─── Public API ───────────────────────────────────────────────────────────

    /// Register a new watch.
    ///
    /// Does an initial fetch to seed the first snapshot.  Returns the new watch
    /// id on success.
    pub async fn add(&self, url: &str, mut opts: AddOptions) -> Result<WatchId> {
        // Validate URL
        url::Url::parse(url).with_context(|| format!("invalid URL: {url}"))?;

        if opts.interval_secs == 0 {
            opts.interval_secs = DEFAULT_INTERVAL_SECS;
        }

        let id = generate_id(url, opts.selector.as_deref());
        info!(%id, %url, "Adding watch");

        // Initial fetch to seed the first snapshot.
        let (etag, last_modified, snapshot) =
            initial_fetch(url, opts.selector.as_deref(), &opts.options).await?;

        let now = Utc::now();
        let watch = Watch {
            id: id.clone(),
            url: url.to_owned(),
            selector: opts.selector.clone(),
            interval_secs: opts.interval_secs,
            created_at: now,
            last_check_at: Some(now),
            last_change_at: Some(now),
            last_etag: etag,
            last_last_modified: last_modified,
            snapshots: snapshot.into_iter().collect(),
            consecutive_errors: 0,
            options: opts.options,
        };

        // Save snapshot body.
        if let Some(snap) = watch.snapshots.first() {
            let content =
                load_initial_content(url, watch.selector.as_deref(), &watch.options).await;
            if let Ok(c) = content {
                let _ = storage::save_snapshot_body(&self.snapshot_dir, &snap.sha256, c.as_bytes());
            }
        }

        storage::save_watch(&self.storage_dir, &watch).context("persist new watch")?;

        {
            let mut table = self.watches.write().await;
            table.insert(id.clone(), watch);
        }

        let _ = self.event_tx.send(WatchEvent::Added(id.clone()));
        Ok(id)
    }

    /// Remove a watch by id.
    ///
    /// The watch file is deleted from disk.  Orphan snapshot files are
    /// garbage-collected.
    pub async fn remove(&self, id: &WatchId) -> Result<()> {
        let watch = {
            let mut table = self.watches.write().await;
            table
                .remove(id)
                .ok_or_else(|| anyhow::anyhow!("watch '{id}' not found"))?
        };

        storage::delete_watch(&self.storage_dir, id).context("delete watch file")?;

        // GC snapshot files no longer referenced by any watch.
        let still_referenced = self.all_snapshot_hashes().await;
        storage::gc_snapshots(&self.snapshot_dir, &still_referenced);

        let _ = self.event_tx.send(WatchEvent::Removed(id.clone()));
        info!(%id, url = %watch.url, "Watch removed");
        Ok(())
    }

    /// Return a snapshot of all registered watches.
    pub async fn list(&self) -> Vec<Watch> {
        self.watches.read().await.values().cloned().collect()
    }

    /// Return a single watch by id, or `None` if not found.
    pub async fn get(&self, id: &WatchId) -> Option<Watch> {
        self.watches.read().await.get(id).cloned()
    }

    /// Subscribe to watch events.
    pub fn subscribe(&self) -> broadcast::Receiver<WatchEvent> {
        self.event_tx.subscribe()
    }

    /// Render the latest snapshot of a watch as markdown.
    ///
    /// Returns `None` if the watch doesn't exist or has no snapshots.
    pub async fn render_resource(&self, id: &WatchId) -> Option<String> {
        let watch = self.get(id).await?;
        let snap = watch.snapshots.first()?;
        let body = storage::load_snapshot_body(&self.snapshot_dir, &snap.sha256)?;
        let text = String::from_utf8_lossy(&body).into_owned();

        Some(format!(
            "# Watch: {}\n\n\
             **URL**: {}\n\
             **Last checked**: {}\n\
             **Last changed**: {}\n\
             **Interval**: {}s\n\
             {}\
             \n---\n\n{}",
            watch.id,
            watch.url,
            watch.last_check_at.map_or_else(
                || "never".into(),
                |t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
            ),
            watch.last_change_at.map_or_else(
                || "never".into(),
                |t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
            ),
            watch.interval_secs,
            watch
                .selector
                .as_deref()
                .map(|s| format!("**Selector**: `{s}`\n"))
                .unwrap_or_default(),
            text,
        ))
    }

    // ─── Background poller ────────────────────────────────────────────────────

    /// Blocking loop that ticks every 60 seconds and polls due watches.
    ///
    /// Spawn this in a background `tokio::task`.
    pub async fn poll_loop(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(POLL_LOOP_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            self.poll_due().await;
        }
    }

    /// Poll all watches that are currently due.
    pub async fn poll_due(&self) {
        let due: Vec<Watch> = {
            let table = self.watches.read().await;
            table.values().filter(|w| w.is_due()).cloned().collect()
        };

        if due.is_empty() {
            return;
        }

        info!("Polling {} due watches", due.len());

        let results = poller::poll_batch(due, &self.storage_dir, &self.snapshot_dir).await;

        let mut table = self.watches.write().await;
        for result in results {
            table.insert(result.id.clone(), result.updated_watch);
            let _ = self.event_tx.send(result.event);
        }
    }

    // ─── Helpers ──────────────────────────────────────────────────────────────

    async fn all_snapshot_hashes(&self) -> HashSet<String> {
        self.watches
            .read()
            .await
            .values()
            .flat_map(|w| w.snapshots.iter().map(|s| s.sha256.clone()))
            .collect()
    }
}

// ─── ID generation ────────────────────────────────────────────────────────────

/// Generate an 8-hex-char watch id: `sha256(url + selector? + timestamp)[..8]`.
fn generate_id(url: &str, selector: Option<&str>) -> WatchId {
    use sha2::{Digest, Sha256};
    let ts = Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let mut h = Sha256::new();
    h.update(url.as_bytes());
    if let Some(s) = selector {
        h.update(b"\x00");
        h.update(s.as_bytes());
    }
    h.update(ts.to_le_bytes());
    hex::encode(&h.finalize()[..4])
}

// ─── Initial fetch ────────────────────────────────────────────────────────────

async fn initial_fetch(
    url: &str,
    selector: Option<&str>,
    options: &WatchOptions,
) -> Result<(Option<String>, Option<String>, Option<WatchSnapshot>)> {
    let client = reqwest::Client::builder()
        .user_agent(poller::WATCH_USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()?;

    let resp = client
        .get(url)
        .send()
        .await
        .context("initial fetch failed")?;

    if !resp.status().is_success() {
        bail!("initial fetch returned HTTP {}", resp.status());
    }

    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);

    let last_modified = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);

    let body = resp.bytes().await.context("read initial body")?;
    let content = diff::extract_content(
        &String::from_utf8_lossy(&body),
        selector,
        &options.diff_kind,
    );
    let sha256 = diff::sha256_hex(content.as_bytes());
    let snap = WatchSnapshot {
        sha256,
        captured_at: Utc::now(),
        size: body.len(),
    };

    Ok((etag, last_modified, Some(snap)))
}

async fn load_initial_content(
    url: &str,
    selector: Option<&str>,
    options: &WatchOptions,
) -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent(poller::WATCH_USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()?;

    let body = client.get(url).send().await?.bytes().await?;

    Ok(diff::extract_content(
        &String::from_utf8_lossy(&body),
        selector,
        &options.diff_kind,
    ))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn manager_with_tmp() -> (Arc<WatchManager>, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(WatchManager::with_storage_dir(dir.path().to_owned()).unwrap());
        (mgr, dir)
    }

    #[test]
    fn generate_id_is_8_hex_chars() {
        let id = generate_id("https://example.com", None);
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "got: {id}");
    }

    #[test]
    fn add_returns_unique_ids_for_same_url() {
        // GIVEN: same URL added twice (timestamps differ)
        let id1 = generate_id("https://example.com", None);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let id2 = generate_id("https://example.com", None);
        // THEN: ids are different (timestamp suffix)
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn list_after_add_contains_watch() {
        // GIVEN: manager with one watch loaded from disk
        let (mgr, _dir) = manager_with_tmp();

        // Inject directly (bypass network)
        let watch = Watch {
            id: "test0001".into(),
            url: "https://example.com".into(),
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
        };

        {
            let mut table = mgr.watches.write().await;
            table.insert(watch.id.clone(), watch.clone());
        }
        storage::save_watch(&mgr.storage_dir, &watch).unwrap();

        // WHEN
        let list = mgr.list().await;
        // THEN
        assert!(list.iter().any(|w| w.id == "test0001"));
    }

    #[tokio::test]
    async fn remove_drops_from_list() {
        // GIVEN: manager with one watch
        let (mgr, _dir) = manager_with_tmp();
        let watch = Watch {
            id: "rmtest01".into(),
            url: "https://remove-me.com".into(),
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
        };
        {
            let mut table = mgr.watches.write().await;
            table.insert(watch.id.clone(), watch.clone());
        }
        storage::save_watch(&mgr.storage_dir, &watch).unwrap();

        // WHEN: removed
        mgr.remove(&watch.id).await.unwrap();

        // THEN: not in list
        let list = mgr.list().await;
        assert!(!list.iter().any(|w| w.id == "rmtest01"));
    }

    #[tokio::test]
    async fn snapshot_dedup_shares_file() {
        // GIVEN: manager
        let (mgr, _dir) = manager_with_tmp();
        let body = b"shared content";
        let sha = diff::sha256_hex(body);

        // WHEN: save same body twice
        storage::save_snapshot_body(&mgr.snapshot_dir, &sha, body).unwrap();
        storage::save_snapshot_body(&mgr.snapshot_dir, &sha, body).unwrap();

        // THEN: only one file
        let count = std::fs::read_dir(&mgr.snapshot_dir).unwrap().count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn render_resource_returns_none_for_unknown_id() {
        // GIVEN
        let (mgr, _dir) = manager_with_tmp();
        // WHEN
        let result = mgr.render_resource(&"unknown1".into()).await;
        // THEN
        assert!(result.is_none());
    }
}
