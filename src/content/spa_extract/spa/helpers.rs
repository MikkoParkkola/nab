//! Shared helper utilities used across SPA extractor modules.

/// Find the longest string value anywhere in a JSON tree.
///
/// This is a last-resort fallback for Next.js / SPA pages that use proprietary
/// content key names. By finding the longest string, we can usually recover the
/// article body even when its field name is unknown.
///
/// Skips strings shorter than `min_len` to avoid picking up IDs, slugs, or
/// short metadata strings.
pub fn find_longest_string(value: &serde_json::Value, min_len: usize) -> Option<String> {
    match value {
        serde_json::Value::String(s) => {
            if s.len() >= min_len {
                Some(s.clone())
            } else {
                None
            }
        }
        serde_json::Value::Object(map) => map
            .values()
            .filter_map(|v| find_longest_string(v, min_len))
            .max_by_key(std::string::String::len),
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| find_longest_string(v, min_len))
            .max_by_key(std::string::String::len),
        _ => None,
    }
}

/// Recursively walk a JSON value tree looking for a string field named `key`.
///
/// Returns the first string value found using depth-first search (object fields
/// before array items). Returns `None` if the key is not present.
pub fn find_content_by_key(value: &serde_json::Value, key: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get(key) {
                return Some(s.clone());
            }
            for (_, v) in map {
                if let Some(found) = find_content_by_key(v, key) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(found) = find_content_by_key(item, key) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Returns `true` when `content` parses as JSON carrying `@context` or `@type`
/// at the top level or on a top-level array item.
fn is_schema_org_json(content: &str) -> bool {
    let trimmed = content.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return false;
    }
    let is_schema = |value: &serde_json::Value| {
        value
            .as_object()
            .is_some_and(|map| map.contains_key("@context") || map.contains_key("@type"))
    };
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Array(items)) => items.iter().any(is_schema),
        Ok(value) => is_schema(&value),
        Err(_) => false,
    }
}

/// Convert a SPA content string (HTML or plain text) to clean markdown.
pub(super) fn render_spa_content(content: &str) -> String {
    // Serialized Schema.org JSON is data, not markup: rendering it as HTML
    // corrupts it, and `html.rs` inspects it as JSON. Any other JSON is rendered.
    if is_schema_org_json(content) {
        return content.to_string();
    }
    if content.contains('<') && content.contains('>') {
        let md = html2md::parse_html(content);
        md.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        content.to_string()
    }
}

/// Recursively collect substantial text strings from a JSON value tree.
///
/// Walks the entire JSON structure and collects string values that look like
/// meaningful text content (minimum length, not URLs/IDs/hashes).
pub(super) fn collect_text_from_json(value: &serde_json::Value, texts: &mut Vec<String>) {
    const MIN_TEXT_LEN: usize = 50;

    match value {
        serde_json::Value::String(s)
            if s.len() >= MIN_TEXT_LEN
                && !s.starts_with("http")
                && !s.starts_with("urn:")
                && !s.chars().all(|c| c.is_ascii_hexdigit() || c == '-') =>
        {
            texts.push(s.clone());
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                collect_text_from_json(v, texts);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_text_from_json(v, texts);
            }
        }
        _ => {}
    }
}

/// Strip `<!--` prefix and `-->` suffix from an HTML comment wrapper.
///
/// `LinkedIn` wraps `<code>` JSON in HTML comments: `<!--{...}-->`.
/// Returns the inner content unchanged if no wrapper is present.
pub(super) fn strip_html_comment_wrapper(s: &str) -> &str {
    let s = s.strip_prefix("<!--").unwrap_or(s);
    let s = s.strip_suffix("-->").unwrap_or(s);
    s.trim()
}

#[cfg(test)]
mod tests {
    use super::render_spa_content;

    #[test]
    fn render_spa_content_returns_serialized_json_unchanged() {
        let input = r#"{"@type":"Article","description":"a <b>bold</b> claim"}"#;
        assert_eq!(render_spa_content(input), input);
    }

    #[test]
    fn render_spa_content_renders_html_inside_non_schema_json() {
        let out = render_spa_content(r#"{"body":"<p>Hello <b>world</b></p>"}"#);
        assert!(
            !out.contains("<b>"),
            "non-schema JSON must still be rendered: {out}"
        );
    }

    #[test]
    fn render_spa_content_still_renders_html() {
        assert!(!render_spa_content("<p>Hello <b>world</b></p>").contains("<p>"));
    }
}
