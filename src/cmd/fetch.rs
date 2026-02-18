use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;

use nab::{AcceleratedClient, CookieSource, OnePasswordAuth, SafeFetchConfig};

use super::output::output_body;
use crate::OutputFormat;

#[allow(clippy::too_many_arguments)]
pub async fn cmd_fetch(
    url: &str,
    show_headers: bool,
    show_body: bool,
    format: OutputFormat,
    output_file: Option<PathBuf>,
    cookies: &str,
    use_1password: bool,
    raw_html: bool,
    links: bool,
    max_body: usize,
    custom_headers: &[String],
    auto_referer: bool,
    warmup_url: Option<&str>,
    method: &str,
    data: Option<&str>,
    capture_cookies: bool,
    no_redirect: bool,
    no_spa: bool,
    batch_file: Option<&str>,
    parallel: usize,
    proxy: Option<&str>,
) -> Result<()> {
    // Handle batch mode
    if let Some(file_path) = batch_file {
        return cmd_fetch_batch(
            file_path,
            parallel,
            show_headers,
            show_body,
            format,
            cookies,
            use_1password,
            raw_html,
            links,
            max_body,
            custom_headers,
            auto_referer,
            method,
            data,
            capture_cookies,
            no_redirect,
            no_spa,
            proxy,
        )
        .await;
    }

    // Create client - with or without redirect following
    let client = build_client(no_redirect, proxy)?;
    let profile = client.profile().await;

    // Extract domain from URL
    let domain = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_default();

    // Get cookies (auto-detect by default, unless "none") — load before site providers
    // so authenticated providers (e.g., Google Workspace) receive the cookie header.
    let mut cookie_header = String::new();
    let browser_name = resolve_browser_name(cookies);

    if let Some(browser) = &browser_name {
        let source = resolve_cookie_source(browser);
        cookie_header = source.get_cookie_header(&domain).unwrap_or_default();
        if !cookie_header.is_empty() && matches!(format, OutputFormat::Full) {
            println!("🍪 Loading {} cookies for {domain}", browser.to_lowercase());
        }
    }

    // Try site-specific providers first (e.g., Twitter via FxTwitter API).
    // Cookies are passed so authenticated providers (e.g., Google Workspace) can use them.
    let site_router = nab::site::SiteRouter::new();
    let cookie_opt = if cookie_header.is_empty() {
        None
    } else {
        Some(cookie_header.as_str())
    };
    if let Some(site_content) = site_router.try_extract(url, &client, cookie_opt).await {
        // Convert raw_html flag to markdown (default is markdown unless --raw-html)
        let markdown = !raw_html;
        output_body(
            &site_content.markdown,
            output_file,
            markdown,
            links,
            max_body,
            !no_spa,
        )?;
        return Ok(());
    }

    // Convert raw_html flag to markdown (default is markdown unless --raw-html)
    let markdown = !raw_html;

    // Handle 1Password
    if use_1password && OnePasswordAuth::is_available() {
        let auth = OnePasswordAuth::new(None);
        if let Ok(Some(cred)) = auth.get_credential_for_url(url) {
            if matches!(format, OutputFormat::Full) {
                println!("🔐 Found 1Password: {}", cred.title);
            }
        }
    }

    // Session warmup (for APIs that require prior page load)
    if let Some(warmup) = warmup_url {
        if matches!(format, OutputFormat::Full) {
            println!("🔥 Warming up session: {warmup}");
        }
        let mut warmup_req = client.inner().get(warmup);
        warmup_req = warmup_req.headers(profile.to_headers());
        if !cookie_header.is_empty() {
            warmup_req = warmup_req.header("Cookie", &cookie_header);
        }
        let _ = warmup_req.send().await; // Ignore result, just establish session
    }

    let start = Instant::now();

    // Use fetch_safe for simple GET requests (SSRF protection, DNS pinning, body size cap).
    // Fall back to manual request building for custom methods, cookies, headers, or data.
    let is_simple_get = method.eq_ignore_ascii_case("GET")
        && cookie_header.is_empty()
        && custom_headers.is_empty()
        && data.is_none()
        && !auto_referer
        && !no_redirect;

    let (status, version, set_cookies, content_type, response_headers, body_bytes) =
        if is_simple_get {
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

            // SafeFetchResponse doesn't expose the HTTP version
            let version_str = String::from("HTTP/2");

            (
                safe_resp.status,
                version_str,
                set_cookies,
                safe_resp.content_type.clone(),
                resp_headers,
                safe_resp.body,
            )
        } else {
            // Manual request building for complex cases
            let mut request = match method.to_uppercase().as_str() {
                "POST" => client.inner().post(url),
                "PUT" => client.inner().put(url),
                "PATCH" => client.inner().patch(url),
                "DELETE" => client.inner().delete(url),
                "HEAD" => client.inner().head(url),
                _ => client.inner().get(url),
            };

            // Add request body for methods that support it
            if let Some(body_data) = data {
                request = request.body(body_data.to_owned());
                // Default to JSON content type if not specified
                if !custom_headers
                    .iter()
                    .any(|h| h.to_lowercase().starts_with("content-type"))
                {
                    request = request.header("Content-Type", "application/json");
                }
            }

            // Add fingerprint headers
            request = request.headers(profile.to_headers());

            // Add cookies if present
            if !cookie_header.is_empty() {
                request = request.header("Cookie", &cookie_header);
            }

            // Add auto-referer if requested (domain origin)
            if auto_referer {
                if let Ok(parsed) = url::Url::parse(url) {
                    let referer =
                        format!("{}://{}/", parsed.scheme(), parsed.host_str().unwrap_or(""));
                    request = request.header("Referer", referer);
                }
            }

            // Add custom headers (--add-header "Name: Value")
            for header_str in custom_headers {
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

            let resp_headers: Vec<(String, String)> = if show_headers {
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

            (
                status,
                version_str,
                set_cookies,
                content_type,
                resp_headers,
                bytes,
            )
        };

    let elapsed = start.elapsed();

    // Keep raw text for link extraction and bot-challenge detection.
    // Computed once here to avoid double allocation.
    let raw_text = String::from_utf8_lossy(&body_bytes).to_string();

    // Detect bot challenge pages (Vercel, Cloudflare) before content conversion.
    // Emits an actionable warning to stderr but continues with best-effort output.
    if let Some(warning) = detect_bot_challenge(status.as_u16(), &raw_text) {
        eprintln!("⚠️  {warning}");
    }

    // Output Set-Cookie headers if requested (for auth flows)
    if capture_cookies && !set_cookies.is_empty() {
        println!("🍪 Set-Cookie:");
        for cookie in &set_cookies {
            if let Some(name_value) = cookie.split(';').next() {
                println!("   {name_value}");
            }
        }
    }

    let body_len = body_bytes.len();

    // Convert body to text using content-type-aware routing
    let body_text = if markdown && !links {
        let router = nab::content::ContentRouter::new();
        let ct = content_type.clone();
        let bytes = body_bytes.to_vec();
        // Pass the real URL so readability can use site-specific heuristics
        // and resolve relative links correctly (fixes LessWrong/Ghost CMS extraction)
        let fetch_url = url.to_string();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            tokio::task::spawn_blocking(move || {
                router.convert_with_url(&bytes, &ct, Some(&fetch_url))
            }),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Content conversion timed out after 60s"))???;

        if matches!(format, OutputFormat::Full) {
            if let Some(pages) = result.page_count {
                println!("   Pages: {pages}");
                println!("   Conversion: {:.1}ms", result.elapsed_ms);
            }
        }
        result.markdown
    } else {
        raw_text.clone()
    };

    // Output based on format
    match format {
        OutputFormat::Compact => {
            println!(
                "{} {}B {:.0}ms",
                status.as_u16(),
                body_len,
                elapsed.as_secs_f64() * 1000.0
            );

            if show_body || output_file.is_some() || markdown || links {
                output_body(&body_text, output_file, markdown, links, max_body, !no_spa)?;
            }
        }
        OutputFormat::Json => {
            let metadata = serde_json::json!({
                "title": extract_title(&raw_text),
                "content_length": body_len,
                "content_type": content_type,
            });
            let output = serde_json::json!({
                "url": url,
                "status": status.as_u16(),
                "content_type": content_type,
                "markdown": body_text,
                "metadata": metadata,
                "elapsed_ms": (elapsed.as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
            });
            println!("{}", serde_json::to_string(&output)?);

            if let Some(path) = output_file {
                let mut file = File::create(&path)?;
                file.write_all(body_text.as_bytes())?;
            }
        }
        OutputFormat::Full => {
            println!("🌐 Fetching: {url}");
            println!("🎭 User-Agent: {}", profile.user_agent);

            if !cookie_header.is_empty() {
                println!(
                    "🍪 Loaded {} cookies from {}",
                    cookie_header.matches('=').count(),
                    if cookies == "auto" {
                        "browser (auto-detected)"
                    } else {
                        cookies
                    }
                );
            }

            println!("\n📊 Response:");
            println!("   Status: {status}");
            println!("   Version: {version}");
            println!("   Time: {:.2}ms", elapsed.as_secs_f64() * 1000.0);

            if show_headers {
                println!("\n📋 Headers:");
                for (name, value) in &response_headers {
                    println!("   {name}: {value}");
                }
            }

            println!("\n📄 Body: {} bytes", body_len);

            if show_body || output_file.is_some() || markdown || links {
                output_body(&body_text, output_file, markdown, links, max_body, !no_spa)?;
            }
        }
    }

    Ok(())
}

/// Extract <title> from HTML for metadata
fn extract_title(html: &str) -> Option<String> {
    let doc = scraper::Html::parse_document(html);
    let sel = scraper::Selector::parse("title").ok()?;
    doc.select(&sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
}

/// Detect bot-challenge pages that nab cannot solve automatically.
///
/// Returns an actionable warning message when a known challenge is detected,
/// or `None` when the response appears to be regular content.
///
/// Currently detects:
/// - Vercel Security Checkpoint (HTTP 429)
/// - Cloudflare browser verification (HTTP 403/503)
fn detect_bot_challenge(status: u16, body: &str) -> Option<String> {
    // Vercel Security Checkpoint
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

    // Cloudflare browser verification
    if matches!(status, 403 | 503) && body.contains("cf-browser-verification") {
        return Some(
            "Cloudflare browser verification detected. \
             Visit the URL in your browser first, then retry with --cookies."
                .to_string(),
        );
    }

    None
}

/// Batch fetch: read URLs from file, fetch with concurrency control
#[allow(clippy::too_many_arguments)]
async fn cmd_fetch_batch(
    file_path: &str,
    parallel: usize,
    _show_headers: bool,
    show_body: bool,
    format: OutputFormat,
    cookies: &str,
    _use_1password: bool,
    raw_html: bool,
    _links: bool,
    max_body: usize,
    custom_headers: &[String],
    auto_referer: bool,
    method: &str,
    data: Option<&str>,
    _capture_cookies: bool,
    no_redirect: bool,
    _no_spa: bool,
    proxy: Option<&str>,
) -> Result<()> {
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    let contents = std::fs::read_to_string(file_path)
        .map_err(|e| anyhow::anyhow!("Failed to read batch file '{}': {}", file_path, e))?;

    let urls: Vec<String> = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(String::from)
        .collect();

    if urls.is_empty() {
        anyhow::bail!("No URLs found in batch file: {}", file_path);
    }

    eprintln!(
        "📦 Batch fetching {} URLs (concurrency: {})",
        urls.len(),
        parallel
    );

    let semaphore = Arc::new(Semaphore::new(parallel));
    let mut handles = Vec::new();

    // Clone data we need to move into tasks
    let custom_headers = custom_headers.to_vec();
    let cookies = cookies.to_string();
    let method = method.to_string();
    let data = data.map(String::from);
    let proxy_owned = proxy.map(String::from);

    for url in urls {
        let sem = semaphore.clone();
        let custom_headers = custom_headers.clone();
        let cookies = cookies.clone();
        let method = method.clone();
        let data = data.clone();
        let proxy_owned = proxy_owned.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let start = Instant::now();

            let client = match build_client(no_redirect, proxy_owned.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    return serde_json::json!({
                        "url": url,
                        "error": e.to_string(),
                    });
                }
            };
            let profile = client.profile().await;

            let domain = url::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(std::string::ToString::to_string))
                .unwrap_or_default();

            let mut cookie_header = String::new();
            let browser_name = resolve_browser_name(&cookies);
            if let Some(browser) = &browser_name {
                let source = resolve_cookie_source(browser);
                cookie_header = source.get_cookie_header(&domain).unwrap_or_default();
            }

            let mut request = match method.to_uppercase().as_str() {
                "POST" => client.inner().post(&url),
                "PUT" => client.inner().put(&url),
                "PATCH" => client.inner().patch(&url),
                "DELETE" => client.inner().delete(&url),
                "HEAD" => client.inner().head(&url),
                _ => client.inner().get(&url),
            };

            if let Some(ref body_data) = data {
                request = request.body(body_data.clone());
                if !custom_headers
                    .iter()
                    .any(|h| h.to_lowercase().starts_with("content-type"))
                {
                    request = request.header("Content-Type", "application/json");
                }
            }

            request = request.headers(profile.to_headers());
            if !cookie_header.is_empty() {
                request = request.header("Cookie", &cookie_header);
            }

            if auto_referer {
                if let Ok(parsed) = url::Url::parse(&url) {
                    let referer =
                        format!("{}://{}/", parsed.scheme(), parsed.host_str().unwrap_or(""));
                    request = request.header("Referer", referer);
                }
            }

            for header_str in &custom_headers {
                let parts: Vec<&str> = header_str.splitn(2, ':').collect();
                if parts.len() == 2 {
                    request = request.header(parts[0].trim(), parts[1].trim());
                }
            }

            match request.send().await {
                Ok(response) => {
                    let elapsed = start.elapsed();
                    let status = response.status().as_u16();
                    let content_type = response
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("text/html")
                        .to_string();

                    let body_bytes = response.bytes().await.unwrap_or_default();
                    let body_len = body_bytes.len();
                    let raw_text = String::from_utf8_lossy(&body_bytes).to_string();

                    let markdown = if !raw_html {
                        let router = nab::content::ContentRouter::new();
                        router
                            .convert(&body_bytes, &content_type)
                            .map(|r| r.markdown)
                            .unwrap_or_else(|_| raw_text.clone())
                    } else {
                        raw_text
                    };

                    let metadata = serde_json::json!({
                        "title": extract_title(&String::from_utf8_lossy(&body_bytes)),
                        "content_length": body_len,
                        "content_type": content_type,
                    });

                    serde_json::json!({
                        "url": url,
                        "status": status,
                        "content_type": content_type,
                        "markdown": markdown,
                        "metadata": metadata,
                        "elapsed_ms": (elapsed.as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
                    })
                }
                Err(e) => {
                    serde_json::json!({
                        "url": url,
                        "error": e.to_string(),
                    })
                }
            }
        });

        handles.push(handle);
    }

    // Collect results
    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => results.push(serde_json::json!({"error": e.to_string()})),
        }
    }

    // In batch mode, always output as JSON array regardless of format
    // This keeps batch output machine-parseable
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string(&results)?);
        }
        OutputFormat::Compact => {
            for r in &results {
                if let Some(err) = r.get("error") {
                    println!(
                        "ERR {} {}",
                        r.get("url").and_then(|u| u.as_str()).unwrap_or("?"),
                        err
                    );
                } else {
                    println!(
                        "{} {}B {:.0}ms {}",
                        r.get("status").and_then(|s| s.as_u64()).unwrap_or(0),
                        r.get("metadata")
                            .and_then(|m| m.get("content_length"))
                            .and_then(|l| l.as_u64())
                            .unwrap_or(0),
                        r.get("elapsed_ms").and_then(|t| t.as_f64()).unwrap_or(0.0),
                        r.get("url").and_then(|u| u.as_str()).unwrap_or("?"),
                    );
                }
            }
        }
        OutputFormat::Full => {
            // In full mode, print each result with markdown body
            for r in &results {
                if let Some(err) = r.get("error") {
                    println!(
                        "\n❌ {} - {}",
                        r.get("url").and_then(|u| u.as_str()).unwrap_or("?"),
                        err
                    );
                } else {
                    println!(
                        "\n🌐 {} [{} {:.0}ms]",
                        r.get("url").and_then(|u| u.as_str()).unwrap_or("?"),
                        r.get("status").and_then(|s| s.as_u64()).unwrap_or(0),
                        r.get("elapsed_ms").and_then(|t| t.as_f64()).unwrap_or(0.0),
                    );
                    if show_body {
                        if let Some(md) = r.get("markdown").and_then(|m| m.as_str()) {
                            let display = if max_body > 0 && md.len() > max_body {
                                &md[..max_body]
                            } else {
                                md
                            };
                            println!("{display}");
                        }
                    }
                }
            }
        }
    }

    let success_count = results.iter().filter(|r| r.get("error").is_none()).count();
    eprintln!(
        "\n📦 Batch complete: {}/{} succeeded",
        success_count,
        results.len()
    );

    Ok(())
}

/// Build HTTP client with optional proxy and redirect settings
fn build_client(no_redirect: bool, proxy: Option<&str>) -> Result<AcceleratedClient> {
    // Check for proxy from argument or environment
    let proxy_url = proxy
        .map(String::from)
        .or_else(|| std::env::var("HTTPS_PROXY").ok())
        .or_else(|| std::env::var("HTTP_PROXY").ok())
        .or_else(|| std::env::var("ALL_PROXY").ok())
        .or_else(|| std::env::var("https_proxy").ok())
        .or_else(|| std::env::var("http_proxy").ok())
        .or_else(|| std::env::var("all_proxy").ok());

    if let Some(ref purl) = proxy_url {
        // Build client with proxy
        let proxy = reqwest::Proxy::all(purl)
            .map_err(|e| anyhow::anyhow!("Invalid proxy URL '{}': {}", purl, e))?;

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

/// Resolve browser name from cookie flag
pub fn resolve_browser_name(cookies: &str) -> Option<String> {
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

/// Resolve CookieSource from browser name string
pub fn resolve_cookie_source(browser: &str) -> CookieSource {
    match browser.to_lowercase().as_str() {
        "brave" => CookieSource::Brave,
        "chrome" => CookieSource::Chrome,
        "firefox" => CookieSource::Firefox,
        "safari" => CookieSource::Safari,
        "edge" => CookieSource::Chrome,
        _ => CookieSource::Chrome,
    }
}

#[cfg(test)]
mod tests {
    use super::detect_bot_challenge;

    // ── Vercel Security Checkpoint ──────────────────────────────────────────

    #[test]
    fn detect_bot_challenge_vercel_checkpoint_keyword_returns_warning() {
        // GIVEN: A 429 response with the canonical Vercel challenge heading
        let body = "<html><body>Vercel Security Checkpoint</body></html>";
        // WHEN: we check for a bot challenge
        let result = detect_bot_challenge(429, body);
        // THEN: a non-empty warning mentioning Vercel is returned
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
        // GIVEN: A 429 response with the alternative Vercel challenge phrase
        let body = "We're verifying your browser. Please wait…";
        // WHEN: we check for a bot challenge
        let result = detect_bot_challenge(429, body);
        // THEN: a warning is returned
        assert!(
            result.is_some(),
            "expected a warning for 'verifying your browser' phrase"
        );
    }

    #[test]
    fn detect_bot_challenge_vercel_wrong_status_no_warning() {
        // GIVEN: The Vercel challenge body but a 200 status (not a real challenge)
        let body = "Vercel Security Checkpoint";
        // WHEN: we check for a bot challenge
        let result = detect_bot_challenge(200, body);
        // THEN: no warning (status mismatch)
        assert!(result.is_none(), "should not warn when status is 200");
    }

    // ── Cloudflare browser verification ────────────────────────────────────

    #[test]
    fn detect_bot_challenge_cloudflare_403_returns_warning() {
        // GIVEN: A 403 response containing the Cloudflare verification marker
        let body = "<div id='cf-browser-verification'>Please wait…</div>";
        // WHEN: we check for a bot challenge
        let result = detect_bot_challenge(403, body);
        // THEN: a warning mentioning Cloudflare is returned
        let warning = result.expect("expected a warning for Cloudflare 403");
        assert!(
            warning.contains("Cloudflare"),
            "warning should mention Cloudflare, got: {warning}"
        );
    }

    #[test]
    fn detect_bot_challenge_cloudflare_503_returns_warning() {
        // GIVEN: A 503 response containing the Cloudflare verification marker
        let body = "cf-browser-verification required";
        // WHEN: we check for a bot challenge
        let result = detect_bot_challenge(503, body);
        // THEN: a warning is returned
        assert!(result.is_some(), "expected a warning for Cloudflare 503");
    }

    #[test]
    fn detect_bot_challenge_cloudflare_wrong_status_no_warning() {
        // GIVEN: Cloudflare marker in body but benign status code
        let body = "cf-browser-verification";
        // WHEN: we check for a bot challenge
        let result = detect_bot_challenge(200, body);
        // THEN: no warning (status mismatch)
        assert!(
            result.is_none(),
            "should not warn for status 200 with CF body"
        );
    }

    // ── Normal responses ────────────────────────────────────────────────────

    #[test]
    fn detect_bot_challenge_normal_200_html_no_warning() {
        // GIVEN: A perfectly ordinary HTML page
        let body = "<html><body><h1>Hello world</h1></body></html>";
        // WHEN: we check for a bot challenge
        let result = detect_bot_challenge(200, body);
        // THEN: no warning
        assert!(result.is_none(), "should not warn for normal 200 response");
    }

    #[test]
    fn detect_bot_challenge_empty_body_no_warning() {
        // GIVEN: An empty response body with a 200 status
        // WHEN: we check for a bot challenge
        let result = detect_bot_challenge(200, "");
        // THEN: no warning
        assert!(result.is_none(), "should not warn for empty body");
    }

    #[test]
    fn detect_bot_challenge_429_unrelated_body_no_warning() {
        // GIVEN: A 429 Too Many Requests with a rate-limit message (not Vercel)
        let body = "Rate limit exceeded. Please slow down.";
        // WHEN: we check for a bot challenge
        let result = detect_bot_challenge(429, body);
        // THEN: no warning (not a Vercel challenge)
        assert!(result.is_none(), "should not warn for generic 429");
    }
}
