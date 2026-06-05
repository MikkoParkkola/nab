use std::path::Path;

use anyhow::{Context, Result};

use super::{resolve_browser_name, resolve_cookie_source};

/// Threshold below which a bare-domain export is considered "thin" and
/// likely to be missing auth cookies that live on a sibling subdomain.
const THIN_COOKIE_RESULT_THRESHOLD: usize = 5;

/// Entry point for `nab cookies export`.
///
/// Dispatches on `format` (`"netscape"` | `"playwright"`) and writes either to
/// `output` (when `Some`) or stdout. Reuses the same `--cookies` browser
/// selection as the rest of nab via [`resolve_browser_name`].
///
/// # Errors
/// Fails on unrecognised formats, missing browser selection, or I/O errors
/// writing the chosen output path.
pub async fn cmd_cookies_export(
    domain: &str,
    browser: &str,
    format: &str,
    output: Option<&Path>,
) -> Result<()> {
    match format.to_lowercase().as_str() {
        "netscape" => cmd_cookies_export_netscape(domain, browser, output),
        "playwright" | "storage_state" | "storage-state" => {
            cmd_cookies_export_playwright(domain, browser, output)
        }
        other => anyhow::bail!(
            "Unrecognised cookies export format: '{other}'. Use 'netscape' or 'playwright'."
        ),
    }
}

/// Resolve the selected browser name, erroring when cookies are disabled.
fn resolve_export_browser(browser: &str) -> Result<String> {
    resolve_browser_name(browser)
        .ok_or_else(|| anyhow::anyhow!("No browser specified. Use --cookies to select one."))
}

/// Write `content` to `output` (file) or stdout, with a success note on stderr.
fn emit(content: &str, output: Option<&Path>, count: usize) -> Result<()> {
    if let Some(path) = output {
        std::fs::write(path, content)
            .with_context(|| format!("Failed to write export to {}", path.display()))?;
        eprintln!("\n✅ Exported {count} cookies to {}", path.display());
    } else {
        println!("{content}");
        eprintln!("\n✅ Exported {count} cookies");
    }
    Ok(())
}

/// Export cookies for a domain as a Playwright/CDP `storage_state` JSON document.
///
/// Implements MIK-5359 `export.1`. The artifact seeds a logged-in browser
/// context later (see the nab task-engine browser-modes ADR).
fn cmd_cookies_export_playwright(domain: &str, browser: &str, output: Option<&Path>) -> Result<()> {
    let browser_name = resolve_export_browser(browser)?;
    let source = resolve_cookie_source(&browser_name);

    eprintln!("🍪 Exporting Playwright storage_state for '{domain}' from {browser_name}");

    let cookies = source.get_cookies_rich(domain)?;
    if cookies.is_empty() {
        eprintln!("No cookies found for domain: {domain}");
    }
    if let Some(msg) = bare_domain_thin_result_warning(domain, cookies.len()) {
        eprintln!("{msg}");
    }

    let count = cookies.len();
    let state = nab::auth::cookies::storage_state::StorageState::from_cookies(cookies);
    let json = state
        .to_json()
        .context("Failed to serialize storage_state JSON")?;

    emit(&json, output, count)
}

/// Export cookies for a domain in Netscape format.
fn cmd_cookies_export_netscape(domain: &str, browser: &str, output: Option<&Path>) -> Result<()> {
    let browser_name = resolve_export_browser(browser)?;
    let source = resolve_cookie_source(&browser_name);

    eprintln!("🍪 Exporting cookies for '{domain}' from {browser_name}");

    let cookies = source.get_cookies(domain)?;

    if cookies.is_empty() {
        eprintln!("No cookies found for domain: {domain}");
        return Ok(());
    }

    // Warn when a bare domain returns few cookies — auth-critical cookies may
    // live under a sibling host_key (e.g. an internal subdomain) that the
    // implicit `www.` expansion did not catch.
    if let Some(msg) = bare_domain_thin_result_warning(domain, cookies.len()) {
        eprintln!("{msg}");
    }

    let body = render_netscape(domain, &browser_name, &cookies);
    emit(&body, output, cookies.len())
}

/// Render a Netscape cookie file body for the given cookies.
///
/// Format: `domain\tinclude_subdomains\tpath\tsecure\texpiry\tname\tvalue`.
fn render_netscape(
    domain: &str,
    browser_name: &str,
    cookies: &std::collections::HashMap<String, String>,
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "# Netscape HTTP Cookie File");
    let _ = writeln!(out, "# Exported by nab from {browser_name}");
    let _ = writeln!(out, "# Domain: {domain}");
    let _ = writeln!(out);

    let include_subdomains = if domain.starts_with('.') {
        "TRUE"
    } else {
        "FALSE"
    };
    // Ensure domain starts with . for subdomain matching (Netscape convention).
    let cookie_domain = if domain.starts_with('.') {
        domain.to_string()
    } else {
        format!(".{domain}")
    };

    for (name, value) in cookies {
        // Far-future expiry sentinel for session cookies (no real expiry known).
        let _ = writeln!(
            out,
            "{cookie_domain}\t{include_subdomains}\t/\tFALSE\t0\t{name}\t{value}"
        );
    }
    out
}

/// Return a warning string when a bare-domain export looks thin, else `None`.
///
/// Triggers when:
/// * `domain` is a bare apex like `linkedin.com` (two labels, no leading dot,
///   not prefixed with `www.`), and
/// * `cookie_count` is below [`THIN_COOKIE_RESULT_THRESHOLD`].
///
/// Bare-domain queries already include the `www.{domain}` expansion (see
/// [`crate::auth::cookies::db::domain_candidates`]); this warning surfaces the
/// residual case where cookies live on other subdomains the user must specify
/// explicitly.
fn bare_domain_thin_result_warning(domain: &str, cookie_count: usize) -> Option<String> {
    let is_bare_domain =
        !domain.starts_with('.') && !domain.starts_with("www.") && domain.split('.').count() == 2;
    if is_bare_domain && cookie_count < THIN_COOKIE_RESULT_THRESHOLD {
        Some(format!(
            "⚠️  Only {cookie_count} cookies found for bare domain '{domain}'. \
             Auth cookies often live on subdomains — try \
             'nab cookies export www.{domain}' or another subdomain if results look thin."
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_fires_on_bare_domain_with_few_cookies() {
        let msg = bare_domain_thin_result_warning("linkedin.com", 3)
            .expect("warning should fire for bare domain with thin results");
        assert!(msg.contains("linkedin.com"));
        assert!(msg.contains("www.linkedin.com"));
        assert!(msg.contains('3'));
    }

    #[test]
    fn warning_silent_on_bare_domain_with_enough_cookies() {
        assert!(bare_domain_thin_result_warning("linkedin.com", 20).is_none());
    }

    #[test]
    fn warning_silent_on_explicit_www_subdomain() {
        // Explicit www. — user already opted in, no nudge needed even if thin.
        assert!(bare_domain_thin_result_warning("www.linkedin.com", 1).is_none());
    }

    #[test]
    fn warning_silent_on_subdomain() {
        assert!(bare_domain_thin_result_warning("login.example.com", 1).is_none());
    }

    #[test]
    fn warning_silent_on_dotted_leading_domain() {
        assert!(bare_domain_thin_result_warning(".linkedin.com", 1).is_none());
    }

    #[test]
    fn render_netscape_emits_header_and_tab_rows() {
        // GIVEN one cookie for a bare domain
        // WHEN rendered to Netscape format
        // THEN the header lines and a tab-separated row are present.
        let mut cookies = std::collections::HashMap::new();
        cookies.insert("session".to_string(), "abc123".to_string());
        let out = render_netscape("example.com", "brave", &cookies);
        assert!(out.contains("# Netscape HTTP Cookie File"));
        assert!(out.contains(".example.com\tFALSE\t/\tFALSE\t0\tsession\tabc123"));
    }

    #[test]
    fn unrecognised_format_is_rejected() {
        // GIVEN an unsupported export format
        // WHEN dispatched
        // THEN the command errors rather than silently defaulting.
        let err = tokio_test_block(cmd_cookies_export("example.com", "none", "yaml", None));
        assert!(err.is_err());
    }

    /// Minimal blocking executor for the single async-fn error-path test, so the
    /// crate's existing tokio dev-dependency configuration is not assumed here.
    fn tokio_test_block<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("failed to build current-thread runtime")
            .block_on(fut)
    }
}
