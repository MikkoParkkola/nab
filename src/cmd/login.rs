use anyhow::Result;

use nab::AcceleratedClient;

use super::fetch::{resolve_browser_name, resolve_cookie_source};
use super::output::output_body;
use crate::OutputFormat;

#[allow(clippy::fn_params_excessive_bools)] // CLI commands require multiple independent bool flags
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

    let (client, cookie_header) = create_login_client(cookies, url)?;

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

/// Create HTTP client with cookie support and return cookie header.
fn create_login_client(
    cookies: &str,
    url: &str,
) -> Result<(AcceleratedClient, Option<String>)> {
    let client = AcceleratedClient::new()?;

    let domain = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_default();

    let mut cookie_header = None;
    if let Some(browser) = resolve_browser_name(cookies) {
        let source = resolve_cookie_source(&browser);
        let header = source.get_cookie_header(&domain).unwrap_or_default();
        if !header.is_empty() {
            println!("🍪 Loading {} cookies for {domain}", browser.to_lowercase());
            cookie_header = Some(header);
        }
    }

    Ok((client, cookie_header))
}
