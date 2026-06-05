//! `nab task` — API-first web-task engine (Phase 1, slice 1: rung 0).
//!
//! `nab task "<goal>" <url>` is the single-contact-point entry. Slice 1
//! implements rung 0 only: fetch the seed URL through the moat (browser
//! cookies, fingerprint, HTTP/3), YARA-screen it, and return the shaped
//! markdown as a [`TaskOutcome`]. Rungs 1-3 (API / form / browser) and the
//! MCP-sampling control loop land in later slices — see
//! `docs/design/2026-05-31-nab-task-engine.md`.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::fetch::{FetchConfig, fetch_screened};
use crate::OutputFormat;
use nab::{ApiDiscovery, ApiEndpoint};

fn default_get_method() -> String {
    "GET".to_string()
}

/// One action the task loop can take at a given rung (§4 of the design).
///
/// Rungs 1-3 are emitted by the MCP-sampling loop in later slices; the schema
/// is defined now so it is stable for the host LLM that will produce these.
/// Variants beyond what slice 1 exercises are intentionally part of the
/// forward API (consumed by the bounded loop in slices 2-3).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TaskAction {
    /// Rung 1: call a discovered JSON API directly.
    ApiCall {
        url: String,
        #[serde(default = "default_get_method")]
        method: String,
        #[serde(default)]
        headers: Vec<(String, String)>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        extract_query: Option<String>,
    },
    /// Rung 1: evaluate page JS via `QuickJS` + the authenticated fetch bridge.
    JsEval { url: String, script: String },
    /// Rung 2: submit a (CSRF-aware) form.
    Submit {
        url: String,
        fields: Vec<(String, String)>,
    },
    /// Shape the current response to a query via the content pipeline.
    Extract { extract_query: String },
    /// Rung 3: escalate to the opt-in external-CDP browser.
    NeedsBrowser { reason: String },
    /// Terminal: the goal is complete.
    Done {
        #[serde(default)]
        summary: Option<String>,
    },
}

/// Terminal status of a task run. `Incomplete` / `NeedsHuman` are produced by
/// the bounded loop in later slices.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Done,
    Incomplete,
    NeedsHuman,
}

/// A candidate API endpoint discovered on the fetched page — a rung-1 lead the
/// host LLM can choose to call directly instead of escalating to a browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredApi {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The detector that surfaced this endpoint (for debugging).
    pub source: String,
}

impl From<ApiEndpoint> for DiscoveredApi {
    fn from(e: ApiEndpoint) -> Self {
        Self {
            url: e.url,
            method: e.method,
            source: e.source,
        }
    }
}

/// The result of a `nab task` run, shaped for an LLM consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub goal: String,
    pub url: String,
    /// The rung that produced the result (0 = fetch … 3 = browser).
    pub rung: u8,
    pub status: TaskStatus,
    pub content: String,
    /// Rung-1 API leads discovered on the page (may be empty). The host LLM can
    /// call these directly rather than escalating to a browser.
    #[serde(default)]
    pub discovered_apis: Vec<DiscoveredApi>,
}

/// Run a web task.
///
/// Slice 1: rung 0 only — fetch the seed URL (moat applied), YARA-screen, and
/// return the shaped markdown. When `as_json` is set the full [`TaskOutcome`]
/// is emitted as pretty JSON; otherwise just the content is printed.
pub async fn cmd_task(goal: &str, url: &str, format: OutputFormat, as_json: bool) -> Result<()> {
    let cfg = FetchConfig::for_url(url.to_string(), format);
    let fetched = fetch_screened(&cfg).await?;

    // Rung-1 discovery: surface API endpoints found in the raw page so the host
    // LLM can call them directly. Actually invoking them is a later slice.
    let discovered_apis = discover_apis(&fetched.raw_html);

    let outcome = TaskOutcome {
        goal: goal.to_string(),
        url: url.to_string(),
        rung: 0, // rung 0 = fetch; rung-1 candidates surfaced in discovered_apis
        status: TaskStatus::Done,
        content: fetched.markdown,
        discovered_apis,
    };
    if as_json {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    } else {
        println!("{}", outcome.content);
        if !outcome.discovered_apis.is_empty() {
            eprintln!(
                "\n[task] {} rung-1 API candidate(s) discovered (use --json to see them)",
                outcome.discovered_apis.len()
            );
        }
    }
    Ok(())
}

/// Discover candidate API endpoints in a raw HTML body. Returns an empty vec
/// when discovery is unavailable or finds nothing (never fails the task).
fn discover_apis(raw_html: &str) -> Vec<DiscoveredApi> {
    if raw_html.is_empty() {
        return Vec::new();
    }
    match ApiDiscovery::new() {
        Ok(d) => d
            .discover_from_html(raw_html)
            .into_iter()
            .map(DiscoveredApi::from)
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_action_roundtrips_through_json() {
        let actions = vec![
            TaskAction::ApiCall {
                url: "https://x/api".into(),
                method: "POST".into(),
                headers: vec![("A".into(), "B".into())],
                body: Some("{}".into()),
                extract_query: Some("q".into()),
            },
            TaskAction::JsEval {
                url: "https://x".into(),
                script: "1".into(),
            },
            TaskAction::Submit {
                url: "https://x".into(),
                fields: vec![("f".into(), "v".into())],
            },
            TaskAction::Extract {
                extract_query: "title".into(),
            },
            TaskAction::NeedsBrowser {
                reason: "captcha".into(),
            },
            TaskAction::Done {
                summary: Some("ok".into()),
            },
        ];
        for a in actions {
            let s = serde_json::to_string(&a).unwrap();
            let back: TaskAction = serde_json::from_str(&s).unwrap();
            assert_eq!(a, back);
        }
    }

    #[test]
    fn api_call_defaults_method_to_get() {
        let a: TaskAction =
            serde_json::from_str(r#"{"kind":"api_call","url":"https://x"}"#).unwrap();
        match a {
            TaskAction::ApiCall { method, .. } => assert_eq!(method, "GET"),
            other => panic!("expected api_call, got {other:?}"),
        }
    }

    #[test]
    fn outcome_serializes_rung_and_status() {
        let o = TaskOutcome {
            goal: "g".into(),
            url: "u".into(),
            rung: 0,
            status: TaskStatus::Done,
            content: "c".into(),
            discovered_apis: vec![],
        };
        let s = serde_json::to_string(&o).unwrap();
        assert!(s.contains("\"rung\":0"));
        assert!(s.contains("\"status\":\"done\""));
    }

    #[test]
    fn discover_apis_finds_endpoints_and_skips_empty() {
        assert!(discover_apis("").is_empty());
        let html = r#"<html><body>
            <script>fetch("/api/v1/users")</script>
            <a href="/graphql">gql</a>
        </body></html>"#;
        let found = discover_apis(html);
        assert!(
            found.iter().any(|a| a.url.contains("/api/v1/users")),
            "expected the /api/v1/users endpoint, got {found:?}"
        );
    }

    #[test]
    fn discovered_api_maps_from_endpoint_and_roundtrips() {
        let ep = ApiEndpoint {
            url: "/api/x".into(),
            method: Some("POST".into()),
            source: "script-fetch".into(),
        };
        let d: DiscoveredApi = ep.into();
        assert_eq!(d.url, "/api/x");
        let s = serde_json::to_string(&d).unwrap();
        let back: DiscoveredApi = serde_json::from_str(&s).unwrap();
        assert_eq!(d, back);
    }
}
