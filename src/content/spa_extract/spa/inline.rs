//! Generic inline `<script>` variable assignment extraction, hidden `<code>` JSON
//! extraction, and balanced-JSON parser.

use super::helpers::{collect_text_from_json, render_spa_content, strip_html_comment_wrapper};
use super::nextjs::extract_nextjs_content;

/// Extract content from inline `<script>` variable assignments.
///
/// Some JS-rendered pages embed data via:
/// ```text
/// <script>window.__NEXT_DATA__ = {"props":{"pageProps":{...}}}</script>
/// ```
/// rather than using a `<script id="__NEXT_DATA__" type="application/json">` tag.
///
/// This function scans all inline scripts for known variable assignment patterns
/// and attempts to extract the JSON payload.
pub fn extract_inline_script_json(html: &str) -> Option<String> {
    const PATTERNS: &[&str] = &[
        "window.__NEXT_DATA__",
        "self.__NEXT_DATA__",
        "__NEXT_DATA__",
        "window.__NUXT__",
        "window.__INITIAL_STATE__",
        "window.__PRELOADED_STATE__",
        "window.__APOLLO_STATE__",
        // Generic SSR state patterns used by various frameworks
        "window.__APP_STATE__",
        "window.__STORE_STATE__",
        "window.__DATA__",
        "window.___GATSBY",
    ];

    const MIN_CONTENT_LEN: usize = 200;
    let document = scraper::Html::parse_document(html);
    let script_sel = scraper::Selector::parse("script").ok()?;

    for script in document.select(&script_sel) {
        if script.value().attr("src").is_some() {
            continue;
        }

        let script_text = script.text().collect::<String>();
        if script_text.trim().is_empty() {
            continue;
        }

        for pattern in PATTERNS {
            if let Some(content) =
                extract_content_from_named_inline_assignment(&script_text, pattern, MIN_CONTENT_LEN)
            {
                return Some(content);
            }
        }

        if let Some(content) =
            extract_content_from_generic_inline_assignments(&script_text, MIN_CONTENT_LEN)
        {
            return Some(content);
        }
    }

    None
}

fn extract_content_from_named_inline_assignment(
    script_text: &str,
    pattern: &str,
    min_content_len: usize,
) -> Option<String> {
    let start_idx = script_text.find(pattern)?;
    let after_pattern = start_idx + pattern.len();
    let remaining = &script_text[after_pattern..];
    let eq_offset = remaining.find('=')?;
    let after_eq = &remaining[eq_offset + 1..];
    let json_offset = after_eq
        .char_indices()
        .find(|(_, c)| *c == '{' || *c == '[')
        .map(|(idx, _)| idx)?;
    let json_start = &after_eq[json_offset..];
    extract_content_from_json_slice(json_start, min_content_len)
}

fn extract_content_from_generic_inline_assignments(
    script_text: &str,
    min_content_len: usize,
) -> Option<String> {
    let mut best: Option<String> = None;
    let mut search_from = 0;

    while let Some(eq_offset) = script_text[search_from..].find('=') {
        let after_eq_idx = search_from + eq_offset + 1;
        let after_eq = &script_text[after_eq_idx..];
        let Some(json_offset) = after_eq
            .char_indices()
            .find(|(_, c)| *c == '{' || *c == '[')
            .map(|(idx, _)| idx)
        else {
            search_from = after_eq_idx;
            continue;
        };

        let json_start_idx = after_eq_idx + json_offset;
        if let Some(content) =
            extract_content_from_json_slice(&script_text[json_start_idx..], min_content_len)
        {
            let current_best_len = best.as_deref().map_or(0, str::len);
            if content.len() > current_best_len {
                best = Some(content);
            }
        }

        search_from = json_start_idx + 1;
    }

    best
}

fn extract_content_from_json_slice(json_start: &str, min_content_len: usize) -> Option<String> {
    use super::helpers::find_longest_string;

    let json_str = extract_balanced_json(json_start)?;
    let data = serde_json::from_str::<serde_json::Value>(json_str).ok()?;

    if let Some(content) = extract_nextjs_content(&data) {
        return Some(content);
    }

    find_longest_string(&data, min_content_len).map(|content| render_spa_content(&content))
}

/// Extract a balanced JSON object or array from the beginning of a string.
///
/// Tracks brace/bracket depth and string escaping to find the end of the
/// outermost JSON structure. Returns the slice containing the full JSON,
/// or `None` if the structure is not balanced.
pub fn extract_balanced_json(s: &str) -> Option<&str> {
    let first_char = s.chars().next()?;
    let (open, close) = match first_char {
        '{' => ('{', '}'),
        '[' => ('[', ']'),
        _ => return None,
    };

    let mut depth: i32 = 0;
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

/// Extract content from hidden `<code>` elements containing JSON.
///
/// Some SPAs (notably `LinkedIn`) embed server-fetched data in hidden `<code>`
/// elements rather than `<script>` tags:
///
/// ```text
/// <code style="display:none" id="bpr-guid-XXXX"><!--{"data":{...}}--></code>
/// ```
///
/// This function scans all `<code>` elements for JSON payloads, unwraps
/// HTML comment wrappers (`<!--` / `-->`), and searches recursively for
/// text content. Also handles pre-fetched API response envelopes where the
/// `body` field contains a nested JSON string.
pub(crate) fn extract_hidden_code_json(document: &scraper::Html) -> Option<String> {
    const MIN_CONTENT_LEN: usize = 200;

    let selector = scraper::Selector::parse("code").ok()?;
    let mut all_text = Vec::new();

    for element in document.select(&selector) {
        let raw = element.inner_html();
        let json_str = strip_html_comment_wrapper(raw.trim());
        if json_str.is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) else {
            continue;
        };

        collect_text_from_json(&value, &mut all_text);
        unwrap_api_response_bodies(&value, &mut all_text);
    }

    if all_text.is_empty() {
        return None;
    }

    all_text
        .into_iter()
        .filter(|s| s.len() >= MIN_CONTENT_LEN)
        .max_by_key(std::string::String::len)
        .map(|content| render_spa_content(&content))
}

/// Unwrap pre-fetched API response envelopes from a JSON value.
///
/// SPAs often embed pre-fetched API responses as:
/// ```json
/// {"request": "/api/endpoint", "status": 200, "body": "{\"data\": ...}", "method": "GET"}
/// ```
///
/// The `body` field contains the full API response as a JSON string.
/// This function finds such envelopes, parses the `body` string as JSON,
/// and recursively collects text content from the parsed payload.
///
/// This pattern is used by `LinkedIn`, Instagram, and other Meta-family SPAs.
pub fn unwrap_api_response_bodies(value: &serde_json::Value, texts: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let (Some(status), Some(body_str)) = (
                map.get("status").and_then(serde_json::Value::as_u64),
                map.get("body").and_then(|v| v.as_str()),
            ) && status == 200
                && !body_str.is_empty()
                && let Ok(body_json) = serde_json::from_str::<serde_json::Value>(body_str)
            {
                collect_text_from_json(&body_json, texts);
            }
            for v in map.values() {
                unwrap_api_response_bodies(v, texts);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                unwrap_api_response_bodies(v, texts);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::helpers::strip_html_comment_wrapper;

    #[test]
    fn strip_html_comment_wrapper_removes_wrapper() {
        assert_eq!(strip_html_comment_wrapper("<!--{\"a\":1}-->"), "{\"a\":1}");
    }

    #[test]
    fn strip_html_comment_wrapper_passthrough_no_wrapper() {
        assert_eq!(strip_html_comment_wrapper("{\"a\":1}"), "{\"a\":1}");
    }

    #[test]
    fn strip_html_comment_wrapper_trims_whitespace() {
        assert_eq!(
            strip_html_comment_wrapper("<!-- {\"a\":1} -->"),
            "{\"a\":1}"
        );
    }

    #[test]
    fn unwrap_api_response_bodies_parses_body_string() {
        let envelope = serde_json::json!({
            "request": "/api/v2/data",
            "status": 200,
            "body": "{\"text\": \"This is a substantial piece of text content that should be extracted from the API response body for display.\"}",
            "method": "GET"
        });
        let mut texts = Vec::new();
        unwrap_api_response_bodies(&envelope, &mut texts);
        assert_eq!(texts.len(), 1);
        assert!(texts[0].contains("substantial piece of text"));
    }

    #[test]
    fn unwrap_api_response_bodies_skips_non_200() {
        let envelope = serde_json::json!({
            "request": "/api/v2/data",
            "status": 404,
            "body": "{\"error\": \"not found with a long enough message to pass the minimum length filter for text extraction\"}",
            "method": "GET"
        });
        let mut texts = Vec::new();
        unwrap_api_response_bodies(&envelope, &mut texts);
        assert!(texts.is_empty());
    }

    #[test]
    fn unwrap_api_response_bodies_handles_nested_envelopes() {
        let outer = serde_json::json!({
            "responses": [
                {
                    "status": 200,
                    "body": "{\"commentary\": \"This is a long post about technology and innovation that should definitely be extracted by the parser.\"}",
                    "request": "/api/feed"
                },
                {
                    "status": 200,
                    "body": "{\"title\": \"Another interesting article with enough content to meet the minimum length threshold for extraction.\"}",
                    "request": "/api/articles"
                }
            ]
        });
        let mut texts = Vec::new();
        unwrap_api_response_bodies(&outer, &mut texts);
        assert_eq!(texts.len(), 2);
    }

    #[test]
    fn unwrap_api_response_bodies_skips_empty_body() {
        let envelope = serde_json::json!({
            "status": 200,
            "body": "",
            "request": "/api/empty"
        });
        let mut texts = Vec::new();
        unwrap_api_response_bodies(&envelope, &mut texts);
        assert!(texts.is_empty());
    }

    #[test]
    fn collect_text_skips_urls_and_short_strings() {
        let data = serde_json::json!({
            "url": "https://example.com/path",
            "urn": "urn:li:member:12345",
            "id": "abc-def-123",
            "short": "too short",
            "content": "This is a long enough string that should be collected by the text extraction function because it passes all filters."
        });
        let mut texts = Vec::new();
        collect_text_from_json(&data, &mut texts);
        assert_eq!(texts.len(), 1);
        assert!(texts[0].contains("long enough string"));
    }

    #[test]
    fn extract_hidden_code_json_from_html() {
        let html = r#"<html><body>
            <code style="display:none"><!--{"data": {"elements": [{"commentary": "This is a substantial article body that contains enough text to meet the minimum content length threshold for extraction from hidden code elements in single-page application frameworks. We need this to be over two hundred characters in total length to pass the minimum content filter that ensures we only return meaningful text content and not short metadata strings or identifiers."}]}}--></code>
        </body></html>"#;
        let document = scraper::Html::parse_document(html);
        let result = extract_hidden_code_json(&document);
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(
            content.contains("substantial article body"),
            "got: {content}"
        );
    }

    #[test]
    fn extract_hidden_code_json_with_api_envelope() {
        let body_json = serde_json::json!({
            "data": {
                "commentary": "This is a pre-fetched API response body containing a long post about marketplace fraud that should be extracted from the envelope format. The text must exceed two hundred characters in total length to pass the minimum content threshold applied by the extraction pipeline to filter out short metadata strings, identifiers, and other non-content values."
            }
        });
        let html = format!(
            r#"<html><body>
            <code style="display:none"><!--{{"request": "/voyager/api/graphql", "status": 200, "body": {}, "method": "GET"}}--></code>
        </body></html>"#,
            serde_json::to_string(&body_json.to_string()).unwrap()
        );
        let document = scraper::Html::parse_document(&html);
        let result = extract_hidden_code_json(&document);
        assert!(result.is_some());
        assert!(result.unwrap().contains("marketplace fraud"));
    }

    #[test]
    fn extract_hidden_code_json_returns_none_for_no_content() {
        let html = r#"<html><body>
            <code>just some code here</code>
            <code>{"id": "short"}</code>
        </body></html>"#;
        let document = scraper::Html::parse_document(html);
        assert!(extract_hidden_code_json(&document).is_none());
    }

    #[test]
    fn extract_inline_script_json_handles_multibyte_named_assignment_prefix() {
        let body = "This is a substantial article body extracted from a named inline assignment after multibyte banner text. It should remain long enough to cross the minimum-content threshold and prove UTF-8-safe scanning."
            .to_string();
        let html = format!(
            r#"<html><body>
                <script>
                    // ─── Banner ───────────────────────────────────────
                    window.__NEXT_DATA__ = {{"props":{{"pageProps":{{"body":"{body}"}}}}}};
                </script>
            </body></html>"#
        );

        let content = extract_inline_script_json(&html).expect("content from named assignment");
        assert!(content.contains("substantial article body"));
    }

    #[test]
    fn extract_inline_script_json_handles_multibyte_generic_assignment_prefix() {
        let commentary = "This is a substantial article body extracted from a generic inline JSON assignment after multibyte banner text. It should remain long enough to cross the minimum-content threshold and prove UTF-8-safe scanning."
            .to_string();
        let html = format!(
            r#"<html><body>
                <script>
                    window.addEventListener('DOMContentLoaded', function () {{
                        // ─── Rotating announcement items ───────────────────────────────────────
                        cfg = {{"commentary":"{commentary}"}};
                    }});
                </script>
            </body></html>"#
        );

        let content =
            extract_inline_script_json(&html).expect("content from generic assignment");
        assert!(content.contains("substantial article body"));
    }

    #[test]
    fn extract_inline_script_json_handles_window_app_state() {
        // GIVEN: a page with window.__APP_STATE__ inline assignment
        let article = "Generic SSR state via window.__APP_STATE__: this content must be substantial \
                       enough to pass the two hundred character minimum threshold applied to inline \
                       script JSON extraction so that the assignment pattern is recognised and returned.";
        let html = format!(
            r#"<html><body>
            <script>window.__APP_STATE__ = {{"content":"{article}"}};</script>
            </body></html>"#
        );

        let result = extract_inline_script_json(&html);
        assert!(result.is_some(), "expected content, got None");
        assert!(result
            .unwrap()
            .contains("Generic SSR state via window.__APP_STATE__"));
    }

    #[test]
    fn extract_inline_script_json_handles_window_store_state() {
        // GIVEN: window.__STORE_STATE__ inline assignment
        let article = "Generic SSR store state: this text is long enough to trigger extraction from \
                       window.__STORE_STATE__ inline assignments. The content deliberately exceeds \
                       the two hundred character minimum to ensure the pattern is picked up correctly.";
        let html = format!(
            r#"<html><body>
            <script>window.__STORE_STATE__ = {{"body":"{article}"}};</script>
            </body></html>"#
        );

        let result = extract_inline_script_json(&html);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Generic SSR store state"));
    }

    #[test]
    fn extract_inline_script_json_handles_window_data() {
        // GIVEN: window.__DATA__ inline assignment
        let article = "Generic window.__DATA__ SSR pattern: this is long enough to exceed the \
                       minimum content threshold of two hundred characters used by the inline JSON \
                       extractor to filter out short metadata values and return only article bodies.";
        let html = format!(
            r#"<html><body>
            <script>window.__DATA__ = {{"text":"{article}"}};</script>
            </body></html>"#
        );

        let result = extract_inline_script_json(&html);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Generic window.__DATA__ SSR pattern"));
    }
}
