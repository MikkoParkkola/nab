// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Pure helper functions for the rule-based provider.
//!
//! Extracted from `provider.rs` to keep the main module focused on the
//! [`ApiRuleProvider`] struct and its trait implementation.

use std::collections::HashMap;

use anyhow::{Context, Result};
use regex::Regex;

use super::config::{ConcurrentFetchConfig, JsonConfig};
use super::json_path;
use crate::http_client::AcceleratedClient;
use crate::site::Engagement;

/// Parse a JSON response body.
///
/// Both JSON objects and bare JSON arrays (e.g. Reddit's `[listing, comments]`
/// response) are accepted.  Paths in the TOML rule use `[N].field` notation to
/// index into root arrays, which [`json_path::extract`] handles natively.
pub(super) fn parse_response_json(body: &str, api_url: &str) -> Result<serde_json::Value> {
    serde_json::from_str(body).with_context(|| format!("failed to parse JSON from '{api_url}'"))
}

/// Thin wrapper around [`json_path::is_non_null`] for use in tests.
#[cfg(test)]
pub(super) fn json_path_is_non_null(json: &serde_json::Value, path: &str) -> bool {
    json_path::is_non_null(json, path)
}

/// Fetch a URL and extract JSON fields according to `json_config`.
///
/// Used for additional fetches.  Returns an empty map (rather than an error)
/// when no fields match — the caller decides whether that warrants a warning.
pub(super) async fn fetch_and_extract_json(
    client: &AcceleratedClient,
    url: &str,
    accept: Option<&str>,
    json_config: &JsonConfig,
    cookies: Option<&str>,
) -> Result<HashMap<String, String>> {
    let mut request = client.inner().get(url);
    if let Some(accept_val) = accept {
        request = request.header(reqwest::header::ACCEPT, accept_val);
    }
    if let Some(cookie_val) = cookies {
        request = request.header(reqwest::header::COOKIE, cookie_val);
    }

    let body = request
        .send()
        .await
        .with_context(|| format!("additional fetch request failed for '{url}'"))?
        .error_for_status()
        .with_context(|| format!("HTTP error for additional fetch '{url}'"))?
        .text()
        .await
        .with_context(|| format!("failed to read additional fetch body from '{url}'"))?;

    let json: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse JSON from additional fetch '{url}'"))?;

    let fields = json_config
        .0
        .iter()
        .filter_map(|(name, path)| {
            let value = if path.contains("[]") {
                let arr = json_path::extract_array(&json, path);
                if arr.is_empty() {
                    return None;
                }
                arr.join(", ")
            } else {
                json_path::extract(&json, path)?
            };
            Some((name.clone(), value))
        })
        .collect();

    Ok(fields)
}

/// Fetch a list endpoint and expand items into numbered fields.
///
/// Walks `items_path` in the JSON response to find an array, then extracts
/// fields from each element using `cf.json`.  Fields are named
/// `{prefix}_{idx}_{field}` (e.g., `story_0_title`).
///
/// Respects `cf.item_limit()` to cap expansion.
pub(super) async fn fetch_and_expand_items(
    client: &AcceleratedClient,
    url: &str,
    cf: &ConcurrentFetchConfig,
    cookies: Option<&str>,
) -> Result<HashMap<String, String>> {
    let mut request = client.inner().get(url);
    if let Some(accept_val) = &cf.accept {
        request = request.header(reqwest::header::ACCEPT, accept_val.as_str());
    }
    if let Some(cookie_val) = cookies {
        request = request.header(reqwest::header::COOKIE, cookie_val);
    }

    let body = request
        .send()
        .await
        .with_context(|| format!("concurrent fetch failed for '{url}'"))?
        .error_for_status()
        .with_context(|| format!("HTTP error for concurrent fetch '{url}'"))?
        .text()
        .await
        .with_context(|| format!("failed to read concurrent fetch body from '{url}'"))?;

    let json: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse JSON from concurrent fetch '{url}'"))?;

    // Navigate to the items array.
    let items_array = extract_items_array(&json, &cf.items_path)?;

    let limit = cf.item_limit();
    let mut fields = HashMap::new();

    for (idx, item) in items_array.iter().take(limit).enumerate() {
        for (field_name, path) in &cf.json.0 {
            if let Some(value) = json_path::extract(item, path) {
                fields.insert(format!("{}_{}_{}", cf.prefix, idx, field_name), value);
            }
        }
    }

    Ok(fields)
}

/// Navigate a JSON value to find an array at `items_path`.
///
/// Supports `"."` for root arrays, and dot-separated paths like `.items` or
/// `.data.results`.
pub(super) fn extract_items_array<'a>(
    json: &'a serde_json::Value,
    items_path: &str,
) -> Result<&'a Vec<serde_json::Value>> {
    let path = items_path.trim_start_matches('.');

    let target = if path.is_empty() {
        json
    } else {
        let mut current = json;
        for segment in path.split('.') {
            current = current.get(segment).with_context(|| {
                format!("items_path segment '{segment}' not found in JSON response")
            })?;
        }
        current
    };

    target
        .as_array()
        .with_context(|| format!("items_path '{items_path}' did not resolve to a JSON array"))
}

/// Rewrite `url` using a compiled regex and template string.
///
/// Same rules as [`ApiRuleProvider::rewrite_url`]: `{url}` is replaced with
/// the URL-encoded original; otherwise capture-group substitution is applied.
pub(super) fn rewrite_url_with(re: &Regex, to: &str, url: &str) -> String {
    if to.contains("{url}") {
        return to.replace("{url}", &urlencoding::encode(url));
    }
    re.replace(url, to).into_owned()
}

/// Extract named fields from HTML using CSS selectors.
///
/// Each entry in `css_map` maps a field name to a selector string.  A
/// `::attr(name)` suffix causes attribute extraction; without it the element's
/// text content is collected.
pub(super) fn extract_css_fields(
    html: &str,
    css_map: &HashMap<String, String>,
) -> HashMap<String, String> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let mut fields = HashMap::new();

    for (field_name, selector_str) in css_map {
        let (selector_str, attr_name) = parse_css_attr_suffix(selector_str);

        let Ok(selector) = Selector::parse(selector_str) else {
            tracing::warn!("Invalid CSS selector for field '{field_name}': '{selector_str}'");
            continue;
        };

        let Some(element) = document.select(&selector).next() else {
            continue;
        };

        let value = if let Some(attr) = attr_name {
            element.value().attr(attr).unwrap_or("").to_string()
        } else {
            element.text().collect::<String>()
        };

        if !value.is_empty() {
            fields.insert(field_name.clone(), value);
        }
    }

    fields
}

/// Split a CSS selector string on `::attr(name)`, returning `(selector, attr)`.
///
/// Returns `(full_string, None)` when no `::attr(...)` suffix is present.
pub(super) fn parse_css_attr_suffix(selector: &str) -> (&str, Option<&str>) {
    // Locate `::attr(` — must be followed by `name)` at the end.
    let Some(attr_start) = selector.rfind("::attr(") else {
        return (selector, None);
    };
    let rest = &selector[attr_start + 7..]; // skip "::attr("
    let Some(close) = rest.rfind(')') else {
        return (selector, None);
    };
    let attr_name = &rest[..close];
    let css_part = &selector[..attr_start];
    (css_part, Some(attr_name))
}

/// Intern a provider name so it has `'static` lifetime.
///
/// Uses a global set to avoid leaking the same name twice.  The set is
/// small (bounded by embedded defaults + user configs) and lives for the
/// entire program, so the memory is effectively static.
pub(super) fn intern_name(s: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::LazyLock;
    use std::sync::Mutex;

    static INTERNED: LazyLock<Mutex<HashSet<&'static str>>> =
        LazyLock::new(|| Mutex::new(HashSet::new()));

    let mut set = INTERNED.lock().expect("intern_name lock");
    if let Some(&existing) = set.get(s) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// Build [`Engagement`] from extracted fields using the engagement config.
pub(super) fn build_engagement(
    eng: &super::config::EngagementConfig,
    fields: &HashMap<String, String>,
) -> Option<Engagement> {
    let likes = eng.likes.as_deref().and_then(|f| parse_u64(fields.get(f)?));
    let reposts = eng
        .reposts
        .as_deref()
        .and_then(|f| parse_u64(fields.get(f)?));
    let replies = eng
        .replies
        .as_deref()
        .and_then(|f| parse_u64(fields.get(f)?));
    let views = eng.views.as_deref().and_then(|f| parse_u64(fields.get(f)?));

    if likes.is_none() && reposts.is_none() && replies.is_none() && views.is_none() {
        None
    } else {
        Some(Engagement {
            likes,
            reposts,
            replies,
            views,
        })
    }
}

/// Parse a numeric string to `u64`, handling float strings like `"42.0"`.
pub(super) fn parse_u64(s: &str) -> Option<u64> {
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    // JSON APIs sometimes return integers as floats (e.g., `8800.0`).
    // The truncation and sign-loss are intentional: engagement counts are
    // always non-negative and whole numbers.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    s.parse::<f64>().ok().map(|f| f as u64)
}
