use anyhow::Result;

use nab::CookieSource;

use super::fetch::resolve_browser_name;

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

    let source = match browser_name.to_lowercase().as_str() {
        "brave" => CookieSource::Brave,
        "chrome" => CookieSource::Chrome,
        "firefox" => CookieSource::Firefox,
        "safari" => CookieSource::Safari,
        "edge" => CookieSource::Chrome,
        _ => CookieSource::Chrome,
    };

    eprintln!(
        "🍪 Exporting cookies for '{}' from {}",
        domain, browser_name
    );

    let cookies = source.get_cookies(domain)?;

    if cookies.is_empty() {
        eprintln!("No cookies found for domain: {domain}");
        return Ok(());
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
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            cookie_domain, include_subdomains, path, secure, expiry, name, value
        );
    }

    eprintln!("\n✅ Exported {} cookies", cookies.len());

    Ok(())
}
