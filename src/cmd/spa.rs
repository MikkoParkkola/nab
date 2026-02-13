use std::time::Instant;

use anyhow::Result;
use scraper::{Html, Selector};

use nab::{inject_fetch_sync, AcceleratedClient, ApiDiscovery, FetchClient, JsEngine};

use super::fetch::{resolve_browser_name, resolve_cookie_source};

#[allow(clippy::too_many_arguments)]
pub async fn cmd_spa(
    url: &str,
    cookies: &str,
    show_html: bool,
    show_console: bool,
    wait_ms: u64,
    _patterns: Option<&str>,
    output: &str,
    extract_path: Option<&str>,
    summary: bool,
    minify: bool,
    max_array: Option<usize>,
    max_depth: Option<usize>,
    _http1: bool,
) -> Result<()> {
    let client = AcceleratedClient::new()?;

    // Extract domain from URL
    let domain = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_default();

    // Get cookies (auto-detect by default, unless "none")
    let mut cookie_header = String::new();
    let browser_name = resolve_browser_name(cookies);

    if let Some(browser) = &browser_name {
        let source = resolve_cookie_source(browser);
        cookie_header = source.get_cookie_header(&domain).unwrap_or_default();
        if !cookie_header.is_empty() {
            println!("🍪 Loading {} cookies for {domain}", browser.to_lowercase());
        }
    }

    let profile = client.profile().await;
    let start = Instant::now();

    let response = if cookie_header.is_empty() {
        client.fetch(url).await?
    } else {
        client
            .inner()
            .get(url)
            .header("Cookie", &cookie_header)
            .headers(profile.to_headers())
            .send()
            .await?
    };

    let html = response.text().await?;
    let elapsed = start.elapsed();

    println!("🕸️  Extracting SPA data from: {url}");

    // Look for common SPA data patterns
    let mut found_data = false;

    // STEP 0: Try static API discovery first (fastest path ~50ms)
    let api_discovery = ApiDiscovery::new()?;
    let discovered_endpoints = api_discovery.discover_from_html(&html);

    if !discovered_endpoints.is_empty() && show_console {
        println!(
            "\n🔍 Discovered {} API endpoints statically:",
            discovered_endpoints.len()
        );
        for (i, endpoint) in discovered_endpoints.iter().take(5).enumerate() {
            let method_str = endpoint.method.as_deref().unwrap_or("?");
            println!(
                "   {}. {} {} (from {})",
                i + 1,
                method_str,
                endpoint.url,
                endpoint.source
            );
        }
        if discovered_endpoints.len() > 5 {
            println!("   ... and {} more", discovered_endpoints.len() - 5);
        }
    }

    // Try fetching discovered endpoints (only GET requests for now)
    if !discovered_endpoints.is_empty() {
        let mut sorted_endpoints = discovered_endpoints.clone();
        sorted_endpoints.sort_by_key(|e| -ApiDiscovery::score_endpoint(e));

        for endpoint in sorted_endpoints.iter().take(3) {
            if endpoint.method.as_deref() != Some("GET") && endpoint.method.is_some() {
                continue;
            }

            let endpoint_url =
                if endpoint.url.starts_with("http://") || endpoint.url.starts_with("https://") {
                    endpoint.url.clone()
                } else if endpoint.url.starts_with('/') {
                    url::Url::parse(url).ok().map_or_else(
                        || endpoint.url.clone(),
                        |u| format!("{}{}", u.origin().unicode_serialization(), endpoint.url),
                    )
                } else {
                    continue;
                };

            if show_console {
                println!("🌐 Trying endpoint: {endpoint_url}");
            }

            let fetch_result = async {
                let resp = if cookie_header.is_empty() {
                    client.fetch(&endpoint_url).await?
                } else {
                    client
                        .inner()
                        .get(&endpoint_url)
                        .header("Cookie", &cookie_header)
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
            .await;

            if let Ok(data) = fetch_result {
                println!(
                    "\n📊 Extraction complete in {:.2}ms",
                    elapsed.as_secs_f64() * 1000.0
                );
                println!("\n✅ API endpoint {endpoint_url} returned data:");
                output_spa_data(
                    &data,
                    output,
                    extract_path,
                    summary,
                    minify,
                    max_array,
                    max_depth,
                )?;
                found_data = true;
                break;
            }
        }
    }

    // STEP 1: Try embedded JSON extraction (fast path ~100ms)
    if !found_data {
        if let Some(data) = extract_script_json(&html, "__NEXT_DATA__") {
            println!(
                "\n📊 Extraction complete in {:.2}ms",
                elapsed.as_secs_f64() * 1000.0
            );
            println!("\n✅ __NEXT_DATA__ found:");
            output_spa_data(&data, output, extract_path, summary, minify, max_array, max_depth)?;
            found_data = true;
        }
    }

    if let Some(data) = extract_script_json(&html, "__INITIAL_STATE__") {
        if !found_data {
            println!(
                "\n📊 Extraction complete in {:.2}ms",
                elapsed.as_secs_f64() * 1000.0
            );
        }
        println!("\n✅ __INITIAL_STATE__ found:");
        output_spa_data(&data, output, extract_path, summary, minify, max_array, max_depth)?;
        found_data = true;
    }

    if let Some(data) = extract_script_json(&html, "__NUXT__") {
        if !found_data {
            println!(
                "\n📊 Extraction complete in {:.2}ms",
                elapsed.as_secs_f64() * 1000.0
            );
        }
        println!("\n✅ __NUXT__ found:");
        output_spa_data(&data, output, extract_path, summary, minify, max_array, max_depth)?;
        found_data = true;
    }

    if let Some(data) = extract_script_json(&html, "__PRELOADED_STATE__") {
        if !found_data {
            println!(
                "\n📊 Extraction complete in {:.2}ms",
                elapsed.as_secs_f64() * 1000.0
            );
        }
        println!("\n✅ __PRELOADED_STATE__ found:");
        output_spa_data(&data, output, extract_path, summary, minify, max_array, max_depth)?;
        found_data = true;
    }

    if !found_data {
        println!("\n⚙️  No embedded JSON found, trying JavaScript execution...");

        let base_url = url::Url::parse(url)
            .ok()
            .map(|u| u.origin().unicode_serialization())
            .unwrap_or_default();

        let js_engine = JsEngine::new()?;
        js_engine.inject_minimal_dom()?;

        let fetch_client = FetchClient::new(
            if cookie_header.is_empty() {
                None
            } else {
                Some(cookie_header.clone())
            },
            if base_url.is_empty() {
                None
            } else {
                Some(base_url.clone())
            },
        );

        let fetch_client_clone = fetch_client.clone();
        inject_fetch_sync(js_engine.context(), fetch_client_clone)?;

        js_engine.set_global("__PAGE_URL__", url)?;
        js_engine.eval(&format!(
            "window.location.href = '{url}'; window.location.hostname = '{domain}';"
        ))?;

        let document = Html::parse_document(&html);
        let script_selector = Selector::parse("script").unwrap();
        let mut scripts_executed = 0;

        for script in document.select(&script_selector) {
            if script.value().attr("src").is_some() {
                continue;
            }

            let script_content = script.text().collect::<String>();
            if script_content.trim().is_empty() {
                continue;
            }

            if show_console {
                println!("📜 Executing script ({} chars)", script_content.len());
            }

            if let Err(e) = js_engine.eval(&script_content) {
                if show_console {
                    println!("⚠️  Script execution error: {e}");
                }
            } else {
                scripts_executed += 1;
            }
        }

        println!("✅ Executed {scripts_executed} inline scripts");

        if wait_ms > 0 {
            println!("⏳ Waiting {wait_ms}ms for async operations...");
            std::thread::sleep(std::time::Duration::from_millis(wait_ms));
        }

        let patterns_to_check = vec![
            ("window.__NEXT_DATA__", "__NEXT_DATA__"),
            ("window.__INITIAL_STATE__", "__INITIAL_STATE__"),
            ("window.__NUXT__", "__NUXT__"),
            ("window.__PRELOADED_STATE__", "__PRELOADED_STATE__"),
        ];

        for (js_path, name) in patterns_to_check {
            if let Ok(json_str) = js_engine.eval(&format!("JSON.stringify({js_path} || null)")) {
                if json_str != "null" {
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str) {
                        println!("\n✅ {name} found via JavaScript execution:");
                        output_spa_data(
                            &data,
                            output,
                            extract_path,
                            summary,
                            minify,
                            max_array,
                            max_depth,
                        )?;
                        found_data = true;
                        break;
                    }
                }
            }
        }

        if !found_data {
            if let Ok(window_json) = js_engine.eval("JSON.stringify(window)") {
                if let Ok(window_data) = serde_json::from_str::<serde_json::Value>(&window_json) {
                    if let Some(obj) = window_data.as_object() {
                        let mut clean_data = serde_json::Map::new();
                        for (key, value) in obj {
                            if !key.starts_with('_')
                                && key != "document"
                                && key != "window"
                                && key != "console"
                                && key != "navigator"
                                && key != "location"
                                && key != "localStorage"
                                && key != "sessionStorage"
                            {
                                clean_data.insert(key.clone(), value.clone());
                            }
                        }

                        if !clean_data.is_empty() {
                            println!("\n✅ Extracted window data via JavaScript:");
                            let data = serde_json::Value::Object(clean_data);
                            output_spa_data(
                                &data,
                                output,
                                extract_path,
                                summary,
                                minify,
                                max_array,
                                max_depth,
                            )?;
                            found_data = true;
                        }
                    }
                }
            }
        }

        let fetched_urls = fetch_client.get_fetch_log();
        if !fetched_urls.is_empty() {
            println!("\n📡 JavaScript made {} fetch() calls:", fetched_urls.len());
            for (i, url) in fetched_urls.iter().enumerate() {
                println!("   {}. {}", i + 1, url);
            }
        }

        if !found_data {
            println!("\n❌ No SPA data found even after JavaScript execution");
            println!("   HTML size: {} bytes", html.len());
            println!("   Scripts executed: {scripts_executed}");
            if show_html {
                println!("\nHTML preview (first 500 chars):");
                println!("{}", &html.chars().take(500).collect::<String>());
            }
        }
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
    let pattern = format!("window.{var_name}");
    if let Some(start_idx) = html.find(&pattern) {
        let after_eq = html[start_idx..].find('=')? + start_idx + 1;
        let json_start = html[after_eq..]
            .chars()
            .position(|c| c == '{' || c == '[')?
            + after_eq;

        let json_str = extract_json_object(&html[json_start..])?;
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
            return Some(json);
        }
    }

    // Try self.__VAR__ pattern (some frameworks)
    let self_pattern = format!("self.{var_name}");
    if let Some(start_idx) = html.find(&self_pattern) {
        let after_eq = html[start_idx..].find('=')? + start_idx + 1;
        let json_start = html[after_eq..]
            .chars()
            .position(|c| c == '{' || c == '[')?
            + after_eq;
        let json_str = extract_json_object(&html[json_start..])?;
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
            return Some(json);
        }
    }

    None
}

/// Lightweight SPA data extraction for auto-detection during `nab fetch`.
#[allow(dead_code)]
pub fn extract_spa_data_inline(html: &str) -> Option<String> {
    const SPA_PATTERNS: &[(&str, &str)] = &[
        ("__NEXT_DATA__", "Next.js"),
        ("__NUXT__", "Nuxt.js"),
        ("__INITIAL_STATE__", "Redux/Vuex"),
        ("__APOLLO_STATE__", "Apollo GraphQL"),
        ("__RELAY_STORE__", "Relay"),
        ("__PRELOADED_STATE__", "Redux"),
    ];

    let mut sections = Vec::new();

    for (var_name, framework) in SPA_PATTERNS {
        if let Some(data) = extract_script_json(html, var_name) {
            let json_str = serde_json::to_string_pretty(&data).unwrap_or_default();
            let truncated = if json_str.len() > 8192 {
                format!(
                    "```json\n{}\n... [{} more bytes]\n```",
                    &json_str[..8192],
                    json_str.len() - 8192
                )
            } else {
                format!("```json\n{json_str}\n```")
            };
            sections.push(format!("### {framework} (`{var_name}`)\n\n{truncated}"));
        }
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
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

fn output_spa_data(
    data: &serde_json::Value,
    output: &str,
    extract_path: Option<&str>,
    summary: bool,
    minify: bool,
    max_array: Option<usize>,
    max_depth: Option<usize>,
) -> Result<()> {
    let target = if let Some(path) = extract_path {
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = data;
        for part in parts {
            current = current.get(part).unwrap_or(&serde_json::Value::Null);
        }
        current.clone()
    } else {
        data.clone()
    };

    let transformed = if max_array.is_some() || max_depth.is_some() {
        transform_json(
            &target,
            max_array.unwrap_or(usize::MAX),
            max_depth.unwrap_or(usize::MAX),
            0,
        )
    } else {
        target
    };

    if summary {
        println!("   {} bytes", serde_json::to_string(&transformed)?.len());
        print_structure(&transformed, 3, 0);
    } else if output == "json" || minify {
        if minify {
            println!("{}", serde_json::to_string(&transformed)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&transformed)?);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&transformed)?);
    }

    Ok(())
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

fn print_structure(value: &serde_json::Value, max_depth: usize, depth: usize) {
    let indent = "  ".repeat(depth);

    if depth >= max_depth {
        println!("{indent}...");
        return;
    }

    match value {
        serde_json::Value::Object(obj) => {
            for (key, val) in obj {
                match val {
                    serde_json::Value::Object(_) => {
                        println!("{indent}{key}: {{...}}");
                        print_structure(val, max_depth, depth + 1);
                    }
                    serde_json::Value::Array(arr) => {
                        println!("{indent}{key}: [{} items]", arr.len());
                    }
                    _ => {
                        let type_name = match val {
                            serde_json::Value::String(_) => "string",
                            serde_json::Value::Number(_) => "number",
                            serde_json::Value::Bool(_) => "bool",
                            serde_json::Value::Null => "null",
                            _ => "?",
                        };
                        println!("{indent}{key}: {type_name}");
                    }
                }
            }
        }
        serde_json::Value::Array(arr) if !arr.is_empty() => {
            println!("{indent}[0]:");
            print_structure(&arr[0], max_depth, depth + 1);
        }
        _ => {}
    }
}
