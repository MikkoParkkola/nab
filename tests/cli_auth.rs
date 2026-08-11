//! Integration tests for the `nab auth` and `nab otp` commands.
//!
//! These commands interact with 1Password and system-level OTP sources in
//! production. Tests shadow those tools with deterministic failing stubs so
//! the default suite can only exercise graceful degradation and never invoke
//! the user's real credential tools.

#![allow(deprecated)] // cargo_bin deprecation — replacement not yet stable

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

/// Helper: get a Command for the `nab` binary.
fn nab() -> Command {
    Command::cargo_bin("nab").expect("binary 'nab' should be built")
}

fn isolate_external_tools(command: &mut Command) -> TempDir {
    let tool_dir = tempfile::tempdir().expect("create isolated tool directory");
    write_failing_tool(tool_dir.path(), "op");
    write_failing_tool(tool_dir.path(), "mcp-cli");

    let mut paths = vec![tool_dir.path().to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    command.env(
        "PATH",
        std::env::join_paths(paths).expect("build isolated PATH"),
    );
    tool_dir
}

#[cfg(unix)]
fn write_failing_tool(directory: &Path, name: &str) {
    use std::os::unix::fs::PermissionsExt;

    let path = directory.join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 1\n").expect("write failing tool stub");
    let mut permissions = std::fs::metadata(&path)
        .expect("read failing tool metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("make failing tool executable");
}

#[cfg(windows)]
fn write_failing_tool(directory: &Path, name: &str) {
    std::fs::write(directory.join(format!("{name}.cmd")), "@exit /b 1\r\n")
        .expect("write failing tool stub");
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
    // Real credential tools are shadowed on PATH, so this exercises the
    // deterministic unavailable-provider path without authentication UI.
    let mut command = nab();
    let _tool_dir = isolate_external_tools(&mut command);
    let output = command
        .args(["auth", "https://example.com"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("command should execute");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // The command should report graceful unavailability without crashing.
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
    // All external OTP providers are shadowed on PATH.
    let mut command = nab();
    let _tool_dir = isolate_external_tools(&mut command);
    let output = command
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
    let mut command = nab();
    let _tool_dir = isolate_external_tools(&mut command);
    let output = command
        .args(["otp", "https://accounts.example.com/login"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "otp should degrade cleanly: {:?}",
        output.status
    );
}
