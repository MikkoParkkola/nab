use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use anyhow::Result;

use nab::content::diff::ContentSnapshot;
use nab::content::diff_format::format_diff_terminal;
use nab::content::snapshot_store::SnapshotStore;
use nab::{AcceleratedClient, CookieSource, OnePasswordAuth, SafeFetchConfig};

use super::output::output_body;
use crate::OutputFormat;

/// All parameters for a `nab fetch` invocation.
///
/// Constructed from CLI arguments in `main.rs` and threaded through the
/// fetch pipeline, replacing the 22-positional-parameter function signature.
#[allow(clippy::struct_excessive_bools)] // 1:1 map of CLI boolean flags
pub struct FetchConfig {
    pub url: String,
    pub show_headers: bool,
    pub show_body: bool,
    pub format: OutputFormat,
    pub output_file: Option<PathBuf>,
    pub cookies: String,
    pub use_1password: bool,
    pub raw_html: bool,
    pub links: bool,
    pub max_body: usize,
    pub custom_headers: Vec<String>,
    pub auto_referer: bool,
    pub warmup_url: Option<String>,
    pub method: String,
    pub data: Option<String>,
    pub capture_cookies: bool,
    pub no_redirect: bool,
    pub no_spa: bool,
    pub batch_file: Option<String>,
    pub parallel: usize,
    pub proxy: Option<String>,
    pub show_diff: bool,
}

#[allow(clippy::too_many_lines)]
pub async fn cmd_fetch(cfg: &FetchConfig) -> Result<()> {
    // Handle batch mode
    if cfg.batch_file.is_some() {
        return super::fetch_batch::cmd_fetch_batch(cfg).await;
    }

    let client = build_client(cfg.no_redirect, cfg.proxy.as_deref())?;
    let profile = client.profile().await;

    let domain = url::Url::parse(&cfg.url)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_default();

    let mut cookie_header = String::new();
    let browser_name = resolve_browser_name(&cfg.cookies);

    if let Some(browser) = &browser_name {
        let source = resolve_cookie_source(browser);
        cookie_header = source.get_cookie_header(&domain).unwrap_or_default();
        if !cookie_header.is_empty() && matches!(cfg.format, OutputFormat::Full) {
            println!("🍪 Loading {} cookies for {domain}", browser.to_lowercase());
        }
    }

    let site_router = nab::site::SiteRouter::new();
    let cookie_opt = non_empty(&cookie_header);
    if let Some(site_content) = site_router.try_extract(&cfg.url, &client, cookie_opt).await {
        let markdown = !cfg.raw_html;
        output_body(
            &site_content.markdown,
            cfg.output_file.as_deref(),
            markdown,
            cfg.links,
            cfg.max_body,
            !cfg.no_spa,
        )?;
        return Ok(());
    }

    let markdown = !cfg.raw_html;

    if cfg.use_1password && OnePasswordAuth::is_available() {
        let auth = OnePasswordAuth::new(None);
        if let Ok(Some(cred)) = auth.get_credential_for_url(&cfg.url)
            && matches!(cfg.format, OutputFormat::Full)
        {
            println!("🔐 Found 1Password: {}", cred.title);
        }
    }

    if let Some(warmup) = &cfg.warmup_url {
        if matches!(cfg.format, OutputFormat::Full) {
            println!("🔥 Warming up session: {warmup}");
        }
        let mut warmup_req = client.inner().get(warmup.as_str());
        warmup_req = warmup_req.headers(profile.to_headers());
        if !cookie_header.is_empty() {
            warmup_req = warmup_req.header("Cookie", &cookie_header);
        }
        let _ = warmup_req.send().await;
    }

    let start = Instant::now();

    let is_simple_get = cfg.method.eq_ignore_ascii_case("GET")
        && cookie_header.is_empty()
        && cfg.custom_headers.is_empty()
        && cfg.data.is_none()
        && !cfg.auto_referer
        && !cfg.no_redirect;

    let (status, version, set_cookies, content_type, response_headers, body_bytes) =
        if is_simple_get {
            execute_safe_get(&client, &cfg.url, cfg.show_headers).await?
        } else {
            execute_manual_request(&client, cfg, &profile, &cookie_header).await?
        };

    let elapsed = start.elapsed();
    let raw_text = String::from_utf8_lossy(&body_bytes).to_string();

    if let Some(warning) = detect_bot_challenge(status.as_u16(), &raw_text) {
        eprintln!("⚠️  {warning}");
    }

    if cfg.capture_cookies && !set_cookies.is_empty() {
        println!("🍪 Set-Cookie:");
        for cookie in &set_cookies {
            if let Some(name_value) = cookie.split(';').next() {
                println!("   {name_value}");
            }
        }
    }

    let body_len = body_bytes.len();

    let body_text = if markdown && !cfg.links {
        convert_body_to_markdown(&body_bytes, &content_type, &cfg.url, cfg.format, body_len)
            .await?
    } else {
        raw_text.clone()
    };

    if cfg.show_diff {
        emit_diff(&cfg.url, &body_text, cfg.format);
    }

    print_output(
        cfg,
        &FetchResponse {
            profile: &profile,
            cookie_header: &cookie_header,
            status,
            version: &version,
            elapsed,
            response_headers: &response_headers,
            body_len,
            body_text: &body_text,
            raw_text: &raw_text,
            content_type: &content_type,
        },
    )?;

    Ok(())
}

/// Execute a safe GET request via `fetch_safe`.
async fn execute_safe_get(
    client: &AcceleratedClient,
    url: &str,
    show_headers: bool,
) -> Result<(
    reqwest::StatusCode,
    String,
    Vec<String>,
    String,
    Vec<(String, String)>,
    bytes::Bytes,
)> {
    let config = SafeFetchConfig::default();
    let safe_resp = client.fetch_safe(url, &config).await?;

    let set_cookies: Vec<String> = safe_resp
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.clone())
        .collect();

    let resp_headers: Vec<(String, String)> = if show_headers {
        safe_resp.headers.clone()
    } else {
        Vec::new()
    };

    Ok((
        safe_resp.status,
        String::from("HTTP/2"),
        set_cookies,
        safe_resp.content_type.clone(),
        resp_headers,
        safe_resp.body,
    ))
}

/// Execute a manually-built request (non-GET, cookies, custom headers, etc.).
async fn execute_manual_request(
    client: &AcceleratedClient,
    cfg: &FetchConfig,
    profile: &nab::fingerprint::BrowserProfile,
    cookie_header: &str,
) -> Result<(
    reqwest::StatusCode,
    String,
    Vec<String>,
    String,
    Vec<(String, String)>,
    bytes::Bytes,
)> {
    let url = &cfg.url;
    let mut request = match cfg.method.to_uppercase().as_str() {
        "POST" => client.inner().post(url),
        "PUT" => client.inner().put(url),
        "PATCH" => client.inner().patch(url),
        "DELETE" => client.inner().delete(url),
        "HEAD" => client.inner().head(url),
        _ => client.inner().get(url),
    };

    if let Some(body_data) = &cfg.data {
        request = request.body(body_data.clone());
        if !cfg
            .custom_headers
            .iter()
            .any(|h| h.to_lowercase().starts_with("content-type"))
        {
            request = request.header("Content-Type", "application/json");
        }
    }

    request = request.headers(profile.to_headers());

    if !cookie_header.is_empty() {
        request = request.header("Cookie", cookie_header);
    }

    if cfg.auto_referer && let Ok(parsed) = url::Url::parse(url) {
        let referer = format!("{}://{}/", parsed.scheme(), parsed.host_str().unwrap_or(""));
        request = request.header("Referer", referer);
    }

    for header_str in &cfg.custom_headers {
        let parts: Vec<&str> = header_str.splitn(2, ':').collect();
        if parts.len() == 2 {
            request = request.header(parts[0].trim(), parts[1].trim());
        }
    }

    let response = request.send().await?;
    let status = response.status();
    let version_str = format!("{:?}", response.version());

    let set_cookies: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(String::from))
        .collect();

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html")
        .to_string();

    let resp_headers: Vec<(String, String)> = if cfg.show_headers {
        response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    value.to_str().unwrap_or("<binary>").to_string(),
                )
            })
            .collect()
    } else {
        Vec::new()
    };

    let bytes = response.bytes().await?;

    Ok((
        status,
        version_str,
        set_cookies,
        content_type,
        resp_headers,
        bytes,
    ))
}

/// Convert body bytes to markdown via `ContentRouter`.
async fn convert_body_to_markdown(
    body_bytes: &bytes::Bytes,
    content_type: &str,
    url: &str,
    format: OutputFormat,
    body_len: usize,
) -> Result<String> {
    let router = nab::content::ContentRouter::new();
    let bytes = body_bytes.to_vec();
    let ct = content_type.to_string();
    let fetch_url = url.to_string();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::task::spawn_blocking(move || router.convert_with_url(&bytes, &ct, Some(&fetch_url))),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Content conversion timed out after 60s"))???;

    if matches!(format, OutputFormat::Full)
        && let Some(pages) = result.page_count
    {
        println!("   Pages: {pages}");
        println!("   Conversion: {:.1}ms", result.elapsed_ms);
    }

    let is_html = content_type.contains("html");
    if is_html
        && let Some(warning) =
            nab::content::html::detect_thin_content(body_len, result.markdown.len())
    {
        eprintln!("Warning: {warning}");
    }

    Ok(result.markdown)
}

/// Response data collected after the HTTP request completes.
struct FetchResponse<'a> {
    profile: &'a nab::fingerprint::BrowserProfile,
    cookie_header: &'a str,
    status: reqwest::StatusCode,
    version: &'a str,
    elapsed: std::time::Duration,
    response_headers: &'a [(String, String)],
    body_len: usize,
    body_text: &'a str,
    raw_text: &'a str,
    content_type: &'a str,
}

/// Print the response according to the requested output format.
fn print_output(cfg: &FetchConfig, resp: &FetchResponse<'_>) -> Result<()> {
    let markdown = !cfg.raw_html;
    let out_path = cfg.output_file.as_deref();

    match cfg.format {
        OutputFormat::Compact => {
            println!(
                "{} {}B {:.0}ms",
                resp.status.as_u16(),
                resp.body_len,
                resp.elapsed.as_secs_f64() * 1000.0
            );
            if cfg.show_body || out_path.is_some() || markdown || cfg.links {
                output_body(resp.body_text, out_path, markdown, cfg.links, cfg.max_body, !cfg.no_spa)?;
            }
        }
        OutputFormat::Json => {
            let metadata = serde_json::json!({
                "title": extract_title(resp.raw_text),
                "content_length": resp.body_len,
                "content_type": resp.content_type,
            });
            let output = serde_json::json!({
                "url": cfg.url,
                "status": resp.status.as_u16(),
                "content_type": resp.content_type,
                "markdown": resp.body_text,
                "metadata": metadata,
                "elapsed_ms": (resp.elapsed.as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
            });
            println!("{}", serde_json::to_string(&output)?);
            if let Some(path) = out_path {
                let mut file = File::create(path)?;
                file.write_all(resp.body_text.as_bytes())?;
            }
        }
        OutputFormat::Full => {
            println!("🌐 Fetching: {}", cfg.url);
            println!("🎭 User-Agent: {}", resp.profile.user_agent);
            if !resp.cookie_header.is_empty() {
                println!(
                    "🍪 Loaded {} cookies from {}",
                    resp.cookie_header.matches('=').count(),
                    if cfg.cookies == "auto" {
                        "browser (auto-detected)"
                    } else {
                        &cfg.cookies
                    }
                );
            }
            println!("\n📊 Response:");
            println!("   Status: {}", resp.status);
            println!("   Version: {}", resp.version);
            println!("   Time: {:.2}ms", resp.elapsed.as_secs_f64() * 1000.0);
            if cfg.show_headers {
                println!("\n📋 Headers:");
                for (name, value) in resp.response_headers {
                    println!("   {name}: {value}");
                }
            }
            println!("\n📄 Body: {} bytes", resp.body_len);
            if cfg.show_body || out_path.is_some() || markdown || cfg.links {
                output_body(resp.body_text, out_path, markdown, cfg.links, cfg.max_body, !cfg.no_spa)?;
            }
        }
    }
    Ok(())
}

/// Load the previous snapshot, compute diff, print it, then save new snapshot.
fn emit_diff(url: &str, current_text: &str, format: OutputFormat) {
    let store = SnapshotStore::default();
    let new_snap = ContentSnapshot::new(url, current_text, SystemTime::now());

    if let Some(old_snap) = store.load_latest_snapshot(url) {
        let diff = nab::content::diff::compute_diff(&old_snap, &new_snap);
        let output = format_diff_terminal(&diff);
        match format {
            OutputFormat::Full | OutputFormat::Compact => print!("{output}"),
            OutputFormat::Json => eprint!("{output}"),
        }
    } else if matches!(format, OutputFormat::Full) {
        println!("(no previous snapshot — storing baseline for future --diff runs)");
    }

    let _ = store.save_snapshot(url, &new_snap);
}

/// Extract `<title>` from HTML.
fn extract_title(html: &str) -> Option<String> {
    let doc = scraper::Html::parse_document(html);
    let sel = scraper::Selector::parse("title").ok()?;
    doc.select(&sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
}

/// Detect bot-challenge pages (Vercel, Cloudflare).
///
/// Returns an actionable warning or `None` for regular content.
pub(super) fn detect_bot_challenge(status: u16, body: &str) -> Option<String> {
    if status == 429
        && (body.contains("Vercel Security Checkpoint")
            || body.contains("We're verifying your browser"))
    {
        return Some(
            "Vercel Security Checkpoint detected. This site requires JavaScript challenge solving.\n\
             Workarounds:\n\
             1. Visit the URL in your browser first to set challenge cookies, then retry with --cookies\n\
             2. Try an alternative URL (e.g., lesswrong.com instead of alignmentforum.org)\n\
             3. Use a proxy service"
                .to_string(),
        );
    }

    // LinkedIn bot detection (HTTP 999)
    if status == 999 {
        return Some(
            "LinkedIn bot detection (HTTP 999). LinkedIn blocks non-browser TLS fingerprints.\n\
             Use: nab fetch <url> --cookies brave\n\
             This uses TLS fingerprint impersonation to match a real Chrome browser."
                .to_string(),
        );
    }

    if matches!(status, 403 | 503) && body.contains("cf-browser-verification") {
        return Some(format!(
            "Cloudflare browser verification detected (HTTP {status}).\n\
             Workarounds:\n\
             1. Visit the URL in your browser first, then: nab fetch <url> --cookies brave\n\
             2. Use a different browser profile: --cookies chrome|firefox|safari"
        ));
    }

    None
}

/// Build HTTP client with optional proxy and redirect settings.
pub(super) fn build_client(no_redirect: bool, proxy: Option<&str>) -> Result<AcceleratedClient> {
    let proxy_url = proxy
        .map(String::from)
        .or_else(|| std::env::var("HTTPS_PROXY").ok())
        .or_else(|| std::env::var("HTTP_PROXY").ok())
        .or_else(|| std::env::var("ALL_PROXY").ok())
        .or_else(|| std::env::var("https_proxy").ok())
        .or_else(|| std::env::var("http_proxy").ok())
        .or_else(|| std::env::var("all_proxy").ok());

    if let Some(ref purl) = proxy_url {
        let proxy = reqwest::Proxy::all(purl)
            .map_err(|e| anyhow::anyhow!("Invalid proxy URL '{purl}': {e}"))?;

        let mut builder = reqwest::Client::builder().proxy(proxy);
        if no_redirect {
            builder = builder.redirect(reqwest::redirect::Policy::none());
        }

        let inner_client = builder.build()?;
        AcceleratedClient::from_client(inner_client)
    } else if no_redirect {
        AcceleratedClient::new_no_redirect()
    } else {
        AcceleratedClient::new()
    }
}

/// Resolve browser name from cookie flag value.
///
/// Returns `None` for `"none"`, auto-detects for `"auto"`, or passes
/// through the explicit browser name.
pub(super) fn resolve_browser_name(cookies: &str) -> Option<String> {
    match cookies.to_lowercase().as_str() {
        "none" => None,
        "auto" => Some(
            nab::detect_default_browser()
                .map_or_else(|_| "chrome".to_string(), |b| b.as_str().to_string()),
        ),
        _ => Some(cookies.to_string()),
    }
}

/// Resolve `CookieSource` from browser name string.
pub(super) fn resolve_cookie_source(browser: &str) -> CookieSource {
    match browser.to_lowercase().as_str() {
        "brave" => CookieSource::Brave,
        "firefox" => CookieSource::Firefox,
        "safari" => CookieSource::Safari,
        _ => CookieSource::Chrome,
    }
}

/// Return `Some(header)` if non-empty, `None` otherwise.
///
/// Eliminates the repeated `if s.is_empty() { None } else { Some(s) }` pattern
/// used when passing optional cookie headers to providers.
pub(super) fn non_empty(s: &str) -> Option<&str> {
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
    use super::detect_bot_challenge;

    // ── Vercel Security Checkpoint ──────────────────────────────────────────

    #[test]
    fn detect_bot_challenge_vercel_checkpoint_keyword_returns_warning() {
        let body = "<html><body>Vercel Security Checkpoint</body></html>";
        let result = detect_bot_challenge(429, body);
        let warning = result.expect("expected a warning for Vercel checkpoint");
        assert!(
            warning.contains("Vercel"),
            "warning should mention Vercel, got: {warning}"
        );
        assert!(
            warning.contains("--cookies"),
            "warning should suggest --cookies workaround, got: {warning}"
        );
    }

    #[test]
    fn detect_bot_challenge_vercel_browser_verification_phrase_returns_warning() {
        let body = "We're verifying your browser. Please wait…";
        assert!(
            detect_bot_challenge(429, body).is_some(),
            "expected a warning for 'verifying your browser' phrase"
        );
    }

    #[test]
    fn detect_bot_challenge_vercel_wrong_status_no_warning() {
        let body = "Vercel Security Checkpoint";
        assert!(
            detect_bot_challenge(200, body).is_none(),
            "should not warn when status is 200"
        );
    }

    // ── Cloudflare browser verification ────────────────────────────────────

    #[test]
    fn detect_bot_challenge_cloudflare_403_returns_warning() {
        let body = "<div id='cf-browser-verification'>Please wait…</div>";
        let result = detect_bot_challenge(403, body);
        let warning = result.expect("expected a warning for Cloudflare 403");
        assert!(
            warning.contains("Cloudflare"),
            "warning should mention Cloudflare, got: {warning}"
        );
    }

    #[test]
    fn detect_bot_challenge_cloudflare_503_returns_warning() {
        let body = "cf-browser-verification required";
        assert!(
            detect_bot_challenge(503, body).is_some(),
            "expected a warning for Cloudflare 503"
        );
    }

    #[test]
    fn detect_bot_challenge_cloudflare_wrong_status_no_warning() {
        let body = "cf-browser-verification";
        assert!(
            detect_bot_challenge(200, body).is_none(),
            "should not warn for status 200 with CF body"
        );
    }

    // ── Normal responses ────────────────────────────────────────────────────

    #[test]
    fn detect_bot_challenge_normal_200_html_no_warning() {
        let body = "<html><body><h1>Hello world</h1></body></html>";
        assert!(
            detect_bot_challenge(200, body).is_none(),
            "should not warn for normal 200 response"
        );
    }

    #[test]
    fn detect_bot_challenge_empty_body_no_warning() {
        assert!(
            detect_bot_challenge(200, "").is_none(),
            "should not warn for empty body"
        );
    }

    #[test]
    fn detect_bot_challenge_429_unrelated_body_no_warning() {
        let body = "Rate limit exceeded. Please slow down.";
        assert!(
            detect_bot_challenge(429, body).is_none(),
            "should not warn for generic 429"
        );
    }
}
