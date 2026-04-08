//! CLI implementation for `nab watch` subcommands.

use anyhow::{Context as _, Result};
use std::sync::Arc;

use nab::watch::{AddOptions, WatchManager, WatchOptions};

// ─── Config structs ───────────────────────────────────────────────────────────

/// Config for `nab watch add`.
pub struct WatchAddConfig {
    pub url: String,
    pub interval: Option<String>,
    pub selector: Option<String>,
    pub diff_kind: Option<String>,
}

/// Config for `nab watch list`.
pub struct WatchListConfig {
    pub format: WatchListFormat,
}

#[derive(Clone, Copy, Default)]
pub enum WatchListFormat {
    #[default]
    Table,
    Json,
}

/// Config for `nab watch logs`.
pub struct WatchLogsConfig {
    pub id: String,
}

// ─── Commands ─────────────────────────────────────────────────────────────────

/// `nab watch add <url> [--interval <d>] [--selector <css>] [--diff-kind <k>]`
pub async fn cmd_watch_add(cfg: &WatchAddConfig) -> Result<()> {
    let mgr = Arc::new(WatchManager::new_default().context("init watch manager")?);

    let interval_secs = parse_interval(cfg.interval.as_deref())
        .map_err(|e| anyhow::anyhow!("invalid interval {:?}: {e}", cfg.interval))?;

    let diff_kind = parse_diff_kind(cfg.diff_kind.as_deref())
        .map_err(|e| anyhow::anyhow!("invalid diff-kind {:?}: {e}", cfg.diff_kind))?;

    let opts = AddOptions {
        selector: cfg.selector.clone(),
        interval_secs,
        options: WatchOptions {
            diff_kind,
            ..WatchOptions::default()
        },
    };

    let id = mgr.add(&cfg.url, opts).await.context("add watch")?;

    println!("Watch created: {id}");
    println!("  URL:          {}", cfg.url);
    println!("  Resource URI: nab://watch/{id}");
    println!("  Interval:     {interval_secs}s");
    if let Some(sel) = &cfg.selector {
        println!("  Selector:     {sel}");
    }
    Ok(())
}

/// `nab watch list [--format json|table]`
pub async fn cmd_watch_list(cfg: &WatchListConfig) -> Result<()> {
    let mgr = WatchManager::new_default().context("init watch manager")?;
    let watches = mgr.list().await;

    if matches!(cfg.format, WatchListFormat::Json) {
        let json = serde_json::to_string_pretty(&watches).context("serialize watches")?;
        println!("{json}");
        return Ok(());
    }

    if watches.is_empty() {
        println!("No watches registered. Run `nab watch add <url>` to create one.");
        return Ok(());
    }

    println!("{:<10} {:<50} {:>10}  LAST CHECK", "ID", "URL", "INTERVAL");
    println!("{}", "-".repeat(80));
    for w in &watches {
        let last_check = w.last_check_at.map_or_else(
            || "never".into(),
            |t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        );
        let interval = if w.interval_secs == 0 {
            "muted".into()
        } else {
            format!("{}s", w.interval_secs)
        };
        println!(
            "{:<10} {:<50} {:>10}  {}",
            w.id,
            truncate(&w.url, 50),
            interval,
            last_check,
        );
    }
    Ok(())
}

/// `nab watch remove <id>`
pub async fn cmd_watch_remove(id: &str) -> Result<()> {
    let mgr = WatchManager::new_default().context("init watch manager")?;
    mgr.remove(&id.to_owned()).await.context("remove watch")?;
    println!("Watch {id} removed.");
    Ok(())
}

/// `nab watch logs <id>` — show snapshot history for a watch.
pub async fn cmd_watch_logs(cfg: &WatchLogsConfig) -> Result<()> {
    let mgr = WatchManager::new_default().context("init watch manager")?;
    let watch = mgr
        .get(&cfg.id)
        .await
        .ok_or_else(|| anyhow::anyhow!("watch '{}' not found", cfg.id))?;

    println!("Watch: {}", watch.id);
    println!("URL:   {}", watch.url);
    println!();
    if watch.snapshots.is_empty() {
        println!("No snapshots yet.");
        return Ok(());
    }
    println!("{:<20} {:>10}  SHA256", "CAPTURED AT", "SIZE");
    println!("{}", "-".repeat(60));
    for snap in &watch.snapshots {
        println!(
            "{:<20} {:>10}  {}",
            snap.captured_at.format("%Y-%m-%dT%H:%M:%SZ"),
            snap.size,
            &snap.sha256[..16],
        );
    }
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn parse_interval(s: Option<&str>) -> Result<u64, String> {
    let s = match s {
        None | Some("") => return Ok(3600),
        Some(s) => s.trim(),
    };
    if let Some(rest) = s.strip_suffix('s') {
        return rest
            .parse::<u64>()
            .map_err(|_| format!("bad seconds: '{rest}'"));
    }
    if let Some(rest) = s.strip_suffix('m') {
        return rest
            .parse::<u64>()
            .map(|v| v * 60)
            .map_err(|_| format!("bad minutes: '{rest}'"));
    }
    if let Some(rest) = s.strip_suffix('h') {
        return rest
            .parse::<u64>()
            .map(|v| v * 3600)
            .map_err(|_| format!("bad hours: '{rest}'"));
    }
    s.parse::<u64>()
        .map_err(|_| format!("unrecognised duration: '{s}'"))
}

fn parse_diff_kind(s: Option<&str>) -> Result<nab::watch::DiffKind, String> {
    use nab::watch::DiffKind;
    match s.unwrap_or("text") {
        "text" | "" => Ok(DiffKind::Text),
        "semantic" => Ok(DiffKind::Semantic),
        "dom" => Ok(DiffKind::Dom),
        other => Err(format!("unknown diff-kind '{other}'")),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}
