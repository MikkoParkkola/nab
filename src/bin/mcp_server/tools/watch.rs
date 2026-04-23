//! Watch management MCP tools: `watch_create`, `watch_list`, `watch_remove`.

use std::sync::Arc;
use std::sync::OnceLock;

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use nab::watch::{AddOptions, WatchManager, WatchOptions};

// ─── Shared WatchManager singleton ───────────────────────────────────────────

/// Global `WatchManager` shared between MCP tools and the server handler.
static WATCH_MANAGER: OnceLock<Arc<WatchManager>> = OnceLock::new();

/// Initialize the shared `WatchManager`.  Must be called once from `main()`.
pub fn init_watch_manager(mgr: Arc<WatchManager>) {
    WATCH_MANAGER.set(mgr).ok(); // Ignore on re-init (tests may call this multiple times)
}

/// Return the shared `WatchManager`, panicking if not yet initialized.
pub fn get_watch_manager() -> Arc<WatchManager> {
    WATCH_MANAGER
        .get()
        .expect("WatchManager not initialized — call init_watch_manager() first")
        .clone()
}

// ─── watch_create ─────────────────────────────────────────────────────────────

#[mcp_tool(
    name = "watch_create",
    description = "Create a URL watch that emits MCP resource notifications when content changes.\n\
    \n\
    The watch appears as a subscribable resource at `nab://watch/<id>`.  Subscribe to it via\n\
    `resources/subscribe` to receive `notifications/resources/updated` when the page changes.\n\
    \n\
    **interval**: duration string — `30s`, `5m`, `1h`, `24h` (default: `1h`).\n\
    **selector**: CSS selector to watch only a specific element (e.g. `#price`, `table.pricing`).\n\
    **diff_kind**: `text` (default) | `semantic` | `dom` — comparison algorithm."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WatchCreateTool {
    /// URL to watch.
    pub url: String,
    /// Optional CSS selector — only the matched element is compared. Empty string = whole page.
    #[serde(default)]
    pub selector: String,
    /// Polling interval as a duration string: `30s`, `5m`, `1h`, `24h`. Empty = default (`1h`).
    #[serde(default)]
    pub interval: String,
    /// Diff algorithm: `text` | `semantic` | `dom`. Empty = default (`text`).
    #[serde(default)]
    pub diff_kind: String,
}

impl WatchCreateTool {
    pub async fn run(self) -> Result<CallToolResult, CallToolError> {
        let interval_opt = (!self.interval.is_empty()).then_some(self.interval.as_str());
        let interval_secs = parse_interval(interval_opt)
            .map_err(|e| CallToolError::from_message(format!("Invalid interval: {e}")))?;

        let diff_kind_opt = (!self.diff_kind.is_empty()).then_some(self.diff_kind.as_str());
        let diff_kind = parse_diff_kind(diff_kind_opt)
            .map_err(|e| CallToolError::from_message(format!("Invalid diff_kind: {e}")))?;

        let opts = AddOptions {
            selector: if self.selector.is_empty() {
                None
            } else {
                Some(self.selector.clone())
            },
            interval_secs,
            options: WatchOptions {
                diff_kind,
                ..WatchOptions::default()
            },
        };

        let mgr = get_watch_manager();
        let id = mgr
            .add(&self.url, opts)
            .await
            .map_err(|e| CallToolError::from_message(format!("Failed to create watch: {e}")))?;

        let text = format!(
            "Watch created.\n\n\
             - **ID**: `{id}`\n\
             - **URL**: {url}\n\
             - **Resource URI**: `nab://watch/{id}`\n\
             - **Interval**: {interval_secs}s\n\
             {selector}\
             \n\
             Subscribe with `resources/subscribe` and URI `nab://watch/{id}` to receive \
             `notifications/resources/updated` when content changes.",
            url = self.url,
            selector = if self.selector.is_empty() {
                String::new()
            } else {
                format!("- **Selector**: `{}`\n", self.selector)
            },
        );

        Ok(CallToolResult::text_content(vec![TextContent::from(text)]))
    }
}

// ─── watch_list ────────────────────────────────────────────────────────────────

#[mcp_tool(
    name = "watch_list",
    description = "List all active URL watches with their IDs, URLs, and polling status."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WatchListTool {}

impl WatchListTool {
    pub async fn run(self) -> Result<CallToolResult, CallToolError> {
        let mgr = get_watch_manager();
        let watches = mgr.list().await;

        if watches.is_empty() {
            return Ok(CallToolResult::text_content(vec![TextContent::from(
                "No watches registered. Use `watch_create` to add one.".to_owned(),
            )]));
        }

        let mut lines = vec![format!("## Active watches ({})\n", watches.len())];

        for w in &watches {
            lines.push(format!(
                "### `{id}` — {url}\n\
                 - **Resource URI**: `nab://watch/{id}`\n\
                 - **Interval**: {interval}s{muted}\n\
                 - **Last checked**: {last_check}\n\
                 - **Last changed**: {last_change}\n\
                 - **Snapshots**: {snaps}\n",
                id = w.id,
                url = w.url,
                interval = w.interval_secs,
                muted = if w.interval_secs == 0 {
                    " **(muted)**"
                } else {
                    ""
                },
                last_check = w.last_check_at.map_or_else(
                    || "never".into(),
                    |t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
                ),
                last_change = w.last_change_at.map_or_else(
                    || "never".into(),
                    |t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
                ),
                snaps = w.snapshots.len(),
            ));
        }

        Ok(CallToolResult::text_content(vec![TextContent::from(
            lines.join("\n"),
        )]))
    }
}

// ─── watch_remove ─────────────────────────────────────────────────────────────

#[mcp_tool(
    name = "watch_remove",
    description = "Remove a URL watch by ID. Use `watch_list` to find the ID."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WatchRemoveTool {
    /// Watch ID to remove (as shown in `watch_list`).
    pub id: String,
}

impl WatchRemoveTool {
    pub async fn run(self) -> Result<CallToolResult, CallToolError> {
        let mgr = get_watch_manager();
        mgr.remove(&self.id).await.map_err(|e| {
            CallToolError::from_message(format!("Failed to remove watch '{}': {e}", self.id))
        })?;

        Ok(CallToolResult::text_content(vec![TextContent::from(
            format!("Watch `{}` removed.", self.id),
        )]))
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Parse a human-friendly duration string into seconds.
///
/// Accepts: `30s`, `5m`, `1h`, `24h`, plain integer (seconds).
fn parse_interval(s: Option<&str>) -> Result<u64, String> {
    let s = match s {
        None | Some("") => return Ok(3600),
        Some(s) => s.trim(),
    };

    if let Some(rest) = s.strip_suffix('s') {
        return rest
            .parse::<u64>()
            .map_err(|_| format!("bad seconds value: '{rest}'"));
    }
    if let Some(rest) = s.strip_suffix('m') {
        return rest
            .parse::<u64>()
            .map(|v| v * 60)
            .map_err(|_| format!("bad minutes value: '{rest}'"));
    }
    if let Some(rest) = s.strip_suffix('h') {
        return rest
            .parse::<u64>()
            .map(|v| v * 3600)
            .map_err(|_| format!("bad hours value: '{rest}'"));
    }
    // Plain integer — treat as seconds
    s.parse::<u64>()
        .map_err(|_| format!("unrecognised duration: '{s}'"))
}

/// Parse a diff kind string into `DiffKind`.
fn parse_diff_kind(s: Option<&str>) -> Result<nab::watch::DiffKind, String> {
    use nab::watch::DiffKind;
    match s.unwrap_or("text") {
        "text" | "" => Ok(DiffKind::Text),
        "semantic" => Ok(DiffKind::Semantic),
        "dom" => Ok(DiffKind::Dom),
        other => Err(format!(
            "unknown diff_kind '{other}'; use text | semantic | dom"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_interval_seconds() {
        assert_eq!(parse_interval(Some("30s")).unwrap(), 30);
    }

    #[test]
    fn parse_interval_minutes() {
        assert_eq!(parse_interval(Some("5m")).unwrap(), 300);
    }

    #[test]
    fn parse_interval_hours() {
        assert_eq!(parse_interval(Some("1h")).unwrap(), 3600);
    }

    #[test]
    fn parse_interval_none_defaults_to_1h() {
        assert_eq!(parse_interval(None).unwrap(), 3600);
    }

    #[test]
    fn parse_interval_plain_int() {
        assert_eq!(parse_interval(Some("120")).unwrap(), 120);
    }

    #[test]
    fn parse_interval_bad_value_errors() {
        assert!(parse_interval(Some("abc")).is_err());
    }

    #[test]
    fn parse_diff_kind_defaults_to_text() {
        use nab::watch::DiffKind;
        assert!(matches!(parse_diff_kind(None).unwrap(), DiffKind::Text));
        assert!(matches!(
            parse_diff_kind(Some("text")).unwrap(),
            DiffKind::Text
        ));
    }

    #[test]
    fn parse_diff_kind_semantic() {
        use nab::watch::DiffKind;
        assert!(matches!(
            parse_diff_kind(Some("semantic")).unwrap(),
            DiffKind::Semantic
        ));
    }

    #[test]
    fn parse_diff_kind_unknown_errors() {
        assert!(parse_diff_kind(Some("fuzzy")).is_err());
    }
}
