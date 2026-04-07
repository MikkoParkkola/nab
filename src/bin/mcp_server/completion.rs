//! Argument completion for `completion/complete` requests.
//!
//! MCP 2025-11-25 §completion: servers that advertise `completions: Some({})`
//! in `ServerCapabilities` MUST respond to `completion/complete` requests.
//!
//! # Completion sources
//!
//! | Argument        | Source                                                     |
//! |-----------------|-------------------------------------------------------------|
//! | `cookies`       | Installed browsers via `nab::detect_default_browser` + fixed list |
//! | `browser`       | Known fingerprint profile names from `nab::fingerprint`    |
//! | `session`       | Session names from `~/.nab/sessions/`                      |
//! | `url` (prompts) | Empty for now — populated in a future phase                |

use rust_mcp_sdk::schema::{
    CompleteRequestParams, CompleteRequestRef, CompleteResult, CompleteResultCompletion, RpcError,
};

// ─── Known values ─────────────────────────────────────────────────────────────

/// All browser names nab knows how to pull cookies from.
const ALL_BROWSERS: &[&str] = &["brave", "chrome", "firefox", "safari", "edge", "auto", "none"];

/// Fingerprint profile names (keep in sync with `nab::fingerprint`).
const FINGERPRINT_PROFILES: &[&str] = &[
    "chrome-131-macos",
    "chrome-131-windows",
    "chrome-131-linux",
    "firefox-133-macos",
    "firefox-133-windows",
    "safari-18-macos",
    "edge-131-windows",
    "random",
];

// ─── Public entry point ───────────────────────────────────────────────────────

/// Handle a `completion/complete` request.
///
/// Returns suggestions for prompt arguments and resource template parameters.
/// An empty `CompleteResult` is always valid per spec — unknown arguments
/// return zero suggestions rather than an error.
pub(crate) fn handle_complete(
    params: &CompleteRequestParams,
) -> Result<CompleteResult, RpcError> {
    let suggestions = match &params.ref_ {
        CompleteRequestRef::PromptReference(prompt_ref) => {
            complete_prompt_arg(&prompt_ref.name, &params.argument.name, &params.argument.value)
        }
        CompleteRequestRef::ResourceTemplateReference(_) => vec![],
    };

    Ok(build_complete_result(suggestions))
}

// ─── Prompt argument completion ───────────────────────────────────────────────

/// Return completion suggestions for a named prompt argument.
fn complete_prompt_arg(prompt_name: &str, arg_name: &str, prefix: &str) -> Vec<String> {
    match arg_name {
        "auth_method" | "cookies" => filter_prefix(ALL_BROWSERS, prefix),
        "browser" => filter_prefix(FINGERPRINT_PROFILES, prefix),
        "session" => complete_session_names(prefix),
        "url" | "urls" => complete_url(prompt_name, prefix),
        _ => vec![],
    }
}

/// Filter a static slice by case-insensitive prefix match.
fn filter_prefix(candidates: &[&str], prefix: &str) -> Vec<String> {
    let lower = prefix.to_ascii_lowercase();
    candidates
        .iter()
        .filter(|c| c.to_ascii_lowercase().starts_with(lower.as_str()))
        .map(|s| (*s).to_string())
        .collect()
}

/// List session names from `~/.nab/sessions/` filtered by prefix.
///
/// Returns an empty list on any I/O error rather than propagating.
fn complete_session_names(prefix: &str) -> Vec<String> {
    let session_dir = match nab::get_session_dir() {
        Ok(p) => p,
        Err(_) => return vec![],
    };

    let Ok(entries) = std::fs::read_dir(&session_dir) else {
        return vec![];
    };

    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            // Sessions are stored as `<name>.json` — strip the extension.
            let session_name = name.strip_suffix(".json").unwrap_or(&name);
            if session_name.starts_with(prefix) {
                Some(session_name.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// URL completion — returns empty for now (history not yet implemented).
#[allow(unused_variables)]
fn complete_url(prompt_name: &str, prefix: &str) -> Vec<String> {
    // Phase 3: populate from fetch history log.
    vec![]
}

// ─── Result builder ───────────────────────────────────────────────────────────

/// Wrap a list of suggestion strings into a `CompleteResult`.
fn build_complete_result(values: Vec<String>) -> CompleteResult {
    let has_more = false; // nab lists are always exhaustive
    CompleteResult {
        completion: CompleteResultCompletion {
            values,
            total: None,
            has_more: if has_more { Some(true) } else { None },
        },
        meta: None,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── filter_prefix ─────────────────────────────────────────────────────────

    #[test]
    fn filter_prefix_returns_matching_browsers() {
        // GIVEN browser candidates and prefix "ch"
        // WHEN filtered
        let results = filter_prefix(ALL_BROWSERS, "ch");
        // THEN only chrome matches
        assert_eq!(results, vec!["chrome"]);
    }

    #[test]
    fn filter_prefix_empty_prefix_returns_all() {
        // GIVEN an empty prefix
        let results = filter_prefix(ALL_BROWSERS, "");
        // THEN all candidates are returned
        assert_eq!(results.len(), ALL_BROWSERS.len());
    }

    #[test]
    fn filter_prefix_case_insensitive() {
        // GIVEN uppercase prefix "BR"
        let results = filter_prefix(ALL_BROWSERS, "BR");
        // THEN brave matches
        assert_eq!(results, vec!["brave"]);
    }

    #[test]
    fn filter_prefix_no_match_returns_empty() {
        // GIVEN prefix that matches nothing
        let results = filter_prefix(ALL_BROWSERS, "zzz");
        // THEN empty
        assert!(results.is_empty());
    }

    // ── complete_prompt_arg ───────────────────────────────────────────────────

    #[test]
    fn cookies_arg_completes_from_browser_list() {
        // GIVEN prompt "fetch-and-extract", arg "cookies", prefix "f"
        let results = complete_prompt_arg("fetch-and-extract", "cookies", "f");
        // THEN "firefox" is in results
        assert!(results.contains(&"firefox".to_string()));
        // AND no other browser starting with different letter
        assert!(!results.contains(&"brave".to_string()));
    }

    #[test]
    fn browser_arg_completes_from_fingerprint_profiles() {
        // GIVEN arg "browser", prefix "chrome"
        let results = complete_prompt_arg("fetch-and-extract", "browser", "chrome");
        // THEN all chrome profiles are returned
        assert!(!results.is_empty());
        assert!(results.iter().all(|s| s.starts_with("chrome")));
    }

    #[test]
    fn unknown_arg_returns_empty() {
        // GIVEN an unknown argument name
        let results = complete_prompt_arg("some-prompt", "unknown_arg", "");
        // THEN empty (no crash, no error)
        assert!(results.is_empty());
    }

    // ── handle_complete ───────────────────────────────────────────────────────

    #[test]
    fn handle_complete_prompt_cookies_returns_browsers() {
        // GIVEN a completion/complete request for cookies arg with empty prefix
        let params = CompleteRequestParams {
            argument: rust_mcp_sdk::schema::CompleteRequestArgument {
                name: "cookies".to_string(),
                value: String::new(),
            },
            ref_: CompleteRequestRef::PromptReference(
                rust_mcp_sdk::schema::PromptReference::new(
                    "fetch-and-extract".to_string(),
                    None,
                ),
            ),
            context: None,
            meta: None,
        };
        // WHEN handled
        let result = handle_complete(&params).expect("completion should not fail");
        // THEN browser names are returned
        assert!(!result.completion.values.is_empty());
        assert!(result.completion.values.contains(&"brave".to_string()));
        assert!(result.completion.values.contains(&"chrome".to_string()));
    }

    #[test]
    fn handle_complete_resource_template_returns_empty() {
        // GIVEN a completion request for a resource template (not yet implemented)
        let params = CompleteRequestParams {
            argument: rust_mcp_sdk::schema::CompleteRequestArgument {
                name: "uri".to_string(),
                value: "nab://".to_string(),
            },
            ref_: CompleteRequestRef::ResourceTemplateReference(
                rust_mcp_sdk::schema::ResourceTemplateReference::new(
                    "nab://watch/{id}".to_string(),
                ),
            ),
            context: None,
            meta: None,
        };
        // WHEN handled
        let result = handle_complete(&params).expect("completion should not fail");
        // THEN empty result (no error)
        assert!(result.completion.values.is_empty());
    }

    #[test]
    fn build_complete_result_has_correct_shape() {
        // GIVEN a list of suggestions
        let values = vec!["brave".to_string(), "chrome".to_string()];
        // WHEN built into a CompleteResult
        let result = build_complete_result(values.clone());
        // THEN values are preserved and has_more is None
        assert_eq!(result.completion.values, values);
        assert!(result.completion.has_more.is_none());
    }
}
