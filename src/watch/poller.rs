//! Background HTTP polling for due watches.
//!
//! `poll_one` fetches a single watch URL with conditional-GET headers,
//! compares the body hash to the previous snapshot, and updates state accordingly.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use chrono::Utc;
use reqwest::StatusCode;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use super::diff::{diff_summary, extract_content, sha256_hex};
use super::storage::{prune_snapshots, save_snapshot_body, save_watch};
use super::types::{Watch, WatchEvent, WatchId, WatchSnapshot};

/// Maximum concurrency for parallel polls.
const MAX_CONCURRENT_POLLS: usize = 8;

/// Minimum polling interval (guards against accidental rapid-fire polls).
const MIN_INTERVAL_SECS: u64 = 30;

/// Maximum backoff interval (24 hours).
const MAX_BACKOFF_SECS: u64 = 86_400;

/// Mute after this many consecutive errors.
const MUTE_AFTER_ERRORS: u32 = 5;

/// User-Agent used for watch polling requests.
pub const WATCH_USER_AGENT: &str =
    "nab-watch/0.7 (https://github.com/MikkoParkkola/nab; web monitoring bot)";

/// Result of polling a single watch.
pub struct PollResult {
    pub id: WatchId,
    pub event: WatchEvent,
    pub updated_watch: Watch,
}

/// Poll all watches in `due` in parallel (capped at [`MAX_CONCURRENT_POLLS`]).
///
/// Returns one `PollResult` per watch.
pub async fn poll_batch(
    due: Vec<Watch>,
    storage_dir: &std::path::Path,
    snapshot_dir: &std::path::Path,
) -> Vec<PollResult> {
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_POLLS));
    let storage_dir = storage_dir.to_owned();
    let snapshot_dir = snapshot_dir.to_owned();

    let futures: Vec<_> = due
        .into_iter()
        .map(|watch| {
            let sem = Arc::clone(&sem);
            let storage_dir = storage_dir.clone();
            let snapshot_dir = snapshot_dir.clone();
            async move {
                let _permit = sem.acquire().await.expect("semaphore not closed");
                poll_one(watch, &storage_dir, &snapshot_dir).await
            }
        })
        .collect();

    futures::future::join_all(futures).await
}

/// Poll a single watch URL.
///
/// Handles:
/// - Conditional GET (ETag / If-Modified-Since)
/// - 304 Not Modified → check counted, no change event
/// - 429 / 503 → adaptive backoff (doubles interval, cap at 24h)
/// - 200 with identical hash → no event
/// - 200 with different hash → `Changed` event, snapshot saved
/// - Connection / other errors → error event, consecutive error count incremented
/// - 5 consecutive errors → watch muted (`interval_secs = 0`)
pub async fn poll_one(
    watch: Watch,
    storage_dir: &std::path::Path,
    snapshot_dir: &std::path::Path,
) -> PollResult {
    let client = build_watch_client();
    let result = fetch_with_conditional_get(&client, &watch).await;

    match result {
        Err(e) => handle_fetch_error(watch, e.to_string(), storage_dir),
        Ok(FetchOutcome::NotModified) => handle_not_modified(watch, storage_dir),
        Ok(FetchOutcome::Backoff { status }) => handle_backoff(watch, status, storage_dir),
        Ok(FetchOutcome::Body {
            body,
            etag,
            last_modified,
        }) => handle_body(watch, body, etag, last_modified, storage_dir, snapshot_dir),
    }
}

// ─── Fetch ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
enum FetchOutcome {
    NotModified,
    Backoff { status: u16 },
    Body {
        body: bytes::Bytes,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

async fn fetch_with_conditional_get(
    client: &reqwest::Client,
    watch: &Watch,
) -> anyhow::Result<FetchOutcome> {
    let mut req = client.get(&watch.url);

    if let Some(etag) = &watch.last_etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    if let Some(lm) = &watch.last_last_modified {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, lm);
    }

    let resp = req.send().await.context("HTTP request failed")?;
    let status = resp.status();

    if status == StatusCode::NOT_MODIFIED {
        return Ok(FetchOutcome::NotModified);
    }

    if matches!(status.as_u16(), 429 | 503) {
        return Ok(FetchOutcome::Backoff {
            status: status.as_u16(),
        });
    }

    if !status.is_success() {
        anyhow::bail!("HTTP {status}");
    }

    let etag = header_str(resp.headers(), reqwest::header::ETAG);
    let last_modified = header_str(resp.headers(), reqwest::header::LAST_MODIFIED);
    let body = resp.bytes().await.context("read body")?;

    Ok(FetchOutcome::Body {
        body,
        etag,
        last_modified,
    })
}

fn header_str(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned)
}

// ─── Outcome handlers ─────────────────────────────────────────────────────────

fn handle_fetch_error(mut watch: Watch, error: String, storage_dir: &std::path::Path) -> PollResult {
    watch.consecutive_errors += 1;
    watch.last_check_at = Some(Utc::now());

    if watch.consecutive_errors >= MUTE_AFTER_ERRORS {
        warn!(
            id = %watch.id,
            url = %watch.url,
            errors = watch.consecutive_errors,
            "Watch muted after {} consecutive errors",
            MUTE_AFTER_ERRORS,
        );
        watch.interval_secs = 0; // muted
    }

    let _ = save_watch(storage_dir, &watch);

    let event = WatchEvent::Error {
        id: watch.id.clone(),
        error,
    };
    PollResult {
        id: watch.id.clone(),
        event,
        updated_watch: watch,
    }
}

fn handle_not_modified(mut watch: Watch, storage_dir: &std::path::Path) -> PollResult {
    info!(id = %watch.id, url = %watch.url, "304 Not Modified");
    watch.consecutive_errors = 0;
    watch.last_check_at = Some(Utc::now());
    let _ = save_watch(storage_dir, &watch);

    let event = WatchEvent::Checked {
        id: watch.id.clone(),
        changed: false,
    };
    PollResult {
        id: watch.id.clone(),
        event,
        updated_watch: watch,
    }
}

fn handle_backoff(mut watch: Watch, status: u16, storage_dir: &std::path::Path) -> PollResult {
    let old_interval = watch.interval_secs;
    let new_interval = (old_interval * 2).clamp(MIN_INTERVAL_SECS, MAX_BACKOFF_SECS);
    watch.interval_secs = new_interval;
    watch.last_check_at = Some(Utc::now());

    warn!(
        id = %watch.id,
        url = %watch.url,
        http_status = status,
        old_interval_secs = old_interval,
        new_interval_secs = new_interval,
        "Adaptive backoff applied",
    );
    let _ = save_watch(storage_dir, &watch);

    let event = WatchEvent::Error {
        id: watch.id.clone(),
        error: format!("HTTP {status} — interval backed off to {new_interval}s"),
    };
    PollResult {
        id: watch.id.clone(),
        event,
        updated_watch: watch,
    }
}

fn handle_body(
    mut watch: Watch,
    body: bytes::Bytes,
    etag: Option<String>,
    last_modified: Option<String>,
    storage_dir: &std::path::Path,
    snapshot_dir: &std::path::Path,
) -> PollResult {
    let now = Utc::now();
    watch.consecutive_errors = 0;
    watch.last_check_at = Some(now);
    watch.last_etag = etag;
    watch.last_last_modified = last_modified;

    let content = extract_content(
        &String::from_utf8_lossy(&body),
        watch.selector.as_deref(),
        &watch.options.diff_kind,
    );
    let new_hash = sha256_hex(content.as_bytes());

    let last_hash = watch.snapshots.first().map(|s| s.sha256.as_str());
    let changed = last_hash != Some(new_hash.as_str());

    if changed {
        let old_content = watch
            .snapshots
            .first()
            .and_then(|s| {
                super::storage::load_snapshot_body(snapshot_dir, &s.sha256)
            })
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();

        let summary = diff_summary(&old_content, &content);

        let snap = WatchSnapshot {
            sha256: new_hash.clone(),
            captured_at: now,
            size: body.len(),
        };

        // Prepend so index 0 is always the latest.
        watch.snapshots.insert(0, snap);

        if let Err(e) = save_snapshot_body(snapshot_dir, &new_hash, content.as_bytes()) {
            warn!(id = %watch.id, error = %e, "Failed to save snapshot body");
        }

        // Prune old snapshots.
        prune_snapshots(&mut watch.snapshots, watch.options.max_snapshots);

        watch.last_change_at = Some(now);
        info!(id = %watch.id, url = %watch.url, %summary, "Content changed");

        let _ = save_watch(storage_dir, &watch);

        let event = WatchEvent::Changed {
            id: watch.id.clone(),
            summary,
        };
        PollResult {
            id: watch.id.clone(),
            event,
            updated_watch: watch,
        }
    } else {
        let _ = save_watch(storage_dir, &watch);
        let event = WatchEvent::Checked {
            id: watch.id.clone(),
            changed: false,
        };
        PollResult {
            id: watch.id.clone(),
            event,
            updated_watch: watch,
        }
    }
}

// ─── HTTP client ──────────────────────────────────────────────────────────────

fn build_watch_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(WATCH_USER_AGENT)
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .expect("watch client builder should not fail with these settings")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::types::WatchOptions;
    use chrono::Utc;
    use tempfile::TempDir;

    fn tmp_watch(id: &str, url: &str) -> (Watch, TempDir, TempDir) {
        let storage = tempfile::tempdir().unwrap();
        let snaps = tempfile::tempdir().unwrap();
        let watch = Watch {
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
        };
        (watch, storage, snaps)
    }

    #[test]
    fn adaptive_backoff_doubles_interval() {
        // GIVEN: watch with 1h interval
        let (watch, storage, _snaps) = tmp_watch("backoff01", "https://x.com");
        let original_interval = watch.interval_secs;
        // WHEN: 429 received
        let result = handle_backoff(watch, 429, storage.path());
        // THEN: interval doubled
        assert_eq!(result.updated_watch.interval_secs, original_interval * 2);
        assert!(matches!(result.event, WatchEvent::Error { .. }));
    }

    #[test]
    fn adaptive_backoff_capped_at_24h() {
        // GIVEN: watch already at near-max interval
        let (mut watch, storage, _snaps) = tmp_watch("backoff02", "https://x.com");
        watch.interval_secs = 50_000; // > 24h after doubling
        // WHEN
        let result = handle_backoff(watch, 429, storage.path());
        // THEN: capped at 24h
        assert_eq!(result.updated_watch.interval_secs, MAX_BACKOFF_SECS);
    }

    #[test]
    fn mute_after_five_consecutive_errors() {
        // GIVEN: watch with 4 prior consecutive errors
        let (mut watch, storage, _snaps) = tmp_watch("mute0001", "https://x.com");
        watch.consecutive_errors = 4;
        // WHEN: 5th error
        let result = handle_fetch_error(watch, "connection refused".into(), storage.path());
        // THEN: muted (interval_secs = 0)
        assert_eq!(result.updated_watch.interval_secs, 0);
    }

    #[test]
    fn not_modified_does_not_change_content() {
        // GIVEN
        let (watch, storage, _snaps) = tmp_watch("nm000001", "https://x.com");
        // WHEN: 304
        let result = handle_not_modified(watch, storage.path());
        // THEN: no change event
        assert!(matches!(result.event, WatchEvent::Checked { changed: false, .. }));
        assert!(result.updated_watch.last_check_at.is_some());
    }
}
