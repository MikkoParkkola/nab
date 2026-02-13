//! Integration tests for the `nab fetch` command.
//!
//! Uses httpbin.org for deterministic responses.  Tests that require network
//! access are gated behind the `NAB_NET_TESTS` env var so CI can skip them
//! when offline.

#![allow(deprecated)] // cargo_bin deprecation — replacement not yet stable

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

/// Helper: get a Command for the `nab` binary.
fn nab() -> Command {
    Command::cargo_bin("nab").expect("binary 'nab' should be built")
}

/// Returns `true` when network integration tests are enabled.
fn net_tests_enabled() -> bool {
    // Default to running network tests unless explicitly disabled.
    std::env::var("NAB_NET_TESTS")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true)
}

// ─── Basic fetch (network) ───────────────────────────────────────────────────

#[test]
fn fetch_example_dot_com_full_format() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args(["fetch", "--cookies", "none", "https://example.com"])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("Fetching:"))
        .stdout(predicate::str::contains("Response:"))
        .stdout(predicate::str::contains("Status:"))
        .stdout(predicate::str::contains("Body:"));
}

#[test]
fn fetch_compact_format() {
    if !net_tests_enabled() {
        return;
    }

    // Compact format outputs: STATUS SIZE TIME
    nab()
        .args([
            "fetch",
            "--format",
            "compact",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"200 \d+B \d+").unwrap());
}

#[test]
fn fetch_json_format() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--format",
            "json",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""status":200"#))
        .stdout(predicate::str::contains(r#""url":"https://example.com""#));
}

#[test]
fn fetch_with_headers_flag() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args(["fetch", "-H", "--cookies", "none", "https://example.com"])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("Headers:"))
        .stdout(predicate::str::contains("content-type"));
}

#[test]
fn fetch_body_flag_shows_content() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--body",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        // example.com body contains "Example Domain" - should appear as markdown
        .stdout(predicate::str::contains("Example Domain"));
}

#[test]
fn fetch_raw_html_flag() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--body",
            "--raw-html",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        // Raw HTML should still contain the text, but without markdown conversion
        .stdout(predicate::str::contains("Example Domain"));
}

#[test]
fn fetch_links_flag() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--links",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        // example.com has a link to iana.org
        .stdout(predicate::str::contains("iana.org"))
        .stdout(predicate::str::contains("links)"));
}

#[test]
fn fetch_output_to_file() {
    if !net_tests_enabled() {
        return;
    }

    let tmp = std::env::temp_dir().join("nab_test_output.html");
    // Clean up from previous runs
    let _ = fs::remove_file(&tmp);

    nab()
        .args([
            "fetch",
            "--output",
            tmp.to_str().unwrap(),
            "--raw-html",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("Saved"));

    // Verify the file was created with content
    let content = fs::read_to_string(&tmp).expect("output file should exist");
    assert!(
        content.contains("Example Domain"),
        "saved file should contain page content"
    );
    assert!(
        content.len() > 100,
        "saved file should have substantial content"
    );

    // Clean up
    let _ = fs::remove_file(&tmp);
}

#[test]
fn fetch_custom_method_head() {
    if !net_tests_enabled() {
        return;
    }

    // HEAD request should succeed (no body)
    nab()
        .args([
            "fetch",
            "-X",
            "HEAD",
            "--format",
            "compact",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        // HEAD returns 200 with 0 bytes body
        .stdout(predicate::str::is_match(r"200 0B \d+").unwrap());
}

#[test]
fn fetch_custom_header() {
    if !net_tests_enabled() {
        return;
    }

    // httpbin.org/headers echoes request headers in JSON
    nab()
        .args([
            "fetch",
            "--body",
            "--raw-html",
            "--add-header",
            "X-Nab-Test: integration",
            "--cookies",
            "none",
            "https://httpbin.org/headers",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("X-Nab-Test"));
}

#[test]
fn fetch_max_body_truncates() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--body",
            "--max-body",
            "50",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("more bytes]"));
}

// ─── Error handling ──────────────────────────────────────────────────────────

#[test]
fn fetch_invalid_url_fails() {
    nab()
        .args(["fetch", "--cookies", "none", "not-a-url"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .failure();
}

#[test]
fn fetch_unreachable_host_fails() {
    nab()
        .args([
            "fetch",
            "--cookies",
            "none",
            "https://this-domain-does-not-exist-12345.example",
        ])
        .timeout(std::time::Duration::from_secs(15))
        .assert()
        .failure();
}

// ─── Cookie flag parsing ─────────────────────────────────────────────────────

#[test]
fn fetch_cookies_none_works() {
    if !net_tests_enabled() {
        return;
    }

    // "none" should skip cookie loading entirely
    nab()
        .args([
            "fetch",
            "--cookies",
            "none",
            "--format",
            "compact",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^200 ").unwrap());
}

#[test]
fn fetch_cookies_flag_accepts_browser_names() {
    // These should be accepted as valid values without crashing on argument
    // parsing. The actual cookie extraction may or may not work depending on
    // local browser state, but the flags should be accepted.
    for browser in &["brave", "chrome", "firefox", "safari", "edge"] {
        nab()
            .args(["fetch", "--cookies", browser, "--help"])
            .assert()
            .success();
    }
}

// ─── No-redirect flag ────────────────────────────────────────────────────────

#[test]
fn fetch_no_redirect_captures_302() {
    if !net_tests_enabled() {
        return;
    }

    // httpbin.org/redirect/1 issues a 302 redirect
    nab()
        .args([
            "fetch",
            "--no-redirect",
            "--format",
            "compact",
            "--cookies",
            "none",
            "https://httpbin.org/redirect/1",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^302 ").unwrap());
}

// ─── POST with data ─────────────────────────────────────────────────────────

#[test]
fn fetch_post_with_data() {
    if !net_tests_enabled() {
        return;
    }

    // httpbin.org/post echoes back the posted data
    nab()
        .args([
            "fetch",
            "-X",
            "POST",
            "-d",
            r#"{"key":"value"}"#,
            "--body",
            "--raw-html",
            "--cookies",
            "none",
            "https://httpbin.org/post",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        // httpbin echoes posted data; the json field will contain parsed key/value
        .stdout(predicate::str::contains(r#""key": "value""#));
}
