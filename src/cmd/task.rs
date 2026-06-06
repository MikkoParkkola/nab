//! `nab task` — API-first web-task engine (Phase 1).
//!
//! `nab task "<goal>" <url>` is the single-contact-point entry. Build slices:
//! * Slice 1 (rung 0): fetch the seed URL through the moat (browser cookies,
//!   fingerprint, HTTP/3), YARA-screen it, return shaped markdown.
//! * Slice 2 (rung-1 discovery): surface API endpoints found on the page as
//!   [`DiscoveredApi`] leads the host LLM can call directly.
//! * Slice 3 (rung-1 execution): execute one caller-chosen [`TaskAction`] —
//!   currently `api_call` — via [`execute_action`], returning an
//!   [`ActionObservation`]. This is the §9.2 host-driven control flow.
//!
//! The self-contained MCP-sampling control loop (rungs 2-3, the bounded
//! brain-driven loop) lands in a later slice — see
//! `docs/design/2026-05-31-nab-task-engine.md` §7/§9.

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

/// The result of executing ONE [`TaskAction`] — the OBSERVE half of the loop
/// (§4 step 4). Returned by [`execute_action`] so the host LLM (or the slice-4
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

/// Run a web task.
///
/// Two modes, both host-driven (no API key — the caller's LLM is the brain):
///
/// * **Seed (no `action`)** — slice 1/2: fetch the seed URL through the moat,
///   YARA-screen, return shaped markdown plus rung-1 API leads discovered on the
///   page. This is the loop's first turn.
/// * **Execute (`action` set)** — slice 3: execute one caller-chosen
///   [`TaskAction`] (currently rung-1 `api_call`) via [`execute_action`] and
///   return its [`ActionObservation`]. This is the §9.2 host-driven control
///   flow: a no-sampling client reads the discovered APIs, then drives nab
///   one step per call. The slice-4 sampling loop calls `execute_action`
///   internally instead of round-tripping through the CLI.
///
/// When `as_json` is set the full structured result is emitted as pretty JSON;
/// otherwise just the content is printed.
pub async fn cmd_task(
    goal: &str,
    url: &str,
    action_json: Option<&str>,
    format: OutputFormat,
    as_json: bool,
) -> Result<()> {
    if let Some(raw) = action_json {
        let action: TaskAction =
            serde_json::from_str(raw).map_err(|e| anyhow::anyhow!("invalid --action JSON: {e}"))?;
        let obs = execute_action(&action, format).await?;
        if as_json {
            println!("{}", serde_json::to_string_pretty(&obs)?);
        } else {
            if let Some(err) = &obs.error {
                eprintln!("[task] rung {} did not complete: {err}", obs.rung);
            }
            if !obs.content.is_empty() {
                println!("{}", obs.content);
            }
        }
        return Ok(());
    }

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

/// Build a [`FetchConfig`] for a rung-1 `api_call`, reusing the moat
/// (`build_client` HTTP/3 + fingerprint, browser cookies, the YARA screen) via
/// [`FetchConfig::for_url`]. The action's method, headers, and body are mapped
/// onto the config so [`fetch_screened`] routes through `execute_manual_request`
/// (cookies + custom headers + body), not the simple-GET fast path.
///
/// Returns `None` for non-`api_call` variants — they execute elsewhere or in a
/// later slice. Kept separate from [`execute_action`] so the pure mapping is
/// unit-testable without a network round-trip.
fn fetch_config_for_api_call(action: &TaskAction, format: OutputFormat) -> Option<FetchConfig> {
    let TaskAction::ApiCall {
        url,
        method,
        headers,
        body,
        ..
    } = action
    else {
        return None;
    };
    let mut cfg = FetchConfig::for_url(url.clone(), format);
    cfg.method.clone_from(method);
    cfg.data.clone_from(body);
    cfg.custom_headers = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect();
    // API responses are typically JSON/structured — return the raw screened body
    // rather than running readability extraction tuned for article HTML.
    cfg.raw_html = true;
    Some(cfg)
}

/// Execute ONE [`TaskAction`] at its rung and return the [`ActionObservation`]
/// (the ROUTE+ACT+OBSERVE of §4, steps 3-4). This is the shared executor both
/// control modes call: the host-driven CLI turn (`nab task … --action`) and the
/// slice-4 self-contained sampling loop.
///
/// Slice 3 executes rung-1 `api_call`. The remaining variants are forward API:
/// `submit` (rung 2) and `extract` (needs trajectory state) land with the loop
/// slice; `needs_browser` (rung 3) is the opt-in CDP backend; `done` is terminal
/// (owned by the loop, not the executor). Deferred variants return an honest
/// `Incomplete` observation with a reason rather than panicking, so a driver can
/// route around them.
pub async fn execute_action(
    action: &TaskAction,
    format: OutputFormat,
) -> Result<ActionObservation> {
    match action {
        TaskAction::ApiCall { .. } => {
            let cfg = fetch_config_for_api_call(action, format)
                .expect("ApiCall always maps to a FetchConfig");
            match fetch_screened(&cfg).await {
                Ok(fetched) => Ok(ActionObservation {
                    rung: 1,
                    status: TaskStatus::Done,
                    content: fetched.markdown,
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

    #[test]
    fn api_call_maps_method_body_and_headers_onto_fetch_config() {
        let action = TaskAction::ApiCall {
            url: "https://api.example.test/v1/items".into(),
            method: "POST".into(),
            headers: vec![
                ("Authorization".into(), "Bearer t0ken".into()),
                ("Accept".into(), "application/json".into()),
            ],
            body: Some(r#"{"q":"rust"}"#.into()),
            extract_query: None,
        };
        let cfg = fetch_config_for_api_call(&action, OutputFormat::Full)
            .expect("api_call maps to a config");
        assert_eq!(cfg.url, "https://api.example.test/v1/items");
        assert_eq!(cfg.method, "POST");
        assert_eq!(cfg.data.as_deref(), Some(r#"{"q":"rust"}"#));
        // Headers become "Name: Value" strings (the format fetch.rs split_once parses).
        assert!(
            cfg.custom_headers
                .contains(&"Authorization: Bearer t0ken".to_string())
        );
        assert!(
            cfg.custom_headers
                .contains(&"Accept: application/json".to_string())
        );
        // raw_html so an API/JSON body is returned screened-but-unmangled.
        assert!(cfg.raw_html);
    }

    #[test]
    fn fetch_config_for_api_call_is_none_for_non_api_actions() {
        let not_api = TaskAction::Submit {
            url: "https://x".into(),
            fields: vec![],
        };
        assert!(fetch_config_for_api_call(&not_api, OutputFormat::Full).is_none());
    }

    #[tokio::test]
    async fn execute_action_defers_unsupported_variants_without_panicking() {
        // submit (rung 2), js_eval, extract, needs_browser (rung 3) are valid
        // schema but not executable this slice — each returns Incomplete + reason.
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
                    extract_query: "title".into(),
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
            let obs = execute_action(&action, OutputFormat::Full).await.unwrap();
            assert_eq!(obs.rung, want_rung, "rung for {action:?}");
            assert_eq!(obs.status, TaskStatus::Incomplete);
            assert!(
                obs.error.is_some(),
                "expected a deferral reason for {action:?}"
            );
            assert!(obs.content.is_empty());
        }
    }

    #[tokio::test]
    async fn execute_action_done_is_terminal() {
        let obs = execute_action(&TaskAction::Done { summary: None }, OutputFormat::Full)
            .await
            .unwrap();
        assert_eq!(obs.status, TaskStatus::Done);
        assert!(obs.error.is_none());
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
