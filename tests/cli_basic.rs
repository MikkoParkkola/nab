//! Integration tests for basic CLI behavior.
//!
//! Tests that the binary exists, accepts standard flags, and each subcommand
//! responds to `--help` with appropriate text.

#![allow(deprecated)] // cargo_bin deprecation — replacement not yet stable

use assert_cmd::Command;
use predicates::prelude::*;

/// Helper: get a Command for the `nab` binary.
fn nab() -> Command {
    Command::cargo_bin("nab").expect("binary 'nab' should be built")
}

// ─── Top-level flags ─────────────────────────────────────────────────────────

#[test]
fn help_flag_shows_usage() {
    nab()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: nab"))
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("spa"))
        .stdout(predicate::str::contains("stream"))
        .stdout(predicate::str::contains("analyze"))
        .stdout(predicate::str::contains("annotate"))
        .stdout(predicate::str::contains("otp"));
}

#[test]
fn short_help_flag_shows_usage() {
    nab()
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: nab"));
}

#[test]
fn version_flag_shows_semver() {
    nab()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^nab \d+\.\d+\.\d+\n$").unwrap());
}

#[test]
fn short_version_flag_shows_semver() {
    nab()
        .arg("-V")
        .assert()
        .success()
        .stdout(predicate::str::contains("nab "));
}

#[test]
fn no_args_shows_error_and_usage() {
    nab()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage: nab"));
}

#[test]
fn invalid_subcommand_fails() {
    nab()
        .arg("this-is-not-a-real-command")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

// ─── Subcommand help ─────────────────────────────────────────────────────────

#[test]
fn fetch_help() {
    nab()
        .args(["fetch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Fetch a URL"))
        .stdout(predicate::str::contains("<URL>"))
        .stdout(predicate::str::contains("--cookies"))
        .stdout(predicate::str::contains("--raw-html"))
        .stdout(predicate::str::contains("--method"));
}

#[test]
fn spa_help() {
    nab()
        .args(["spa", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Extract data from JavaScript-heavy SPA pages",
        ))
        .stdout(predicate::str::contains("<URL>"))
        .stdout(predicate::str::contains("--extract"))
        .stdout(predicate::str::contains("--summary"));
}

#[test]
fn stream_help() {
    nab()
        .args(["stream", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Stream media"))
        .stdout(predicate::str::contains("<SOURCE>"))
        .stdout(predicate::str::contains("<ID>"))
        .stdout(predicate::str::contains("--quality"));
}

#[test]
fn analyze_help() {
    nab()
        .args(["analyze", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Analyze video"))
        .stdout(predicate::str::contains("<VIDEO>"))
        .stdout(predicate::str::contains("--audio-only"))
        .stdout(predicate::str::contains("--diarize"));
}

#[test]
fn annotate_help() {
    nab()
        .args(["annotate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Add overlays to video"))
        .stdout(predicate::str::contains("<VIDEO>"))
        .stdout(predicate::str::contains("--subtitles"))
        .stdout(predicate::str::contains("--speaker-labels"));
}

#[test]
fn otp_help() {
    nab()
        .args(["otp", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Get OTP code"))
        .stdout(predicate::str::contains("<DOMAIN>"));
}

#[test]
fn fingerprint_help() {
    nab()
        .args(["fingerprint", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("browser fingerprint"))
        .stdout(predicate::str::contains("--count"));
}

#[test]
fn bench_help() {
    nab()
        .args(["bench", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Benchmark"))
        .stdout(predicate::str::contains("<URLS>"))
        .stdout(predicate::str::contains("--iterations"));
}

#[test]
fn auth_help() {
    nab()
        .args(["auth", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1Password"))
        .stdout(predicate::str::contains("<URL>"));
}

// ─── Subcommand argument validation ──────────────────────────────────────────

#[test]
fn fetch_missing_url_fails() {
    nab()
        .arg("fetch")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<URL>"));
}

#[test]
fn spa_missing_url_fails() {
    nab()
        .arg("spa")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<URL>"));
}

#[test]
fn stream_missing_args_fails() {
    nab()
        .arg("stream")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<SOURCE>"));
}

#[test]
fn analyze_missing_video_fails() {
    nab()
        .arg("analyze")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<VIDEO>"));
}

#[test]
fn annotate_missing_args_fails() {
    nab()
        .arg("annotate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<VIDEO>"));
}

#[test]
fn otp_missing_domain_fails() {
    nab()
        .arg("otp")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<DOMAIN>"));
}

#[test]
fn auth_missing_url_fails() {
    nab()
        .arg("auth")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<URL>"));
}

#[test]
fn bench_missing_urls_fails() {
    nab()
        .arg("bench")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<URLS>"));
}

// ─── Invalid flag combinations ───────────────────────────────────────────────

#[test]
fn fetch_invalid_format_fails() {
    nab()
        .args(["fetch", "--format", "xml", "https://example.com"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn analyze_invalid_format_fails() {
    nab()
        .args(["analyze", "--format", "csv", "video.mp4"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn annotate_invalid_style_fails() {
    nab()
        .args(["annotate", "--style", "neon", "in.mp4", "out.mp4"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}
