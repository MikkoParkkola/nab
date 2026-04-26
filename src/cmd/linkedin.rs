// SPDX-License-Identifier: MIT
#![allow(clippy::doc_markdown)]

//! `nab linkedin export` — automated LinkedIn data archive request and
//! retrieval. See `src/site/linkedin/export.rs` for the protocol details.
//!
//! Workflow:
//! 1. Resolve cookies for `www.linkedin.com` (note MIK-3068: bare
//!    `linkedin.com` only returns 10 cookies; the auth-critical `li_at`
//!    + `JSESSIONID` live under `www.`).
//! 2. Extract csrf-token from JSESSIONID.
//! 3. POST the archive request (FAST or FULL).
//! 4. If `--wait` is set: poll the settings page on exponential backoff
//!    until the archive is ready, then download. Otherwise: print the
//!    request-id-shaped status and exit.

use std::path::PathBuf;
#[cfg(feature = "impersonate")]
use std::time::{Duration, Instant};

#[cfg(feature = "impersonate")]
use anyhow::Context;
use anyhow::{Result, bail};
#[cfg(feature = "impersonate")]
use tokio::io::AsyncWriteExt;

#[cfg(feature = "impersonate")]
use nab::impersonate_client::{ImpersonatedMethod, request_impersonated};
#[cfg(feature = "impersonate")]
use nab::site::linkedin::export::{
    ArchiveKind, ArchiveStatus, DEFAULT_FORM_URL, DEFAULT_REQUEST_URL, csrf_from_cookies,
    next_poll_delay, poll_archive_status, request_archive,
};

/// Configuration for `cmd_linkedin_export`.
#[derive(Debug, Clone)]
pub struct LinkedinExportConfig {
    /// `--cookies` browser flag (`auto`, `brave`, `chrome`, …, `none`).
    pub cookies: String,
    /// Archive kind: `fast` (~10 min) or `full` (~24 h).
    pub kind: ArchiveKindArg,
    /// Where to write the downloaded ZIP. When `None` and `--wait` set,
    /// derives `~/Downloads/linkedin-export-YYYYMMDD-HHMMSS.zip`.
    pub output: Option<PathBuf>,
    /// Block until the archive is ready and download it.
    /// When `false`, just kick off the request and exit.
    pub wait: bool,
    /// Initial poll interval seconds (exponential backoff up to `poll_max_secs`).
    pub poll_base_secs: u64,
    /// Cap on the polling interval.
    pub poll_max_secs: u64,
    /// Total wallclock cap. Default 26 h (covers a FULL request that drags).
    pub max_wait_secs: u64,
    /// Override the status-page URL used for polling (escape hatch when
    /// LinkedIn rotates the path).
    pub form_url: Option<String>,
    /// Override the request POST URL (escape hatch when LinkedIn rotates the
    /// internal mysettings-api path).
    pub request_url: Option<String>,
    /// Override the JSON body (escape hatch when the field name rotates).
    /// Capture from Chrome DevTools → Network tab → click "Get a copy of your
    /// data" → copy the JSON payload.
    pub body_override: Option<String>,
    /// Skip the request POST and only poll. Useful when the request was
    /// fired earlier (web UI, prior `--no-wait` invocation).
    pub poll_only: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ArchiveKindArg {
    Fast,
    Full,
}

#[cfg(feature = "impersonate")]
impl From<ArchiveKindArg> for ArchiveKind {
    fn from(a: ArchiveKindArg) -> Self {
        match a {
            ArchiveKindArg::Fast => ArchiveKind::Fast,
            ArchiveKindArg::Full => ArchiveKind::Full,
        }
    }
}

impl Default for LinkedinExportConfig {
    fn default() -> Self {
        Self {
            cookies: "auto".to_string(),
            kind: ArchiveKindArg::Full,
            output: None,
            wait: false,
            poll_base_secs: 60,
            poll_max_secs: 600,
            max_wait_secs: 26 * 60 * 60,
            form_url: None,
            request_url: None,
            body_override: None,
            poll_only: false,
        }
    }
}

#[cfg(not(feature = "impersonate"))]
pub async fn cmd_linkedin_export(_cfg: LinkedinExportConfig) -> Result<()> {
    bail!(
        "nab linkedin export requires the `impersonate` feature.\n\
         Build with: cargo build --release --features impersonate"
    )
}

#[cfg(feature = "impersonate")]
pub async fn cmd_linkedin_export(cfg: LinkedinExportConfig) -> Result<()> {
    let browser = super::resolve_browser_name(&cfg.cookies);
    let cookies =
        nab::util::resolve_cookie_header_for_domain("www.linkedin.com", browser.as_deref());

    if cookies.trim().is_empty() {
        bail!(
            "no cookies for www.linkedin.com — log into LinkedIn in {} first, \
             then re-run with --cookies {}",
            browser.as_deref().unwrap_or("your browser"),
            browser.as_deref().unwrap_or("auto")
        );
    }

    let csrf = csrf_from_cookies(&cookies)?;
    let form_url = cfg
        .form_url
        .as_deref()
        .unwrap_or(DEFAULT_FORM_URL)
        .to_string();
    let request_url = cfg
        .request_url
        .as_deref()
        .unwrap_or(DEFAULT_REQUEST_URL)
        .to_string();

    if !cfg.poll_only {
        eprintln!(
            "📨 Requesting LinkedIn data archive ({})…",
            match cfg.kind {
                ArchiveKindArg::Fast => "FAST, ~10 min",
                ArchiveKindArg::Full => "FULL, ~24 h",
            }
        );
        request_archive(
            &cookies,
            &csrf,
            cfg.kind.into(),
            &request_url,
            cfg.body_override.as_deref(),
        )
        .await?;
        eprintln!("✅ Archive request submitted.");
    }

    if !cfg.wait {
        eprintln!(
            "ℹ️  Skip --wait set. Re-run with `nab linkedin export --poll-only --wait` \
             once you receive the LinkedIn email confirmation, or set --wait now \
             to block in this process."
        );
        return Ok(());
    }

    let download_url = wait_for_ready(&cookies, &csrf, &form_url, &cfg).await?;
    let dest = resolve_output_path(cfg.output.as_deref())?;

    eprintln!("⬇️  Downloading archive → {}", dest.display());
    download_to_file(&download_url, &cookies, &dest).await?;
    eprintln!("✅ Saved {}", dest.display());

    Ok(())
}

#[cfg(feature = "impersonate")]
async fn wait_for_ready(
    cookies: &str,
    csrf: &str,
    form_url: &str,
    cfg: &LinkedinExportConfig,
) -> Result<String> {
    let started = Instant::now();
    let total_cap = Duration::from_secs(cfg.max_wait_secs);
    let mut attempt: u32 = 0;

    loop {
        let elapsed = started.elapsed();
        if elapsed >= total_cap {
            bail!(
                "timed out after {}s waiting for LinkedIn archive (cap: {}s)",
                elapsed.as_secs(),
                cfg.max_wait_secs
            );
        }

        let (status, _resp) = poll_archive_status(cookies, csrf, form_url).await?;
        match status {
            ArchiveStatus::Ready { download_url } => return Ok(download_url),
            ArchiveStatus::Pending { message } => {
                let delay = next_poll_delay(attempt, cfg.poll_base_secs, cfg.poll_max_secs);
                eprintln!(
                    "⏳ {} | next poll in {}s ({} elapsed)",
                    message.as_deref().unwrap_or("archive pending"),
                    delay.as_secs(),
                    fmt_elapsed(elapsed)
                );
                tokio::time::sleep(delay).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

#[cfg(feature = "impersonate")]
async fn download_to_file(url: &str, cookies: &str, dest: &std::path::Path) -> Result<()> {
    let resp = request_impersonated(ImpersonatedMethod::Get, url, Some(cookies), None, None)
        .await
        .with_context(|| format!("download GET failed for {url}"))?;

    if !resp.status.is_success() {
        bail!(
            "download GET returned HTTP {} for {}",
            resp.status.as_u16(),
            url
        );
    }

    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }

    let mut file = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("create {}", dest.display()))?;
    file.write_all(resp.body.as_bytes())
        .await
        .with_context(|| format!("write {}", dest.display()))?;
    file.flush().await?;
    Ok(())
}

#[cfg(feature = "impersonate")]
fn resolve_output_path(explicit: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let downloads = dirs::download_dir()
        .or_else(dirs::home_dir)
        .context("could not resolve a Downloads directory and home dir is unset")?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    Ok(downloads.join(format!("linkedin-export-{stamp}.zip")))
}

#[cfg(feature = "impersonate")]
fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m{sec:02}s")
    }
}

#[cfg(all(test, feature = "impersonate"))]
mod tests {
    use super::*;

    #[test]
    fn fmt_elapsed_under_hour() {
        assert_eq!(fmt_elapsed(Duration::from_secs(125)), "2m05s");
    }

    #[test]
    fn fmt_elapsed_over_hour() {
        assert_eq!(
            fmt_elapsed(Duration::from_secs(3 * 3600 + 12 * 60 + 7)),
            "3h12m"
        );
    }

    #[test]
    fn default_config_is_full_no_wait() {
        let c = LinkedinExportConfig::default();
        assert!(matches!(c.kind, ArchiveKindArg::Full));
        assert!(!c.wait);
        assert!(!c.poll_only);
        assert_eq!(c.poll_base_secs, 60);
        assert_eq!(c.poll_max_secs, 600);
    }

    #[test]
    fn resolve_output_path_uses_explicit_when_given() {
        let p = PathBuf::from("/tmp/explicit.zip");
        let resolved = resolve_output_path(Some(&p)).unwrap();
        assert_eq!(resolved, p);
    }

    #[test]
    fn resolve_output_path_default_has_correct_shape() {
        let resolved = resolve_output_path(None).unwrap();
        let name = resolved.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("linkedin-export-"));
        assert!(name.ends_with(".zip"));
    }
}
