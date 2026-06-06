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

use crate::{ApiDiscovery, ApiEndpoint};
use std::fmt::Write as _;

fn default_get_method() -> String {
    "GET".to_string()
}

/// Discover candidate API endpoints in a raw HTML body. Returns an empty vec
/// when discovery is unavailable or finds nothing (never fails). Shared by the
/// `nab` CLI (`cmd::task`) and the `nab-mcp` `task` tool.
#[must_use]
pub fn discover_apis(raw_html: &str) -> Vec<DiscoveredApi> {
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
/// Native async-fn-in-trait (not `async_trait`): the returned future inherits
/// the concrete impl's `Send`-ness. The CLI's `fetch_screened` future holds a
/// `RefCell` (not `Send`) and is awaited inline; the `nab-mcp` backend's future
/// is `Send`, so `run_task_loop` over it satisfies the MCP framework's `Send`
/// requirement. `async_trait(?Send)` would box both as non-`Send`, breaking the
/// MCP path.
#[allow(async_fn_in_trait)]
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
        TaskAction::Extract { .. } => Ok(deferred(
            0,
            "extract is a loop-level action (shapes trajectory state); \
             not available in stateless single-action mode",
        )),
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

/// Apply query-focused extraction (the BM25-lite content pipeline) to the
/// CURRENT response — the loop-level `extract` action (§4). Unlike the rung
/// actions in [`execute_action`], `extract` performs no network work; it reshapes
/// content already in the trajectory, so it lives in the loop (which owns that
/// state) and is classified rung 0. Returns an honest `Incomplete` when there is
/// no content to shape rather than a misleading empty `Done`.
fn extract_from_content(prior: &str, extract_query: &str) -> ActionObservation {
    if prior.is_empty() {
        return ActionObservation {
            rung: 0,
            status: TaskStatus::Incomplete,
            content: String::new(),
            error: Some("extract: no prior content available to shape".to_string()),
        };
    }
    let focused = crate::content::focus::extract_focused(prior, extract_query);
    ActionObservation {
        rung: 0,
        status: TaskStatus::Done,
        content: focused.markdown,
        error: None,
    }
}

/// Bounds on a [`run_task_loop`] run — the loop stops at the first limit hit.
#[derive(Debug, Clone)]
pub struct LoopBounds {
    /// Hard cap on the number of executed steps.
    pub max_steps: usize,
    /// Wall-clock cap across the whole run.
    pub max_wall_clock: std::time::Duration,
    /// Crude token proxy: cap on total observation content carried forward, so
    /// the prompt cannot grow unbounded.
    pub max_total_content_chars: usize,
}

impl Default for LoopBounds {
    fn default() -> Self {
        Self {
            max_steps: 12,
            max_wall_clock: std::time::Duration::from_mins(2),
            max_total_content_chars: 32_000,
        }
    }
}

/// The loop's brain: given a prompt (goal + trajectory + discovered APIs), return
/// the next action as JSON text. `nab-mcp` wraps `sampling/createMessage`; tests
/// script a fixed sequence. Native async-fn-in-trait for the same `Send`-inheritance
/// reason as [`TaskFetcher`].
#[allow(async_fn_in_trait)]
pub trait Sampler {
    /// Return the next action as a JSON object (optionally fenced in markdown).
    async fn next_action(&self, prompt: &str) -> anyhow::Result<String>;
}

/// One executed step of a task run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryStep {
    pub action: TaskAction,
    pub observation: ActionObservation,
}

/// Why a [`run_task_loop`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStop {
    /// The brain emitted a `done` action.
    Done,
    /// The `max_steps` bound was hit.
    MaxSteps,
    /// The `max_wall_clock` bound was hit.
    Timeout,
    /// The `max_total_content_chars` bound was hit.
    Budget,
    /// The sampler (brain) returned an error.
    SamplerError,
    /// The sampler's reply could not be parsed as a `TaskAction`.
    ParseError,
}

/// The result of a bounded task loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopOutcome {
    pub goal: String,
    pub stop: LoopStop,
    pub status: TaskStatus,
    pub steps: Vec<TrajectoryStep>,
    pub final_content: String,
}

/// Parse a sampler reply into a [`TaskAction`], tolerating a ```` ```json ````
/// (or bare ```` ``` ````) markdown fence around the JSON object.
fn parse_action(reply: &str) -> anyhow::Result<TaskAction> {
    let trimmed = reply.trim();
    let body = if let Some(rest) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
    {
        rest.trim()
            .strip_suffix("```")
            .map_or(rest, str::trim)
            .trim()
    } else {
        trimmed
    };
    serde_json::from_str(body).map_err(|e| anyhow::anyhow!("could not parse action JSON: {e}"))
}

/// Build the brain prompt from the goal, seed content, discovered APIs, and the
/// trajectory so far. The seed is truncated so the prompt stays bounded.
fn build_prompt(
    goal: &str,
    seed: &str,
    discovered: &[DiscoveredApi],
    steps: &[TrajectoryStep],
) -> String {
    let mut p = String::new();
    p.push_str("Goal: ");
    p.push_str(goal);
    p.push_str("\n\n");
    p.push_str("Seed page (markdown, truncated):\n");
    let seed_cap = 4000;
    if seed.len() > seed_cap {
        p.push_str(&seed[..seed_cap]);
        p.push_str("\n…(truncated)\n\n");
    } else {
        p.push_str(seed);
        p.push_str("\n\n");
    }
    if !discovered.is_empty() {
        p.push_str("Discovered API endpoints (rung-1 leads):\n");
        for d in discovered {
            writeln!(p, "- {} {}", d.method.as_deref().unwrap_or("GET"), d.url).unwrap();
        }
        p.push('\n');
    }
    if !steps.is_empty() {
        p.push_str("Trajectory so far:\n");
        for (i, s) in steps.iter().enumerate() {
            writeln!(
                p,
                "Step {}: rung {} {:?}",
                i + 1,
                s.observation.rung,
                s.observation.status
            )
            .unwrap();
        }
        p.push('\n');
    }
    p.push_str(
        "Reply with the NEXT action as a single JSON object (TaskAction schema), e.g.\n\
         {\"kind\":\"api_call\",\"url\":\"https://...\",\"method\":\"GET\"} \
         or {\"kind\":\"done\",\"summary\":\"...\"}.\n",
    );
    p
}

/// Run the bounded brain-driven loop (§4 steps 2-6 / §9.1): seed context →
/// sample the next action → execute it via the injected `fetcher` → observe →
/// repeat, until a `done` action, a bound, or an error. Pure logic over the
/// injected `sampler` + `fetcher`, so it is fully testable without an LLM or a
/// network. The host LLM is the brain (via `sampler`); nab supplies execution.
pub async fn run_task_loop<S: Sampler, F: TaskFetcher>(
    goal: &str,
    seed: &str,
    discovered: &[DiscoveredApi],
    sampler: &S,
    fetcher: &F,
    bounds: &LoopBounds,
) -> LoopOutcome {
    let start = std::time::Instant::now();
    let mut steps: Vec<TrajectoryStep> = Vec::new();
    let mut content_chars: usize = 0;

    let finish = |stop: LoopStop, status: TaskStatus, steps: Vec<TrajectoryStep>| {
        let final_content = steps
            .last()
            .map(|s| s.observation.content.clone())
            .unwrap_or_default();
        LoopOutcome {
            goal: goal.to_string(),
            stop,
            status,
            steps,
            final_content,
        }
    };

    while steps.len() < bounds.max_steps {
        if start.elapsed() > bounds.max_wall_clock {
            return finish(LoopStop::Timeout, TaskStatus::Incomplete, steps);
        }
        let prompt = build_prompt(goal, seed, discovered, &steps);
        let Ok(reply) = sampler.next_action(&prompt).await else {
            return finish(LoopStop::SamplerError, TaskStatus::Incomplete, steps);
        };
        let Ok(action) = parse_action(&reply) else {
            return finish(LoopStop::ParseError, TaskStatus::Incomplete, steps);
        };
        if let TaskAction::Done { summary } = &action {
            let final_content = summary.clone().unwrap_or_else(|| {
                steps
                    .last()
                    .map(|s| s.observation.content.clone())
                    .unwrap_or_default()
            });
            return LoopOutcome {
                goal: goal.to_string(),
                stop: LoopStop::Done,
                status: TaskStatus::Done,
                steps,
                final_content,
            };
        }
        // `extract` is a loop-level action: it shapes the CURRENT response, so it
        // needs trajectory state `execute_action` (stateless) does not have. The
        // current response is the most recent step's content, or the seed page
        // before any step has run. Pure content shaping, no network → rung 0.
        let observation = if let TaskAction::Extract { extract_query } = &action {
            let prior = steps
                .last()
                .map_or(seed, |s| s.observation.content.as_str());
            extract_from_content(prior, extract_query)
        } else {
            execute_action(&action, fetcher)
                .await
                .unwrap_or_else(|e| ActionObservation {
                    rung: 0,
                    status: TaskStatus::Incomplete,
                    content: String::new(),
                    error: Some(e.to_string()),
                })
        };
        content_chars += observation.content.len();
        steps.push(TrajectoryStep {
            action,
            observation,
        });
        if content_chars > bounds.max_total_content_chars {
            return finish(LoopStop::Budget, TaskStatus::Incomplete, steps);
        }
    }
    finish(LoopStop::MaxSteps, TaskStatus::Incomplete, steps)
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

    impl TaskFetcher for MockFetcher {
        async fn fetch(&self, req: FetchRequest) -> anyhow::Result<String> {
            *self.last.lock().unwrap() = Some(req);
            match &self.reply {
                Ok(s) => Ok(s.clone()),
                Err(e) => Err(anyhow::anyhow!("{e}")),
            }
        }
    }

    /// The URL of the last request a [`MockFetcher`] saw, or `None` if it was
    /// never called — used to assert that loop-level actions (`extract`) do not
    /// touch the network.
    fn f_last_url(f: &MockFetcher) -> Option<String> {
        f.last.lock().unwrap().as_ref().map(|r| r.url.clone())
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
                0,
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

    /// A scripted sampler — returns a fixed sequence of replies, then errors.
    struct ScriptedSampler {
        replies: Vec<String>,
        idx: std::sync::Mutex<usize>,
    }

    impl ScriptedSampler {
        fn new(replies: &[&str]) -> Self {
            Self {
                replies: replies.iter().map(|s| (*s).to_string()).collect(),
                idx: std::sync::Mutex::new(0),
            }
        }
    }

    impl Sampler for ScriptedSampler {
        async fn next_action(&self, _prompt: &str) -> anyhow::Result<String> {
            let mut i = self.idx.lock().unwrap();
            if *i >= self.replies.len() {
                anyhow::bail!("script exhausted");
            }
            let r = self.replies[*i].clone();
            *i += 1;
            Ok(r)
        }
    }

    #[test]
    fn parse_action_strips_json_fences() {
        let a = parse_action("```json\n{\"kind\":\"done\",\"summary\":\"ok\"}\n```").unwrap();
        assert!(matches!(a, TaskAction::Done { .. }));
        let b = parse_action("{\"kind\":\"api_call\",\"url\":\"https://x\"}").unwrap();
        assert!(matches!(b, TaskAction::ApiCall { .. }));
        assert!(parse_action("not json").is_err());
    }

    #[tokio::test]
    async fn loop_runs_api_call_then_done() {
        let sampler = ScriptedSampler::new(&[
            "{\"kind\":\"api_call\",\"url\":\"https://api/x\",\"method\":\"GET\"}",
            "```json\n{\"kind\":\"done\",\"summary\":\"found it\"}\n```",
        ]);
        let fetcher = MockFetcher::ok("{\"result\":42}");
        let out = run_task_loop(
            "find the answer",
            "seed page",
            &[],
            &sampler,
            &fetcher,
            &LoopBounds::default(),
        )
        .await;
        assert_eq!(out.stop, LoopStop::Done);
        assert_eq!(out.status, TaskStatus::Done);
        assert_eq!(out.steps.len(), 1, "one api_call executed before done");
        assert_eq!(out.steps[0].observation.content, "{\"result\":42}");
        assert_eq!(out.final_content, "found it");
    }

    #[tokio::test]
    async fn route_1_stays_at_lowest_rung_when_api_completes() {
        // route.1 (design §6): when an API path completes the goal, the router
        // must stay at the lowest rung — rung 3 (browser) must never fire. Brain
        // solves via one rung-1 api_call then done.
        let sampler = ScriptedSampler::new(&[
            "{\"kind\":\"api_call\",\"url\":\"https://api/x\"}",
            "{\"kind\":\"done\",\"summary\":\"ok\"}",
        ]);
        let fetcher = MockFetcher::ok("data");
        let out = run_task_loop("g", "seed", &[], &sampler, &fetcher, &LoopBounds::default()).await;
        assert_eq!(out.stop, LoopStop::Done);
        // Every executed step stayed at the lowest rung (≤ 1); the browser rung
        // (3) never fired because an API path existed. This is the route.1 gate:
        // the rung telemetry on each step is the proof.
        assert!(
            out.steps.iter().all(|s| s.observation.rung <= 1),
            "router escalated above rung 1 when an API path completed: {:?}",
            out.steps
        );
        assert!(
            !out.steps.iter().any(|s| s.observation.rung == 3),
            "rung 3 (browser) fired despite an available API path"
        );
    }

    /// A 6-section markdown doc the focus pipeline can actually filter
    /// (`extract_focused` passes through ≤3 sections unchanged).
    const MULTI_SECTION_MD: &str = "# Intro\n\nWelcome.\n\n## Auth\n\nBearer tokens and \
        authentication flow.\n\n## Styling\n\nCSS rules.\n\n## Deploy\n\nDocker deploy.\n\n\
        ## Logging\n\nJSON logs.\n\n## Metrics\n\nProm metrics.";

    #[tokio::test]
    async fn loop_extract_shapes_prior_step_content() {
        // api_call → extract → done. `extract` is loop-level: it reshapes the
        // CURRENT response (the api_call's content) via the focus pipeline, at
        // rung 0 (no network), without a fetcher round-trip.
        let sampler = ScriptedSampler::new(&[
            "{\"kind\":\"api_call\",\"url\":\"https://api/doc\"}",
            "{\"kind\":\"extract\",\"extract_query\":\"authentication bearer tokens\"}",
            "{\"kind\":\"done\"}",
        ]);
        let fetcher = MockFetcher::ok(MULTI_SECTION_MD);
        let out = run_task_loop("g", "seed", &[], &sampler, &fetcher, &LoopBounds::default()).await;
        assert_eq!(out.stop, LoopStop::Done);
        assert_eq!(
            out.steps.len(),
            2,
            "api_call + extract executed before done"
        );
        let extract_step = &out.steps[1];
        assert!(
            matches!(extract_step.action, TaskAction::Extract { .. }),
            "second step should be the extract action"
        );
        assert_eq!(extract_step.observation.rung, 0, "extract is rung 0");
        assert_eq!(extract_step.observation.status, TaskStatus::Done);
        assert!(extract_step.observation.error.is_none());
        // Relevant section kept; the doc was actually filtered (omitted marker).
        assert!(
            extract_step.observation.content.contains("## Auth"),
            "extract dropped the relevant section: {}",
            extract_step.observation.content
        );
        assert!(
            extract_step.observation.content.contains("omitted —"),
            "extract did not filter — no omitted-section marker"
        );
        assert!(
            extract_step.observation.content.len() < MULTI_SECTION_MD.len(),
            "focused content should be shorter than the source"
        );
        // No fetch happened for the extract step (fetcher only saw the api_call).
        assert_eq!(
            f_last_url(&fetcher),
            Some("https://api/doc".to_string()),
            "extract must not hit the fetcher"
        );
    }

    #[tokio::test]
    async fn loop_extract_falls_back_to_seed_when_no_prior_step() {
        // extract as the FIRST action shapes the seed page (the current response
        // before any step has run).
        let sampler = ScriptedSampler::new(&[
            "{\"kind\":\"extract\",\"extract_query\":\"authentication bearer tokens\"}",
            "{\"kind\":\"done\"}",
        ]);
        let fetcher = MockFetcher::ok("unused");
        let out = run_task_loop(
            "g",
            MULTI_SECTION_MD,
            &[],
            &sampler,
            &fetcher,
            &LoopBounds::default(),
        )
        .await;
        assert_eq!(out.stop, LoopStop::Done);
        assert_eq!(out.steps.len(), 1);
        let obs = &out.steps[0].observation;
        assert_eq!(obs.rung, 0);
        assert_eq!(obs.status, TaskStatus::Done);
        assert!(obs.content.contains("## Auth"));
        assert!(obs.content.contains("omitted —"));
        // The fetcher was never called — extract shaped the seed directly.
        assert!(f_last_url(&fetcher).is_none());
    }

    #[tokio::test]
    async fn loop_extract_incomplete_when_no_content_to_shape() {
        // extract first, with an EMPTY seed → honest Incomplete, not empty Done.
        let sampler = ScriptedSampler::new(&[
            "{\"kind\":\"extract\",\"extract_query\":\"anything\"}",
            "{\"kind\":\"done\"}",
        ]);
        let fetcher = MockFetcher::ok("unused");
        let out = run_task_loop("g", "", &[], &sampler, &fetcher, &LoopBounds::default()).await;
        assert_eq!(out.stop, LoopStop::Done);
        let obs = &out.steps[0].observation;
        assert_eq!(obs.status, TaskStatus::Incomplete);
        assert!(obs.content.is_empty());
        assert!(obs.error.as_deref().unwrap().contains("no prior content"));
    }

    #[tokio::test]
    async fn loop_stops_at_max_steps() {
        // Sampler always asks for another api_call; never done.
        let sampler = ScriptedSampler::new(&[
            "{\"kind\":\"api_call\",\"url\":\"https://a\"}",
            "{\"kind\":\"api_call\",\"url\":\"https://b\"}",
            "{\"kind\":\"api_call\",\"url\":\"https://c\"}",
        ]);
        let fetcher = MockFetcher::ok("x");
        let bounds = LoopBounds {
            max_steps: 2,
            ..LoopBounds::default()
        };
        let out = run_task_loop("g", "s", &[], &sampler, &fetcher, &bounds).await;
        assert_eq!(out.stop, LoopStop::MaxSteps);
        assert_eq!(out.status, TaskStatus::Incomplete);
        assert_eq!(out.steps.len(), 2);
    }

    #[tokio::test]
    async fn loop_reports_parse_error_on_garbage_reply() {
        let sampler = ScriptedSampler::new(&["this is not json"]);
        let fetcher = MockFetcher::ok("x");
        let out = run_task_loop("g", "s", &[], &sampler, &fetcher, &LoopBounds::default()).await;
        assert_eq!(out.stop, LoopStop::ParseError);
        assert!(out.steps.is_empty());
    }

    #[tokio::test]
    async fn loop_reports_sampler_error_when_brain_fails() {
        let sampler = ScriptedSampler::new(&[]); // exhausted immediately → error
        let fetcher = MockFetcher::ok("x");
        let out = run_task_loop("g", "s", &[], &sampler, &fetcher, &LoopBounds::default()).await;
        assert_eq!(out.stop, LoopStop::SamplerError);
    }
}
