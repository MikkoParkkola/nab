use anyhow::Result;

use nab::{AcceleratedClient, CookieSource};

use super::output::output_body;
use crate::OutputFormat;

#[allow(clippy::too_many_arguments)]
pub async fn cmd_login(
    url: &str,
    use_1password: bool,
    save_session: bool,
    cookies: &str,
    _show_headers: bool,
    format: OutputFormat,
    #[cfg(feature = "browser")] use_browser: bool,
) -> Result<()> {
    use nab::LoginFlow;

    if !use_1password {
        anyhow::bail!("Login requires 1Password integration. Use --1password flag.");
    }

    if !nab::OnePasswordAuth::is_available() {
        anyhow::bail!(
            "1Password CLI not available. Install with: brew install 1password-cli\n\
             Then authenticate with: op account add"
        );
    }

    println!("🔐 Starting auto-login for: {url}");

    let (client, cookie_header) = create_client_with_cookies(cookies, url)?;

    #[cfg(feature = "browser")]
    let login_flow = {
        let mut flow = LoginFlow::new(client, use_1password, cookie_header);
        if use_browser {
            flow = flow.with_browser(true);
        }
        flow
    };

    #[cfg(not(feature = "browser"))]
    let login_flow = LoginFlow::new(client, use_1password, cookie_header);

    let result = login_flow.login(url).await?;

    if save_session {
        login_flow.save_session(url, save_session)?;
        println!("✅ Session saved");
    }

    println!("\n✅ Login successful!");
    println!("   Final URL: {}", result.final_url);

    if matches!(format, OutputFormat::Full) {
        println!("\n📄 Final page content:");
    }

    let router = nab::content::ContentRouter::new();
    let content_type = if result.body.starts_with('<') {
        "text/html"
    } else {
        "text/plain"
    };
    let conversion = router.convert(result.body.as_bytes(), content_type)?;

    output_body(&conversion.markdown, None, true, false, 0, false)?;

    Ok(())
}

/// Create HTTP client with cookie support and return cookie header
fn create_client_with_cookies(
    cookies: &str,
    url: &str,
) -> Result<(AcceleratedClient, Option<String>)> {
    let client = AcceleratedClient::new()?;

    // Extract domain from URL
    let domain = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_default();

    // Get cookies (auto-detect by default, unless "none")
    let mut cookie_header = None;
    let browser_name = resolve_browser_name(cookies);

    if let Some(browser) = &browser_name {
        let source = resolve_cookie_source(browser);
        let header = source.get_cookie_header(&domain).unwrap_or_default();
        if !header.is_empty() {
            println!("🍪 Loading {} cookies for {domain}", browser.to_lowercase());
            cookie_header = Some(header);
        }
    }

    Ok((client, cookie_header))
}

/// Resolve browser name from cookies parameter
fn resolve_browser_name(cookies: &str) -> Option<String> {
    if cookies.to_lowercase() == "none" {
        None
    } else if cookies.to_lowercase() == "auto" {
        if let Ok(detected) = nab::detect_default_browser() {
            Some(detected.as_str().to_string())
        } else {
            Some("chrome".to_string()) // fallback
        }
    } else {
        Some(cookies.to_string())
    }
}

/// Resolve `CookieSource` from browser name string
fn resolve_cookie_source(browser: &str) -> CookieSource {
    match browser.to_lowercase().as_str() {
        "brave" => CookieSource::Brave,
        "firefox" => CookieSource::Firefox,
        "safari" => CookieSource::Safari,
        _ => CookieSource::Chrome, // chrome, edge, or unknown -> Chrome format
    }
}
