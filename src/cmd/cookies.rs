use anyhow::Result;

use super::{resolve_browser_name, resolve_cookie_source};

/// Threshold below which a bare-domain export is considered "thin" and
/// likely to be missing auth cookies that live on a sibling subdomain.
const THIN_COOKIE_RESULT_THRESHOLD: usize = 5;

pub async fn cmd_cookies(subcommand: &str, domain: &str, browser: &str) -> Result<()> {
    match subcommand {
        "export" => cmd_cookies_export(domain, browser),
        _ => anyhow::bail!("Unknown cookies subcommand: {subcommand}. Use 'export'."),
    }
}

/// Export cookies for a domain in Netscape format
fn cmd_cookies_export(domain: &str, browser: &str) -> Result<()> {
    let browser_name = resolve_browser_name(browser)
        .ok_or_else(|| anyhow::anyhow!("No browser specified. Use --cookies to select one."))?;

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

    // Output in Netscape cookie format
    // Format: domain\tinclude_subdomains\tpath\tsecure\texpiry\tname\tvalue
    println!("# Netscape HTTP Cookie File");
    println!("# Exported by nab from {browser_name}");
    println!("# Domain: {domain}");
    println!();

    for (name, value) in &cookies {
        let include_subdomains = if domain.starts_with('.') {
            "TRUE"
        } else {
            "FALSE"
        };
        // Use a far-future expiry for session cookies (we don't have the actual expiry)
        let expiry = "0";
        let secure = "FALSE";
        let path = "/";

        // Ensure domain starts with . for subdomain matching (Netscape convention)
        let cookie_domain = if domain.starts_with('.') {
            domain.to_string()
        } else {
            format!(".{domain}")
        };

        println!(
            "{cookie_domain}\t{include_subdomains}\t{path}\t{secure}\t{expiry}\t{name}\t{value}"
        );
    }

    eprintln!("\n✅ Exported {} cookies", cookies.len());

    Ok(())
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
    let is_bare_domain = !domain.starts_with('.')
        && !domain.starts_with("www.")
        && domain.split('.').count() == 2;
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
}
