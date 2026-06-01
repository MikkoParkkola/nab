//! `nab doctor` subcommand — environment diagnostics.
//!
//! Today this focuses on one concrete, frequently-hit problem: when `nab` is
//! installed via more than one channel (e.g. a Homebrew tap *and*
//! `cargo install`), the binary that wins on `PATH` may not be the newest one.
//! A common symptom is `nab --version` reporting a stale version because an
//! earlier `PATH` entry (often `/opt/homebrew/bin`) shadows `~/.cargo/bin`.
//!
//! The diagnostic is split into a pure analysis function ([`analyze_shadow`])
//! and the thin I/O edges that feed it ([`enumerate_path_binaries`],
//! [`probe_version`]). The pure function is deterministic and unit-tested; the
//! edges enumerate `PATH`, canonicalize entries, and best-effort probe each
//! candidate's `--version`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use super::upgrade::Version;

/// Platform-specific binary file name for `nab`.
#[cfg(windows)]
pub const BINARY_NAME: &str = "nab.exe";
/// Platform-specific binary file name for `nab`.
#[cfg(not(windows))]
pub const BINARY_NAME: &str = "nab";

/// A single `nab` binary discovered on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathBinary {
    /// Canonicalized absolute path to the binary (symlinks resolved).
    pub path: PathBuf,
    /// Version reported by the binary, when it could be determined.
    pub version: Option<Version>,
}

/// Outcome of analysing the `nab` binaries found on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowReport {
    /// All distinct `nab` binaries found on `PATH`, in `PATH` precedence order.
    pub binaries: Vec<PathBinary>,
    /// `true` when a different binary than the running one wins `PATH`
    /// precedence — i.e. the running binary is shadowed.
    pub shadowed: bool,
    /// `true` when the winning binary's version is strictly older than the
    /// running binary's version. Only meaningful when [`Self::shadowed`].
    pub winner_is_stale: bool,
}

impl ShadowReport {
    /// `true` when the running binary is shadowed by an older one — the exact
    /// situation reported in issue #105 and the only case warranting a warning.
    #[must_use]
    pub fn is_problem(&self) -> bool {
        self.shadowed && self.winner_is_stale
    }
}

/// Analyse the `nab` binaries on `PATH` relative to the running binary.
///
/// This is the pure core of the diagnostic: it performs no I/O so it can be
/// unit-tested deterministically. Callers supply the already-canonicalized,
/// `PATH`-ordered list of candidate binaries plus the canonicalized path and
/// version of the running binary.
///
/// `entries` must be in `PATH` precedence order (first = highest precedence)
/// and already de-duplicated by canonical path. The first entry is the binary
/// that actually runs when the user types `nab`.
///
/// # Examples
///
/// ```
/// # // Doctests cannot reach the private `analyze_shadow`; see unit tests.
/// ```
#[must_use]
pub fn analyze_shadow(
    entries: &[PathBinary],
    current_path: &Path,
    current_version: Version,
) -> ShadowReport {
    let winner = entries.first();
    let shadowed = winner.is_some_and(|w| w.path != current_path);
    let winner_is_stale = shadowed
        && winner
            .and_then(|w| w.version)
            .is_some_and(|wv| wv < current_version);

    ShadowReport {
        binaries: entries.to_vec(),
        shadowed,
        winner_is_stale,
    }
}

/// Enumerate every `nab` binary reachable via `PATH`, in precedence order.
///
/// Entries are canonicalized (resolving Homebrew's symlinks) and de-duplicated
/// by canonical path while preserving first-seen precedence order. The version
/// of each binary is resolved with `probe`, a best-effort resolver that may
/// return `None` when the binary cannot be executed.
///
/// `path_var` is the raw `PATH` value; splitting uses [`std::env::split_paths`]
/// so the platform path separator (`:` on Unix, `;` on Windows) is honoured.
fn enumerate_path_binaries(
    path_var: &std::ffi::OsStr,
    probe: impl Fn(&Path) -> Option<Version>,
) -> Vec<PathBinary> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out: Vec<PathBinary> = Vec::new();

    for dir in std::env::split_paths(path_var) {
        let candidate = dir.join(BINARY_NAME);
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if seen.contains(&canonical) {
            continue;
        }
        seen.push(canonical.clone());
        let version = probe(&canonical);
        out.push(PathBinary {
            path: canonical,
            version,
        });
    }

    out
}

/// Best-effort probe of a binary's version by executing `<path> --version`.
///
/// Parses the trailing whitespace-separated token of stdout (the `clap`
/// `--version` line is `"nab X.Y.Z"`). Returns `None` on any execution or parse
/// failure — the caller treats an unknown version as "could not determine".
///
/// # Security
///
/// This executes a binary already present on the user's `PATH`. The
/// highest-precedence such binary is what already runs when the user types
/// `nab`, so probing it for `--version` introduces no privilege beyond the
/// status quo.
fn probe_version(path: &Path) -> Option<Version> {
    let output = Command::new(path).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let token = stdout.split_whitespace().next_back()?;
    Version::parse(token).ok()
}

/// Best-effort uninstall command for the install channel that owns `path`.
///
/// Maps a binary's canonical location to the command that removes it, so the
/// runtime hint names the *correct* channel for the winner rather than assuming
/// Homebrew. Returns `None` for unrecognised locations (e.g. a hand-placed
/// binary), in which case the caller prints a generic path-based hint.
///
/// Recognised:
/// - `cargo` — any path under a `.cargo/bin` directory
/// - `brew`  — Homebrew prefixes (`/opt/homebrew`, `/usr/local/Cellar`,
///   `linuxbrew`)
#[must_use]
pub fn uninstall_hint(path: &Path) -> Option<&'static str> {
    let s = path.to_string_lossy();
    if s.contains("/.cargo/bin/") || s.contains("\\.cargo\\bin\\") {
        Some("cargo uninstall nab")
    } else if s.contains("/Cellar/") || s.contains("/opt/homebrew/") || s.contains("linuxbrew") {
        Some("brew uninstall nab")
    } else {
        None
    }
}

/// `nab doctor` subcommand entry point.
///
/// Prints the `nab` binaries discovered on `PATH` and warns when the running
/// binary is shadowed by an older install. Always exits `Ok` — diagnostics
/// never fail the process.
///
/// # Errors
///
/// Returns an error only if the running binary's own version (compiled in via
/// `CARGO_PKG_VERSION`) cannot be parsed, which would indicate a build defect.
pub fn cmd_doctor() -> Result<()> {
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let current_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());

    println!("nab doctor");
    println!("  running binary: nab {current_version}");
    if let Some(p) = &current_path {
        println!("  running path:   {}", p.display());
    }

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let entries = enumerate_path_binaries(&path_var, probe_version);

    report(&entries, current_path.as_deref(), current_version);
    Ok(())
}

/// Render the shadow report to stdout/stderr. Split out for clarity; the
/// decision logic lives in [`analyze_shadow`].
fn report(entries: &[PathBinary], current_path: Option<&Path>, current_version: Version) {
    println!("\nnab binaries on PATH ({} found):", entries.len());
    for (i, bin) in entries.iter().enumerate() {
        let marker = if i == 0 { "→" } else { " " };
        let ver = bin
            .version
            .map_or_else(|| "version unknown".to_string(), |v| format!("nab {v}"));
        println!("  {marker} {} ({ver})", bin.path.display());
    }

    let Some(current_path) = current_path else {
        println!("\nCould not resolve the running binary's path; skipping shadow check.");
        return;
    };

    let report = analyze_shadow(entries, current_path, current_version);

    if report.is_problem() {
        let winner = &report.binaries[0];
        let winner_ver = winner
            .version
            .map_or_else(|| "unknown".to_string(), |v| v.to_string());
        eprintln!(
            "\nwarning: an older nab ({winner_ver}) at {} shadows this binary ({current_version}).",
            winner.path.display()
        );
        eprintln!(
            "  The shadowing install wins PATH precedence, so `nab --version` may report a stale version."
        );
        eprintln!("  To resolve, keep a single install channel:");
        if let Some(cmd) = uninstall_hint(&winner.path) {
            eprintln!("    - Remove the install that wins PATH:  {cmd}");
        } else {
            eprintln!(
                "    - Remove or update the install that wins PATH ({}).",
                winner.path.display()
            );
        }
        eprintln!("    - Or reorder PATH so your preferred install's directory comes first.");
    } else if report.shadowed {
        println!(
            "\nnote: another nab install wins PATH precedence but is not older; no action needed."
        );
    } else {
        println!("\nNo shadowing detected — the running binary wins PATH precedence.");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    fn bin(path: &str, ver: Option<Version>) -> PathBinary {
        PathBinary {
            path: PathBuf::from(path),
            version: ver,
        }
    }

    /// Issue #105 exact case: an older Homebrew binary wins, shadowing the newer
    /// running cargo binary → problem flagged.
    #[test]
    fn analyze_older_winner_shadows_current_is_problem() {
        // GIVEN brew's 0.8.5 first on PATH, cargo's 0.11.0 is the running binary
        let current = PathBuf::from("/home/u/.cargo/bin/nab");
        let entries = [
            bin("/opt/homebrew/Cellar/nab/0.8.5/bin/nab", Some(v(0, 8, 5))),
            bin("/home/u/.cargo/bin/nab", Some(v(0, 11, 0))),
        ];

        // WHEN analysed
        let report = analyze_shadow(&entries, &current, v(0, 11, 0));

        // THEN it is the reported problem
        assert!(report.shadowed);
        assert!(report.winner_is_stale);
        assert!(report.is_problem());
    }

    /// When the running binary wins PATH precedence, nothing is flagged.
    #[test]
    fn analyze_current_wins_no_problem() {
        // GIVEN the cargo binary is both first on PATH and the running one
        let current = PathBuf::from("/home/u/.cargo/bin/nab");
        let entries = [bin("/home/u/.cargo/bin/nab", Some(v(0, 11, 0)))];

        // WHEN analysed
        let report = analyze_shadow(&entries, &current, v(0, 11, 0));

        // THEN no shadowing
        assert!(!report.shadowed);
        assert!(!report.winner_is_stale);
        assert!(!report.is_problem());
    }

    /// A newer (not older) binary winning precedence is shadowing but not a
    /// problem — we don't nag when the winner is at least as new.
    #[test]
    fn analyze_newer_winner_shadows_but_not_problem() {
        // GIVEN a newer 0.12.0 wins, the running binary is 0.11.0
        let current = PathBuf::from("/home/u/.cargo/bin/nab");
        let entries = [
            bin("/opt/homebrew/bin/nab", Some(v(0, 12, 0))),
            bin("/home/u/.cargo/bin/nab", Some(v(0, 11, 0))),
        ];

        // WHEN analysed
        let report = analyze_shadow(&entries, &current, v(0, 11, 0));

        // THEN shadowed but not a problem
        assert!(report.shadowed);
        assert!(!report.winner_is_stale);
        assert!(!report.is_problem());
    }

    /// When the winner's version cannot be determined, we do not claim it is
    /// stale (avoids false positives), though shadowing is still reported.
    #[test]
    fn analyze_unknown_winner_version_is_not_stale() {
        // GIVEN the winning binary's version could not be probed
        let current = PathBuf::from("/home/u/.cargo/bin/nab");
        let entries = [
            bin("/opt/homebrew/bin/nab", None),
            bin("/home/u/.cargo/bin/nab", Some(v(0, 11, 0))),
        ];

        // WHEN analysed
        let report = analyze_shadow(&entries, &current, v(0, 11, 0));

        // THEN shadowed but stale is unproven, so not flagged as the problem
        assert!(report.shadowed);
        assert!(!report.winner_is_stale);
        assert!(!report.is_problem());
    }

    /// An empty PATH (no nab found) yields no shadowing.
    #[test]
    fn analyze_empty_entries_no_shadow() {
        // GIVEN no binaries discovered
        let current = PathBuf::from("/home/u/.cargo/bin/nab");
        let entries: [PathBinary; 0] = [];

        // WHEN analysed
        let report = analyze_shadow(&entries, &current, v(0, 11, 0));

        // THEN nothing flagged
        assert!(!report.shadowed);
        assert!(!report.is_problem());
        assert!(report.binaries.is_empty());
    }

    /// `enumerate_path_binaries` honours the platform separator and
    /// de-duplicates by canonical path. We build a real temp layout so
    /// canonicalization runs against the filesystem.
    #[test]
    fn enumerate_dedupes_and_orders() {
        // GIVEN two PATH dirs, one of which appears twice
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir_a = tmp.path().join("a");
        let dir_b = tmp.path().join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        let bin_a = dir_a.join(BINARY_NAME);
        let bin_b = dir_b.join(BINARY_NAME);
        std::fs::write(&bin_a, b"#!/bin/sh\n").unwrap();
        std::fs::write(&bin_b, b"#!/bin/sh\n").unwrap();

        let path_var = std::env::join_paths([&dir_a, &dir_b, &dir_a]).unwrap();

        // WHEN enumerated with a probe that reports a fixed version
        let found = enumerate_path_binaries(&path_var, |_| Some(v(1, 0, 0)));

        // THEN exactly two distinct binaries, in PATH order
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, bin_a.canonicalize().unwrap());
        assert_eq!(found[1].path, bin_b.canonicalize().unwrap());
        assert_eq!(found[0].version, Some(v(1, 0, 0)));
    }

    /// Directories on PATH that contain no `nab` binary are skipped.
    #[test]
    fn enumerate_skips_missing_binaries() {
        // GIVEN a PATH dir with no nab in it
        let tmp = tempfile::tempdir().expect("tempdir");
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();

        let path_var = std::env::join_paths([&empty]).unwrap();

        // WHEN enumerated
        let found = enumerate_path_binaries(&path_var, |_| Some(v(1, 0, 0)));

        // THEN nothing discovered
        assert!(found.is_empty());
    }

    /// A cargo-installed winner maps to `cargo uninstall nab`.
    #[test]
    fn uninstall_hint_recognises_cargo() {
        // GIVEN a path under ~/.cargo/bin
        let p = PathBuf::from("/Users/u/.cargo/bin/nab");
        // THEN the cargo channel is named
        assert_eq!(uninstall_hint(&p), Some("cargo uninstall nab"));
    }

    /// A Homebrew Cellar winner maps to `brew uninstall nab`.
    #[test]
    fn uninstall_hint_recognises_homebrew() {
        // GIVEN a canonical Homebrew Cellar path (what a brew symlink resolves to)
        let p = PathBuf::from("/opt/homebrew/Cellar/nab/0.8.5/bin/nab");
        // THEN the brew channel is named
        assert_eq!(uninstall_hint(&p), Some("brew uninstall nab"));
    }

    /// An unrecognised location yields no specific command (caller falls back
    /// to a generic path-based hint).
    #[test]
    fn uninstall_hint_unrecognised_location_is_none() {
        // GIVEN a hand-placed binary in a non-standard directory
        let p = PathBuf::from("/usr/local/bin/nab");
        // THEN no channel-specific command is offered
        assert_eq!(uninstall_hint(&p), None);
    }
}
