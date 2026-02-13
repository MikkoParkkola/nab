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
    // The auth command should succeed (exit 0) even if 1Password is not
    // available -- it prints an error message to stdout but does not fail.
    nab()
        .args(["auth", "https://example.com"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1Password")
                .or(predicate::str::contains("credential"))
                .or(predicate::str::contains("Searching")),
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
    // OTP should succeed (exit 0) even without available OTP sources.
    // It prints to stdout what it searched.
    nab()
        .args(["otp", "example.com"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success()
        .stdout(predicate::str::contains("OTP").or(predicate::str::contains("Searching")));
}

#[test]
fn otp_accepts_url_format() {
    // The otp command should also work when given a full URL
    // (it strips down to domain internally).
    nab()
        .args(["otp", "https://accounts.example.com/login"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();
}
