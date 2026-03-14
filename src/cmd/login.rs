use anyhow::Result;

use nab::AcceleratedClient;

use super::fetch::{resolve_browser_name, resolve_cookie_source};
use super::output::output_body;
use crate::OutputFormat;

/// All parameters for a `nab login` invocation.
#[allow(clippy::struct_excessive_bools)] // 1:1 map of CLI boolean flags
pub struct LoginConfig {
    pub url: String,
    pub use_1password: bool,
    pub save_session: bool,
    pub cookies: String,
    pub format: OutputFormat,
    #[cfg(feature = "browser")]
    pub use_browser: bool,
}

pub async fn cmd_login(cfg: &LoginConfig) -> Result<()> {
    use nab::LoginFlow;

    if !cfg.use_1password {
        anyhow::bail!("Login requires 1Password integration. Use --1password flag.");
    }

    if !nab::OnePasswordAuth::is_available() {
        anyhow::bail!(
            "1Password CLI not available. Install with: brew install 1password-cli\n\
             Then authenticate with: op account add"
        );
    }

    println!("🔐 Starting auto-login for: {}", cfg.url);

    let (client, cookie_header) = create_login_client(&cfg.cookies, &cfg.url)?;

    #[cfg(feature = "browser")]
    let login_flow = {
        let mut flow = LoginFlow::new(client, cfg.use_1password, cookie_header);
        if cfg.use_browser {
            flow = flow.with_browser(true);
        }
        flow
    };

    #[cfg(not(feature = "browser"))]
    let login_flow = LoginFlow::new(client, cfg.use_1password, cookie_header);

    let result = login_flow.login(&cfg.url).await?;

    if cfg.save_session {
        login_flow.save_session(&cfg.url, cfg.save_session)?;
        println!("✅ Session saved");
    }

    println!("\n✅ Login successful!");
    println!("   Final URL: {}", result.final_url);

    if matches!(cfg.format, OutputFormat::Full) {
        println!("\n📄 Final page content:");
    }

    let router = nab::content::ContentRouter::new();
    let content_type = if result.body.starts_with('<') {
        "text/html"
    } else {
        "text/plain"
    };
    let conversion = router.convert(result.body.as_bytes(), content_type)?;

    output_body(&conversion.markdown, None, false, 0)?;

    Ok(())
}

/// Create HTTP client with cookie support and return cookie header.
fn create_login_client(
    cookies: &str,
    url: &str,
) -> Result<(AcceleratedClient, Option<String>)> {
    let client = AcceleratedClient::new()?;

    let domain = super::extract_domain(url);

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
