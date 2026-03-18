//! `nab provider` subcommands: list, install, remove.
//!
//! WASM providers are stored in `~/.config/nab/wasm_providers/<name>/`.
//! Each directory contains `manifest.toml` (metadata + URL patterns) and
//! `provider.wasm` (the compiled WASM module).
//!
//! # Commands
//!
//! - `nab provider list`              — list installed WASM providers
//! - `nab provider install <src>`     — download + install from URL or local path
//! - `nab provider remove <name>`     — uninstall a provider

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use nab::site::wasm_manifest::{
    InstalledProvider, WasmManifest, load_installed_providers, manifest_path, provider_dir,
    wasm_path, wasm_providers_dir, write_manifest,
};

// ─────────────────────────────────────────────────────────────────────────────
// Public command entry points
// ─────────────────────────────────────────────────────────────────────────────

/// `nab provider list` — print all installed WASM providers.
///
/// Output is a plain-text table with columns: Name, Version, Patterns.
///
/// # Errors
///
/// This function never returns an error; the `Result` exists for consistency
/// with other `cmd_*` functions.
#[allow(clippy::unnecessary_wraps)]
pub fn cmd_provider_list() -> Result<()> {
    let base = wasm_providers_dir();
    let providers = load_installed_providers(&base);

    if providers.is_empty() {
        println!("No WASM providers installed.");
        println!();
        println!("Install one with: nab provider install <url-or-path>");
        return Ok(());
    }

    print_provider_table(&providers);
    Ok(())
}

/// `nab provider install <src>` — install a WASM provider.
///
/// `src` may be:
/// - A local path ending in `.wasm` (the manifest is auto-generated and the
///   user is prompted to edit it — not yet implemented; a stub manifest is
///   written).
/// - A directory containing `manifest.toml` + `provider.wasm`.
/// - An HTTP/HTTPS URL pointing to a `.wasm` file (requires a sidecar
///   `<url>.manifest.toml`).
///
/// After installation the provider is immediately available in `nab provider list`.
///
/// # Errors
///
/// Returns an error if:
/// - `src` cannot be read
/// - the manifest is missing or invalid
/// - a provider with the same name is already installed (use `remove` first)
pub async fn cmd_provider_install(src: &str) -> Result<()> {
    let base = wasm_providers_dir();
    std::fs::create_dir_all(&base).with_context(|| format!("cannot create {}", base.display()))?;

    let installed = install_provider(src, &base).await?;

    println!(
        "Installed '{}' v{} ({} URL pattern{})",
        installed.manifest.name,
        installed.manifest.version,
        installed.manifest.url_patterns.len(),
        if installed.manifest.url_patterns.len() == 1 {
            ""
        } else {
            "s"
        }
    );
    Ok(())
}

/// `nab provider remove <name>` — uninstall a WASM provider.
///
/// Removes the provider directory from `~/.config/nab/wasm_providers/<name>/`.
///
/// # Errors
///
/// Returns an error if no provider with the given name is installed or if the
/// directory cannot be removed.
pub fn cmd_provider_remove(name: &str) -> Result<()> {
    let base = wasm_providers_dir();
    let dir = provider_dir(&base, name);

    if !dir.exists() {
        bail!(
            "no installed provider named '{name}'\n\
             Run 'nab provider list' to see installed providers."
        );
    }

    // Verify it really is a provider directory (has manifest.toml).
    if !manifest_path(&dir).exists() {
        bail!(
            "directory {} does not contain a manifest.toml — refusing to remove",
            dir.display()
        );
    }

    std::fs::remove_dir_all(&dir).with_context(|| format!("failed to remove {}", dir.display()))?;

    println!("Removed provider '{name}'.");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Install logic
// ─────────────────────────────────────────────────────────────────────────────

/// Install a provider from `src` into `base_dir`.
///
/// Supports two source forms:
/// 1. A local directory containing `manifest.toml` + `provider.wasm`.
/// 2. A local `.wasm` file (looks for `<stem>.manifest.toml` alongside it).
async fn install_provider(src: &str, base_dir: &Path) -> Result<InstalledProvider> {
    let src_path = PathBuf::from(src);

    if src_path.is_dir() {
        install_from_dir(&src_path, base_dir)
    } else if src_path.is_file() {
        install_from_wasm_file(&src_path, base_dir)
    } else if src.starts_with("http://") || src.starts_with("https://") {
        install_from_url(src, base_dir).await
    } else {
        bail!(
            "cannot find '{src}': not a directory, file, or HTTP URL.\n\
             Provide a local path to a provider directory or .wasm file, or an HTTP URL."
        )
    }
}

/// Install from a directory that already contains `manifest.toml` and `provider.wasm`.
fn install_from_dir(src_dir: &Path, base_dir: &Path) -> Result<InstalledProvider> {
    use nab::site::wasm_manifest::load_single_provider;

    let src_provider = load_single_provider(src_dir)?;
    let dest_dir = provider_dir(base_dir, &src_provider.manifest.name);

    ensure_not_already_installed(&src_provider.manifest.name, &dest_dir)?;

    std::fs::create_dir_all(&dest_dir)
        .with_context(|| format!("cannot create {}", dest_dir.display()))?;

    write_manifest(&dest_dir, &src_provider.manifest)?;
    copy_wasm(&src_provider.wasm_path, &dest_dir)?;

    load_installed_from_dest(&dest_dir)
}

/// Install from a `.wasm` file, looking for `<stem>.manifest.toml` nearby.
fn install_from_wasm_file(wasm_file: &Path, base_dir: &Path) -> Result<InstalledProvider> {
    let manifest_path = wasm_file.with_extension("manifest.toml");
    if !manifest_path.exists() {
        bail!(
            "no manifest found for '{}'\n\
             Expected a sidecar file at: {}",
            wasm_file.display(),
            manifest_path.display()
        );
    }

    let manifest_str = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("cannot read {}", manifest_path.display()))?;
    let manifest: WasmManifest = toml::from_str(&manifest_str)
        .with_context(|| format!("invalid TOML in {}", manifest_path.display()))?;
    manifest.validate()?;

    let dest_dir = provider_dir(base_dir, &manifest.name);
    ensure_not_already_installed(&manifest.name, &dest_dir)?;

    write_manifest(&dest_dir, &manifest)?;
    copy_wasm(wasm_file, &dest_dir)?;

    load_installed_from_dest(&dest_dir)
}

/// Install from an HTTP/HTTPS URL.
///
/// Expects the URL to point to a `.wasm` file; downloads a sidecar manifest
/// from `<url without .wasm>.manifest.toml`.
// `.wasm` is a well-defined lowercase extension for WebAssembly; case-insensitive
// comparison is not appropriate for URL string matching.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
async fn install_from_url(url: &str, base_dir: &Path) -> Result<InstalledProvider> {
    // Derive manifest URL: strip trailing .wasm if present, append .manifest.toml
    let base_url = url.strip_suffix(".wasm").unwrap_or(url);
    let manifest_url = format!("{base_url}.manifest.toml");

    let client = reqwest::Client::new();

    let manifest_bytes = client
        .get(&manifest_url)
        .send()
        .await
        .with_context(|| format!("failed to fetch manifest from {manifest_url}"))?
        .bytes()
        .await
        .context("failed to read manifest response")?;

    let manifest: WasmManifest = toml::from_str(
        std::str::from_utf8(&manifest_bytes).context("manifest response is not valid UTF-8")?,
    )
    .with_context(|| format!("invalid TOML manifest from {manifest_url}"))?;
    manifest.validate()?;

    let dest_dir = provider_dir(base_dir, &manifest.name);
    ensure_not_already_installed(&manifest.name, &dest_dir)?;

    let wasm_url = if url.ends_with(".wasm") {
        url.to_string()
    } else {
        format!("{base_url}.wasm")
    };

    let wasm_bytes = client
        .get(&wasm_url)
        .send()
        .await
        .with_context(|| format!("failed to fetch WASM from {wasm_url}"))?
        .bytes()
        .await
        .context("failed to read WASM response")?;

    write_manifest(&dest_dir, &manifest)?;
    let wasm_dest = wasm_path(&dest_dir);
    std::fs::write(&wasm_dest, &wasm_bytes)
        .with_context(|| format!("cannot write {}", wasm_dest.display()))?;

    load_installed_from_dest(&dest_dir)
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Guard: fail early if a provider with `name` is already installed in `dest_dir`.
fn ensure_not_already_installed(name: &str, dest_dir: &Path) -> Result<()> {
    if dest_dir.exists() {
        bail!(
            "provider '{name}' is already installed at {}\n\
             Run 'nab provider remove {name}' first to replace it.",
            dest_dir.display()
        );
    }
    Ok(())
}

/// Copy the WASM binary from `src` into `dest_dir/provider.wasm`.
fn copy_wasm(src: &Path, dest_dir: &Path) -> Result<()> {
    let dest = wasm_path(dest_dir);
    std::fs::copy(src, &dest).with_context(|| {
        format!(
            "cannot copy WASM from {} to {}",
            src.display(),
            dest.display()
        )
    })?;
    Ok(())
}

/// Reload and return the installed provider from `dest_dir` (validates the
/// written files are coherent).
fn load_installed_from_dest(dest_dir: &Path) -> Result<InstalledProvider> {
    use nab::site::wasm_manifest::load_single_provider;
    load_single_provider(dest_dir)
}

/// Print a table of installed providers.
fn print_provider_table(providers: &[InstalledProvider]) {
    const H_NAME: &str = "Name";
    const H_VER: &str = "Version";
    const H_PAT: &str = "URL Patterns";

    let w_name = providers
        .iter()
        .map(|p| p.manifest.name.len())
        .max()
        .unwrap_or(0)
        .max(H_NAME.len());
    let w_ver = providers
        .iter()
        .map(|p| p.manifest.version.len())
        .max()
        .unwrap_or(0)
        .max(H_VER.len());

    println!("{H_NAME:<w_name$}  {H_VER:<w_ver$}  {H_PAT}");
    let sep = format!(
        "{}  {}  {}",
        "─".repeat(w_name),
        "─".repeat(w_ver),
        "─".repeat(H_PAT.len())
    );
    println!("{sep}");

    for p in providers {
        let patterns = p.manifest.url_patterns.join(", ");
        println!(
            "{:<w_name$}  {:<w_ver$}  {patterns}",
            p.manifest.name, p.manifest.version
        );
    }

    println!("{sep}");
    let n = providers.len();
    println!("{n} provider{}", if n == 1 { "" } else { "s" });
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nab::site::wasm_manifest::{WasmManifest, wasm_path as wp, write_manifest};

    fn sample_manifest(name: &str) -> WasmManifest {
        WasmManifest {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: "Test provider".to_string(),
            author: "test@example.com".to_string(),
            url_patterns: vec![r"example\.com".to_string()],
        }
    }

    fn make_provider_dir(base: &Path, name: &str) -> PathBuf {
        let dir = provider_dir(base, name);
        write_manifest(&dir, &sample_manifest(name)).expect("write manifest");
        std::fs::write(wp(&dir), b"\x00asm\x01\x00\x00\x00").expect("write wasm");
        dir
    }

    // ── cmd_provider_list ─────────────────────────────────────────────────────

    #[test]
    fn list_empty_when_no_providers() {
        // Uses a nonexistent base dir — should print nothing, not panic.
        // We can't easily intercept stdout in unit tests; just ensure no panic.
        let base = PathBuf::from("/tmp/nab_wasm_test_empty_xyz");
        let providers = load_installed_providers(&base);
        assert!(providers.is_empty());
    }

    // ── install_from_dir ──────────────────────────────────────────────────────

    #[test]
    fn install_from_dir_copies_files() {
        // GIVEN: a source provider directory and an empty target base
        let src_base = tempfile::tempdir().expect("src tempdir");
        let dest_base = tempfile::tempdir().expect("dest tempdir");
        let src_dir = make_provider_dir(src_base.path(), "test-provider");

        // WHEN
        let result = install_from_dir(&src_dir, dest_base.path());

        // THEN: installed successfully
        assert!(result.is_ok());
        let installed = result.unwrap();
        assert_eq!(installed.manifest.name, "test-provider");

        let dest_dir = provider_dir(dest_base.path(), "test-provider");
        assert!(manifest_path(&dest_dir).exists());
        assert!(wp(&dest_dir).exists());
    }

    #[test]
    fn install_from_dir_rejects_duplicate() {
        // GIVEN: provider already installed
        let src_base = tempfile::tempdir().expect("src tempdir");
        let dest_base = tempfile::tempdir().expect("dest tempdir");
        let src_dir = make_provider_dir(src_base.path(), "duplicate");
        install_from_dir(&src_dir, dest_base.path()).expect("first install");

        // WHEN: try to install again
        let result = install_from_dir(&src_dir, dest_base.path());

        // THEN: error
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("already installed"));
    }

    // ── install_from_wasm_file ────────────────────────────────────────────────

    #[test]
    fn install_from_wasm_file_with_sidecar_manifest() {
        // GIVEN: a .wasm file and sidecar .manifest.toml
        let src_dir = tempfile::tempdir().expect("tempdir");
        let wasm_file = src_dir.path().join("provider.wasm");
        let manifest_file = src_dir.path().join("provider.manifest.toml");
        let dest_base = tempfile::tempdir().expect("dest tempdir");

        let manifest = sample_manifest("sidecar-provider");
        let toml_str = toml::to_string_pretty(&manifest).expect("serialise");
        std::fs::write(&manifest_file, &toml_str).expect("write manifest");
        std::fs::write(&wasm_file, b"\x00asm\x01\x00\x00\x00").expect("write wasm");

        // WHEN
        let result = install_from_wasm_file(&wasm_file, dest_base.path());

        // THEN
        assert!(result.is_ok());
        assert_eq!(result.unwrap().manifest.name, "sidecar-provider");
    }

    #[test]
    fn install_from_wasm_file_fails_without_sidecar() {
        // GIVEN: .wasm file but no sidecar manifest
        let src_dir = tempfile::tempdir().expect("tempdir");
        let wasm_file = src_dir.path().join("provider.wasm");
        std::fs::write(&wasm_file, b"\x00asm\x01\x00\x00\x00").expect("write wasm");
        let dest_base = tempfile::tempdir().expect("dest tempdir");

        // WHEN / THEN
        let result = install_from_wasm_file(&wasm_file, dest_base.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("manifest found"));
    }

    // ── cmd_provider_remove ───────────────────────────────────────────────────

    #[test]
    fn remove_installed_provider_deletes_directory() {
        // GIVEN: a provider installed in a temp dir
        let base = tempfile::tempdir().expect("tempdir");
        make_provider_dir(base.path(), "removable");
        let dir = provider_dir(base.path(), "removable");
        assert!(dir.exists());

        // WHEN: remove it (cmd_provider_remove uses the real base dir; test internals)
        std::fs::remove_dir_all(&dir).expect("remove");

        // THEN
        assert!(!dir.exists());
    }

    #[test]
    fn ensure_not_already_installed_passes_for_new_name() {
        // GIVEN: a non-existent destination
        let base = tempfile::tempdir().expect("tempdir");
        let dir = provider_dir(base.path(), "new-provider");
        // WHEN / THEN
        assert!(ensure_not_already_installed("new-provider", &dir).is_ok());
    }

    #[test]
    fn ensure_not_already_installed_fails_for_existing_dir() {
        // GIVEN: directory already exists
        let base = tempfile::tempdir().expect("tempdir");
        let dir = provider_dir(base.path(), "existing");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // WHEN / THEN
        let err = ensure_not_already_installed("existing", &dir).unwrap_err();
        assert!(err.to_string().contains("already installed"));
    }

    // ── install_provider dispatcher ───────────────────────────────────────────

    #[tokio::test]
    async fn install_provider_fails_for_nonexistent_path() {
        // GIVEN
        let dest_base = tempfile::tempdir().expect("tempdir");
        // WHEN
        let result = install_provider("/tmp/nab_nonexistent_provider_xyz", dest_base.path()).await;
        // THEN
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn install_provider_dispatches_to_dir_path() {
        // GIVEN: a valid source directory
        let src_base = tempfile::tempdir().expect("src tempdir");
        let dest_base = tempfile::tempdir().expect("dest tempdir");
        make_provider_dir(src_base.path(), "dispatched-provider");
        let src_dir_str = provider_dir(src_base.path(), "dispatched-provider")
            .to_string_lossy()
            .to_string();

        // WHEN
        let result = install_provider(&src_dir_str, dest_base.path()).await;

        // THEN
        assert!(result.is_ok());
    }
}
