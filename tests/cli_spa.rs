//! Integration tests for the `nab spa` command.
//!
//! SPA extraction requires a JS engine and real page content, so these tests
//! focus on argument parsing, error handling, and verifying the command starts
//! correctly. Regressions here also cover the JavaScript fallback path for
//! pages without embedded JSON so the command exits cleanly instead of
//! panicking inside async execution.

#![allow(deprecated)] // cargo_bin deprecation — replacement not yet stable

use assert_cmd::Command;
use predicates::prelude::*;

/// Helper: get a Command for the `nab` binary.
fn nab() -> Command {
    Command::cargo_bin("nab").expect("binary 'nab' should be built")
}

/// Returns `true` when network integration tests are enabled.
fn net_tests_enabled() -> bool {
    std::env::var("NAB_NET_TESTS").map_or(true, |v| v != "0" && v.to_lowercase() != "false")
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
        .stdout(predicate::str::contains("--patterns"))
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

    // example.com has no embedded SPA data, so this exercises the full
    // JavaScript fallback path without relying on framework-specific globals.
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

    assert!(
        output.status.success(),
        "SPA command should exit successfully, got: {:?}",
        output.status
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Status messages go to stderr (data-only on stdout since v0.6.0)
    assert!(
        stderr.contains("Extracting SPA data from") || stderr.contains("example.com"),
        "SPA command should start extraction pipeline, got stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Cannot drop a runtime"),
        "SPA command should not panic on Tokio runtime shutdown: {stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "SPA command should not panic during fallback execution: {stderr}"
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
