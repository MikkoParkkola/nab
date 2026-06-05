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

use super::fetch::{FetchConfig, fetch_to_markdown};
use crate::OutputFormat;

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

/// The result of a `nab task` run, shaped for an LLM consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub goal: String,
    pub url: String,
    /// The rung that produced the result (0 = fetch … 3 = browser).
    pub rung: u8,
    pub status: TaskStatus,
    pub content: String,
}

/// Run a web task.
///
/// Slice 1: rung 0 only — fetch the seed URL (moat applied), YARA-screen, and
/// return the shaped markdown. When `as_json` is set the full [`TaskOutcome`]
/// is emitted as pretty JSON; otherwise just the content is printed.
pub async fn cmd_task(goal: &str, url: &str, format: OutputFormat, as_json: bool) -> Result<()> {
    let cfg = FetchConfig::for_url(url.to_string(), format);
    let content = fetch_to_markdown(&cfg).await?;
    let outcome = TaskOutcome {
        goal: goal.to_string(),
        url: url.to_string(),
        rung: 0, // rung 0 = fetch
        status: TaskStatus::Done,
        content,
    };
    if as_json {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    } else {
        println!("{}", outcome.content);
    }
    Ok(())
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
        };
        let s = serde_json::to_string(&o).unwrap();
        assert!(s.contains("\"rung\":0"));
        assert!(s.contains("\"status\":\"done\""));
    }
}
