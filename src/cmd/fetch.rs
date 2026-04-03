use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use anyhow::Result;

use nab::content::diff::ContentSnapshot;
use nab::content::diff_format::format_diff_terminal;
use nab::content::response_classifier::{
    AuthRequiredKind, ResponseDiagnosticKind, ThinContentDiagnostic, classify_http_response,
    classify_thin_content,
};
use nab::content::snapshot_store::SnapshotStore;
use nab::{AcceleratedClient, OnePasswordAuth, SafeFetchConfig};

use super::output::{output_body, write_stdout, write_stdout_line};
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
    pub batch_file: Option<String>,
    pub parallel: usize,
    pub proxy: Option<String>,
    pub show_diff: bool,
    pub html_options: nab::content::html::HtmlConversionOptions,
}

#[allow(clippy::too_many_lines)] // Orchestration function; splitting would hurt readability
pub async fn cmd_fetch(cfg: &FetchConfig) -> Result<()> {
    // Handle batch mode
    if cfg.batch_file.is_some() {
        return super::fetch_batch::cmd_fetch_batch(cfg).await;
    }

    let client = build_client(cfg.no_redirect, cfg.proxy.as_deref())?;
    let profile = client.profile().await;

    let domain = super::extract_domain(&cfg.url);

    let cookie_header = super::resolve_cookie_header(&cfg.cookies, &domain);
    if !cookie_header.is_empty() && matches!(cfg.format, OutputFormat::Full) {
        write_stdout_line(&format!("🍪 Loading cookies for {domain}"))?;
    }

    let site_router = nab::site::SiteRouter::new();
    let cookie_opt = non_empty(&cookie_header);
    if let Some(site_content) = site_router.try_extract(&cfg.url, &client, cookie_opt).await {
        output_body(
            &site_content.markdown,
            cfg.output_file.as_deref(),
            cfg.links,
            cfg.max_body,
        )?;
        return Ok(());
    }

    let markdown = !cfg.raw_html;

    if cfg.use_1password && OnePasswordAuth::is_available() {
        let auth = OnePasswordAuth::new(None);
        if let Ok(Some(cred)) = auth.get_credential_for_url(&cfg.url)
            && matches!(cfg.format, OutputFormat::Full)
        {
            write_stdout_line(&format!("🔐 Found 1Password: {}", cred.title))?;
        }
    }

    if let Some(warmup) = &cfg.warmup_url {
        if matches!(cfg.format, OutputFormat::Full) {
            write_stdout_line(&format!("🔥 Warming up session: {warmup}"))?;
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

    if cfg.capture_cookies && !set_cookies.is_empty() {
        write_stdout_line("🍪 Set-Cookie:")?;
        for cookie in &set_cookies {
            if let Some(name_value) = cookie.split(';').next() {
                write_stdout_line(&format!("   {name_value}"))?;
            }
        }
    }

    let body_len = body_bytes.len();

    let (body_text, quality) = if markdown && !cfg.links {
        let converted = convert_body_to_markdown(
            &body_bytes,
            &content_type,
            &cfg.url,
            cfg.format,
            cfg.html_options,
        )
        .await?;

        // Attempt Next.js content chunk recovery when extraction yields thin content.
        // The readability extractor may capture 300-600 chars of nav/header/footer
        // even when the article body is empty, so we use a higher threshold (800)
        // combined with a low quality confidence score (<0.5) to detect this case.
        let quality_is_low = converted
            .quality
            .as_ref()
            .is_some_and(|q| q.confidence < 0.5);
        let is_nextjs_meta = content_type.contains("html")
            && body_len > 5_000
            && nab::content::spa_extract::is_nextjs_metadata_only(&raw_text);
        let (final_markdown, final_quality) = if is_nextjs_meta
            && (converted.markdown.len() < 800 || quality_is_low)
        {
            match recover_nextjs_content_chunks(&client, &raw_text, &cfg.url, cfg.format).await {
                Some(recovered) => (recovered, converted.quality),
                None => (converted.markdown, converted.quality),
            }
        } else {
            (converted.markdown, converted.quality)
        };

        (final_markdown, final_quality)
    } else {
        (raw_text.clone(), None)
    };

    for warning in build_fetch_diagnostics(
        status.as_u16(),
        &raw_text,
        Some(&content_type),
        body_len,
        body_text.len(),
        quality.as_ref(),
        cfg.html_options.allow_jina_fallback,
    ) {
        eprintln!("⚠️  {warning}");
    }

    if cfg.show_diff {
        emit_diff(&cfg.url, &body_text, cfg.format)?;
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
            quality: quality.as_ref(),
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

    if cfg.auto_referer
        && let Some(referer) = super::build_referer(url)
    {
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

/// Attempt to recover article content from Next.js webpack content chunks.
///
/// When a Next.js page has `__NEXT_DATA__` with only metadata (no article body),
/// the content is often compiled into a lazy-loaded webpack chunk (common for MDX
/// blogs).  This function:
///
/// 1. Discovers the webpack runtime and page component script URLs from the HTML
/// 2. Fetches them to find the content chunk filename
/// 3. Fetches the content chunk
/// 4. Extracts readable text from the compiled JSX
///
/// Returns `Some(markdown)` on success, `None` if recovery fails at any step.
async fn recover_nextjs_content_chunks(
    client: &nab::AcceleratedClient,
    html: &str,
    page_url: &str,
    format: crate::OutputFormat,
) -> Option<String> {
    if matches!(format, crate::OutputFormat::Full) {
        eprintln!("   Attempting Next.js content chunk recovery...");
    }
    let recovered = nab::util::recover_nextjs_chunks(client, html, page_url).await;
    if matches!(format, crate::OutputFormat::Full)
        && let Some(content) = recovered.as_ref()
    {
        eprintln!("   Recovered {} chars from content chunk", content.len());
    }
    recovered
}

/// Conversion result bundled with quality metadata for the output layer.
struct ConvertedBody {
    markdown: String,
    quality: Option<nab::content::quality::QualityScore>,
}

/// Convert body bytes to markdown via `ContentRouter`.
async fn convert_body_to_markdown(
    body_bytes: &bytes::Bytes,
    content_type: &str,
    url: &str,
    format: OutputFormat,
    html_options: nab::content::html::HtmlConversionOptions,
) -> Result<ConvertedBody> {
    let router = nab::content::ContentRouter::with_html_options(html_options);
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
        write_stdout_line(&format!("   Pages: {pages}"))?;
        write_stdout_line(&format!("   Conversion: {:.1}ms", result.elapsed_ms))?;
    }

    Ok(ConvertedBody {
        quality: result.quality,
        markdown: result.markdown,
    })
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
    /// Extraction quality score — present for HTML, absent for raw/binary content.
    quality: Option<&'a nab::content::quality::QualityScore>,
}

/// Print the response according to the requested output format.
fn print_output(cfg: &FetchConfig, resp: &FetchResponse<'_>) -> Result<()> {
    let markdown = !cfg.raw_html;
    let out_path = cfg.output_file.as_deref();

    match cfg.format {
        OutputFormat::Compact => {
            write_stdout_line(&format!(
                "{} {}B {:.0}ms",
                resp.status.as_u16(),
                resp.body_len,
                resp.elapsed.as_secs_f64() * 1000.0
            ))?;
            if cfg.show_body || out_path.is_some() || markdown || cfg.links {
                output_body(resp.body_text, out_path, cfg.links, cfg.max_body)?;
            }
        }
        OutputFormat::Json => {
            let metadata = serde_json::json!({
                "title": extract_title(resp.raw_text),
                "content_length": resp.body_len,
                "content_type": resp.content_type,
            });
            let mut output = serde_json::json!({
                "url": cfg.url,
                "status": resp.status.as_u16(),
                "content_type": resp.content_type,
                "markdown": resp.body_text,
                "metadata": metadata,
                "elapsed_ms": (resp.elapsed.as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
            });
            if let Some(q) = resp.quality {
                output["confidence"] = serde_json::json!(q.confidence);
                output["quality"] = serde_json::to_value(q)?;
            }
            write_stdout_line(&serde_json::to_string(&output)?)?;
            if let Some(path) = out_path {
                let mut file = File::create(path)?;
                file.write_all(resp.body_text.as_bytes())?;
            }
        }
        OutputFormat::Full => {
            write_stdout_line(&format!("🌐 Fetching: {}", cfg.url))?;
            write_stdout_line(&format!("🎭 User-Agent: {}", resp.profile.user_agent))?;
            if !resp.cookie_header.is_empty() {
                write_stdout_line(&format!(
                    "🍪 Loaded {} cookies from {}",
                    resp.cookie_header.matches('=').count(),
                    if cfg.cookies == "auto" {
                        "browser (auto-detected)"
                    } else {
                        &cfg.cookies
                    }
                ))?;
            }
            write_stdout_line("\n📊 Response:")?;
            write_stdout_line(&format!("   Status: {}", resp.status))?;
            write_stdout_line(&format!("   Version: {}", resp.version))?;
            write_stdout_line(&format!(
                "   Time: {:.2}ms",
                resp.elapsed.as_secs_f64() * 1000.0
            ))?;
            if cfg.show_headers {
                write_stdout_line("\n📋 Headers:")?;
                for (name, value) in resp.response_headers {
                    write_stdout_line(&format!("   {name}: {value}"))?;
                }
            }
            write_stdout_line(&format!("\n📄 Body: {} bytes", resp.body_len))?;
            if cfg.show_body || out_path.is_some() || markdown || cfg.links {
                output_body(resp.body_text, out_path, cfg.links, cfg.max_body)?;
            }
        }
    }
    Ok(())
}

/// Load the previous snapshot, compute diff, print it, then save new snapshot.
fn emit_diff(url: &str, current_text: &str, format: OutputFormat) -> Result<()> {
    let store = SnapshotStore::default();
    let new_snap = ContentSnapshot::new(url, current_text, SystemTime::now());

    if let Some(old_snap) = store.load_latest_snapshot(url) {
        let diff = nab::content::diff::compute_diff(&old_snap, &new_snap);
        let output = format_diff_terminal(&diff);
        match format {
            OutputFormat::Full | OutputFormat::Compact => write_stdout(&output)?,
            OutputFormat::Json => eprint!("{output}"),
        }
    } else if matches!(format, OutputFormat::Full) {
        write_stdout_line("(no previous snapshot — storing baseline for future --diff runs)")?;
    }

    let _ = store.save_snapshot(url, &new_snap);
    Ok(())
}

/// Extract `<title>` from HTML.
fn extract_title(html: &str) -> Option<String> {
    let doc = scraper::Html::parse_document(html);
    let sel = scraper::Selector::parse("title").ok()?;
    doc.select(&sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
}

fn build_fetch_diagnostics(
    status: u16,
    raw_text: &str,
    content_type: Option<&str>,
    html_len: usize,
    markdown_len: usize,
    quality: Option<&nab::content::quality::QualityScore>,
    allow_jina_fallback: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();

    if let Some(diagnostic) = classify_http_response(status, raw_text) {
        let warning = match diagnostic.kind {
            ResponseDiagnosticKind::BrowserChallenge(_) => format!(
                "{} Browser challenge likely requires cookies or JavaScript.\nTry:\n1. Visit the URL in a browser first\n2. Let nab reuse your default browser cookies automatically unless you intentionally disabled them\n3. Use --cookies brave|chrome|firefox|safari only to override the default browser profile",
                diagnostic.summary()
            ),
            ResponseDiagnosticKind::RateLimited => format!(
                "Rate limiting detected (HTTP {status}).\n{}",
                diagnostic.guidance()
            ),
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::LoginRequired) => {
                "The response looks like a login page.\nSign in in your browser first, then retry. nab already uses your default browser cookies automatically unless you disabled them."
                    .to_string()
            }
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::SessionExpired) => {
                diagnostic.message()
            }
        };
        warnings.push(warning);
    }

    if let Some(thin) = classify_thin_content(content_type, html_len, markdown_len, quality) {
        warnings.push(thin_content_message(thin));
        if !allow_jina_fallback {
            warnings.push("Remote reader fallback is disabled by --no-fallback.".to_string());
        }
    }

    warnings
}

fn thin_content_message(diagnostic: ThinContentDiagnostic) -> String {
    if let Some(message) =
        nab::content::html::detect_thin_content(diagnostic.html_bytes, diagnostic.markdown_chars)
    {
        return message;
    }

    format!(
        "Output is suspiciously thin ({} chars from {} bytes of HTML). \
         Extraction confidence is low, so the main content may be missing.\n  \
         1. nab spa <url>              (extract embedded SPA data)\n  \
         2. nab fetch <url>            (uses default browser cookies automatically)\n  \
         3. nab fetch --cookies brave <url>  (override the browser profile if needed)",
        diagnostic.markdown_chars, diagnostic.html_bytes
    )
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

// Re-export from mod.rs for internal use.
pub(super) use super::non_empty;

#[cfg(test)]
mod tests {
    use super::build_fetch_diagnostics;

    #[test]
    fn build_fetch_diagnostics_for_bot_challenge_mentions_cookies() {
        let warning = build_fetch_diagnostics(
            429,
            "<html><body>Vercel Security Checkpoint</body></html>",
            Some("text/html"),
            0,
            0,
            None,
            true,
        )
        .into_iter()
        .next()
        .expect("expected challenge warning");
        assert!(
            warning.contains("challenge"),
            "warning should mention challenge, got: {warning}"
        );
        assert!(
            warning.contains("--cookies"),
            "warning should suggest --cookies workaround, got: {warning}"
        );
    }

    #[test]
    fn build_fetch_diagnostics_for_rate_limit_is_not_bot_specific() {
        let warning = build_fetch_diagnostics(
            429,
            "Rate limit exceeded. Please slow down.",
            Some("text/html"),
            0,
            0,
            None,
            true,
        )
        .into_iter()
        .next()
        .expect("expected rate-limit warning");
        assert!(
            warning.contains("Rate limiting"),
            "warning should mention rate limiting, got: {warning}"
        );
    }

    #[test]
    fn build_fetch_diagnostics_for_thin_content_includes_no_fallback_hint() {
        let warnings = build_fetch_diagnostics(
            200,
            "<html></html>",
            Some("text/html"),
            20_000,
            100,
            None,
            false,
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("suspiciously thin")),
            "expected thin-content warning, got: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("--no-fallback")),
            "expected no-fallback hint, got: {warnings:?}"
        );
    }
}
