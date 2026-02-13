use anyhow::Result;

use nab::AcceleratedClient;

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

    let client = create_client_with_cookies(cookies, false, url).await?;

    let login_flow = LoginFlow::new(client, use_1password);

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

/// Create HTTP client with cookie support
async fn create_client_with_cookies(
    _cookies: &str,
    _use_1password: bool,
    _url: &str,
) -> Result<AcceleratedClient> {
    AcceleratedClient::new()
}
