//! Integration tests for the `nab stream` command.
//!
//! Streaming tests are limited to argument validation and `--info` mode
//! (which queries metadata without downloading). Actual media downloads
//! are not tested because they require significant bandwidth and time.

#![allow(deprecated)] // cargo_bin deprecation — replacement not yet stable

use assert_cmd::Command;
use predicates::prelude::*;

/// Helper: get a Command for the `nab` binary.
fn nab() -> Command {
    Command::cargo_bin("nab").expect("binary 'nab' should be built")
}

// ─── Argument validation ─────────────────────────────────────────────────────

#[test]
fn stream_missing_source_fails() {
    nab()
        .arg("stream")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<SOURCE>"));
}

#[test]
fn stream_missing_id_fails() {
    nab()
        .args(["stream", "generic"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("<ID>"));
}

#[test]
fn stream_help_shows_all_options() {
    nab()
        .args(["stream", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--quality"))
        .stdout(predicate::str::contains("--native"))
        .stdout(predicate::str::contains("--ffmpeg"))
        .stdout(predicate::str::contains("--info"))
        .stdout(predicate::str::contains("--list"))
        .stdout(predicate::str::contains("--duration"))
        .stdout(predicate::str::contains("--player"));
}

#[cfg(unix)]
#[test]
fn stream_info_piped_to_closed_stdout_exits_cleanly() {
    let nab_bin = assert_cmd::cargo::cargo_bin("nab");
    let mut shell = std::process::Command::new("bash");
    shell
        .arg("-lc")
        .arg("set -o pipefail; \"$NAB_BIN\" stream generic https://example.com/stream.m3u8 --info | true")
        .env("NAB_BIN", nab_bin);

    Command::from_std(shell)
        .assert()
        .success()
        .stderr(predicate::str::contains("panicked").not());
}

// ─── Analyze/Annotate argument validation ────────────────────────────────────

#[test]
fn analyze_missing_video_fails() {
    nab()
        .arg("analyze")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<VIDEO>"));
}

#[test]
fn annotate_missing_video_fails() {
    nab()
        .arg("annotate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<VIDEO>"));
}

#[test]
fn annotate_missing_output_fails() {
    nab()
        .args(["annotate", "input.mp4"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("<OUTPUT>"));
}
