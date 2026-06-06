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

/// A single fetch request the executor hands to a [`TaskFetcher`] — a rung-1
/// `api_call` reduced to wire essentials. The fetcher owns the moat (client,
/// cookies, fingerprint, YARA screen, budget); the library owns routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// The fetch backend the task executor runs actions through. Each binary injects
/// its own: the `nab` CLI wraps `cmd::fetch::fetch_screened` (full moat);
/// `nab-mcp` wraps its `FetchTool` path. The library never references a
/// binary-only fetch type, so it stays buildable on both sides of the binary
/// boundary (design §12.2). Injection (not a library-internal fetch) is what
/// lets the moat scope stay a per-binary concern.
///
/// `?Send`: the CLI's `fetch_screened` future holds a `RefCell` across an await
/// (not `Send`), so the trait must not require `Send` futures. Backends are
/// awaited inline (the CLI turn, the MCP tool handler), never `spawn`ed across
/// threads, so this costs nothing.
#[async_trait::async_trait(?Send)]
pub trait TaskFetcher {
    /// Execute the request through the moat and return screened, shaped content.
    async fn fetch(&self, req: FetchRequest) -> anyhow::Result<String>;
}

/// Execute ONE [`TaskAction`] at its rung and return the [`ActionObservation`]
/// (the ROUTE+ACT+OBSERVE of §4, steps 3-4). The shared executor both control
/// modes call — the host-driven CLI turn and the slice-4 sampling loop — with
/// the fetch backend injected as a [`TaskFetcher`].
///
/// Executes rung-1 `api_call`. `submit` (rung 2) and `extract` (needs trajectory
/// state) are forward API; `needs_browser` (rung 3) is the opt-in CDP backend;
/// `done` is terminal. Deferred variants return an honest `Incomplete`
/// observation rather than panicking, so a driver can route around them.
pub async fn execute_action<F: TaskFetcher>(
    action: &TaskAction,
    fetcher: &F,
) -> anyhow::Result<ActionObservation> {
    match action {
        TaskAction::ApiCall {
            url,
            method,
            headers,
            body,
            ..
        } => {
            let req = FetchRequest {
                url: url.clone(),
                method: method.clone(),
                headers: headers.clone(),
                body: body.clone(),
            };
            match fetcher.fetch(req).await {
                Ok(content) => Ok(ActionObservation {
                    rung: 1,
                    status: TaskStatus::Done,
                    content,
                    error: None,
                }),
                Err(e) => Ok(ActionObservation {
                    rung: 1,
                    status: TaskStatus::Incomplete,
                    content: String::new(),
                    error: Some(e.to_string()),
                }),
            }
        }
        TaskAction::Done { .. } => Ok(ActionObservation {
            rung: 0,
            status: TaskStatus::Done,
            content: String::new(),
            error: None,
        }),
        TaskAction::Submit { .. } => Ok(deferred(2, "submit (rung 2) lands in a later slice")),
        TaskAction::JsEval { .. } => Ok(deferred(1, "js_eval lands in a later slice")),
        TaskAction::Extract { .. } => {
            Ok(deferred(1, "extract needs trajectory state (loop slice)"))
        }
        TaskAction::NeedsBrowser { reason } => Ok(deferred(
            3,
            &format!("browser rung is opt-in and lands in a later slice: {reason}"),
        )),
    }
}

/// An observation for an action that is valid schema but not executable in the
/// current slice — honest `Incomplete` with the reason, never a panic.
fn deferred(rung: u8, why: &str) -> ActionObservation {
    ActionObservation {
        rung,
        status: TaskStatus::Incomplete,
        content: String::new(),
        error: Some(why.to_string()),
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

    /// A scripted fetcher — no network — for executor routing tests.
    struct MockFetcher {
        reply: anyhow::Result<String>,
        last: std::sync::Mutex<Option<FetchRequest>>,
    }

    impl MockFetcher {
        fn ok(body: &str) -> Self {
            Self {
                reply: Ok(body.to_string()),
                last: std::sync::Mutex::new(None),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                reply: Err(anyhow::anyhow!("{msg}")),
                last: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl TaskFetcher for MockFetcher {
        async fn fetch(&self, req: FetchRequest) -> anyhow::Result<String> {
            *self.last.lock().unwrap() = Some(req);
            match &self.reply {
                Ok(s) => Ok(s.clone()),
                Err(e) => Err(anyhow::anyhow!("{e}")),
            }
        }
    }

    #[tokio::test]
    async fn execute_action_routes_api_call_through_the_fetcher() {
        let f = MockFetcher::ok("{\"ok\":true}");
        let action = TaskAction::ApiCall {
            url: "https://api/x".into(),
            method: "POST".into(),
            headers: vec![("Accept".into(), "application/json".into())],
            body: Some("{}".into()),
            extract_query: None,
        };
        let obs = execute_action(&action, &f).await.unwrap();
        assert_eq!(obs.rung, 1);
        assert_eq!(obs.status, TaskStatus::Done);
        assert_eq!(obs.content, "{\"ok\":true}");
        assert!(obs.error.is_none());
        // The action's wire essentials reached the fetcher unchanged.
        let req = f.last.lock().unwrap().clone().unwrap();
        assert_eq!(req.url, "https://api/x");
        assert_eq!(req.method, "POST");
        assert_eq!(req.body.as_deref(), Some("{}"));
        assert_eq!(
            req.headers,
            vec![("Accept".to_string(), "application/json".to_string())]
        );
    }

    #[tokio::test]
    async fn execute_action_maps_fetcher_error_to_incomplete() {
        let f = MockFetcher::err("boom");
        let action = TaskAction::ApiCall {
            url: "https://api/x".into(),
            method: "GET".into(),
            headers: vec![],
            body: None,
            extract_query: None,
        };
        let obs = execute_action(&action, &f).await.unwrap();
        assert_eq!(obs.rung, 1);
        assert_eq!(obs.status, TaskStatus::Incomplete);
        assert!(obs.content.is_empty());
        assert!(obs.error.unwrap().contains("boom"));
    }

    #[tokio::test]
    async fn execute_action_defers_unsupported_and_terminates_done() {
        let f = MockFetcher::ok("unused");
        let cases = vec![
            (
                TaskAction::Submit {
                    url: "https://x".into(),
                    fields: vec![],
                },
                2u8,
            ),
            (
                TaskAction::JsEval {
                    url: "https://x".into(),
                    script: "1".into(),
                },
                1,
            ),
            (
                TaskAction::Extract {
                    extract_query: "t".into(),
                },
                1,
            ),
            (
                TaskAction::NeedsBrowser {
                    reason: "captcha".into(),
                },
                3,
            ),
        ];
        for (action, want_rung) in cases {
            let obs = execute_action(&action, &f).await.unwrap();
            assert_eq!(obs.rung, want_rung, "rung for {action:?}");
            assert_eq!(obs.status, TaskStatus::Incomplete);
            assert!(obs.error.is_some());
            assert!(obs.content.is_empty());
        }
        // Done is terminal.
        let obs = execute_action(&TaskAction::Done { summary: None }, &f)
            .await
            .unwrap();
        assert_eq!(obs.status, TaskStatus::Done);
        assert!(obs.error.is_none());
    }
}
