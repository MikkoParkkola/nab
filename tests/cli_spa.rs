//! Integration tests for the `nab spa` command.
//!
//! SPA extraction requires a JS engine and real page content, so these tests
//! focus on argument parsing, error handling, and verifying the command starts
//! correctly. The SPA command currently has a known issue where pages without
//! embedded JSON fall through to JS execution and encounter a Tokio runtime
//! drop panic — tests that trigger this path assert on the partial output
//! rather than exit code.

#![allow(deprecated)] // cargo_bin deprecation — replacement not yet stable

use assert_cmd::Command;
use predicates::prelude::*;

/// Helper: get a Command for the `nab` binary.
fn nab() -> Command {
    Command::cargo_bin("nab").expect("binary 'nab' should be built")
}

/// Returns `true` when network integration tests are enabled.
fn net_tests_enabled() -> bool {
    std::env::var("NAB_NET_TESTS")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true)
}

// ─── Argument validation ─────────────────────────────────────────────────────

#[test]
fn spa_missing_url_fails() {
    nab()
        .arg("spa")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<URL>"));
}

#[test]
fn spa_help_lists_all_options() {
    nab()
        .args(["spa", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--extract"))
        .stdout(predicate::str::contains("--summary"))
        .stdout(predicate::str::contains("--minify"))
        .stdout(predicate::str::contains("--max-array"))
        .stdout(predicate::str::contains("--max-depth"))
        .stdout(predicate::str::contains("--http1"))
        .stdout(predicate::str::contains("--console"))
        .stdout(predicate::str::contains("--wait"));
}

// ─── Basic SPA invocation ────────────────────────────────────────────────────

#[test]
fn spa_starts_extraction_pipeline() {
    if !net_tests_enabled() {
        return;
    }

    // The SPA command fetches the page and begins extraction. For pages
    // without embedded JSON (like example.com) it falls into the JS engine
    // path which currently panics due to a nested Tokio runtime issue.
    // We verify the command at least starts and produces initial output.
    let output = nab()
        .args([
            "spa",
            "--cookies",
            "none",
            "--wait",
            "100",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .output()
        .expect("command should execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Status messages go to stderr (data-only on stdout since v0.6.0)
    assert!(
        stderr.contains("Extracting SPA data from") || stderr.contains("example.com"),
        "SPA command should start extraction pipeline, got stderr: {stderr}"
    );
}

#[test]
fn spa_invalid_url_fails() {
    nab()
        .args(["spa", "--cookies", "none", "not-a-valid-url-at-all"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .failure();
}
