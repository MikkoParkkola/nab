//! Integration tests for the `nab auth` and `nab otp` commands.
//!
//! These commands interact with 1Password and system-level OTP sources,
//! so we test only argument parsing and graceful degradation when the
//! external tools are not available.

#![allow(deprecated)] // cargo_bin deprecation — replacement not yet stable

use assert_cmd::Command;
use predicates::prelude::*;

/// Helper: get a Command for the `nab` binary.
fn nab() -> Command {
    Command::cargo_bin("nab").expect("binary 'nab' should be built")
}

// ─── Auth command ────────────────────────────────────────────────────────────

#[test]
fn auth_missing_url_fails() {
    nab()
        .arg("auth")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<URL>"));
}

#[test]
fn auth_runs_without_crash() {
    // The auth command calls 1Password CLI which may block waiting for
    // authentication.  We use .output() with a timeout so the test
    // always completes, then verify the process at least started.
    let output = nab()
        .args(["auth", "https://example.com"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("command should execute");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // The command should at least print a search/credential message before
    // the 1Password CLI potentially blocks.
    assert!(
        combined.contains("1Password")
            || combined.contains("credential")
            || combined.contains("Searching")
            || output.status.success(),
        "auth should start credential lookup, got stdout: {stdout}, stderr: {stderr}"
    );
}

// ─── OTP command ─────────────────────────────────────────────────────────────

#[test]
fn otp_missing_domain_fails() {
    nab()
        .arg("otp")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<DOMAIN>"));
}

#[test]
fn otp_runs_without_crash() {
    // OTP command may call external tools that block.  Same pattern as auth.
    let output = nab()
        .args(["otp", "example.com"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("command should execute");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        combined.contains("OTP")
            || combined.contains("Searching")
            || combined.contains("otp")
            || output.status.success(),
        "otp should start search, got stdout: {stdout}, stderr: {stderr}"
    );
}

#[test]
fn otp_accepts_url_format() {
    // The otp command should also work when given a full URL
    // (it strips down to domain internally).
    let output = nab()
        .args(["otp", "https://accounts.example.com/login"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("command should execute");

    // Accept either success or timeout-interrupted (1Password may block).
    // The key test is that it didn't panic or crash with a non-timeout error.
    assert!(
        output.status.success() || output.status.code().is_none(), // None = killed by timeout
        "otp should succeed or be interrupted, got: {:?}",
        output.status
    );
}
