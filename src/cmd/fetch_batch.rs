//! Batch URL fetching for the `nab fetch --batch` flag.
//!
//! Supports per-domain rate limiting to avoid triggering anti-bot protections
//! while maximising throughput across different domains.

use std::time::{Duration, Instant};

use anyhow::Result;

use nab::content::ContentRouter;
use nab::rate_limit::DomainRateLimiter;

use super::fetch::{FetchConfig, build_client};
use crate::OutputFormat;

/// Default minimum delay between requests to the same domain (milliseconds).
const DEFAULT_DOMAIN_DELAY_MS: u64 = 200;

/// Fetch URLs from a file in parallel and print results.
#[allow(clippy::too_many_lines)]
pub async fn cmd_fetch_batch(cfg: &FetchConfig) -> Result<()> {
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    let file_path = cfg.batch_file.as_deref().expect("batch_file is Some");
    let contents = std::fs::read_to_string(file_path)
        .map_err(|e| anyhow::anyhow!("Failed to read batch file '{file_path}': {e}"))?;

    let urls: Vec<String> = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(String::from)
        .collect();

    if urls.is_empty() {
        anyhow::bail!("No URLs found in batch file: {file_path}");
    }

    eprintln!(
        "📦 Batch fetching {} URLs (concurrency: {})",
        urls.len(),
        cfg.parallel
    );

    let semaphore = Arc::new(Semaphore::new(cfg.parallel));
    let limiter = Arc::new(DomainRateLimiter::new(Duration::from_millis(
        DEFAULT_DOMAIN_DELAY_MS,
    )));
    let mut handles = Vec::new();

    let shared = std::sync::Arc::new(BatchRequestParams {
        cookies: cfg.cookies.clone(),
        method: cfg.method.clone(),
        data: cfg.data.clone(),
        custom_headers: cfg.custom_headers.clone(),
        proxy: cfg.proxy.clone(),
        no_redirect: cfg.no_redirect,
        auto_referer: cfg.auto_referer,
        raw_html: cfg.raw_html,
    });

    for url in urls {
        let sem = semaphore.clone();
        let lim = limiter.clone();
        let params = shared.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            // Per-domain rate limiting: avoid hammering the same host.
            let domain = super::extract_domain(&url);
            lim.wait(&domain).await;

            fetch_one_batch_url(url, &params).await
        });

        handles.push(handle);
    }

    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => results.push(serde_json::json!({"error": e.to_string()})),
        }
    }

    print_batch_results(&results, cfg.format, cfg.show_body, cfg.max_body);

    let success_count = results.iter().filter(|r| r.get("error").is_none()).count();
    eprintln!(
        "\n📦 Batch complete: {}/{} succeeded",
        success_count,
        results.len()
    );

    Ok(())
}

/// Shared request parameters cloned once and shared across batch tasks via `Arc`.
#[allow(clippy::struct_excessive_bools)] // Subset of CLI flags for batch requests
struct BatchRequestParams {
    cookies: String,
    method: String,
    data: Option<String>,
    custom_headers: Vec<String>,
    proxy: Option<String>,
    no_redirect: bool,
    auto_referer: bool,
    raw_html: bool,
}

/// Fetch a single URL in a batch context and return a JSON result.
async fn fetch_one_batch_url(url: String, params: &BatchRequestParams) -> serde_json::Value {
    let start = Instant::now();

    let client = match build_client(params.no_redirect, params.proxy.as_deref()) {
        Ok(c) => c,
        Err(e) => return serde_json::json!({"url": url, "error": e.to_string()}),
    };
    let profile = client.profile().await;

    let domain = super::extract_domain(&url);

    let cookie_header = super::resolve_cookie_header(&params.cookies, &domain);

    let mut request = match params.method.to_uppercase().as_str() {
        "POST" => client.inner().post(&url),
        "PUT" => client.inner().put(&url),
        "PATCH" => client.inner().patch(&url),
        "DELETE" => client.inner().delete(&url),
        "HEAD" => client.inner().head(&url),
        _ => client.inner().get(&url),
    };

    if let Some(body_data) = &params.data {
        request = request.body(body_data.clone());
        if !params
            .custom_headers
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

    if params.auto_referer && let Some(referer) = super::build_referer(&url) {
        request = request.header("Referer", referer);
    }

    for header_str in &params.custom_headers {
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

            let markdown = if params.raw_html {
                raw_text
            } else {
                let router = ContentRouter::new();
                router.convert(&body_bytes, &content_type).map_or_else(
                    |_| String::from_utf8_lossy(&body_bytes).to_string(),
                    |r| r.markdown,
                )
            };

            let title = extract_title_from_bytes(&body_bytes);
            let metadata = serde_json::json!({
                "title": title,
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
        Err(e) => serde_json::json!({"url": url, "error": e.to_string()}),
    }
}

/// Extract `<title>` from raw HTML bytes.
fn extract_title_from_bytes(bytes: &bytes::Bytes) -> Option<String> {
    let html = String::from_utf8_lossy(bytes);
    let doc = scraper::Html::parse_document(&html);
    let sel = scraper::Selector::parse("title").ok()?;
    doc.select(&sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
}

/// Print batch results according to format.
fn print_batch_results(
    results: &[serde_json::Value],
    format: OutputFormat,
    show_body: bool,
    max_body: usize,
) {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string(results).unwrap_or_default());
        }
        OutputFormat::Compact => print_batch_compact(results),
        OutputFormat::Full => print_batch_full(results, show_body, max_body),
    }
}

fn print_batch_compact(results: &[serde_json::Value]) {
    for r in results {
        if let Some(err) = r.get("error") {
            println!(
                "ERR {} {}",
                r.get("url").and_then(|u| u.as_str()).unwrap_or("?"),
                err
            );
        } else {
            println!(
                "{} {}B {:.0}ms {}",
                r.get("status")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                r.get("metadata")
                    .and_then(|m| m.get("content_length"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                r.get("elapsed_ms")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0),
                r.get("url").and_then(|u| u.as_str()).unwrap_or("?"),
            );
        }
    }
}

fn print_batch_full(results: &[serde_json::Value], show_body: bool, max_body: usize) {
    for r in results {
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
                r.get("status")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                r.get("elapsed_ms")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0),
            );
            if show_body && let Some(md) = r.get("markdown").and_then(|m| m.as_str()) {
                if max_body > 0 && md.len() > max_body {
                    // Find char boundary at or before limit (UTF-8 safe)
                    let at = md.floor_char_boundary(max_body);
                    println!("{}", &md[..at]);
                } else {
                    println!("{md}");
                }
            }
        }
    }
}
