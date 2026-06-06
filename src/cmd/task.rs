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
//! The schema types ([`TaskAction`], [`TaskOutcome`], [`ActionObservation`], …)
//! live in the `nab::task` LIBRARY module so the `nab-mcp` self-contained loop
//! (slice 4) can share them across the binary boundary; this module is the
//! `nab` CLI executor over that schema. See
//! `docs/design/2026-05-31-nab-task-engine.md` §12.

use anyhow::Result;

use super::fetch::{FetchConfig, fetch_screened};
use crate::OutputFormat;
use nab::ApiDiscovery;
use nab::task::{ActionObservation, DiscoveredApi, TaskAction, TaskOutcome, TaskStatus};

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
}
