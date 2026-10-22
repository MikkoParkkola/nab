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

/// Returns `true` for a Schema.org type name that denotes an article
/// (`Article`, `NewsArticle`, `BlogPosting`, `SocialMediaPosting`, `Report`, ...).
pub(super) fn is_article_type(name: &str) -> bool {
    ["Article", "Posting", "Report"]
        .iter()
        .any(|marker| name.contains(marker))
}

/// Parses `content` as Schema.org JSON: an object, or an array of objects, with
/// `@type` or a `@context` naming schema.org. Other JSON (`ActivityStreams` uses
/// `type`, not `@type`) is not Schema.org and returns `None`.
fn parse_schema_org_json(content: &str) -> Option<serde_json::Value> {
    let trimmed = content.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let is_schema = |value: &serde_json::Value| {
        value.as_object().is_some_and(|map| {
            map.contains_key("@type")
                || map
                    .get("@context")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|context| context.contains("schema.org"))
        })
    };
    let value = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
    let schema = match &value {
        serde_json::Value::Array(items) => items.iter().any(is_schema),
        other => is_schema(other),
    };
    schema.then_some(value)
}

/// Returns the longest `articleBody`/`text` of at least 200 bytes on an
/// article-typed node anywhere in `value`. A long `text` on an Answer, Comment
/// or other non-article node is not an article body.
fn find_article_body(value: &serde_json::Value) -> Option<&str> {
    // Mirrors `MIN_CONTENT_LEN` in `jsonld.rs`.
    const MIN_BODY_LEN: usize = 200;
    match value {
        serde_json::Value::Object(map) => {
            let is_article = match map.get("@type") {
                Some(serde_json::Value::String(name)) => is_article_type(name),
                Some(serde_json::Value::Array(names)) => names
                    .iter()
                    .any(|name| name.as_str().is_some_and(is_article_type)),
                _ => false,
            };
            let own = is_article
                .then(|| {
                    ["articleBody", "text"]
                        .iter()
                        .filter_map(|key| map.get(*key).and_then(serde_json::Value::as_str))
                        .filter(|body| body.len() >= MIN_BODY_LEN)
                        .max_by_key(|body| body.len())
                })
                .flatten();
            own.into_iter()
                .chain(map.values().filter_map(find_article_body))
                .max_by_key(|body| body.len())
        }
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(find_article_body)
            .max_by_key(|body| body.len()),
        _ => None,
    }
}

/// Convert a SPA content string (HTML or plain text) to clean markdown.
///
/// Serialized Schema.org JSON is data, not markup: it renders as its article
/// body, or as an empty string when it is metadata only (headline, author,
/// dates), so the caller falls through to readability extraction (MIK-8673).
pub(super) fn render_spa_content(content: &str) -> String {
    if let Some(value) = parse_schema_org_json(content) {
        return find_article_body(&value).map_or_else(String::new, render_markup);
    }
    render_markup(content)
}

fn render_markup(content: &str) -> String {
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

    const BODY: &str = "The full article text, long enough to count as a body. ";

    #[test]
    fn render_spa_content_drops_schema_org_metadata_without_body() {
        for input in [
            r#"{"@context":"https://schema.org","@type":"SocialMediaPosting","headline":"H","description":"a <b>bold</b> claim"}"#,
            r#"[{"@context":"https://schema.org","@type":"SocialMediaPosting","headline":"H"}]"#,
            r#"{"@context":"https://schema.org","@type":"Article","articleBody":"Short stub."}"#,
        ] {
            assert_eq!(render_spa_content(input), "", "metadata-only: {input}");
        }
    }

    #[test]
    fn render_spa_content_ignores_long_text_on_non_article_nodes() {
        let answer = "A long answer that is not the article. ".repeat(10);
        let input = format!(
            r#"{{"@context":"https://schema.org","@graph":[{{"@type":"Article","headline":"H"}},{{"@type":"FAQPage","mainEntity":[{{"@type":"Question","acceptedAnswer":{{"@type":"Answer","text":"{answer}"}}}}]}}]}}"#
        );
        assert_eq!(render_spa_content(&input), "");
    }

    #[test]
    fn render_spa_content_renders_the_article_body_of_schema_org_json() {
        let body = BODY.repeat(5);
        let input = format!(
            r#"{{"@context":"https://schema.org","@graph":[{{"@type":["NewsArticle"],"headline":"H","articleBody":"<p>{body}<b>end</b></p>"}}]}}"#
        );
        let out = render_spa_content(&input);
        assert!(
            out.contains("The full article text"),
            "body expected: {out}"
        );
        assert!(
            !out.contains("<b>") && !out.contains("@context"),
            "rendered prose expected: {out}"
        );
    }

    #[test]
    fn render_spa_content_leaves_non_schema_org_json_on_the_original_path() {
        let body = BODY.repeat(5);
        let activity = format!(
            r#"{{"@context":"https://www.w3.org/ns/activitystreams","type":"Article","content":"{body}"}}"#
        );
        assert_eq!(render_spa_content(&activity), activity);
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
