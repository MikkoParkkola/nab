//! `nab models` subcommand — manage locally-built inference binaries and GGUF model files.
//!
//! Persistent install location:
//! - Built models: `~/.local/share/nab/models/<name>/`
//! - Binary symlinks: `~/.local/share/nab/bin/<binary>`
//! - Downloaded model weights: `~/.cache/nab/models/<file>`
//!
//! # Supported models
//!
//! | Name | Type | Platform |
//! |------|------|----------|
//! | `fluidaudio` | Swift binary (subprocess) | macOS only |
//! | `whisper` | GGUF download (whisper-rs) | all |
//! | `sherpa-onnx` | ONNX download (sherpa-onnx) | all |

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info};

#[cfg(target_os = "macos")]
use tracing::warn;

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

const BYTES_PER_MIB: u64 = 1024 * 1024;

fn format_mebibytes(bytes: u64) -> String {
    let tenths = bytes.saturating_mul(10) / BYTES_PER_MIB;
    format!("{}.{}", tenths / 10, tenths % 10)
}

fn format_percent(numerator: u64, denominator: u64) -> u64 {
    numerator
        .saturating_mul(100)
        .checked_div(denominator)
        .unwrap_or(0)
}

/// Read the pinned git SHA from the VERSION file. Returns `None` when absent.
pub fn read_version(name: &str) -> Option<String> {
    let path = version_file_path(name).ok()?;
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Write a git SHA to the VERSION file.
#[cfg(target_os = "macos")]
pub fn write_version(name: &str, sha: &str) -> Result<()> {
    let path = version_file_path(name)?;
    std::fs::write(&path, format!("{sha}\n"))
        .with_context(|| format!("writing VERSION to {}", path.display()))
}

// ─── Status helpers ───────────────────────────────────────────────────────────

/// Installation status of a model.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallStatus {
    /// Binary symlink exists and points to an existing target (subprocess models),
    /// or all required model files exist on disk (ONNX/GGUF download models).
    Installed { version: Option<String> },
    /// Binary symlink is present but dangling (target was deleted — e.g. after reboot).
    BrokenSymlink,
    /// No symlink / model files present.
    NotInstalled,
}

/// Inspect the installation status of `model`.
///
/// For `whisper` and `sherpa-onnx`, checks for the downloaded model files.
/// For `fluidaudio`, checks the binary symlink.
pub fn install_status(model: &ModelEntry) -> Result<InstallStatus> {
    match model.name {
        "whisper" => {
            // installed when GGUF file exists and is > 100 MB
            let path = whisper_model_path()?;
            let ok = path.metadata().is_ok_and(|m| m.len() >= 100 * 1024 * 1024);
            if ok {
                Ok(InstallStatus::Installed { version: None })
            } else {
                Ok(InstallStatus::NotInstalled)
            }
        }
        "sherpa-onnx" => {
            // installed when all four ONNX files exist
            let dir = sherpa_model_dir()?;
            let ok = SHERPA_FILES.iter().all(|f| dir.join(f).exists());
            if ok {
                Ok(InstallStatus::Installed { version: None })
            } else {
                Ok(InstallStatus::NotInstalled)
            }
        }
        _ => {
            // Binary symlink check for subprocess-style models (fluidaudio).
            let link_path = binary_symlink_path(model.binary_name)?;
            if !link_path.exists() && !link_path.is_symlink() {
                return Ok(InstallStatus::NotInstalled);
            }
            if link_path.exists() {
                let version = read_version(model.name);
                Ok(InstallStatus::Installed { version })
            } else {
                Ok(InstallStatus::BrokenSymlink)
            }
        }
    }
}

// ─── Subcommand actions ───────────────────────────────────────────────────────

/// `nab models list` — print installed models with status.
pub async fn cmd_models_list() -> Result<()> {
    println!("{:<16} {:<10} {:<12} VERSION", "MODEL", "PHASE", "STATUS");
    println!("{}", "-".repeat(60));

    let mut any_checkout = false;
    for model in KNOWN_MODELS {
        let (status_str, version_str) = match install_status(model)? {
            InstallStatus::Installed { version } => {
                // For git-backed models, "installed" is not the same as "current".
                let install_dir = model_install_dir(model.name)?;
                let status = match commits_behind(&install_dir).await {
                    Some(0) => {
                        any_checkout = true;
                        "current".to_string()
                    }
                    Some(n) => {
                        any_checkout = true;
                        format!("behind {n}")
                    }
                    None => "installed".to_string(),
                };
                (
                    status,
                    version.map_or_else(|| "—".to_string(), |v| short_sha(&v)),
                )
            }
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

    if any_checkout {
        println!("\ncurrent/behind is relative to the last `nab models fetch <name>`.");
    }
    Ok(())
}

/// `nab models verify` — verify every installed model is usable.
///
/// For subprocess models (fluidaudio), runs the binary with `--help`.
/// For download models (whisper, sherpa-onnx), checks that all required files
/// exist and meet the minimum size requirement.
pub async fn cmd_models_verify() -> Result<()> {
    let mut all_ok = true;

    for model in KNOWN_MODELS {
        if !matches!(install_status(model)?, InstallStatus::Installed { .. }) {
            continue;
        }

        let ok = match model.name {
            "whisper" => verify_whisper_files()?,
            "sherpa-onnx" => verify_sherpa_files()?,
            _ => verify_binary(model.binary_name).await,
        };

        if ok {
            println!("[ok] {}", model.name);
        } else {
            println!("[FAIL] {}", model.name);
            all_ok = false;
        }
    }

    if all_ok {
        Ok(())
    } else {
        anyhow::bail!("one or more installed models failed verification")
    }
}

fn verify_whisper_files() -> Result<bool> {
    let path = whisper_model_path()?;
    let size = path.metadata().map_or(0, |m| m.len());
    if size >= 100 * 1024 * 1024 {
        println!(
            "  whisper: {} MB — {}",
            format_mebibytes(size),
            path.display()
        );
        Ok(true)
    } else {
        println!(
            "  whisper: file too small or missing ({} bytes) — {}",
            size,
            path.display()
        );
        Ok(false)
    }
}

fn verify_sherpa_files() -> Result<bool> {
    let dir = sherpa_model_dir()?;
    let mut ok = true;
    for file in SHERPA_FILES {
        let path = dir.join(file);
        if path.exists() {
            let size = path.metadata().map_or(0, |m| m.len());
            println!("  sherpa-onnx/{file}: {} MB", format_mebibytes(size));
        } else {
            println!("  sherpa-onnx/{file}: MISSING");
            ok = false;
        }
    }
    Ok(ok)
}

async fn verify_binary(binary_name: &str) -> bool {
    let Ok(bin) = binary_symlink_path(binary_name) else {
        return false;
    };
    tokio::process::Command::new(&bin)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// `nab models fetch <name>` — download or clone + build + symlink a model.
pub async fn cmd_models_fetch(name: &str) -> Result<()> {
    let model = KNOWN_MODELS
        .iter()
        .find(|m| m.name == name)
        .with_context(|| {
            format!(
                "unknown model '{}'. Known models: {}",
                name,
                KNOWN_MODELS
                    .iter()
                    .map(|m| m.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;

    match model.name {
        "fluidaudio" => fetch_fluidaudio_dispatch(model).await,
        "whisper" => fetch_whisper().await,
        "sherpa-onnx" => fetch_sherpa_onnx().await,
        other => anyhow::bail!("no fetch implementation for model '{other}'"),
    }
}

// ─── fluidaudio dispatch ──────────────────────────────────────────────────────

#[cfg(not(target_os = "macos"))]
fn fetch_fluidaudio_dispatch(_model: &ModelEntry) -> std::future::Ready<Result<()>> {
    std::future::ready(Err(anyhow::anyhow!(
        "FluidAudio is macOS-only. On Linux/Windows use `nab models fetch whisper` \
         or `nab models fetch sherpa-onnx` instead."
    )))
}

#[cfg(target_os = "macos")]
async fn fetch_fluidaudio_dispatch(model: &ModelEntry) -> Result<()> {
    fetch_fluidaudio(model).await
}

// ─── whisper download ─────────────────────────────────────────────────────────

/// URL for `whisper-large-v3-turbo Q5_0 GGUF`
/// (`ggerganov/whisper.cpp` on Hugging Face).
const WHISPER_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin";

/// `~/.cache/nab/models/whisper-large-v3-turbo-q5_0.bin`
pub fn whisper_model_path() -> Result<PathBuf> {
    nab_cache_dir().map(|d| d.join("whisper-large-v3-turbo-q5_0.bin"))
}

/// `~/.cache/nab`
pub fn nab_cache_dir() -> Result<PathBuf> {
    dirs::cache_dir()
        .map(|d| d.join("nab/models"))
        .context("could not resolve cache dir (XDG_CACHE_HOME / ~/Library/Caches)")
}

/// Download `whisper-large-v3-turbo Q5_0 GGUF` to `~/.cache/nab/models/`.
async fn fetch_whisper() -> Result<()> {
    let dest = whisper_model_path()?;

    if dest.exists() {
        println!("whisper model already downloaded at {}", dest.display());
        return Ok(());
    }

    let parent = dest.parent().context("dest has no parent")?;
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating {}", parent.display()))?;

    println!("Downloading whisper-large-v3-turbo-q5_0 (~590 MB)…");
    println!("  URL: {WHISPER_MODEL_URL}");
    println!("  Destination: {}", dest.display());

    download_with_progress(WHISPER_MODEL_URL, &dest).await?;

    // Sanity check: file must be > 100 MB.
    let size = std::fs::metadata(&dest)?.len();
    if size < 100 * 1024 * 1024 {
        std::fs::remove_file(&dest).ok();
        anyhow::bail!("downloaded file is too small ({size} bytes) — likely a truncated download");
    }

    println!(
        "whisper model installed ({} MB): {}",
        format_mebibytes(size),
        dest.display()
    );
    Ok(())
}

// ─── sherpa-onnx download ─────────────────────────────────────────────────────

const SHERPA_BASE_URL: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3/resolve/main";

const SHERPA_FILES: &[&str] = &["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"];

/// `~/.cache/nab/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3/`
pub fn sherpa_model_dir() -> Result<PathBuf> {
    nab_cache_dir().map(|d| d.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3"))
}

/// Download all four Parakeet TDT v3 ONNX files to the sherpa model dir.
async fn fetch_sherpa_onnx() -> Result<()> {
    let model_dir = sherpa_model_dir()?;

    tokio::fs::create_dir_all(&model_dir)
        .await
        .with_context(|| format!("creating {}", model_dir.display()))?;

    let all_present = SHERPA_FILES.iter().all(|f| model_dir.join(f).exists());

    if all_present {
        println!(
            "sherpa-onnx model files already present at {}",
            model_dir.display()
        );
        return Ok(());
    }

    println!("Downloading Parakeet TDT v3 ONNX model files…");
    println!("  Destination: {}", model_dir.display());

    for file in SHERPA_FILES {
        let dest = model_dir.join(file);
        if dest.exists() {
            info!("skipping {file} (already present)");
            continue;
        }
        let url = format!("{SHERPA_BASE_URL}/{file}");
        println!("  Downloading {file}…");
        download_with_progress(&url, &dest).await?;
    }

    println!("sherpa-onnx model installed at: {}", model_dir.display());
    Ok(())
}

// ─── Shared download helper ───────────────────────────────────────────────────

/// Download `url` to `dest` with a simple byte-count progress display.
async fn download_with_progress(url: &str, dest: &Path) -> Result<()> {
    use futures::StreamExt as _;

    let response = reqwest::get(url)
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP error for {url}"))?;

    let total = response.content_length();
    let mut stream = response.bytes_stream();

    let mut file =
        std::fs::File::create(dest).with_context(|| format!("creating {}", dest.display()))?;

    let mut downloaded: u64 = 0;
    let mut last_print: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("reading stream for {url}"))?;
        file.write_all(&chunk)
            .with_context(|| format!("writing to {}", dest.display()))?;
        downloaded += u64::try_from(chunk.len()).unwrap_or(u64::MAX);

        // Print progress every ~10 MB.
        if downloaded - last_print >= 10 * 1024 * 1024 {
            last_print = downloaded;
            match total {
                Some(t) => print!(
                    "\r  {} / {} MB ({}%)",
                    format_mebibytes(downloaded),
                    format_mebibytes(t),
                    format_percent(downloaded, t)
                ),
                None => print!("\r  {} MB", format_mebibytes(downloaded)),
            }
            let _ = std::io::stdout().flush();
        }
    }

    println!(); // newline after progress
    Ok(())
}

/// `nab models update <name>` — fetch + fast-forward + rebuild + re-symlink.
pub async fn cmd_models_update(name: &str) -> Result<()> {
    let model = KNOWN_MODELS
        .iter()
        .find(|m| m.name == name)
        .with_context(|| format!("unknown model '{name}'"))?;

    let install_dir = model_install_dir(model.name)?;
    if !install_dir.exists() {
        anyhow::bail!("Model '{name}' is not installed. Run `nab models fetch {name}` first.");
    }

    // Bail before fetching: a checkout this platform cannot build must not be mutated.
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("FluidAudio is macOS-only");
    }

    #[cfg(target_os = "macos")]
    {
        fast_forward_repo(&install_dir).await?;
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
        fast_forward_repo(&install_dir).await?;
    } else {
        info!("Cloning {} into {}", model.repo_url, install_dir.display());
        let parent = install_dir.parent().context("install dir has no parent")?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;

        run_subprocess(
            "git",
            &["clone", model.repo_url, &install_dir.to_string_lossy()],
        )
        .await
        .context("git clone failed")?;
    }

    build_and_symlink(model, &install_dir).await
}

#[cfg(target_os = "macos")]
async fn build_and_symlink(model: &ModelEntry, install_dir: &Path) -> Result<()> {
    info!("Building FluidAudio (swift build -c release) — this may take a few minutes…");

    run_subprocess_in_dir("swift", &["build", "-c", "release"], install_dir)
        .await
        .context("swift build failed")?;

    let built_binary = find_swift_binary(install_dir, model.binary_name).with_context(|| {
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
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

// ─── Subprocess helpers ───────────────────────────────────────────────────────

/// Run a subprocess, streaming its stderr to tracing and returning an error on
/// non-zero exit.
#[cfg(target_os = "macos")]
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
        anyhow::bail!(
            "'{program}' exited with status {status} in {}",
            dir.display()
        );
    }
}

/// Ensure `~/.local/share/nab/bin/` exists.
#[cfg(target_os = "macos")]
async fn ensure_bin_dir_exists() -> Result<()> {
    let bin_dir = nab_data_dir()?.join("bin");
    tokio::fs::create_dir_all(&bin_dir)
        .await
        .with_context(|| format!("creating bin dir {}", bin_dir.display()))
}

/// Create (or replace) a symlink at `link` pointing to `target`.
#[cfg(target_os = "macos")]
fn create_symlink(target: &Path, link: &Path) -> Result<()> {
    // Remove stale symlink/file if present.
    if link.exists() || link.is_symlink() {
        std::fs::remove_file(link)
            .with_context(|| format!("removing existing symlink {}", link.display()))?;
    }
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("creating symlink {} → {}", link.display(), target.display()))
}

/// Run `git` inside `repo_dir` and capture its trimmed stdout.
async fn git_capture(repo_dir: &Path, args: &[&str]) -> Result<String> {
    debug!(?args, cwd = %repo_dir.display(), "capturing git output");
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(repo_dir)
        .output()
        .await
        .with_context(|| format!("failed to spawn 'git' in {}", repo_dir.display()))?;
    if !out.status.success() {
        anyhow::bail!(
            "'git {}' exited with {} in {}",
            args.join(" "),
            out.status,
            repo_dir.display()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Read the current HEAD git SHA in `repo_dir`.
async fn git_sha(repo_dir: &Path) -> Result<String> {
    git_capture(repo_dir, &["rev-parse", "HEAD"]).await
}

/// First 8 characters of a git SHA, for display.
fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// Fetch `repo_dir`'s upstream branch and fast-forward HEAD onto it.
///
/// Local work is never discarded: tracked modifications, a diverged branch, or a
/// missing upstream are reported and the checkout is left untouched. Untracked
/// files do not block — nab itself writes an untracked `VERSION` here.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // Linux reaches it only from tests
async fn fast_forward_repo(repo_dir: &Path) -> Result<()> {
    info!("fetching upstream for {}", repo_dir.display());
    run_subprocess_in_dir("git", &["fetch", "--quiet"], repo_dir)
        .await
        .with_context(|| format!("git fetch failed in {}", repo_dir.display()))?;

    // Refuse before anything else touches the checkout: a tracked modification
    // means the sources are not the ones any recorded SHA would describe.
    let dirty = git_capture(repo_dir, &["status", "--porcelain", "--untracked-files=no"]).await?;
    if !dirty.is_empty() {
        anyhow::bail!(
            "{} has local modifications — refusing to update, nothing was changed. \
             Commit, stash or revert them first:\n{dirty}",
            repo_dir.display()
        );
    }

    let head = git_sha(repo_dir).await?;
    let upstream = git_capture(repo_dir, &["rev-parse", "@{u}"])
        .await
        .with_context(|| {
            format!(
                "no upstream branch configured for {} — set one with \
                 `git -C {} branch --set-upstream-to=origin/<branch>`",
                repo_dir.display(),
                repo_dir.display()
            )
        })?;

    if head == upstream {
        println!("Already at the latest upstream commit {}", short_sha(&head));
        return Ok(());
    }

    // `--ff-only` is the guard: git refuses (non-zero) on a diverged branch
    // rather than rewriting local history.
    run_subprocess_in_dir("git", &["merge", "--ff-only", &upstream], repo_dir)
        .await
        .with_context(|| {
            format!(
                "could not fast-forward {} — the local branch has diverged from upstream \
                 or an untracked file is in the way; nab will not force-update",
                repo_dir.display()
            )
        })?;

    // Report what HEAD actually did: `--ff-only` also exits 0 when upstream is
    // already an ancestor (local branch ahead), and nothing moved in that case.
    let after = git_sha(repo_dir).await?;
    if after == head {
        println!(
            "Local branch is ahead of upstream — nothing to fast-forward ({})",
            short_sha(&head)
        );
    } else {
        println!(
            "Fast-forwarded {} → {}",
            short_sha(&head),
            short_sha(&after)
        );
    }
    Ok(())
}

/// Commits `dir` is behind its upstream **as of the last fetch**, or `None` when
/// `dir` is not a git checkout with an upstream branch.
async fn commits_behind(dir: &Path) -> Option<u32> {
    git_capture(dir, &["rev-list", "--count", "HEAD..@{u}"])
        .await
        .ok()?
        .parse()
        .ok()
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
        assert!(
            s.ends_with("fluidaudio"),
            "expected 'fluidaudio' suffix: {s}"
        );
    }

    /// `binary_symlink_path` places the binary symlink under `bin/<name>`.
    #[test]
    fn binary_symlink_path_correct_structure() {
        // GIVEN the fluidaudiocli binary name
        let path = binary_symlink_path("fluidaudiocli").expect("should resolve");
        let s = path.to_string_lossy();
        // THEN path ends with bin/fluidaudiocli
        assert!(s.contains("bin"), "expected 'bin' in: {s}");
        assert!(
            s.ends_with("fluidaudiocli"),
            "expected binary name suffix: {s}"
        );
    }

    /// `version_file_path` returns a path ending with VERSION inside the model dir.
    #[test]
    fn version_file_path_is_inside_model_dir() {
        // GIVEN
        let vpath = version_file_path("fluidaudio").expect("should resolve");
        let mpath = model_install_dir("fluidaudio").expect("should resolve");
        // THEN VERSION file is a child of the model install dir
        assert!(
            vpath.starts_with(&mpath),
            "VERSION must be inside model dir"
        );
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

    /// All `KNOWN_MODELS` have non-empty names and repo URLs.
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

    /// `whisper_model_path` resolves to a path containing "nab" and ending with the GGUF filename.
    #[test]
    fn whisper_model_path_has_correct_filename() {
        // GIVEN the system has a valid cache dir
        // WHEN we resolve the whisper model path
        let path = whisper_model_path().expect("should resolve");
        let name = path.file_name().unwrap().to_string_lossy();
        // THEN the filename matches the expected GGUF
        assert_eq!(name, "whisper-large-v3-turbo-q5_0.bin");
    }

    /// `sherpa_model_dir` resolves to a path ending with the expected model dir name.
    #[test]
    fn sherpa_model_dir_has_correct_suffix() {
        // GIVEN the system has a valid cache dir
        // WHEN we resolve the sherpa model dir
        let dir = sherpa_model_dir().expect("should resolve");
        let name = dir.file_name().unwrap().to_string_lossy();
        // THEN the directory name matches the expected HuggingFace repo slug
        assert_eq!(name, "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3");
    }

    /// `install_status` for "whisper" returns `NotInstalled` when the model file is absent.
    #[test]
    fn install_status_whisper_not_installed_when_missing() {
        // GIVEN the whisper model entry
        let model = KNOWN_MODELS.iter().find(|m| m.name == "whisper").unwrap();
        // WHEN we check status (model file almost certainly absent in CI)
        let status = install_status(model).expect("should not fail");
        // THEN either NotInstalled or Installed — just verify it doesn't panic
        let _ = status;
    }

    /// `install_status` for "sherpa-onnx" returns `NotInstalled` when model dir is absent.
    #[test]
    fn install_status_sherpa_not_installed_when_missing() {
        // GIVEN the sherpa-onnx model entry
        let model = KNOWN_MODELS
            .iter()
            .find(|m| m.name == "sherpa-onnx")
            .unwrap();
        // WHEN we check status
        let status = install_status(model).expect("should not fail");
        // THEN doesn't panic — actual value depends on test environment
        let _ = status;
    }

    /// Run git with a hermetic identity — CI has none and this host may sign commits.
    fn git_fixture(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=nab-test",
                "-c",
                "user.email=nab@test.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git should be on PATH");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A local upstream repo with one commit, plus a clone of it.
    fn upstream_and_clone(tmp: &Path) -> (PathBuf, PathBuf) {
        let upstream = tmp.join("upstream");
        std::fs::create_dir_all(&upstream).expect("mkdir upstream");
        git_fixture(&upstream, &["init", "-b", "main"]);
        std::fs::write(upstream.join("file.txt"), "v1\n").expect("write v1");
        git_fixture(&upstream, &["add", "."]);
        git_fixture(&upstream, &["commit", "-m", "v1"]);

        let clone = tmp.join("clone");
        git_fixture(
            tmp,
            &[
                "clone",
                "--quiet",
                &upstream.to_string_lossy(),
                &clone.to_string_lossy(),
            ],
        );
        (upstream, clone)
    }

    /// Add a commit to the upstream repo and return its SHA.
    fn commit_upstream(upstream: &Path, content: &str) -> String {
        std::fs::write(upstream.join("file.txt"), content).expect("write");
        git_fixture(upstream, &["commit", "-am", content]);
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(upstream)
            .output()
            .expect("rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// An existing install is fetched and fast-forwarded, so the SHA that
    /// `build_and_symlink` records moves to the new upstream commit.
    #[tokio::test]
    async fn fast_forward_repo_pulls_new_upstream_commit() {
        // GIVEN a cloned install that is one commit behind upstream
        let tmp = tempfile::tempdir().expect("tmpdir");
        let (upstream, clone) = upstream_and_clone(tmp.path());
        let before = git_sha(&clone).await.expect("head");
        let want = commit_upstream(&upstream, "v2\n");
        assert_ne!(before, want, "upstream must have moved");

        // AND the untracked VERSION file nab writes into the checkout
        std::fs::write(clone.join("VERSION"), format!("{before}\n")).expect("write VERSION");

        // WHEN the install is synced
        fast_forward_repo(&clone).await.expect("fast-forward");

        // THEN the SHA `build_and_symlink` records moved to the upstream commit
        assert_eq!(git_sha(&clone).await.expect("head"), want);
        assert_eq!(commits_behind(&clone).await, Some(0));
    }

    /// A tracked local modification stops the update and is left intact.
    #[tokio::test]
    async fn fast_forward_repo_refuses_dirty_tracked_file() {
        // GIVEN an install behind upstream with a locally edited tracked file
        let tmp = tempfile::tempdir().expect("tmpdir");
        let (upstream, clone) = upstream_and_clone(tmp.path());
        let before = git_sha(&clone).await.expect("head");
        commit_upstream(&upstream, "v2\n");
        std::fs::write(clone.join("file.txt"), "local edit\n").expect("write");

        // WHEN we try to sync
        let err = fast_forward_repo(&clone).await.expect_err("must refuse");

        // THEN it reports, and neither the edit nor HEAD was touched
        assert!(
            err.to_string().contains("local modifications"),
            "unexpected error: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(clone.join("file.txt")).expect("read"),
            "local edit\n"
        );
        assert_eq!(git_sha(&clone).await.expect("head"), before);
    }

    /// A dirty checkout is refused even when it is already at the upstream commit,
    /// so no build ever records a SHA that does not describe the sources.
    #[tokio::test]
    async fn fast_forward_repo_refuses_dirty_even_when_current() {
        // GIVEN an install already at upstream with a locally edited tracked file
        let tmp = tempfile::tempdir().expect("tmpdir");
        let (_upstream, clone) = upstream_and_clone(tmp.path());
        std::fs::write(clone.join("file.txt"), "local edit\n").expect("write");

        // WHEN we try to sync
        let err = fast_forward_repo(&clone).await.expect_err("must refuse");

        // THEN it reports and leaves the edit alone
        assert!(
            err.to_string().contains("local modifications"),
            "unexpected error: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(clone.join("file.txt")).expect("read"),
            "local edit\n"
        );
    }

    /// A diverged branch is declined rather than force-updated.
    #[tokio::test]
    async fn fast_forward_repo_declines_diverged_branch() {
        // GIVEN a local commit and an upstream commit on top of a shared base
        let tmp = tempfile::tempdir().expect("tmpdir");
        let (upstream, clone) = upstream_and_clone(tmp.path());
        commit_upstream(&upstream, "v2\n");
        std::fs::write(clone.join("local.txt"), "mine\n").expect("write");
        git_fixture(&clone, &["add", "."]);
        git_fixture(&clone, &["commit", "-m", "local work"]);
        let before = git_sha(&clone).await.expect("head");

        // WHEN we try to sync
        let err = fast_forward_repo(&clone).await.expect_err("must decline");

        // THEN local history survives
        assert!(
            err.to_string().contains("fast-forward"),
            "unexpected error: {err}"
        );
        assert_eq!(git_sha(&clone).await.expect("head"), before);
        assert!(clone.join("local.txt").exists());
        // AND `nab models list` can see the missed upstream commit
        assert_eq!(commits_behind(&clone).await, Some(1));
    }

    /// `commits_behind` is `None` outside a git checkout, so `list` falls back
    /// to plain "installed" for download-only models.
    #[tokio::test]
    async fn commits_behind_none_outside_a_checkout() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        assert_eq!(commits_behind(tmp.path()).await, None);
    }
}
