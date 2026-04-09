//! JSON-LD structured data extraction (`<script type="application/ld+json">`).

use super::helpers::render_spa_content;

/// Extract article content from `<script type="application/ld+json">` tags.
///
/// Many modern blogs (including JS-rendered ones) embed Schema.org structured
/// data as JSON-LD. This contains the article body in `articleBody`, or at
/// minimum `description`, which gives us content without needing JS execution.
///
/// Handles both single JSON-LD objects and arrays of objects (some sites
/// emit `[{...}, {...}]` with multiple schema types).
pub fn extract_jsonld_content(document: &scraper::Html) -> Option<String> {
    const MIN_CONTENT_LEN: usize = 200;

    let sel = scraper::Selector::parse(r#"script[type="application/ld+json"]"#).ok()?;

    // Ordered by preference: articleBody > text.
    // `description` is deliberately excluded — Schema.org defines it as a
    // short summary/excerpt, not the full article body.  Ghost CMS (and many
    // other blogs) populate only `description` in their JSON-LD, and the
    // value is truncated to ~500 chars.  Falling through to the readability
    // path yields the complete article from the actual HTML DOM.
    let content_keys = ["articleBody", "text"];

    let mut best: Option<String> = None;

    for script in document.select(&sel) {
        let json_text = script.text().collect::<String>();
        let json_text = json_text.trim();
        if json_text.is_empty() {
            continue;
        }

        let values: Vec<serde_json::Value> = if json_text.starts_with('[') {
            serde_json::from_str(json_text).ok()?
        } else if json_text.starts_with('{') {
            vec![serde_json::from_str(json_text).ok()?]
        } else {
            continue;
        };

        for value in &values {
            // Only consider Article-like types (skip Organization, WebSite, BreadcrumbList)
            if let Some(schema_type) = value.get("@type").and_then(|t| t.as_str()) {
                let is_article = schema_type.contains("Article")
                    || schema_type.contains("Posting")
                    || schema_type.contains("Report")
                    || schema_type.contains("ScholarlyArticle")
                    || schema_type.contains("TechArticle")
                    || schema_type.contains("NewsArticle")
                    || schema_type.contains("BlogPosting");
                if !is_article {
                    continue;
                }
            } else {
                continue;
            }

            for key in &content_keys {
                if let Some(serde_json::Value::String(s)) = value.get(*key)
                    && s.len() >= MIN_CONTENT_LEN
                {
                    let current_best_len = best.as_deref().map_or(0, str::len);
                    if s.len() > current_best_len {
                        best = Some(s.clone());
                    }
                }
            }
        }
    }

    best.map(|content| render_spa_content(&content))
}
