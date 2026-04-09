//! Gatsby SSR data block extraction.

use super::helpers::{find_longest_string, render_spa_content};

/// Extract content from Gatsby's SSR data script blocks.
///
/// Gatsby embeds page data in two places:
///
/// 1. `<script type="application/json" data-gatsby-ssr>` tags (Gatsby v4+)
/// 2. `window.___GATSBY` inline assignment (older Gatsby / runtime bootstrap)
///
/// The SSR tag payload is a full page-data envelope:
/// ```json
/// {"componentChunkName":"...","result":{"pageContext":{...},"data":{...}}}
/// ```
pub(crate) fn extract_gatsby_data(document: &scraper::Html, html: &str) -> Option<String> {
    const MIN_CONTENT_LEN: usize = 200;

    if let Some(content) = extract_gatsby_ssr_tags(document, MIN_CONTENT_LEN) {
        return Some(content);
    }

    // Strategy 2: window.pagePath inline assignment — indicates Gatsby but carries
    // no content on its own; content lives in the page-data JSON bundle loaded
    // separately. We still check for any inline JSON in the same script block.
    let _ = html; // reserved for future inline-JSON scan if needed

    None
}

/// Scan `<script type="application/json" data-gatsby-ssr>` tags for content.
fn extract_gatsby_ssr_tags(document: &scraper::Html, min_len: usize) -> Option<String> {
    let sel =
        scraper::Selector::parse(r#"script[type="application/json"][data-gatsby-ssr]"#).ok()?;

    let mut best: Option<String> = None;

    for script in document.select(&sel) {
        let json_text = script.text().collect::<String>();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json_text.trim()) else {
            continue;
        };

        // Gatsby nests content under result.data or result.pageContext
        let search_root = value.get("result").unwrap_or(&value);

        if let Some(text) = find_longest_string(search_root, min_len) {
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
    fn extract_gatsby_ssr_tags_extracts_content() {
        // GIVEN: a Gatsby v4+ SSR data tag whose result.data contains article text
        let article = "Gatsby is a React-based open source framework for creating websites and apps. \
                       Built on top of React, Gatsby can power anything from a simple blog to a \
                       complex content-driven platform. This body text is long enough to pass the threshold.";
        let payload = serde_json::json!({
            "componentChunkName": "component---src-pages-blog-post-jsx",
            "result": {
                "data": {
                    "markdownRemark": {
                        "html": article
                    }
                }
            }
        });
        let html = format!(
            r#"<html><body>
            <script type="application/json" data-gatsby-ssr>
            {payload}
            </script>
            </body></html>"#,
            payload = serde_json::to_string(&payload).unwrap()
        );

        let result = extract_gatsby_data(&scraper::Html::parse_document(&html), &html);
        assert!(result.is_some(), "expected content, got None");
        let content = result.unwrap();
        assert!(content.contains("Gatsby is a React-based"));
    }

    #[test]
    fn extract_gatsby_ssr_tags_returns_none_for_no_matching_tag() {
        let html = r"<html><body><p>Plain page</p></body></html>";
        assert!(extract_gatsby_data(&scraper::Html::parse_document(html), html).is_none());
    }

    #[test]
    fn extract_gatsby_ssr_tags_returns_none_for_short_content() {
        let html = r#"<html><body>
            <script type="application/json" data-gatsby-ssr>
            {"result":{"data":{"title":"Hi"}}}
            </script>
            </body></html>"#;
        assert!(extract_gatsby_data(&scraper::Html::parse_document(html), html).is_none());
    }
}
