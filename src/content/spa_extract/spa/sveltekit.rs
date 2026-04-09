//! `SvelteKit` prefetch data block extraction.

use super::helpers::{find_longest_string, render_spa_content};

/// Extract content from `SvelteKit`'s prefetch data script blocks.
///
/// `SvelteKit` embeds server-side fetched data as:
/// ```html
/// <script type="application/json" data-sveltekit-fetched data-url="...">
///   {"status":200,"statusText":"","headers":{},"body":"..."}
/// </script>
/// ```
///
/// Multiple such blocks may exist on one page. We scan all of them and return
/// the longest substantial text found.
pub(crate) fn extract_sveltekit_data(document: &scraper::Html) -> Option<String> {
    const MIN_CONTENT_LEN: usize = 200;

    let sel = scraper::Selector::parse(
        r#"script[type="application/json"][data-sveltekit-fetched]"#,
    )
    .ok()?;

    let mut best: Option<String> = None;

    for script in document.select(&sel) {
        let json_text = script.text().collect::<String>();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json_text.trim()) else {
            continue;
        };

        // Unwrap `body` if it is a JSON string (stringified response body)
        let payload = match value.get("body") {
            Some(serde_json::Value::String(body_str))
                if body_str.starts_with('{') || body_str.starts_with('[') =>
            {
                serde_json::from_str::<serde_json::Value>(body_str).unwrap_or(value.clone())
            }
            _ => value.clone(),
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
    fn extract_sveltekit_data_extracts_plain_json_body() {
        // GIVEN: a SvelteKit page with a prefetch block whose body is a JSON object
        let article = "SvelteKit is a framework for building web applications of all sizes, \
                       with a beautiful development experience and flexible filesystem-based routing. \
                       This article body is long enough to exceed the two hundred character minimum threshold.";
        let payload = serde_json::json!({
            "status": 200,
            "statusText": "",
            "headers": {},
            "body": {"content": article}
        });
        let html = format!(
            r#"<html><body>
            <script type="application/json" data-sveltekit-fetched data-url="/api/post">
            {payload}
            </script>
            </body></html>"#,
            payload = serde_json::to_string(&payload).unwrap()
        );

        // WHEN: we run the extractor
        let result = extract_sveltekit_data(&scraper::Html::parse_document(&html));

        // THEN: the article text is returned
        assert!(result.is_some(), "expected content, got None");
        assert!(result.unwrap().contains("SvelteKit is a framework"));
    }

    #[test]
    fn extract_sveltekit_data_unwraps_stringified_body() {
        // GIVEN: body is a JSON-encoded string (double-encoded response)
        let article = "SvelteKit sometimes encodes the response body as a JSON string rather than \
                       an inline object. This test verifies the extractor unwraps the string and \
                       recovers the content. The text must exceed the two hundred character minimum.";
        let inner = serde_json::json!({"content": article});
        let payload = serde_json::json!({
            "status": 200,
            "body": serde_json::to_string(&inner).unwrap()
        });
        let html = format!(
            r#"<html><body>
            <script type="application/json" data-sveltekit-fetched data-url="/api/article">
            {payload}
            </script>
            </body></html>"#,
            payload = serde_json::to_string(&payload).unwrap()
        );

        let result = extract_sveltekit_data(&scraper::Html::parse_document(&html));
        assert!(result.is_some());
        assert!(result.unwrap().contains("sometimes encodes the response body"));
    }

    #[test]
    fn extract_sveltekit_data_returns_none_for_no_matching_tags() {
        // GIVEN: HTML with no SvelteKit prefetch blocks
        let html =
            r#"<html><body><script type="application/json">{"other":"data"}</script></body></html>"#;
        assert!(extract_sveltekit_data(&scraper::Html::parse_document(html)).is_none());
    }

    #[test]
    fn extract_sveltekit_data_returns_none_for_short_content() {
        // GIVEN: a SvelteKit block but with content below the 200-char minimum
        let html = r#"<html><body>
            <script type="application/json" data-sveltekit-fetched data-url="/api/meta">
            {"status":200,"body":{"title":"Short"}}
            </script>
            </body></html>"#;
        assert!(extract_sveltekit_data(&scraper::Html::parse_document(html)).is_none());
    }
}
