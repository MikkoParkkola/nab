//! `nab rules export` — write embedded default TOML rules to `~/.config/nab/sites/`.

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

/// Write a single rule file, skipping it if it already exists.
fn export_rule(name: &str, content: &str, dir: &Path) -> Result<()> {
    let path = dir.join(format!("{name}.toml"));

    if path.exists() {
        println!("Skipped {name}.toml (already exists at {})", path.display());
        return Ok(());
    }

    fs::write(&path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;

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
}
