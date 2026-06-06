//! `nab task` — API-first web-task engine (Phase 1).
//!
//! `nab task "<goal>" <url>` is the single-contact-point entry. Build slices:
//! * Slice 1 (rung 0): fetch the seed URL through the moat (browser cookies,
//!   fingerprint, HTTP/3), YARA-screen it, return shaped markdown.
//! * Slice 2 (rung-1 discovery): surface API endpoints found on the page as
//!   [`DiscoveredApi`] leads the host LLM can call directly.
//! * Slice 3 (rung-1 execution): execute one caller-chosen [`TaskAction`].
//!
//! The schema AND the rung-routing executor ([`nab::task::execute_action`]) live
//! in the `nab::task` LIBRARY module so the `nab-mcp` self-contained loop (slice
//! 4) can share them across the binary boundary. This module is the `nab` CLI
//! adapter: it supplies a [`CmdFetcher`] (the full `cmd_fetch` moat via
//! [`fetch_screened`]) as the injected [`TaskFetcher`] backend and wires the CLI
//! surface. See `docs/design/2026-05-31-nab-task-engine.md` §12.

use anyhow::Result;

use super::fetch::{FetchConfig, fetch_screened};
use crate::OutputFormat;
use nab::ApiDiscovery;
use nab::task::{
    DiscoveredApi, FetchRequest, TaskAction, TaskFetcher, TaskOutcome, TaskStatus, execute_action,
};

/// The `nab` CLI's fetch backend: maps a library [`FetchRequest`] onto a
/// [`FetchConfig`] and runs it through [`fetch_screened`], so a rung-1 `api_call`
/// gets the full `cmd_fetch` moat (HTTP/3 + fingerprint, browser cookies, the
/// issue-#117 cookie-profile fallback, the YARA screen, the token budget).
struct CmdFetcher {
    format: OutputFormat,
}

#[async_trait::async_trait(?Send)]
impl TaskFetcher for CmdFetcher {
    async fn fetch(&self, req: FetchRequest) -> Result<String> {
        let cfg = fetch_request_to_config(req, self.format);
        Ok(fetch_screened(&cfg).await?.markdown)
    }
}

/// Pure mapping of a library [`FetchRequest`] onto a [`FetchConfig`] (headers →
/// `Name: Value`, body → `data`, `raw_html=true` for JSON). Split out so it is
/// unit-testable without a network round-trip.
fn fetch_request_to_config(req: FetchRequest, format: OutputFormat) -> FetchConfig {
    let FetchRequest {
        url,
        method,
        headers,
        body,
    } = req;
    let mut cfg = FetchConfig::for_url(url, format);
    cfg.method = method;
    cfg.data = body;
    cfg.custom_headers = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect();
    // API responses are typically JSON/structured — return the raw screened body
    // rather than running readability extraction tuned for article HTML.
    cfg.raw_html = true;
    cfg
}

/// Run a web task.
///
/// Two modes, both host-driven (no API key — the caller's LLM is the brain):
///
/// * **Seed (no `action`)** — slice 1/2: fetch the seed URL through the moat,
///   YARA-screen, return shaped markdown plus rung-1 API leads discovered on the
///   page. This is the loop's first turn.
/// * **Execute (`action` set)** — slice 3: execute one caller-chosen
///   [`TaskAction`] (currently rung-1 `api_call`) via the library executor
///   [`nab::task::execute_action`], with [`CmdFetcher`] as the injected backend,
///   and return its `ActionObservation`. This is the §9.2 host-driven control
///   flow: a no-sampling client reads the discovered APIs, then drives nab one
///   step per call. The slice-4 sampling loop calls `execute_action` internally
///   with its own fetcher instead of round-tripping through the CLI.
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
        let obs = execute_action(&action, &CmdFetcher { format }).await?;
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
    fn fetch_request_to_config_maps_method_body_and_headers() {
        let req = FetchRequest {
            url: "https://api.example.test/v1/items".into(),
            method: "POST".into(),
            headers: vec![
                ("Authorization".into(), "Bearer t0ken".into()),
                ("Accept".into(), "application/json".into()),
            ],
            body: Some(r#"{"q":"rust"}"#.into()),
        };
        let cfg = fetch_request_to_config(req, OutputFormat::Full);
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
}
