//! Integration tests for the `nab-mcp` helper modules.
//!
//! Tests for `elicitation` and `structured` module functions,
//! co-located here so they can reference any `crate::` module freely.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use std::collections::BTreeMap;

use rust_mcp_sdk::schema::{ElicitResultContent, ElicitResultContentPrimitive};

use crate::elicitation::{extract_multiselect_field, is_oauth_redirect};
use crate::structured::{
    FetchStructuredParams, build_fetch_structured_v2, build_structured, server_icons,
};

// ── is_oauth_redirect ────────────────────────────────────────────────────────

#[test]
fn oauth_redirect_detects_google() {
    // GIVEN a Google OAuth URL
    let url = "https://accounts.google.com/o/oauth2/auth?client_id=xxx";
    // WHEN checked for OAuth redirect
    // THEN it is detected
    assert!(is_oauth_redirect(url));
}

#[test]
fn oauth_redirect_detects_github() {
    assert!(is_oauth_redirect(
        "https://github.com/login/oauth/authorize?client_id=abc"
    ));
}

#[test]
fn oauth_redirect_detects_microsoft() {
    assert!(is_oauth_redirect(
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
    ));
}

#[test]
fn oauth_redirect_rejects_normal_site() {
    // GIVEN a regular website URL
    let url = "https://example.com/login";
    // WHEN checked for OAuth redirect
    // THEN it is NOT detected
    assert!(!is_oauth_redirect(url));
}

#[test]
fn oauth_redirect_case_insensitive() {
    assert!(is_oauth_redirect(
        "https://ACCOUNTS.GOOGLE.COM/o/oauth2/auth"
    ));
}

// ── extract_multiselect_field ────────────────────────────────────────────────

#[test]
fn multiselect_parses_json_array() {
    // GIVEN a JSON-encoded array string in the content map
    let mut content = BTreeMap::new();
    content.insert(
        "sources".to_string(),
        ElicitResultContent::Primitive(ElicitResultContentPrimitive::String(
            r#"["brave","chrome"]"#.to_string(),
        )),
    );
    // WHEN extracted
    let result = extract_multiselect_field(&content, "sources");
    // THEN the values are returned as a Vec
    assert_eq!(result, vec!["brave", "chrome"]);
}

#[test]
fn multiselect_parses_comma_separated() {
    // GIVEN a comma-separated string (fallback encoding)
    let mut content = BTreeMap::new();
    content.insert(
        "sources".to_string(),
        ElicitResultContent::Primitive(ElicitResultContentPrimitive::String(
            "brave, firefox".to_string(),
        )),
    );
    // WHEN extracted
    let result = extract_multiselect_field(&content, "sources");
    // THEN whitespace is trimmed and values are split
    assert_eq!(result, vec!["brave", "firefox"]);
}

#[test]
fn multiselect_returns_empty_on_missing_field() {
    // GIVEN content without the requested field
    let content: BTreeMap<String, ElicitResultContent> = BTreeMap::new();
    // WHEN extracted
    let result = extract_multiselect_field(&content, "sources");
    // THEN empty vec is returned
    assert!(result.is_empty());
}

// ── build_structured ─────────────────────────────────────────────────────────

#[test]
fn build_structured_produces_correct_keys() {
    // GIVEN a set of key-value pairs
    // WHEN built into a structured map
    let map = build_structured([
        (
            "url",
            serde_json::Value::String("https://example.com".into()),
        ),
        ("status", serde_json::Value::Number(200.into())),
    ]);
    // THEN all keys are present with correct values
    assert_eq!(
        map["url"],
        serde_json::Value::String("https://example.com".into())
    );
    assert_eq!(map["status"], serde_json::Value::Number(200.into()));
}

#[test]
fn lock_subscribed_uris_recovers_after_mutex_poisoning() {
    let subscribed_uris = Arc::new(Mutex::new(HashSet::new()));
    let poisoned = Arc::clone(&subscribed_uris);

    let result = std::panic::catch_unwind(move || {
        let mut guard = poisoned.lock().expect("test subscription lock");
        guard.insert("nab://watch/demo".to_string());
        panic!("poison subscription set");
    });
    assert!(result.is_err());

    let recovered = super::lock_subscribed_uris(&subscribed_uris);
    assert!(recovered.contains("nab://watch/demo"));
}

// ── build_fetch_structured ───────────────────────────────────────────────────

#[test]
fn fetch_structured_has_all_required_fields() {
    // GIVEN a complete fetch result
    let map = build_fetch_structured_v2(&FetchStructuredParams {
        url: "https://example.com",
        status: 200,
        content_type: "text/html",
        markdown: "# Hello\n\nworld",
        timing_ms: 42.5,
        has_diff: false,
        omitted_sections: 0,
        total_sections: 0,
        truncated: false,
        full_tokens: 0,
        response_class: None,
        response_confidence: None,
        response_reason: None,
        thin_content_detected: false,
    });
    // WHEN inspected
    // THEN all outputSchema fields are present
    assert!(map.contains_key("url"));
    assert!(map.contains_key("status"));
    assert!(map.contains_key("content_type"));
    assert!(map.contains_key("content"));
    assert!(map.contains_key("timing_ms"));
    assert!(map.contains_key("has_diff"));
    assert_eq!(map["status"], serde_json::Value::Number(200.into()));
}

#[test]
fn fetch_output_schema_advertises_optional_metadata_fields() {
    let schema = crate::fetch_output_schema();
    let properties = schema.properties.expect("fetch schema properties");

    for key in [
        "omitted_sections",
        "total_sections",
        "truncated",
        "full_tokens",
        "response_class",
        "response_confidence",
        "response_reason",
        "thin_content_detected",
    ] {
        assert!(
            properties.contains_key(key),
            "fetch outputSchema should advertise optional field {key}"
        );
        assert!(
            !schema.required.iter().any(|required| required == key),
            "optional field {key} should not be required"
        );
    }
}

#[test]
fn fetch_structured_preserves_content_verbatim() {
    // GIVEN content passed to the structured builder
    // (truncation is now handled upstream by budget::truncate_to_budget)
    let content_str = "x".repeat(5000);
    let map = build_fetch_structured_v2(&FetchStructuredParams {
        url: "https://example.com",
        status: 200,
        content_type: "text/plain",
        markdown: &content_str,
        timing_ms: 10.0,
        has_diff: false,
        omitted_sections: 0,
        total_sections: 0,
        truncated: false,
        full_tokens: 0,
        response_class: None,
        response_confidence: None,
        response_reason: None,
        thin_content_detected: false,
    });
    // WHEN inspected
    // THEN content is stored verbatim (no internal truncation)
    let content = map["content"].as_str().unwrap();
    assert_eq!(content.len(), 5000);
}

#[test]
fn fetch_structured_includes_truncation_metadata_when_flagged() {
    // GIVEN a response marked as truncated with known full_tokens
    let map = build_fetch_structured_v2(&FetchStructuredParams {
        url: "https://example.com",
        status: 200,
        content_type: "text/html",
        markdown: "truncated content",
        timing_ms: 10.0,
        has_diff: false,
        omitted_sections: 0,
        total_sections: 0,
        truncated: true,
        full_tokens: 8000,
        response_class: None,
        response_confidence: None,
        response_reason: None,
        thin_content_detected: false,
    });
    // THEN truncation metadata fields are present
    assert_eq!(map["truncated"], serde_json::Value::Bool(true));
    assert_eq!(map["full_tokens"], serde_json::Value::Number(8000.into()));
}

#[test]
fn fetch_structured_includes_response_diagnostics_when_present() {
    let map = build_fetch_structured_v2(&FetchStructuredParams {
        url: "https://example.com",
        status: 403,
        content_type: "text/html",
        markdown: "challenge page",
        timing_ms: 10.0,
        has_diff: false,
        omitted_sections: 0,
        total_sections: 0,
        truncated: false,
        full_tokens: 0,
        response_class: Some("bot_challenge"),
        response_confidence: Some(0.97),
        response_reason: Some("browser-challenge or CAPTCHA markers detected"),
        thin_content_detected: true,
    });
    assert_eq!(
        map["response_class"],
        serde_json::Value::String("bot_challenge".into())
    );
    assert_eq!(map["thin_content_detected"], serde_json::Value::Bool(true));
    assert_eq!(
        map["response_reason"],
        serde_json::Value::String("browser-challenge or CAPTCHA markers detected".into())
    );
    assert!(
        map.contains_key("response_confidence"),
        "response_confidence should be present when provided"
    );
}

// ── server_icons ─────────────────────────────────────────────────────────────

#[test]
fn server_icons_returns_light_and_dark() {
    use rust_mcp_sdk::schema::IconTheme;
    // GIVEN the server icon list
    let icons = server_icons();
    // WHEN inspected
    // THEN both light and dark variants are present
    assert_eq!(icons.len(), 2);
    assert!(icons.iter().any(|i| i.theme == Some(IconTheme::Light)));
    assert!(icons.iter().any(|i| i.theme == Some(IconTheme::Dark)));
}

#[test]
fn server_icons_have_svg_mime_type() {
    for icon in server_icons() {
        assert_eq!(icon.mime_type.as_deref(), Some("image/svg+xml"));
        assert_eq!(icon.sizes, vec!["any"]);
        assert!(icon.src.starts_with("data:image/svg+xml;base64,"));
    }
}

// ── tool_annotations ─────────────────────────────────────────────────────────

#[test]
fn fetch_annotation_is_read_only() {
    // GIVEN the fetch tool name
    // WHEN annotations are generated
    let ann = crate::tool_annotations("fetch");
    // THEN it is read-only, non-destructive, and idempotent
    assert_eq!(ann.read_only_hint, Some(true));
    assert_eq!(ann.destructive_hint, Some(false));
    assert_eq!(ann.idempotent_hint, Some(true));
}

#[test]
fn submit_annotation_is_destructive() {
    // GIVEN the submit tool name
    let ann = crate::tool_annotations("submit");
    // THEN read_only=false, destructive=true, idempotent=false
    assert_eq!(ann.read_only_hint, Some(false));
    assert_eq!(ann.destructive_hint, Some(true));
    assert_eq!(ann.idempotent_hint, Some(false));
}

#[test]
fn login_annotation_is_non_destructive_write() {
    // GIVEN the login tool
    let ann = crate::tool_annotations("login");
    // THEN read_only=false, destructive=false, idempotent=false
    assert_eq!(ann.read_only_hint, Some(false));
    assert_eq!(ann.destructive_hint, Some(false));
    assert_eq!(ann.idempotent_hint, Some(false));
}

// ── all_prompts ───────────────────────────────────────────────────────────────

#[test]
fn all_prompts_returns_four_prompts() {
    // GIVEN the static prompt list
    let prompts = crate::all_prompts();
    // THEN exactly four prompts are present (fetch-and-extract, multi-page-research,
    // authenticated-fetch, match-speakers-with-hebb)
    assert_eq!(prompts.len(), 4);
}

#[test]
fn prompts_have_expected_names() {
    let prompts = crate::all_prompts();
    let names: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"fetch-and-extract"));
    assert!(names.contains(&"multi-page-research"));
    assert!(names.contains(&"authenticated-fetch"));
    assert!(names.contains(&"match-speakers-with-hebb"));
}

#[test]
fn match_speakers_prompt_is_listed() {
    // GIVEN the prompt list
    let prompts = crate::all_prompts();
    // WHEN searching for match-speakers-with-hebb
    let p = prompts
        .iter()
        .find(|p| p.name == "match-speakers-with-hebb");
    // THEN it exists with required `input` argument
    let p = p.expect("match-speakers-with-hebb must be in all_prompts()");
    let input_arg = p.arguments.iter().find(|a| a.name == "input");
    assert!(input_arg.is_some(), "must have 'input' argument");
    assert_eq!(
        input_arg.unwrap().required,
        Some(true),
        "'input' must be required"
    );
}

#[test]
fn match_speakers_prompt_result_contains_workflow_steps() {
    // GIVEN match-speakers-with-hebb with an input argument
    let mut args = std::collections::BTreeMap::new();
    args.insert("input".into(), "/tmp/meeting.wav".into());
    // WHEN rendered
    let result = crate::build_prompt_result("match-speakers-with-hebb", &args).unwrap();
    let rust_mcp_sdk::schema::ContentBlock::TextContent(tc) = &result.messages[0].content else {
        panic!("expected TextContent");
    };
    // THEN workflow mentions diarization + embeddings
    assert!(
        tc.text.contains("include_embeddings=true"),
        "text: {}",
        tc.text
    );
    assert!(tc.text.contains("voice_match"), "text: {}", tc.text);
    assert!(tc.text.contains("voice_remember"), "text: {}", tc.text);
    assert!(tc.text.contains("/tmp/meeting.wav"), "text: {}", tc.text);
}

#[test]
fn fetch_and_extract_prompt_has_two_required_args() {
    // GIVEN the fetch-and-extract prompt
    let prompts = crate::all_prompts();
    let p = prompts
        .iter()
        .find(|p| p.name == "fetch-and-extract")
        .unwrap();
    // THEN it has exactly 2 arguments, both required
    assert_eq!(p.arguments.len(), 2);
    assert!(p.arguments.iter().all(|a| a.required == Some(true)));
}

#[test]
fn authenticated_fetch_has_optional_auth_method() {
    // GIVEN the authenticated-fetch prompt
    let prompts = crate::all_prompts();
    let p = prompts
        .iter()
        .find(|p| p.name == "authenticated-fetch")
        .unwrap();
    // THEN auth_method argument is optional
    let auth_arg = p
        .arguments
        .iter()
        .find(|a| a.name == "auth_method")
        .unwrap();
    assert_eq!(auth_arg.required, Some(false));
}

// ── build_prompt_result ───────────────────────────────────────────────────────

#[test]
fn build_prompt_result_returns_none_for_unknown_name() {
    // GIVEN an unknown prompt name
    let args = std::collections::BTreeMap::new();
    // WHEN rendered
    let result = crate::build_prompt_result("no-such-prompt", &args);
    // THEN None is returned
    assert!(result.is_none());
}

#[test]
fn build_prompt_result_fetch_and_extract_interpolates_url() {
    // GIVEN url and extract_query arguments
    let mut args = std::collections::BTreeMap::new();
    args.insert("url".into(), "https://example.com".into());
    args.insert("extract_query".into(), "all headings".into());
    // WHEN rendered
    let result = crate::build_prompt_result("fetch-and-extract", &args).unwrap();
    // THEN the message contains the URL and query
    let rust_mcp_sdk::schema::ContentBlock::TextContent(tc) = &result.messages[0].content else {
        panic!("expected TextContent");
    };
    assert!(tc.text.contains("https://example.com"), "text: {}", tc.text);
    assert!(tc.text.contains("all headings"), "text: {}", tc.text);
}

#[test]
fn build_prompt_result_multi_page_research_uses_fetch_batch_hint() {
    // GIVEN urls and question
    let mut args = std::collections::BTreeMap::new();
    args.insert("urls".into(), "https://a.com, https://b.com".into());
    args.insert("question".into(), "What is the price?".into());
    // WHEN rendered
    let result = crate::build_prompt_result("multi-page-research", &args).unwrap();
    let rust_mcp_sdk::schema::ContentBlock::TextContent(tc) = &result.messages[0].content else {
        panic!("expected TextContent");
    };
    // THEN message references fetch_batch
    assert!(tc.text.contains("fetch_batch"), "text: {}", tc.text);
}

#[test]
fn build_prompt_result_authenticated_fetch_uses_automatic_browser_cookies() {
    // GIVEN only url provided (no auth_method)
    let mut args = std::collections::BTreeMap::new();
    args.insert("url".into(), "https://secure.example.com".into());
    // WHEN rendered without auth_method
    let result = crate::build_prompt_result("authenticated-fetch", &args).unwrap();
    let rust_mcp_sdk::schema::ContentBlock::TextContent(tc) = &result.messages[0].content else {
        panic!("expected TextContent");
    };
    // THEN it describes automatic browser-cookie behavior instead of CLI flags
    assert!(
        tc.text.contains("automatically uses cookies"),
        "text: {}",
        tc.text
    );
    assert!(tc.text.contains("cookies = \"none\""), "text: {}", tc.text);
    assert!(!tc.text.contains("--cookies"), "text: {}", tc.text);
}

#[test]
fn build_prompt_result_authenticated_fetch_uses_login_session_flow() {
    // GIVEN auth_method = "1password"
    let mut args = std::collections::BTreeMap::new();
    args.insert("url".into(), "https://secure.example.com".into());
    args.insert("auth_method".into(), "1password".into());
    // WHEN rendered
    let result = crate::build_prompt_result("authenticated-fetch", &args).unwrap();
    let rust_mcp_sdk::schema::ContentBlock::TextContent(tc) = &result.messages[0].content else {
        panic!("expected TextContent");
    };
    // THEN it points to the login + session workflow
    assert!(tc.text.contains("login"), "text: {}", tc.text);
    assert!(tc.text.contains("session"), "text: {}", tc.text);
    assert!(tc.text.contains("1Password"), "text: {}", tc.text);
    assert!(!tc.text.contains("--1password"), "text: {}", tc.text);
}

// ── static_resources ──────────────────────────────────────────────────────────

#[test]
fn static_resources_returns_two_resources() {
    // GIVEN the static resource list (excludes dynamic watch resources)
    let resources = crate::static_resources();
    // THEN exactly two static resources are present
    assert_eq!(resources.len(), 2);
}

#[test]
fn static_resources_have_expected_uris() {
    let resources = crate::static_resources();
    let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
    assert!(uris.contains(&"nab://guide/quickstart"));
    assert!(uris.contains(&"nab://status"));
}

// ── static_resource_content ───────────────────────────────────────────────────

#[test]
fn static_resource_content_returns_none_for_unknown_uri() {
    assert!(crate::static_resource_content("nab://unknown").is_none());
}

#[test]
fn quickstart_resource_contains_key_sections() {
    // GIVEN the quickstart guide
    let content = crate::static_resource_content("nab://guide/quickstart").unwrap();
    // THEN key sections are present
    assert!(content.contains("Basic Fetch"));
    assert!(content.contains("Batch Fetch"));
    assert!(content.contains("Authentication"));
}

#[test]
fn quickstart_resource_describes_automatic_cookie_defaults() {
    let content = crate::static_resource_content("nab://guide/quickstart").unwrap();
    assert!(content.contains("automatically try cookies from the default browser"));
    assert!(content.contains("cookies = \"none\""));
}

#[test]
fn status_resource_contains_version() {
    // GIVEN the status resource
    let content = crate::static_resource_content("nab://status").unwrap();
    // THEN it includes the crate version
    assert!(content.contains(env!("CARGO_PKG_VERSION")));
    assert!(content.contains("running"));
}

// ── apply_diff ────────────────────────────────────────────────────────────────

use crate::tools::apply_diff_with_store;
use nab::content::snapshot_store::SnapshotStore;
use tempfile::TempDir;

fn tmp_store() -> (TempDir, SnapshotStore) {
    let dir = tempfile::tempdir().expect("tmp dir");
    let store = SnapshotStore::with_root(dir.path());
    (dir, store)
}

#[test]
fn apply_diff_first_fetch_returns_first_fetch_prefix() {
    // GIVEN: no prior snapshot exists for the URL
    let (_dir, store) = tmp_store();
    // WHEN: apply_diff called on a fresh URL
    let (output, has_diff) =
        apply_diff_with_store(&store, "https://example.com/first", "Hello world.");
    // THEN: output signals first fetch and has_diff is false
    assert!(output.starts_with("First fetch"), "got: {output}");
    assert!(!has_diff);
}

#[test]
fn apply_diff_unchanged_content_returns_no_changes() {
    // GIVEN: a prior snapshot already stored with identical content
    let (_dir, store) = tmp_store();
    let url = "https://example.com/unchanged";
    let content = "Same content forever.";
    let (_, _) = apply_diff_with_store(&store, url, content); // prime
    // WHEN: fetched again with identical content
    let (output, has_diff) = apply_diff_with_store(&store, url, content);
    // THEN: no-change confirmation returned, has_diff false
    assert!(output.starts_with("No changes"), "got: {output}");
    assert!(!has_diff);
}

#[test]
fn apply_diff_changed_content_returns_diff_with_has_diff_true() {
    // GIVEN: a prior snapshot with different content
    let (_dir, store) = tmp_store();
    let url = "https://example.com/changed";
    apply_diff_with_store(&store, url, "Old paragraph.\n\nShared footer.");
    // WHEN: fetched with new content
    let (output, has_diff) = apply_diff_with_store(&store, url, "New paragraph.\n\nShared footer.");
    // THEN: diff output returned and has_diff is true
    assert!(
        output.starts_with("Changed since last fetch"),
        "got: {output}"
    );
    assert!(has_diff);
}
