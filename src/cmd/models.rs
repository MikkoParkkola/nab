//! `nab models` subcommand — manage locally-built inference binaries.
//!
//! Persistent install location: `~/.local/share/nab/models/<name>/`
//! Binary symlinks: `~/.local/share/nab/bin/<binary>`
//!
//! # Supported models
//!
//! | Name | Binary | Platform |
//! |------|--------|----------|
//! | `fluidaudio` | `fluidaudiocli` | macOS only |
//! | `whisper` | (Phase 3 stub) | all |
//! | `sherpa-onnx` | (Phase 3 stub) | all |

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

// ─── Model registry ──────────────────────────────────────────────────────────

/// A registered model that `nab models` can manage.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    /// Short name used on the CLI (`fluidaudio`, `whisper`, …).
    pub name: &'static str,
    /// Source repository URL (HTTPS, cloneable with `git`).
    pub repo_url: &'static str,
    /// Binary produced after a successful build.
    pub binary_name: &'static str,
    /// Phase number — Phase 3 models are stubs.
    pub phase: u8,
}

/// All models known to `nab models`.
pub const KNOWN_MODELS: &[ModelEntry] = &[
    ModelEntry {
        name: "fluidaudio",
        repo_url: "https://github.com/FluidInference/FluidAudio",
        binary_name: "fluidaudiocli",
        phase: 1,
    },
    ModelEntry {
        name: "whisper",
        repo_url: "https://github.com/ggerganov/whisper.cpp",
        binary_name: "whisper-cli",
        phase: 3,
    },
    ModelEntry {
        name: "sherpa-onnx",
        repo_url: "https://github.com/k2-fsa/sherpa-onnx",
        binary_name: "sherpa-onnx",
        phase: 3,
    },
];

// ─── Path helpers ─────────────────────────────────────────────────────────────

/// `~/.local/share/nab`
pub fn nab_data_dir() -> Result<PathBuf> {
    dirs::data_local_dir()
        .map(|d| d.join("nab"))
        .context("could not resolve data-local dir (XDG_DATA_HOME / ~/Library/Application Support)")
}

/// `~/.local/share/nab/models/<name>`
pub fn model_install_dir(name: &str) -> Result<PathBuf> {
    Ok(nab_data_dir()?.join("models").join(name))
}

/// `~/.local/share/nab/bin/<binary_name>`
pub fn binary_symlink_path(binary_name: &str) -> Result<PathBuf> {
    Ok(nab_data_dir()?.join("bin").join(binary_name))
}

/// `~/.local/share/nab/models/<name>/VERSION`
pub fn version_file_path(name: &str) -> Result<PathBuf> {
    Ok(model_install_dir(name)?.join("VERSION"))
}

/// Read the pinned git SHA from the VERSION file. Returns `None` when absent.
pub fn read_version(name: &str) -> Option<String> {
    let path = version_file_path(name).ok()?;
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Write a git SHA to the VERSION file.
pub fn write_version(name: &str, sha: &str) -> Result<()> {
    let path = version_file_path(name)?;
    std::fs::write(&path, format!("{sha}\n"))
        .with_context(|| format!("writing VERSION to {}", path.display()))
}

// ─── Status helpers ───────────────────────────────────────────────────────────

/// Installation status of a model binary.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallStatus {
    /// Binary symlink exists and points to an existing target.
    Installed { version: Option<String> },
    /// Binary symlink is present but dangling (target was deleted — e.g. after reboot).
    BrokenSymlink,
    /// No symlink present.
    NotInstalled,
}

/// Inspect the installation status of `model`.
pub fn install_status(model: &ModelEntry) -> Result<InstallStatus> {
    let link_path = binary_symlink_path(model.binary_name)?;

    if !link_path.exists() && !link_path.is_symlink() {
        return Ok(InstallStatus::NotInstalled);
    }

    // Symlink present — check whether the target actually exists.
    if link_path.exists() {
        let version = read_version(model.name);
        Ok(InstallStatus::Installed { version })
    } else {
        // is_symlink() == true but exists() == false → dangling
        Ok(InstallStatus::BrokenSymlink)
    }
}

// ─── Subcommand actions ───────────────────────────────────────────────────────

/// `nab models list` — print installed models with status.
pub async fn cmd_models_list() -> Result<()> {
    println!("{:<16} {:<10} {:<12} {}", "MODEL", "PHASE", "STATUS", "VERSION");
    println!("{}", "-".repeat(60));

    for model in KNOWN_MODELS {
        let (status_str, version_str) = match install_status(model)? {
            InstallStatus::Installed { version } => (
                "installed".to_string(),
                version.unwrap_or_else(|| "unknown".to_string()),
            ),
            InstallStatus::BrokenSymlink => ("broken".to_string(), "—".to_string()),
            InstallStatus::NotInstalled => ("not installed".to_string(), "—".to_string()),
        };
        println!(
            "{:<16} {:<10} {:<12} {}",
            model.name,
            format!("Phase {}", model.phase),
            status_str,
            version_str,
        );
    }
    Ok(())
}

/// `nab models verify` — ensure every installed binary is runnable.
pub async fn cmd_models_verify() -> Result<()> {
    let mut all_ok = true;

    for model in KNOWN_MODELS {
        if !matches!(install_status(model)?, InstallStatus::Installed { .. }) {
            continue;
        }

        let bin = binary_symlink_path(model.binary_name)?;
        let ok = tokio::process::Command::new(&bin)
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);

        if ok {
            println!("[ok] {} — {}", model.name, bin.display());
        } else {
            println!("[FAIL] {} — {} did not run cleanly", model.name, bin.display());
            all_ok = false;
        }
    }

    if all_ok {
        Ok(())
    } else {
        anyhow::bail!("one or more installed models failed verification")
    }
}

/// `nab models fetch <name>` — clone + build + symlink a model.
pub async fn cmd_models_fetch(name: &str) -> Result<()> {
    let model = KNOWN_MODELS
        .iter()
        .find(|m| m.name == name)
        .with_context(|| {
            format!(
                "unknown model '{}'. Known models: {}",
                name,
                KNOWN_MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
            )
        })?;

    if model.phase > 1 {
        println!(
            "Model '{}' is a Phase {} stub. Use `nab models fetch whisper` in Phase 3 \
             when whisper.cpp support lands.",
            name, model.phase
        );
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    {
        println!(
            "FluidAudio is macOS-only. Use `nab models fetch whisper` instead (Phase 3)."
        );
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    fetch_fluidaudio(model).await
}

/// `nab models update <name>` — git pull + rebuild + re-symlink.
pub async fn cmd_models_update(name: &str) -> Result<()> {
    let model = KNOWN_MODELS
        .iter()
        .find(|m| m.name == name)
        .with_context(|| format!("unknown model '{name}'"))?;

    let install_dir = model_install_dir(model.name)?;
    if !install_dir.exists() {
        anyhow::bail!(
            "Model '{}' is not installed. Run `nab models fetch {}` first.",
            name,
            name
        );
    }

    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("FluidAudio is macOS-only");
    }

    #[cfg(target_os = "macos")]
    {
        info!("pulling latest changes in {}", install_dir.display());
        run_subprocess("git", &["-C", &install_dir.to_string_lossy(), "pull"])
            .await
            .context("git pull failed")?;

        build_and_symlink(model, &install_dir).await
    }
}

// ─── macOS-specific implementation ───────────────────────────────────────────

#[cfg(target_os = "macos")]
async fn fetch_fluidaudio(model: &ModelEntry) -> Result<()> {
    let install_dir = model_install_dir(model.name)?;

    ensure_bin_dir_exists().await?;

    if install_dir.exists() {
        info!("FluidAudio already cloned at {}", install_dir.display());
    } else {
        info!("Cloning {} into {}", model.repo_url, install_dir.display());
        let parent = install_dir.parent().context("install dir has no parent")?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;

        run_subprocess("git", &["clone", model.repo_url, &install_dir.to_string_lossy()])
            .await
            .context("git clone failed")?;
    }

    build_and_symlink(model, &install_dir).await
}

#[cfg(target_os = "macos")]
async fn build_and_symlink(model: &ModelEntry, install_dir: &Path) -> Result<()> {
    info!("Building FluidAudio (swift build -c release) — this may take a few minutes…");

    run_subprocess_in_dir(
        "swift",
        &["build", "-c", "release"],
        install_dir,
    )
    .await
    .context("swift build failed")?;

    let built_binary = find_swift_binary(install_dir, model.binary_name)
        .with_context(|| {
            format!(
                "could not find '{}' after swift build in {}",
                model.binary_name,
                install_dir.display()
            )
        })?;

    debug!("built binary at {}", built_binary.display());

    let symlink_path = binary_symlink_path(model.binary_name)?;
    create_symlink(&built_binary, &symlink_path)?;

    // Capture git SHA for VERSION file.
    let sha = git_sha(install_dir).await.unwrap_or_else(|e| {
        warn!("could not read git SHA: {e}");
        "unknown".to_string()
    });
    write_version(model.name, &sha)?;

    println!(
        "FluidAudio installed successfully!\n  Binary: {}\n  Version: {}",
        symlink_path.display(),
        sha
    );
    Ok(())
}

/// Detect the swift build output dir based on host architecture.
#[cfg(target_os = "macos")]
fn swift_build_arch_dir() -> &'static str {
    // We probe the common archs. On Apple Silicon it's arm64-apple-macosx.
    if std::env::consts::ARCH == "aarch64" {
        "arm64-apple-macosx"
    } else {
        "x86_64-apple-macosx"
    }
}

/// Find the built binary produced by `swift build -c release`.
#[cfg(target_os = "macos")]
fn find_swift_binary(install_dir: &Path, binary_name: &str) -> Option<PathBuf> {
    let arch_dir = swift_build_arch_dir();
    let candidate = install_dir
        .join(".build")
        .join(arch_dir)
        .join("release")
        .join(binary_name);
    if candidate.exists() { Some(candidate) } else { None }
}

// ─── Subprocess helpers ───────────────────────────────────────────────────────

/// Run a subprocess, streaming its stderr to tracing and returning an error on
/// non-zero exit.
async fn run_subprocess(program: &str, args: &[&str]) -> Result<()> {
    debug!(cmd = program, ?args, "spawning subprocess");
    let status = tokio::process::Command::new(program)
        .args(args)
        .status()
        .await
        .with_context(|| format!("failed to spawn '{program}'"))?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("'{program}' exited with status {status}");
    }
}

/// Run a subprocess inside a working directory.
async fn run_subprocess_in_dir(program: &str, args: &[&str], dir: &Path) -> Result<()> {
    debug!(cmd = program, ?args, cwd = %dir.display(), "spawning subprocess");
    let status = tokio::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .await
        .with_context(|| format!("failed to spawn '{program}' in {}", dir.display()))?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("'{program}' exited with status {status} in {}", dir.display());
    }
}

/// Ensure `~/.local/share/nab/bin/` exists.
async fn ensure_bin_dir_exists() -> Result<()> {
    let bin_dir = nab_data_dir()?.join("bin");
    tokio::fs::create_dir_all(&bin_dir)
        .await
        .with_context(|| format!("creating bin dir {}", bin_dir.display()))
}

/// Create (or replace) a symlink at `link` pointing to `target`.
fn create_symlink(target: &Path, link: &Path) -> Result<()> {
    // Remove stale symlink/file if present.
    if link.exists() || link.is_symlink() {
        std::fs::remove_file(link)
            .with_context(|| format!("removing existing symlink {}", link.display()))?;
    }
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("creating symlink {} → {}", link.display(), target.display()))
}

/// Read the current HEAD git SHA in `repo_dir`.
async fn git_sha(repo_dir: &Path) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .args(["-C", &repo_dir.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .await
        .context("git rev-parse failed")?;
    if !out.status.success() {
        anyhow::bail!("git rev-parse exited with {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `nab_data_dir` resolves to a path containing "nab".
    #[test]
    fn nab_data_dir_contains_nab() {
        // GIVEN the system has a valid data-local dir
        // WHEN we resolve the nab data dir
        let dir = nab_data_dir().expect("should resolve");
        // THEN it contains "nab"
        assert!(
            dir.to_string_lossy().contains("nab"),
            "expected 'nab' in path: {}",
            dir.display()
        );
    }

    /// `model_install_dir` places the model under `models/<name>`.
    #[test]
    fn model_install_dir_correct_structure() {
        // GIVEN the fluidaudio model name
        let dir = model_install_dir("fluidaudio").expect("should resolve");
        let s = dir.to_string_lossy();
        // THEN path ends with models/fluidaudio
        assert!(s.contains("models"), "expected 'models' in: {s}");
        assert!(s.ends_with("fluidaudio"), "expected 'fluidaudio' suffix: {s}");
    }

    /// `binary_symlink_path` places the binary symlink under `bin/<name>`.
    #[test]
    fn binary_symlink_path_correct_structure() {
        // GIVEN the fluidaudiocli binary name
        let path = binary_symlink_path("fluidaudiocli").expect("should resolve");
        let s = path.to_string_lossy();
        // THEN path ends with bin/fluidaudiocli
        assert!(s.contains("bin"), "expected 'bin' in: {s}");
        assert!(s.ends_with("fluidaudiocli"), "expected binary name suffix: {s}");
    }

    /// `version_file_path` returns a path ending with VERSION inside the model dir.
    #[test]
    fn version_file_path_is_inside_model_dir() {
        // GIVEN
        let vpath = version_file_path("fluidaudio").expect("should resolve");
        let mpath = model_install_dir("fluidaudio").expect("should resolve");
        // THEN VERSION file is a child of the model install dir
        assert!(vpath.starts_with(&mpath), "VERSION must be inside model dir");
        assert_eq!(vpath.file_name().unwrap(), "VERSION");
    }

    /// `read_version` returns `None` when the VERSION file does not exist.
    #[test]
    fn read_version_absent_returns_none() {
        // GIVEN no model installed at a temp name
        // WHEN we read it
        let result = read_version("__nonexistent_test_model__");
        // THEN None
        assert!(result.is_none());
    }

    /// `write_version` + `read_version` round-trip.
    #[test]
    fn version_write_read_roundtrip() {
        // GIVEN a temp directory simulating the model install dir
        let tmp = tempfile::tempdir().expect("tmpdir");
        let version_path = tmp.path().join("VERSION");
        let sha = "abc123def456";

        // WHEN we write and read back
        std::fs::write(&version_path, format!("{sha}\n")).expect("write");
        let read_back = std::fs::read_to_string(&version_path)
            .ok()
            .map(|s| s.trim().to_string());

        // THEN round-trip is lossless
        assert_eq!(read_back, Some(sha.to_string()));
    }

    /// All KNOWN_MODELS have non-empty names and repo URLs.
    #[test]
    fn known_models_are_well_formed() {
        for m in KNOWN_MODELS {
            assert!(!m.name.is_empty(), "model name must not be empty");
            assert!(
                m.repo_url.starts_with("https://"),
                "repo_url must be HTTPS for model '{}'",
                m.name
            );
            assert!(!m.binary_name.is_empty(), "binary_name must not be empty");
        }
    }

    /// `install_status` returns `NotInstalled` for a model with no binary on disk.
    #[test]
    fn install_status_not_installed_for_missing_model() {
        // GIVEN a model whose binary cannot exist (temp name)
        let model = ModelEntry {
            name: "__test_absent__",
            repo_url: "https://example.com",
            binary_name: "__test_absent_bin__",
            phase: 1,
        };
        // WHEN we check status
        let status = install_status(&model).expect("should not fail");
        // THEN not installed
        assert_eq!(status, InstallStatus::NotInstalled);
    }
}
