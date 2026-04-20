//! `nab upgrade` subcommand — post-install migration and onboarding.
//!
//! # Version stamp
//!
//! A plain-text file at `~/.nab/version.stamp` records the last version that
//! ran migrations.  On every startup (or when `nab upgrade` is invoked
//! explicitly) [`check_upgrade`] compares that stamp to `CARGO_PKG_VERSION`
//! and runs any pending migrations.
//!
//! # Migration registry
//!
//! Migrations are listed in [`MIGRATIONS`] in ascending semver order.  Each
//! entry runs exactly once: when the installed stamp is strictly older than
//! the migration's `since` version.  The registry is empty today — this file
//! establishes the framework.
//!
//! # Model hints
//!
//! After running migrations, [`check_installed_models`] prints a hint for any
//! installed model that has a known newer version available.  No automatic
//! download is performed.

use std::cmp::Ordering;
use std::path::PathBuf;

use anyhow::{Context, Result};

// ── Version type ──────────────────────────────────────────────────────────────

/// Minimal semver triple — sufficient for stamp comparisons without pulling in
/// a full semver crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// Parse a `"major.minor.patch"` string.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not in the expected format or if any
    /// component cannot be parsed as a `u32`.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let mut parts = s.splitn(3, '.');
        let parse_part = |raw: Option<&str>, label: &str| -> Result<u32> {
            raw.with_context(|| format!("version '{s}' is missing the {label} component"))?
                .parse::<u32>()
                .with_context(|| format!("version '{s}': {label} component is not a valid u32"))
        };
        Ok(Self {
            major: parse_part(parts.next(), "major")?,
            minor: parse_part(parts.next(), "minor")?,
            patch: parse_part(parts.next(), "patch")?,
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// ── Stamp file path ───────────────────────────────────────────────────────────

/// Path to `~/.nab/version.stamp`.
///
/// # Errors
///
/// Returns an error if the home directory cannot be resolved.
pub fn stamp_path() -> Result<PathBuf> {
    dirs::home_dir()
        .context("could not resolve home directory")
        .map(|home| home.join(".nab").join("version.stamp"))
}

// ── Stamp I/O ─────────────────────────────────────────────────────────────────

/// Read the version stamp, returning `None` if the file does not exist.
///
/// # Errors
///
/// Returns an error only on unexpected I/O failures (permissions, etc.).
pub fn read_stamp() -> Result<Option<Version>> {
    let path = stamp_path()?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Version::parse(&s).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write `version` to the stamp file, creating `~/.nab/` if needed.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file cannot be
/// written.
pub fn write_stamp(version: &Version) -> Result<()> {
    let path = stamp_path()?;
    let dir = path.parent().context("stamp path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&path, format!("{version}\n"))
        .with_context(|| format!("writing stamp to {}", path.display()))
}

// ── Migration registry ────────────────────────────────────────────────────────

/// A single migration step.
pub struct Migration {
    /// This migration applies when the installed stamp is older than `since`.
    pub since: Version,
    /// Human-readable description printed during a dry-run or real run.
    pub description: &'static str,
    /// The actual migration logic.
    pub run: fn() -> Result<()>,
}

/// All known migrations in ascending `since` order.
///
/// Empty for now — add entries here as new versions require data changes.
pub const MIGRATIONS: &[Migration] = &[];

// ── What's new registry ──────────────────────────────────────────────────────

/// "What's new" entries shown when upgrading to a specific version.
struct WhatsNew {
    /// Version that introduced these changes.
    version: Version,
    /// Bullet points shown to the user.
    items: &'static [&'static str],
}

/// All known what's-new entries in ascending version order.
///
/// Add entries here when a release has user-facing changes worth announcing.
static WHATS_NEW: &[WhatsNew] = &[WhatsNew {
    version: Version {
        major: 0,
        minor: 7,
        patch: 1,
    },
    items: &[
        "New `upgrade` command with version stamp and migration framework",
        "Agent-first install: tell your AI to read the README",
        "12 MCP tools (analyze tool added)",
    ],
}];

/// Print "what's new" items for all versions strictly after `from` up to
/// `current` (inclusive).
fn print_whats_new(from: &Version, current: &Version) {
    let items: Vec<&str> = WHATS_NEW
        .iter()
        .filter(|w| w.version > *from && w.version <= *current)
        .flat_map(|w| w.items.iter().copied())
        .collect();

    if items.is_empty() {
        return;
    }

    println!("What's new since v{from}:");
    for item in items {
        println!("  - {item}");
    }
}

// ── Model hint registry ───────────────────────────────────────────────────────

/// A model that may have a newer version available.
struct ModelHint {
    /// Short name matching `nab models list` output.
    name: &'static str,
    /// Version that introduced the newer model variant.
    available_since: Version,
    /// Human-readable hint text.
    hint: &'static str,
}

/// Known model update hints (add entries here as upstream releases arrive).
const MODEL_HINTS: &[ModelHint] = &[];

// ── Config struct for the subcommand ─────────────────────────────────────────

/// Configuration for the `nab upgrade` subcommand.
#[derive(Debug, Default)]
pub struct UpgradeConfig {
    /// Print what would happen without making any changes.
    pub dry_run: bool,
    /// Suppress informational output.
    pub quiet: bool,
}

// ── Core logic ────────────────────────────────────────────────────────────────

/// Check for pending upgrades and run them.
///
/// Called at startup (before dispatch) so every invocation benefits from
/// migrations, not only `nab upgrade`.
///
/// # Behaviour
///
/// | Stamp state             | Action                                     |
/// |-------------------------|--------------------------------------------|
/// | Missing (fresh install) | Write current version; print welcome line  |
/// | `stamp < current`       | Run pending migrations; update stamp       |
/// | `stamp == current`      | No-op                                      |
/// | `stamp > current`       | Print a downgrade warning; no other action |
///
/// # Errors
///
/// Returns an error if the stamp file cannot be read or written, or if any
/// migration fails.
pub fn check_upgrade() -> Result<()> {
    let current = current_version()?;

    match read_stamp()? {
        None => {
            write_stamp(&current)?;
            println!("Welcome to nab {current}!");
            Ok(())
        }
        Some(stamp) if stamp < current => run_upgrade_inner(&stamp, &current, false, false, false),
        Some(stamp) if stamp > current => {
            eprintln!(
                "warning: nab {stamp} stamp is newer than this binary ({current}); \
                 you may be running a downgraded binary"
            );
            Ok(())
        }
        _ => Ok(()), // stamp == current
    }
}

/// `nab upgrade` subcommand entry point.
///
/// # Errors
///
/// Same as [`check_upgrade`].
pub fn cmd_upgrade(cfg: &UpgradeConfig) -> Result<()> {
    let current = current_version()?;

    let Some(stamp) = read_stamp()? else {
        if !cfg.quiet {
            println!("nab v{current} — fresh install, stamp created.");
        }
        if !cfg.dry_run {
            write_stamp(&current)?;
        }
        print_model_hints(&current, cfg.quiet);
        return Ok(());
    };

    match stamp.cmp(&current) {
        Ordering::Greater => {
            eprintln!(
                "warning: stamp {stamp} is newer than binary {current}; \
                 skipping migrations (possible downgrade)"
            );
            Ok(())
        }
        Ordering::Equal => {
            if !cfg.quiet {
                println!("nab {current} — already up to date.");
            }
            print_model_hints(&current, cfg.quiet);
            Ok(())
        }
        Ordering::Less => run_upgrade_inner(&stamp, &current, cfg.dry_run, cfg.quiet, false),
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Return the current binary version from `CARGO_PKG_VERSION`.
fn current_version() -> Result<Version> {
    Version::parse(env!("CARGO_PKG_VERSION"))
}

/// Run all pending migrations from `from` (exclusive) up to `to` (inclusive).
fn run_upgrade_inner(
    from: &Version,
    to: &Version,
    dry_run: bool,
    quiet: bool,
    fresh: bool,
) -> Result<()> {
    let pending: Vec<&Migration> = MIGRATIONS
        .iter()
        .filter(|m| m.since > *from && m.since <= *to)
        .collect();

    // Print what's-new (skip on fresh install — the user already sees the
    // welcome message and doesn't need a delta).
    if !quiet && !fresh {
        print_whats_new(from, to);
    }

    for migration in &pending {
        if !quiet {
            println!("  [migration] {}", migration.description);
        }
        if !dry_run {
            (migration.run)()
                .with_context(|| format!("migration '{}' failed", migration.description))?;
        }
    }

    if !dry_run {
        write_stamp(to)?;
    } else if !quiet {
        println!("  (dry-run: stamp not updated)");
    }

    print_model_hints(to, quiet);

    if !quiet {
        println!("nab upgraded v{from} → v{to}");
    }

    Ok(())
}

/// Print hints for any installed model that could be updated.
fn print_model_hints(current: &Version, quiet: bool) {
    if quiet {
        return;
    }
    for hint in MODEL_HINTS {
        if *current >= hint.available_since && super::models::read_version(hint.name).is_some() {
            println!("  hint [{}] {}", hint.name, hint.hint);
        }
    }
}

/// Print a one-line completion summary (kept for potential future use by
/// callers that want migration-count output distinct from the upgrade line).
// Allow: `Result<()>` kept for API symmetry with the rest of the upgrade
// flow; callers propagate `?` uniformly.
#[allow(dead_code, clippy::unnecessary_wraps)]
fn print_summary(migration_count: usize, quiet: bool) -> Result<()> {
    if !quiet && migration_count > 0 {
        println!(
            "({migration_count} migration{} applied)",
            if migration_count == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Version parsing ───────────────────────────────────────────────────────

    /// `Version::parse` accepts a canonical `"major.minor.patch"` string.
    #[test]
    fn version_parse_canonical() {
        // GIVEN a well-formed semver string
        // WHEN parsed
        let v = Version::parse("1.2.3").unwrap();
        // THEN fields are correct
        assert_eq!(
            v,
            Version {
                major: 1,
                minor: 2,
                patch: 3
            }
        );
    }

    /// `Version::parse` trims surrounding whitespace (as read from a stamp file).
    #[test]
    fn version_parse_trims_whitespace() {
        // GIVEN a string with leading/trailing whitespace and a newline
        // WHEN parsed
        let v = Version::parse("  0.7.1\n").unwrap();
        // THEN parsed correctly
        assert_eq!(
            v,
            Version {
                major: 0,
                minor: 7,
                patch: 1
            }
        );
    }

    /// `Version::parse` rejects a string with fewer than three components.
    #[test]
    fn version_parse_rejects_incomplete() {
        // GIVEN a two-part string
        // WHEN parsed
        let result = Version::parse("1.2");
        // THEN error
        assert!(result.is_err());
    }

    /// `Version::parse` rejects a non-numeric component.
    #[test]
    fn version_parse_rejects_non_numeric_component() {
        // GIVEN a string with a non-numeric patch
        // WHEN parsed
        let result = Version::parse("1.2.beta");
        // THEN error
        assert!(result.is_err());
    }

    // ── Version ordering ──────────────────────────────────────────────────────

    /// A lower patch version compares less than a higher one.
    #[test]
    fn version_ord_patch_comparison() {
        // GIVEN
        let old = Version::parse("0.7.0").unwrap();
        let new = Version::parse("0.7.1").unwrap();
        // THEN
        assert!(old < new);
        assert!(new > old);
    }

    /// A lower minor version compares less than a higher one regardless of patch.
    #[test]
    fn version_ord_minor_dominates_patch() {
        // GIVEN
        let old = Version::parse("0.6.99").unwrap();
        let new = Version::parse("0.7.0").unwrap();
        // THEN
        assert!(old < new);
    }

    /// Equal versions compare equal.
    #[test]
    fn version_ord_equal() {
        // GIVEN
        let a = Version::parse("1.0.0").unwrap();
        let b = Version::parse("1.0.0").unwrap();
        // THEN
        assert_eq!(a, b);
        assert_eq!(a.cmp(&b), Ordering::Equal);
    }

    // ── Display ───────────────────────────────────────────────────────────────

    /// `Version::fmt` produces the canonical `"major.minor.patch"` form.
    #[test]
    fn version_display_round_trips() {
        // GIVEN
        let v = Version {
            major: 2,
            minor: 10,
            patch: 0,
        };
        // WHEN formatted
        let s = v.to_string();
        // THEN round-trips
        assert_eq!(s, "2.10.0");
        assert_eq!(Version::parse(&s).unwrap(), v);
    }

    // ── Stamp I/O ─────────────────────────────────────────────────────────────

    /// `read_stamp` returns `None` when the file does not exist.
    #[test]
    fn read_stamp_absent_returns_none() {
        // GIVEN a path that does not exist
        // WHEN read_stamp is called (uses the real path — almost certainly absent in CI)
        // We test the logic directly via a helper.
        let result = read_stamp_from_path(&PathBuf::from("/tmp/__nab_nonexistent_stamp_xyz__"));
        // THEN None
        assert!(result.unwrap().is_none());
    }

    /// `write_stamp_to_path` + `read_stamp_from_path` round-trip preserves version.
    #[test]
    fn stamp_write_read_round_trip() {
        // GIVEN a temp directory
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("version.stamp");
        let v = Version {
            major: 0,
            minor: 8,
            patch: 0,
        };

        // WHEN written and read back
        write_stamp_to_path(&v, &path).unwrap();
        let read_back = read_stamp_from_path(&path).unwrap();

        // THEN round-trip is lossless
        assert_eq!(read_back, Some(v));
    }

    /// `write_stamp_to_path` creates parent directories when they don't exist.
    #[test]
    fn write_stamp_creates_parent_dirs() {
        // GIVEN a path nested inside a non-existent directory
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("new_dir").join("version.stamp");
        let v = Version {
            major: 1,
            minor: 0,
            patch: 0,
        };

        // WHEN written
        let result = write_stamp_to_path(&v, &path);

        // THEN no error and file exists
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert!(path.exists());
    }

    // ── Migration filtering ───────────────────────────────────────────────────

    /// Migrations with `since > from && since <= to` are selected.
    #[test]
    fn pending_migrations_selects_correct_range() {
        // GIVEN fabricated migration list
        let from = Version::parse("0.7.0").unwrap();
        let to = Version::parse("0.9.0").unwrap();

        let versions = [
            Version::parse("0.7.0").unwrap(), // equal to from — excluded
            Version::parse("0.8.0").unwrap(), // in range
            Version::parse("0.9.0").unwrap(), // equal to to — included
            Version::parse("1.0.0").unwrap(), // above to — excluded
        ];

        let pending: Vec<&Version> = versions
            .iter()
            .filter(|v| **v > from && **v <= to)
            .collect();

        // THEN only 0.8.0 and 0.9.0 are selected
        assert_eq!(pending.len(), 2);
        assert_eq!(*pending[0], versions[1]);
        assert_eq!(*pending[1], versions[2]);
    }

    // ── What's new filtering ────────────────────────────────────────────────

    /// What's-new entries with `version > from && version <= current` are selected.
    #[test]
    fn whats_new_selects_correct_range() {
        // GIVEN a from version older than any WHATS_NEW entry
        let from = Version::parse("0.6.0").unwrap();
        let current = Version::parse("0.7.1").unwrap();

        // WHEN filtering what's-new items
        let items: Vec<&str> = WHATS_NEW
            .iter()
            .filter(|w| w.version > from && w.version <= current)
            .flat_map(|w| w.items.iter().copied())
            .collect();

        // THEN all 0.7.1 items are included
        assert_eq!(items.len(), 3);
        assert!(items[0].contains("upgrade"));
    }

    /// What's-new returns nothing when from == current.
    #[test]
    fn whats_new_empty_when_already_current() {
        // GIVEN from == the version in WHATS_NEW
        let from = Version::parse("0.7.1").unwrap();
        let current = Version::parse("0.7.1").unwrap();

        // WHEN filtering
        let items: Vec<&str> = WHATS_NEW
            .iter()
            .filter(|w| w.version > from && w.version <= current)
            .flat_map(|w| w.items.iter().copied())
            .collect();

        // THEN nothing matches (from is not strictly less)
        assert!(items.is_empty());
    }

    /// What's-new returns nothing when from is ahead of all entries.
    #[test]
    fn whats_new_empty_when_from_is_newer() {
        // GIVEN from beyond any registered entry
        let from = Version::parse("1.0.0").unwrap();
        let current = Version::parse("1.0.1").unwrap();

        // WHEN filtering
        let items: Vec<&str> = WHATS_NEW
            .iter()
            .filter(|w| w.version > from && w.version <= current)
            .flat_map(|w| w.items.iter().copied())
            .collect();

        // THEN empty — all entries are below from
        assert!(items.is_empty());
    }

    // ── Testable stamp path helpers ───────────────────────────────────────────

    fn read_stamp_from_path(path: &PathBuf) -> Result<Option<Version>> {
        match std::fs::read_to_string(path) {
            Ok(s) => Version::parse(&s).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn write_stamp_to_path(version: &Version, path: &PathBuf) -> Result<()> {
        let dir = path.parent().context("stamp path has no parent")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(path, format!("{version}\n"))
            .with_context(|| format!("writing stamp to {}", path.display()))
    }
}
