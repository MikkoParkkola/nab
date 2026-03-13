//! Integration tests for the `nab-mcp` helper modules.
//!
//! Tests for `elicitation` and `structured` module functions,
//! co-located here so they can reference any `crate::` module freely.

use std::collections::HashMap;

use rust_mcp_sdk::schema::{ElicitResultContent, ElicitResultContentPrimitive};

use crate::elicitation::{extract_multiselect_field, is_oauth_redirect};
use crate::structured::{build_fetch_structured, build_structured, server_icons};

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
    assert!(is_oauth_redirect("https://github.com/login/oauth/authorize?client_id=abc"));
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
    assert!(is_oauth_redirect("https://ACCOUNTS.GOOGLE.COM/o/oauth2/auth"));
}

// ── extract_multiselect_field ────────────────────────────────────────────────

#[test]
fn multiselect_parses_json_array() {
    // GIVEN a JSON-encoded array string in the content map
    let mut content = HashMap::new();
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
    let mut content = HashMap::new();
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
    let content: HashMap<String, ElicitResultContent> = HashMap::new();
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
        ("url", serde_json::Value::String("https://example.com".into())),
        ("status", serde_json::Value::Number(200.into())),
    ]);
    // THEN all keys are present with correct values
    assert_eq!(map["url"], serde_json::Value::String("https://example.com".into()));
    assert_eq!(map["status"], serde_json::Value::Number(200.into()));
}

// ── build_fetch_structured ───────────────────────────────────────────────────

#[test]
fn fetch_structured_has_all_required_fields() {
    // GIVEN a complete fetch result
    let map = build_fetch_structured(
        "https://example.com",
        200,
        "text/html",
        "# Hello\n\nworld",
        42.5,
    );
    // WHEN inspected
    // THEN all outputSchema fields are present
    assert!(map.contains_key("url"));
    assert!(map.contains_key("status"));
    assert!(map.contains_key("content_type"));
    assert!(map.contains_key("content"));
    assert!(map.contains_key("timing_ms"));
    assert_eq!(map["status"], serde_json::Value::Number(200.into()));
}

#[test]
fn fetch_structured_truncates_long_content() {
    // GIVEN content longer than 4000 chars
    let long_content = "x".repeat(5000);
    let map = build_fetch_structured("https://example.com", 200, "text/plain", &long_content, 10.0);
    // WHEN inspected
    // THEN content is truncated
    let content = map["content"].as_str().unwrap();
    assert!(content.len() < 5000);
    assert!(content.contains("truncated"));
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
