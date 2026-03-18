//! WASM provider manifest: on-disk format and loading logic.
//!
//! Each installed WASM provider lives in:
//! ```text
//! ~/.config/nab/wasm_providers/<name>/
//!   manifest.toml   — plugin metadata and URL patterns
//!   provider.wasm   — compiled WASM module
//! ```
//!
//! # Manifest Format
//!
//! ```toml
//! name        = "medium-extractor"
//! version     = "1.0.0"
//! description = "Extracts article content from medium.com"
//! author      = "example@email.com"
//! url_patterns = ["medium\\.com/@?[^/]+/[^/]+"]
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// Manifest type
// ─────────────────────────────────────────────────────────────────────────────

/// Metadata and configuration for an installed WASM provider.
///
/// Loaded from `manifest.toml` inside the provider's directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmManifest {
    /// Plugin identifier — must be unique across all installed providers.
    /// Used as directory name and display label.
    pub name: String,

    /// Semantic version string (e.g., `"1.0.0"`).
    pub version: String,

    /// Human-readable description shown in `nab provider list`.
    #[serde(default)]
    pub description: String,

    /// Plugin author (name or email).
    #[serde(default)]
    pub author: String,

    /// Regex patterns matched against the full request URL.
    ///
    /// First provider whose pattern matches the URL wins.
    /// An empty list means the provider never matches.
    #[serde(default)]
    pub url_patterns: Vec<String>,
}

impl WasmManifest {
    /// Validate the manifest fields are coherent.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `name` is empty
    /// - `version` is empty
    /// - `url_patterns` is empty (plugin would never match)
    /// - any URL pattern is not a valid regex
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            bail!("WASM manifest 'name' must not be empty");
        }
        if self.version.is_empty() {
            bail!("WASM manifest '{}': 'version' must not be empty", self.name);
        }
        if self.url_patterns.is_empty() {
            bail!(
                "WASM manifest '{}': 'url_patterns' must not be empty (provider would never match)",
                self.name
            );
        }
        for pattern in &self.url_patterns {
            regex::Regex::new(pattern).with_context(|| {
                format!(
                    "WASM manifest '{}': invalid url_pattern '{pattern}'",
                    self.name
                )
            })?;
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Directory helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Return `~/.config/nab/wasm_providers/`.
pub fn wasm_providers_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nab")
        .join("wasm_providers")
}

/// Return the directory for a specific installed provider.
pub fn provider_dir(base: &Path, name: &str) -> PathBuf {
    base.join(name)
}

/// Return the manifest path inside a provider directory.
pub fn manifest_path(provider_dir: &Path) -> PathBuf {
    provider_dir.join("manifest.toml")
}

/// Return the WASM binary path inside a provider directory.
pub fn wasm_path(provider_dir: &Path) -> PathBuf {
    provider_dir.join("provider.wasm")
}

// ─────────────────────────────────────────────────────────────────────────────
// Loading
// ─────────────────────────────────────────────────────────────────────────────

/// A validated manifest paired with its resolved WASM binary path.
#[derive(Debug, Clone)]
pub struct InstalledProvider {
    /// Parsed and validated manifest.
    pub manifest: WasmManifest,
    /// Absolute path to `provider.wasm`.
    pub wasm_path: PathBuf,
}

/// Discover and load all installed WASM providers from `base_dir`.
///
/// Each sub-directory of `base_dir` that contains both `manifest.toml` and
/// `provider.wasm` is treated as an installed provider.  Invalid or incomplete
/// entries are skipped with a `tracing::warn` log.
///
/// Returns an empty vec if `base_dir` does not exist.
pub fn load_installed_providers(base_dir: &Path) -> Vec<InstalledProvider> {
    let Ok(entries) = std::fs::read_dir(base_dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            if !dir.is_dir() {
                return None;
            }
            match load_single_provider(&dir) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!("Skipping WASM provider at {}: {e}", dir.display());
                    None
                }
            }
        })
        .collect()
}

/// Load and validate a single provider from its directory.
///
/// # Errors
///
/// Returns an error if `manifest.toml` is missing, unparseable, or invalid,
/// or if `provider.wasm` is absent.
pub fn load_single_provider(dir: &Path) -> Result<InstalledProvider> {
    let mpath = manifest_path(dir);
    let wpath = wasm_path(dir);

    let toml_str = std::fs::read_to_string(&mpath)
        .with_context(|| format!("missing manifest.toml in {}", dir.display()))?;

    let manifest: WasmManifest = toml::from_str(&toml_str)
        .with_context(|| format!("invalid manifest.toml in {}", dir.display()))?;

    manifest.validate()?;

    if !wpath.exists() {
        bail!("provider.wasm not found in {}", dir.display());
    }

    Ok(InstalledProvider {
        manifest,
        wasm_path: wpath,
    })
}

/// Write a manifest to `dir/manifest.toml`.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file cannot be written.
pub fn write_manifest(dir: &Path, manifest: &WasmManifest) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("cannot create provider directory {}", dir.display()))?;
    let toml_str = toml::to_string_pretty(manifest)
        .with_context(|| format!("cannot serialise manifest for '{}'", manifest.name))?;
    let path = manifest_path(dir);
    std::fs::write(&path, toml_str).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> WasmManifest {
        WasmManifest {
            name: "test-provider".to_string(),
            version: "1.0.0".to_string(),
            description: "A test provider".to_string(),
            author: "test@example.com".to_string(),
            url_patterns: vec![r"example\.com/.*".to_string()],
        }
    }

    // ── validate ─────────────────────────────────────────────────────────────

    #[test]
    fn validate_accepts_complete_manifest() {
        // GIVEN: a fully populated valid manifest
        let m = valid_manifest();
        // WHEN / THEN: validation passes
        assert!(m.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_name() {
        // GIVEN: manifest with empty name
        let m = WasmManifest {
            name: String::new(),
            ..valid_manifest()
        };
        // WHEN / THEN
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn validate_rejects_empty_version() {
        // GIVEN: manifest with empty version
        let m = WasmManifest {
            version: String::new(),
            ..valid_manifest()
        };
        // WHEN / THEN
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("version"));
    }

    #[test]
    fn validate_rejects_empty_url_patterns() {
        // GIVEN: manifest with no URL patterns
        let m = WasmManifest {
            url_patterns: vec![],
            ..valid_manifest()
        };
        // WHEN / THEN
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("url_patterns"));
    }

    #[test]
    fn validate_rejects_invalid_regex_pattern() {
        // GIVEN: manifest with an invalid regex
        let m = WasmManifest {
            url_patterns: vec![r"[invalid".to_string()],
            ..valid_manifest()
        };
        // WHEN / THEN
        assert!(m.validate().is_err());
    }

    #[test]
    fn validate_accepts_multiple_valid_patterns() {
        // GIVEN: manifest with multiple valid patterns
        let m = WasmManifest {
            url_patterns: vec![
                r"foo\.com/.*".to_string(),
                r"bar\.org/article/\d+".to_string(),
            ],
            ..valid_manifest()
        };
        // WHEN / THEN
        assert!(m.validate().is_ok());
    }

    // ── serialise / deserialise ───────────────────────────────────────────────

    #[test]
    fn manifest_round_trips_through_toml() {
        // GIVEN: a valid manifest
        let original = valid_manifest();
        // WHEN: serialised and deserialised
        let toml_str = toml::to_string_pretty(&original).expect("serialise");
        let parsed: WasmManifest = toml::from_str(&toml_str).expect("deserialise");
        // THEN: fields match
        assert_eq!(parsed.name, original.name);
        assert_eq!(parsed.version, original.version);
        assert_eq!(parsed.url_patterns, original.url_patterns);
    }

    #[test]
    fn manifest_defaults_description_and_author() {
        // GIVEN: minimal TOML without optional fields
        let toml_str = r#"
name = "minimal"
version = "0.1.0"
url_patterns = ["minimal\\.com"]
"#;
        // WHEN
        let m: WasmManifest = toml::from_str(toml_str).expect("parse");
        // THEN: optional fields default to empty strings
        assert_eq!(m.description, "");
        assert_eq!(m.author, "");
    }

    // ── load_installed_providers ─────────────────────────────────────────────

    #[test]
    fn load_installed_providers_returns_empty_for_missing_dir() {
        // GIVEN: a path that does not exist
        let dir = PathBuf::from("/tmp/nab_wasm_test_nonexistent_xyz");
        // WHEN / THEN
        let providers = load_installed_providers(&dir);
        assert!(providers.is_empty());
    }

    #[test]
    fn load_installed_providers_discovers_valid_provider() {
        // GIVEN: a temp directory with one valid provider
        let base = tempfile::tempdir().expect("tempdir");
        let pdir = base.path().join("my-provider");
        std::fs::create_dir_all(&pdir).expect("mkdir");

        write_manifest(&pdir, &valid_manifest()).expect("write manifest");
        // Fake the WASM file (content doesn't matter for discovery)
        std::fs::write(wasm_path(&pdir), b"(module)").expect("write wasm");

        // WHEN
        let providers = load_installed_providers(base.path());

        // THEN
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].manifest.name, "test-provider");
    }

    #[test]
    fn load_installed_providers_skips_dir_without_wasm() {
        // GIVEN: a provider directory with manifest but no .wasm
        let base = tempfile::tempdir().expect("tempdir");
        let pdir = base.path().join("no-wasm");
        write_manifest(&pdir, &valid_manifest()).expect("write manifest");
        // Deliberately omit provider.wasm

        // WHEN
        let providers = load_installed_providers(base.path());

        // THEN: skipped with warning
        assert!(providers.is_empty());
    }

    #[test]
    fn load_installed_providers_skips_dir_without_manifest() {
        // GIVEN: a directory with no manifest.toml
        let base = tempfile::tempdir().expect("tempdir");
        let pdir = base.path().join("no-manifest");
        std::fs::create_dir_all(&pdir).expect("mkdir");
        std::fs::write(wasm_path(&pdir), b"(module)").expect("write wasm");

        // WHEN
        let providers = load_installed_providers(base.path());

        // THEN
        assert!(providers.is_empty());
    }

    #[test]
    fn load_installed_providers_skips_invalid_manifest() {
        // GIVEN: a provider with an invalid manifest (empty name)
        let base = tempfile::tempdir().expect("tempdir");
        let pdir = base.path().join("bad-manifest");
        std::fs::create_dir_all(&pdir).expect("mkdir");
        std::fs::write(
            manifest_path(&pdir),
            r#"name = ""
version = "1.0.0"
url_patterns = ["x\\.com"]
"#,
        )
        .expect("write");
        std::fs::write(wasm_path(&pdir), b"(module)").expect("write wasm");

        // WHEN
        let providers = load_installed_providers(base.path());

        // THEN: skipped
        assert!(providers.is_empty());
    }

    #[test]
    fn load_single_provider_returns_error_for_missing_manifest() {
        // GIVEN: empty directory
        let dir = tempfile::tempdir().expect("tempdir");
        // WHEN / THEN
        assert!(load_single_provider(dir.path()).is_err());
    }

    #[test]
    fn write_manifest_creates_directory_and_file() {
        // GIVEN: a path that does not yet exist
        let base = tempfile::tempdir().expect("tempdir");
        let pdir = base.path().join("new-provider");
        let m = valid_manifest();

        // WHEN
        write_manifest(&pdir, &m).expect("write");

        // THEN: the manifest file exists and is valid TOML
        let path = manifest_path(&pdir);
        assert!(path.exists());
        let read_back: WasmManifest =
            toml::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        assert_eq!(read_back.name, m.name);
    }

    // ── path helpers ─────────────────────────────────────────────────────────

    #[test]
    fn wasm_providers_dir_ends_with_nab_wasm_providers() {
        let d = wasm_providers_dir();
        assert!(d.ends_with("nab/wasm_providers"));
    }

    #[test]
    fn provider_dir_appends_name_to_base() {
        let base = PathBuf::from("/tmp/base");
        assert_eq!(provider_dir(&base, "foo"), PathBuf::from("/tmp/base/foo"));
    }
}
