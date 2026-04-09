//! Angular Universal server transfer state extraction.

use super::helpers::{find_longest_string, render_spa_content};

/// Extract content from Angular Universal's server transfer state.
///
/// Angular Universal serializes the server-side rendered state as:
/// ```html
/// <script id="serverApp-state" type="application/json">
///   {"key":{"body":"...","status":200}}
/// </script>
/// ```
///
/// The state is an object whose values are Angular `HttpResponse`-shaped
/// objects with a `body` field. We unwrap nested JSON strings and return
/// the longest substantial text found.
pub(crate) fn extract_angular_universal_state(document: &scraper::Html) -> Option<String> {
    const MIN_CONTENT_LEN: usize = 200;

    let sel =
        scraper::Selector::parse(r#"script#serverApp-state[type="application/json"]"#).ok()?;
    let script = document.select(&sel).next()?;
    let json_text = script.text().collect::<String>();
    let value: serde_json::Value = serde_json::from_str(json_text.trim()).ok()?;

    // The root is an object whose keys are Angular transfer state keys.
    // Each value may be a JSON-encoded string or a plain object.
    let mut best: Option<String> = None;

    let entries = match &value {
        serde_json::Value::Object(map) => map.values().cloned().collect::<Vec<_>>(),
        other => vec![other.clone()],
    };

    for entry in &entries {
        // Unwrap body if it is a stringified JSON response
        let payload = match entry.get("body") {
            Some(serde_json::Value::String(body_str))
                if body_str.starts_with('{') || body_str.starts_with('[') =>
            {
                serde_json::from_str::<serde_json::Value>(body_str).unwrap_or(entry.clone())
            }
            _ => entry.clone(),
        };

        if let Some(text) = find_longest_string(&payload, MIN_CONTENT_LEN) {
            let current_best_len = best.as_deref().map_or(0, str::len);
            if text.len() > current_best_len {
                best = Some(text);
            }
        }
    }

    best.map(|content| render_spa_content(&content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_angular_universal_state_extracts_plain_object_body() {
        // GIVEN: Angular Universal transfer state with a plain-object body field
        let article = "Angular Universal enables server-side rendering for Angular applications, \
                       improving initial load performance and SEO. The transfer state allows the \
                       server to pass pre-fetched data to the client without redundant HTTP requests. \
                       This text is well over two hundred characters.";
        let state = serde_json::json!({
            "G.http.cache.v1./api/article": {
                "status": 200,
                "body": {"content": article}
            }
        });
        let html = format!(
            r#"<html><body>
            <script id="serverApp-state" type="application/json">
            {state}
            </script>
            </body></html>"#,
            state = serde_json::to_string(&state).unwrap()
        );

        let result = extract_angular_universal_state(&scraper::Html::parse_document(&html));
        assert!(result.is_some(), "expected content, got None");
        assert!(result.unwrap().contains("Angular Universal enables"));
    }

    #[test]
    fn extract_angular_universal_state_unwraps_stringified_body() {
        // GIVEN: transfer state where body is a JSON-encoded string
        let article = "Angular Universal sometimes serializes the HTTP response body as a JSON \
                       string within the transfer state object. This test verifies the extractor \
                       correctly parses and unwraps the double-encoded payload to recover the content.";
        let inner = serde_json::json!({"content": article});
        let state = serde_json::json!({
            "cache.key": {
                "status": 200,
                "body": serde_json::to_string(&inner).unwrap()
            }
        });
        let html = format!(
            r#"<html><body>
            <script id="serverApp-state" type="application/json">
            {state}
            </script>
            </body></html>"#,
            state = serde_json::to_string(&state).unwrap()
        );

        let result = extract_angular_universal_state(&scraper::Html::parse_document(&html));
        assert!(result.is_some());
        assert!(result.unwrap().contains("sometimes serializes"));
    }

    #[test]
    fn extract_angular_universal_state_returns_none_for_missing_tag() {
        let html = r"<html><body><p>No Angular here</p></body></html>";
        assert!(extract_angular_universal_state(&scraper::Html::parse_document(html)).is_none());
    }

    #[test]
    fn extract_angular_universal_state_returns_none_for_short_content() {
        let html = r#"<html><body>
            <script id="serverApp-state" type="application/json">
            {"key":{"status":200,"body":{"title":"Hi"}}}
            </script>
            </body></html>"#;
        assert!(extract_angular_universal_state(&scraper::Html::parse_document(html)).is_none());
    }
}
