//! `nab rules export` — write embedded default TOML rules to `~/.config/nab/sites/`.
//! `nab rules list`   — display all loaded rules with their sources.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use nab::site::rules::embedded_rules;

/// Export all embedded default TOML rules to `~/.config/nab/sites/`.
///
/// Existing files are left untouched so user customisations are never
/// overwritten.  The path of each written (or skipped) file is printed to
/// stdout.
///
/// # Errors
///
/// Returns an error if the target directory cannot be created or if a file
/// write fails.
pub fn cmd_export_rules() -> Result<()> {
    let sites_dir = user_sites_dir();

    fs::create_dir_all(&sites_dir)
        .with_context(|| format!("Failed to create directory: {}", sites_dir.display()))?;

    for (name, content) in embedded_rules() {
        export_rule(name, content, &sites_dir)?;
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// `nab rules list`
// ─────────────────────────────────────────────────────────────────────────────

/// List all active site rules and their sources.
///
/// Output columns:
///
/// - **Rule** — the rule name (TOML file stem)
/// - **Source** — `embedded` or `user override`
/// - **Status** — `active` or `active (overrides embedded)`
///
/// Embedded rules appear first (in the order defined by [`embedded_rules`]),
/// followed by any user-only rules that have no embedded counterpart.
///
/// # Errors
///
/// This function is infallible; it never returns an error.  The `Result`
/// return type exists only for consistency with the other `cmd_*` functions.
#[allow(clippy::unnecessary_wraps)]
pub fn cmd_list_rules() -> Result<()> {
    let user_dir = user_sites_dir();
    let user_names = collect_user_rule_names(&user_dir);
    let embedded_names: HashSet<&str> = embedded_rules().into_iter().map(|(n, _)| n).collect();

    let rows = build_rows(&embedded_names, &user_names);
    print_table(&rows);

    Ok(())
}

/// A single row in the rules table.
struct RuleRow {
    name: String,
    source: &'static str,
    status: &'static str,
}

/// Collect the stems of all `*.toml` files in `dir`.
fn collect_user_rule_names(dir: &Path) -> HashSet<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_owned)
            } else {
                None
            }
        })
        .collect()
}

/// Build the ordered list of [`RuleRow`] values.
///
/// Order: embedded rules first (in definition order), then user-only rules
/// (alphabetically).
fn build_rows(embedded_names: &HashSet<&str>, user_names: &HashSet<String>) -> Vec<RuleRow> {
    // Embedded rules — preserve canonical declaration order.
    let mut rows: Vec<RuleRow> = embedded_rules()
        .into_iter()
        .map(|(name, _)| {
            let overridden = user_names.contains(name);
            RuleRow {
                name: name.to_owned(),
                source: if overridden { "user override" } else { "embedded" },
                status: if overridden {
                    "active (overrides embedded)"
                } else {
                    "active"
                },
            }
        })
        .collect();

    // User-only rules that have no embedded counterpart.
    let mut user_only: Vec<String> = user_names
        .iter()
        .filter(|n| !embedded_names.contains(n.as_str()))
        .cloned()
        .collect();
    user_only.sort_unstable();

    for name in user_only {
        rows.push(RuleRow {
            name,
            source: "user (~/.config/…)",
            status: "active",
        });
    }

    rows
}

/// Print `rows` as a plain-text table with aligned columns.
fn print_table(rows: &[RuleRow]) {
    const COL_NAME: &str = "Rule";
    const COL_SRC: &str = "Source";
    const COL_STATUS: &str = "Status";

    let w_name = rows
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(0)
        .max(COL_NAME.len());
    let w_src = rows
        .iter()
        .map(|r| r.source.len())
        .max()
        .unwrap_or(0)
        .max(COL_SRC.len());

    println!(
        "{COL_NAME:<w_name$}  {COL_SRC:<w_src$}  {COL_STATUS}"
    );
    let separator = format!(
        "{}  {}  {}",
        "─".repeat(w_name),
        "─".repeat(w_src),
        "─".repeat(COL_STATUS.len())
    );
    println!("{separator}");

    for row in rows {
        println!(
            "{:<w_name$}  {:<w_src$}  {}",
            row.name, row.source, row.status
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// `nab rules export` internals
// ─────────────────────────────────────────────────────────────────────────────

/// Write a single rule file, skipping it if it already exists.
fn export_rule(name: &str, content: &str, dir: &Path) -> Result<()> {
    let path = dir.join(format!("{name}.toml"));

    if path.exists() {
        println!("Skipped {name}.toml (already exists at {})", path.display());
        return Ok(());
    }

    fs::write(&path, content).with_context(|| format!("Failed to write {}", path.display()))?;

    println!("Exported {name}.toml to {}", path.display());
    Ok(())
}

/// Return `~/.config/nab/sites/`.
fn user_sites_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nab")
        .join("sites")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_rule_writes_new_file() {
        // GIVEN: a temporary directory with no pre-existing files
        let dir = tempfile::tempdir().expect("tempdir");
        let content = "[site]\nname = \"test\"\n";

        // WHEN: we export a rule
        export_rule("test", content, dir.path()).expect("export");

        // THEN: the file exists with correct content
        let path = dir.path().join("test.toml");
        assert!(path.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn export_rule_skips_existing_file() {
        // GIVEN: a pre-existing rule file
        let dir = tempfile::tempdir().expect("tempdir");
        let original = "[site]\nname = \"existing\"\n";
        let path = dir.path().join("existing.toml");
        fs::write(&path, original).expect("write original");

        // WHEN: we attempt to export with different content
        let new_content = "[site]\nname = \"overwrite attempt\"\n";
        export_rule("existing", new_content, dir.path()).expect("export");

        // THEN: original content is preserved
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn cmd_export_rules_creates_directory_and_files() {
        // GIVEN: override dirs::config_dir is not feasible in unit tests, so
        // we test the internal helper directly with a temp dir.
        let dir = tempfile::tempdir().expect("tempdir");

        for (name, content) in embedded_rules() {
            export_rule(name, content, dir.path()).expect("export");
        }

        // THEN: all four default rules are written
        for (name, _) in embedded_rules() {
            let path = dir.path().join(format!("{name}.toml"));
            assert!(path.exists(), "missing {name}.toml");
        }
    }

    #[test]
    fn user_sites_dir_ends_with_nab_sites() {
        let dir = user_sites_dir();
        assert!(dir.ends_with("nab/sites"));
    }

    // ── cmd_list_rules helpers ────────────────────────────────────────────────

    #[test]
    fn collect_user_rule_names_empty_when_dir_missing() {
        // GIVEN: a path that does not exist
        let dir = PathBuf::from("/tmp/nab_test_nonexistent_dir_xyz");

        // WHEN: we collect names
        let names = collect_user_rule_names(&dir);

        // THEN: result is empty
        assert!(names.is_empty());
    }

    #[test]
    fn collect_user_rule_names_includes_toml_stems() {
        // GIVEN: a temp dir with two .toml files and one non-toml file
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("alpha.toml"), "").expect("write");
        fs::write(dir.path().join("beta.toml"), "").expect("write");
        fs::write(dir.path().join("ignored.txt"), "").expect("write");

        // WHEN: we collect names
        let names = collect_user_rule_names(dir.path());

        // THEN: only .toml stems are returned
        assert_eq!(names.len(), 2);
        assert!(names.contains("alpha"));
        assert!(names.contains("beta"));
        assert!(!names.contains("ignored"));
    }

    #[test]
    fn build_rows_marks_overridden_embedded_rules() {
        // GIVEN: embedded names and a user override for "twitter"
        let embedded: HashSet<&str> = ["twitter", "youtube"].iter().copied().collect();
        let user: HashSet<String> = ["twitter".to_owned()].into_iter().collect();

        // WHEN: we build rows
        let rows = build_rows(&embedded, &user);

        // THEN: twitter row is marked as user override
        let twitter = rows.iter().find(|r| r.name == "twitter").expect("twitter");
        assert_eq!(twitter.source, "user override");
        assert_eq!(twitter.status, "active (overrides embedded)");

        // AND: youtube row is plain embedded
        let youtube = rows.iter().find(|r| r.name == "youtube").expect("youtube");
        assert_eq!(youtube.source, "embedded");
        assert_eq!(youtube.status, "active");
    }

    #[test]
    fn build_rows_appends_user_only_rules_alphabetically() {
        // GIVEN: no embedded names and two user-only rules
        let embedded: HashSet<&str> = HashSet::new();
        let user: HashSet<String> = ["zebra".to_owned(), "apple".to_owned()].into_iter().collect();

        // WHEN: we build rows
        let rows = build_rows(&embedded, &user);

        // THEN: user-only rows appear after the embedded rows, in alphabetical order.
        // build_rows always includes all embedded_rules() entries first.
        let user_rows: Vec<_> = rows.iter().filter(|r| r.source == "user (~/.config/…)").collect();
        assert_eq!(user_rows.len(), 2);
        assert_eq!(user_rows[0].name, "apple");
        assert_eq!(user_rows[1].name, "zebra");
    }

    #[test]
    fn build_rows_contains_all_eight_embedded_rules_when_no_overrides() {
        // GIVEN: the real embedded set, no user overrides
        let embedded: HashSet<&str> = embedded_rules().into_iter().map(|(n, _)| n).collect();
        let user: HashSet<String> = HashSet::new();

        // WHEN: we build rows
        let rows = build_rows(&embedded, &user);

        // THEN: exactly 8 rows, all embedded, all active
        assert_eq!(rows.len(), 8);
        assert!(rows.iter().all(|r| r.source == "embedded"));
        assert!(rows.iter().all(|r| r.status == "active"));
    }

    #[test]
    fn build_rows_preserves_embedded_declaration_order() {
        // GIVEN: no overrides
        let embedded: HashSet<&str> = embedded_rules().into_iter().map(|(n, _)| n).collect();
        let user: HashSet<String> = HashSet::new();

        // WHEN
        let rows = build_rows(&embedded, &user);
        let row_names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let declared_names: Vec<&str> = embedded_rules().into_iter().map(|(n, _)| n).collect();

        // THEN: order matches embedded_rules() declaration
        assert_eq!(row_names, declared_names);
    }

    #[test]
    fn cmd_list_rules_succeeds_without_error() {
        // GIVEN / WHEN: call the public function (may or may not have user config)
        let result = cmd_list_rules();

        // THEN: no error is returned
        assert!(result.is_ok());
    }
}
