//! `nab::task` — the task-engine schema (shared contract).
//!
//! These are the plain serde types the API-first web-task engine speaks: the
//! action the loop emits ([`TaskAction`]), the per-step observation
//! ([`ActionObservation`]), the rung-1 API leads ([`DiscoveredApi`]), the final
//! [`TaskOutcome`], and the terminal [`TaskStatus`].
//!
//! They live in the **library** (not the `nab` binary's `cmd` module) so both
//! consumers share one contract: the `nab` CLI executor (`cmd::task`) and the
//! `nab-mcp` self-contained sampling loop (slice 4). The executor and the loop
//! are built on top of these types; see
//! `docs/design/2026-05-31-nab-task-engine.md` §12 for the binary-boundary
//! rationale behind keeping the schema here.
//!
//! Feature-gated behind `task` (experimental until the loop is proven).

use serde::{Deserialize, Serialize};

use crate::ApiEndpoint;

fn default_get_method() -> String {
    "GET".to_string()
}

/// One action the task loop can take at a given rung (§4 of the design).
///
/// The schema is stable for the host LLM (or the slice-4 sampling loop) that
/// produces these. Variants beyond what the current executor runs are part of
/// the forward API — `submit` (rung 2) and `extract` land with the loop slice.
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

/// The result of executing ONE [`TaskAction`] — the OBSERVE half of the loop
/// (§4 step 4). Returned by the executor so the host LLM (or the slice-4
/// sampling loop) can inspect the outcome and decide the next step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionObservation {
    /// The rung that executed the action (1 = API/JS, 2 = submit, 3 = browser).
    pub rung: u8,
    pub status: TaskStatus,
    /// YARA-screened, token-budgeted content the action produced (empty on error
    /// or when the action is not executable in the current slice).
    pub content: String,
    /// Set when the action failed or is deferred to a later slice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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

    #[test]
    fn action_observation_serializes_and_omits_absent_error() {
        let obs = ActionObservation {
            rung: 1,
            status: TaskStatus::Done,
            content: "ok".into(),
            error: None,
        };
        let s = serde_json::to_string(&obs).unwrap();
        assert!(s.contains("\"rung\":1"));
        assert!(s.contains("\"status\":\"done\""));
        // error is None -> skipped, not serialized as null.
        assert!(!s.contains("error"));
        let back: ActionObservation = serde_json::from_str(&s).unwrap();
        assert_eq!(obs, back);
    }
}
