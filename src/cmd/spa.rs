use std::sync::LazyLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use scraper::{Html, Selector};

use super::output::write_stdout_line;
use nab::fetch_bridge::{FetchClient, inject_fetch_sync};
use nab::js_engine::JsEngine;
use nab::{AcceleratedClient, ApiDiscovery};

static SCRIPT_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("script").expect("static script selector"));

#[derive(Clone)]
/// Configuration for the `nab spa` command.
///
/// Each field is a 1:1 mapping of a CLI boolean flag; grouping them further
/// would only create indirection without conceptual benefit.
#[allow(clippy::struct_excessive_bools)] // 1:1 map of CLI boolean flags
pub struct SpaConfig {
    pub url: String,
    pub cookies: String,
    pub show_html: bool,
    pub show_console: bool,
    pub wait_ms: u64,
    pub endpoint_hints: Vec<String>,
    pub output: String,
    pub extract_path: Option<String>,
    pub summary: bool,
    pub minify: bool,
    pub max_array: Option<usize>,
    pub max_depth: Option<usize>,
    pub force_http1: bool,
}

pub async fn cmd_spa(cfg: &SpaConfig) -> Result<()> {
    let client = if cfg.force_http1 {
        AcceleratedClient::new_http1_only()?
    } else {
        AcceleratedClient::new()?
    };

    let domain = super::extract_domain(&cfg.url);
    let cookie_header = super::resolve_cookie_header(&cfg.cookies, &domain);
    if !cookie_header.is_empty() {
        eprintln!("🍪 Loading cookies for {domain}");
    }

    let profile = client.profile().await;
    let start = Instant::now();

    let response = if cookie_header.is_empty() {
        client.fetch(&cfg.url).await?
    } else {
        client
            .inner()
            .get(&cfg.url)
            .header("Cookie", &cookie_header)
            .headers(profile.to_headers())
            .send()
            .await?
    };

    let html = response.text().await?;
    let elapsed = start.elapsed();

    eprintln!("🕸️  Extracting SPA data from: {}", cfg.url);

    let mut found_data = false;

    // STEP 0: Try static API discovery first (fastest path ~50ms)
    try_api_discovery(
        &client,
        &html,
        &cookie_header,
        &profile,
        elapsed,
        &mut found_data,
        cfg,
    )
    .await?;

    // STEP 1: Try embedded JSON extraction (fast path ~100ms)
    if !found_data {
        try_extract_and_output(&html, "__NEXT_DATA__", elapsed, &mut found_data, cfg)?;
    }

    for name in &["__INITIAL_STATE__", "__NUXT__", "__PRELOADED_STATE__"] {
        try_extract_and_output(&html, name, elapsed, &mut found_data, cfg)?;
    }

    // STEP 2: Fall back to full JavaScript execution
    if !found_data {
        try_javascript_extraction(&html, &cookie_header, elapsed, cfg).await?;
    }

    Ok(())
}

/// Attempt static API endpoint discovery and fetch the highest-scored GET endpoint.
async fn try_api_discovery(
    client: &AcceleratedClient,
    html: &str,
    cookie_header: &str,
    profile: &nab::fingerprint::BrowserProfile,
    elapsed: Duration,
    found_data: &mut bool,
    cfg: &SpaConfig,
) -> Result<()> {
    let api_discovery = ApiDiscovery::with_url_hints(&cfg.endpoint_hints)?;
    let discovered_endpoints = api_discovery.discover_from_html(html);

    if discovered_endpoints.is_empty() {
        return Ok(());
    }

    if cfg.show_console {
        eprintln!(
            "\n🔍 Discovered {} API endpoints statically:",
            discovered_endpoints.len()
        );
        for (i, endpoint) in discovered_endpoints.iter().take(5).enumerate() {
            let method_str = endpoint.method.as_deref().unwrap_or("?");
            eprintln!(
                "   {}. {} {} (from {})",
                i + 1,
                method_str,
                endpoint.url,
                endpoint.source
            );
        }
        if discovered_endpoints.len() > 5 {
            eprintln!("   ... and {} more", discovered_endpoints.len() - 5);
        }
    }

    let mut sorted_endpoints = discovered_endpoints.clone();
    sorted_endpoints.sort_by_key(|e| -score_discovered_endpoint(&api_discovery, e));

    for endpoint in sorted_endpoints.iter().take(3) {
        if endpoint.method.as_deref() != Some("GET") && endpoint.method.is_some() {
            continue;
        }

        let endpoint_url = resolve_endpoint_url(&endpoint.url, &cfg.url);
        let Some(endpoint_url) = endpoint_url else {
            continue;
        };

        if cfg.show_console {
            eprintln!("🌐 Trying endpoint: {endpoint_url}");
        }

        let fetch_result = fetch_json_endpoint(client, &endpoint_url, cookie_header, profile).await;

        if let Ok(data) = fetch_result {
            eprintln!(
                "\n📊 Extraction complete in {:.2}ms",
                elapsed.as_secs_f64() * 1000.0
            );
            eprintln!("\n✅ API endpoint {endpoint_url} returned data:");
            output_spa_data(&data, cfg)?;
            *found_data = true;
            break;
        }
    }

    Ok(())
}

fn score_discovered_endpoint(api_discovery: &ApiDiscovery, endpoint: &nab::ApiEndpoint) -> i32 {
    let mut score = ApiDiscovery::score_endpoint(endpoint);
    if api_discovery.matches_hint(endpoint) {
        score += 50;
    }
    score
}

/// Fetch a single endpoint and parse its response as JSON.
async fn fetch_json_endpoint(
    client: &AcceleratedClient,
    url: &str,
    cookie_header: &str,
    profile: &nab::fingerprint::BrowserProfile,
) -> Result<serde_json::Value> {
    let resp = if cookie_header.is_empty() {
        client.fetch(url).await?
    } else {
        client
            .inner()
            .get(url)
            .header("Cookie", cookie_header)
            .headers(profile.to_headers())
            .send()
            .await?
    };

    let text = resp.text().await?;
    let data = serde_json::from_str::<serde_json::Value>(&text)?;

    if data.is_object() || data.is_array() {
        Ok(data)
    } else {
        Err(anyhow::anyhow!("Not an object or array"))
    }
}

/// Resolve an endpoint path to an absolute URL, returning `None` for relative non-root paths.
fn resolve_endpoint_url(endpoint: &str, page_url: &str) -> Option<String> {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        Some(endpoint.to_string())
    } else if endpoint.starts_with('/') {
        url::Url::parse(page_url)
            .ok()
            .map(|u| format!("{}{}", u.origin().unicode_serialization(), endpoint))
    } else {
        None
    }
}

/// Execute inline scripts in the page and probe known SPA globals.
///
/// This is the slow fallback path (~500ms+) used when static extraction fails.
async fn try_javascript_extraction(
    html: &str,
    cookie_header: &str,
    elapsed: Duration,
    cfg: &SpaConfig,
) -> Result<()> {
    let html = html.to_string();
    let cookie_header = cookie_header.to_string();
    let cfg = cfg.clone();

    tokio::task::spawn_blocking(move || {
        try_javascript_extraction_blocking(&html, &cookie_header, elapsed, &cfg)
    })
    .await
    .context("JavaScript extraction task failed")?
}

fn try_javascript_extraction_blocking(
    html: &str,
    cookie_header: &str,
    elapsed: Duration,
    cfg: &SpaConfig,
) -> Result<()> {
    eprintln!("\n⚙️  No embedded JSON found, trying JavaScript execution...");
    let mut found_data = false;

    let domain = super::extract_domain(&cfg.url);
    let base_url = url::Url::parse(&cfg.url)
        .ok()
        .map(|u| u.origin().unicode_serialization())
        .unwrap_or_default();

    let js_engine = JsEngine::new()?;
    js_engine.inject_minimal_dom()?;

    let fetch_client = FetchClient::new_with_options(
        (!cookie_header.is_empty()).then(|| cookie_header.to_owned()),
        (!base_url.is_empty()).then(|| base_url.clone()),
        cfg.force_http1,
    );

    inject_fetch_sync(js_engine.context(), fetch_client.clone())?;

    js_engine.set_global("__PAGE_URL__", &cfg.url)?;
    js_engine.eval(&format!(
        "window.location.href = '{}'; window.location.hostname = '{domain}';",
        cfg.url
    ))?;

    let scripts_executed = execute_inline_scripts(html, &js_engine, cfg.show_console);
    eprintln!("✅ Executed {scripts_executed} inline scripts");

    if cfg.wait_ms > 0 {
        eprintln!("⏳ Waiting {}ms for async operations...", cfg.wait_ms);
        std::thread::sleep(std::time::Duration::from_millis(cfg.wait_ms));
    }

    probe_spa_globals(&js_engine, elapsed, &mut found_data, cfg)?;

    if !found_data {
        probe_window_object(&js_engine, elapsed, &mut found_data, cfg)?;
    }

    report_fetch_calls(&fetch_client);

    if !found_data {
        report_extraction_failure(html, scripts_executed, cfg);
    }

    Ok(())
}

/// Execute all inline (non-src) scripts and return the count of successful executions.
fn execute_inline_scripts(html: &str, js_engine: &JsEngine, show_console: bool) -> usize {
    let document = Html::parse_document(html);
    let mut scripts_executed = 0;

    for script in document.select(&SCRIPT_SELECTOR) {
        if script.value().attr("src").is_some() {
            continue;
        }

        let script_content = script.text().collect::<String>();
        if script_content.trim().is_empty() {
            continue;
        }

        if show_console {
            eprintln!("📜 Executing script ({} chars)", script_content.len());
        }

        if let Err(e) = js_engine.eval(&script_content) {
            if show_console {
                eprintln!("⚠️  Script execution error: {e}");
            }
        } else {
            scripts_executed += 1;
        }
    }

    scripts_executed
}

/// Check well-known SPA global variables via `JSON.stringify`.
fn probe_spa_globals(
    js_engine: &JsEngine,
    elapsed: Duration,
    found_data: &mut bool,
    cfg: &SpaConfig,
) -> Result<()> {
    let patterns = [
        ("window.__NEXT_DATA__", "__NEXT_DATA__"),
        ("window.__INITIAL_STATE__", "__INITIAL_STATE__"),
        ("window.__NUXT__", "__NUXT__"),
        ("window.__PRELOADED_STATE__", "__PRELOADED_STATE__"),
    ];

    for (js_path, name) in patterns {
        if let Ok(json_str) = js_engine.eval(&format!("JSON.stringify({js_path} || null)"))
            && json_str != "null"
            && let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str)
        {
            eprintln!(
                "\n📊 Extraction complete in {:.2}ms",
                elapsed.as_secs_f64() * 1000.0
            );
            eprintln!("\n✅ {name} found via JavaScript execution:");
            output_spa_data(&data, cfg)?;
            *found_data = true;
            break;
        }
    }

    Ok(())
}

/// Fall back to extracting non-internal keys from the window object.
fn probe_window_object(
    js_engine: &JsEngine,
    elapsed: Duration,
    found_data: &mut bool,
    cfg: &SpaConfig,
) -> Result<()> {
    let Ok(window_json) = js_engine.eval("JSON.stringify(window)") else {
        return Ok(());
    };
    let Ok(window_data) = serde_json::from_str::<serde_json::Value>(&window_json) else {
        return Ok(());
    };
    let Some(obj) = window_data.as_object() else {
        return Ok(());
    };

    let excluded = [
        "document",
        "window",
        "console",
        "navigator",
        "location",
        "localStorage",
        "sessionStorage",
    ];
    let clean_data: serde_json::Map<String, serde_json::Value> = obj
        .iter()
        .filter(|(k, _)| !k.starts_with('_') && !excluded.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    if !clean_data.is_empty() {
        eprintln!(
            "\n📊 Extraction complete in {:.2}ms",
            elapsed.as_secs_f64() * 1000.0
        );
        eprintln!("\n✅ Extracted window data via JavaScript:");
        let data = serde_json::Value::Object(clean_data);
        output_spa_data(&data, cfg)?;
        *found_data = true;
    }

    Ok(())
}

/// Print the list of URLs fetched by JavaScript's `fetch()` shim.
fn report_fetch_calls(fetch_client: &FetchClient) {
    let fetched_urls = fetch_client.get_fetch_log();
    if !fetched_urls.is_empty() {
        eprintln!("\n📡 JavaScript made {} fetch() calls:", fetched_urls.len());
        for (i, url) in fetched_urls.iter().enumerate() {
            eprintln!("   {}. {}", i + 1, url);
        }
    }
}

/// Print a diagnostic failure report when no data was found.
fn report_extraction_failure(html: &str, scripts_executed: usize, cfg: &SpaConfig) {
    eprintln!("\n❌ No SPA data found even after JavaScript execution");
    eprintln!("   HTML size: {} bytes", html.len());
    eprintln!("   Scripts executed: {scripts_executed}");
    if cfg.show_html {
        eprintln!("\nHTML preview (first 500 chars):");
        eprintln!("{}", html.chars().take(500).collect::<String>());
    }
}

/// Try to extract a named SPA JSON variable from HTML and output it.
fn try_extract_and_output(
    html: &str,
    var_name: &str,
    elapsed: Duration,
    found_data: &mut bool,
    cfg: &SpaConfig,
) -> Result<()> {
    if let Some(data) = extract_script_json(html, var_name) {
        if !*found_data {
            eprintln!(
                "\n📊 Extraction complete in {:.2}ms",
                elapsed.as_secs_f64() * 1000.0
            );
        }
        eprintln!("\n✅ {var_name} found:");
        output_spa_data(&data, cfg)?;
        *found_data = true;
    }
    Ok(())
}

fn extract_script_json(html: &str, var_name: &str) -> Option<serde_json::Value> {
    let document = Html::parse_document(html);

    // Try script tag with id
    let id_selector = Selector::parse(&format!("script#{var_name}")).ok()?;
    if let Some(script) = document.select(&id_selector).next() {
        let content = script.text().collect::<String>();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            return Some(json);
        }
    }

    // Try window.__VAR__ = pattern
    if let Some(json) = extract_assigned_json(html, &format!("window.{var_name}")) {
        return Some(json);
    }

    // Try self.__VAR__ pattern (some frameworks)
    if let Some(json) = extract_assigned_json(html, &format!("self.{var_name}")) {
        return Some(json);
    }

    None
}

/// Extract a JSON value from `<pattern> = <json>` in raw HTML text.
fn extract_assigned_json(html: &str, pattern: &str) -> Option<serde_json::Value> {
    let start_idx = html.find(pattern)?;
    let after_eq = html[start_idx..].find('=')? + start_idx + 1;
    let json_start = html[after_eq..]
        .chars()
        .position(|c| c == '{' || c == '[')?
        + after_eq;

    let json_str = extract_json_object(&html[json_start..])?;
    serde_json::from_str::<serde_json::Value>(json_str).ok()
}

fn extract_json_object(s: &str) -> Option<&str> {
    let first_char = s.chars().next()?;
    let (open, close) = match first_char {
        '{' => ('{', '}'),
        '[' => ('[', ']'),
        _ => return None,
    };

    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, c) in s.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match c {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            _ if in_string => {}
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
        }
    }

    None
}

fn output_spa_data(data: &serde_json::Value, cfg: &SpaConfig) -> Result<()> {
    let target = apply_extract_path(data, cfg.extract_path.as_deref());

    let transformed = if cfg.max_array.is_some() || cfg.max_depth.is_some() {
        transform_json(
            &target,
            cfg.max_array.unwrap_or(usize::MAX),
            cfg.max_depth.unwrap_or(usize::MAX),
            0,
        )
    } else {
        target
    };

    if cfg.summary {
        write_stdout_line(&format!(
            "   {} bytes",
            serde_json::to_string(&transformed)?.len()
        ))?;
        print_structure(&transformed, 3, 0)?;
    } else if cfg.output == "json" || cfg.minify {
        if cfg.minify {
            write_stdout_line(&serde_json::to_string(&transformed)?)?;
        } else {
            write_stdout_line(&serde_json::to_string_pretty(&transformed)?)?;
        }
    } else {
        write_stdout_line(&serde_json::to_string_pretty(&transformed)?)?;
    }

    Ok(())
}

/// Navigate a dot-separated path into a JSON value, cloning the target node.
fn apply_extract_path(data: &serde_json::Value, path: Option<&str>) -> serde_json::Value {
    let Some(path) = path else {
        return data.clone();
    };

    let mut current = data;
    for part in path.split('.') {
        current = current.get(part).unwrap_or(&serde_json::Value::Null);
    }
    current.clone()
}

fn transform_json(
    value: &serde_json::Value,
    max_array: usize,
    max_depth: usize,
    depth: usize,
) -> serde_json::Value {
    if depth >= max_depth {
        return serde_json::Value::String("[depth limit]".to_string());
    }

    match value {
        serde_json::Value::Array(arr) => {
            let limited: Vec<serde_json::Value> = arr
                .iter()
                .take(max_array)
                .map(|v| transform_json(v, max_array, max_depth, depth + 1))
                .collect();
            if arr.len() > max_array {
                let mut result = limited;
                result.push(serde_json::Value::String(format!(
                    "... +{} more",
                    arr.len() - max_array
                )));
                serde_json::Value::Array(result)
            } else {
                serde_json::Value::Array(limited)
            }
        }
        serde_json::Value::Object(obj) => {
            let transformed: serde_json::Map<String, serde_json::Value> = obj
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        transform_json(v, max_array, max_depth, depth + 1),
                    )
                })
                .collect();
            serde_json::Value::Object(transformed)
        }
        _ => value.clone(),
    }
}

fn print_structure(value: &serde_json::Value, max_depth: usize, depth: usize) -> Result<()> {
    let indent = "  ".repeat(depth);

    if depth >= max_depth {
        write_stdout_line(&format!("{indent}..."))?;
        return Ok(());
    }

    match value {
        serde_json::Value::Object(obj) => {
            for (key, val) in obj {
                match val {
                    serde_json::Value::Object(_) => {
                        write_stdout_line(&format!("{indent}{key}: {{...}}"))?;
                        print_structure(val, max_depth, depth + 1)?;
                    }
                    serde_json::Value::Array(arr) => {
                        write_stdout_line(&format!("{indent}{key}: [{} items]", arr.len()))?;
                    }
                    _ => {
                        let type_name = match val {
                            serde_json::Value::String(_) => "string",
                            serde_json::Value::Number(_) => "number",
                            serde_json::Value::Bool(_) => "bool",
                            serde_json::Value::Null => "null",
                            _ => "?",
                        };
                        write_stdout_line(&format!("{indent}{key}: {type_name}"))?;
                    }
                }
            }
        }
        serde_json::Value::Array(arr) if !arr.is_empty() => {
            write_stdout_line(&format!("{indent}[0]:"))?;
            print_structure(&arr[0], max_depth, depth + 1)?;
        }
        _ => {}
    }

    Ok(())
}
