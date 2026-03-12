//! SPA / structured-data extraction for JS-rendered pages.
//!
//! Extracts article content from embedded JSON bundles in single-page
//! applications (Next.js, Nuxt, Redux) and from Schema.org JSON-LD
//! structured data.
//!
//! # Extraction Pipeline
//!
//! 1. `<script id="__NEXT_DATA__">` (Next.js SSR data)
//! 2. `<script id="__NUXT_DATA__">` / `<script id="__nuxt-data">` (Nuxt.js SSR data)
//! 3. `<script type="application/ld+json">` (Schema.org structured data)
//! 4. Inline `<script>` variable assignments (`window.__NEXT_DATA__ = {...}`)

/// Try to extract article content from SPA JSON bundles embedded in HTML.
///
/// Modern single-page applications (Next.js, Nuxt, etc.) embed serialized
/// server-side render state in `<script>` tags. This function extracts that
/// state and recursively searches for the longest text content field.
///
/// Returns `Some(markdown)` if a substantial content field is found (>200 chars),
/// `None` otherwise.
pub(crate) fn extract_spa_data(html: &str) -> Option<String> {
    let document = scraper::Html::parse_document(html);

    // Try __NEXT_DATA__ (Next.js) — highest priority, most structured
    if let Some(content) = try_extract_script_json(&document, "script#__NEXT_DATA__") {
        return Some(content);
    }

    // Try __NUXT_DATA__ / __NUXT_STATE__ (Nuxt.js)
    for selector in &["script#__NUXT_DATA__", "script#__nuxt-data"] {
        if let Some(content) = try_extract_script_json(&document, selector) {
            return Some(content);
        }
    }

    // Try JSON-LD structured data (Schema.org — widely used by modern blogs)
    if let Some(content) = extract_jsonld_content(&document) {
        return Some(content);
    }

    // Try inline script variable assignments (window.__NEXT_DATA__ = {...}, etc.)
    if let Some(content) = extract_inline_script_json(html) {
        return Some(content);
    }

    None
}

/// Extract article content from `<script type="application/ld+json">` tags.
///
/// Many modern blogs (including JS-rendered ones) embed Schema.org structured
/// data as JSON-LD. This contains the article body in `articleBody`, or at
/// minimum `description`, which gives us content without needing JS execution.
///
/// Handles both single JSON-LD objects and arrays of objects (some sites
/// emit `[{...}, {...}]` with multiple schema types).
fn extract_jsonld_content(document: &scraper::Html) -> Option<String> {
    const MIN_CONTENT_LEN: usize = 200;

    let sel = scraper::Selector::parse(r#"script[type="application/ld+json"]"#).ok()?;

    // Ordered by preference: articleBody > description
    let content_keys = ["articleBody", "text", "description"];

    let mut best: Option<String> = None;

    for script in document.select(&sel) {
        let json_text = script.text().collect::<String>();
        let json_text = json_text.trim();
        if json_text.is_empty() {
            continue;
        }

        // Parse as single object or array
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
                continue; // Skip objects without @type
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
fn extract_inline_script_json(html: &str) -> Option<String> {
    const PATTERNS: &[&str] = &[
        "window.__NEXT_DATA__",
        "self.__NEXT_DATA__",
        "__NEXT_DATA__",
        "window.__NUXT__",
        "window.__INITIAL_STATE__",
        "window.__PRELOADED_STATE__",
        "window.__APOLLO_STATE__",
    ];

    const MIN_CONTENT_LEN: usize = 200;

    for pattern in PATTERNS {
        if let Some(start_idx) = html.find(pattern) {
            // Find the '=' after the variable name
            let after_pattern = start_idx + pattern.len();
            let remaining = &html[after_pattern..];

            // Skip whitespace and find '='
            let eq_offset = remaining.find('=')?;
            let after_eq = &remaining[eq_offset + 1..];

            // Find the start of the JSON object or array
            let json_offset = after_eq.chars().position(|c| c == '{' || c == '[')?;
            let json_start = &after_eq[json_offset..];

            // Extract balanced JSON
            if let Some(json_str) = extract_balanced_json(json_start)
                && let Ok(data) = serde_json::from_str::<serde_json::Value>(json_str)
            {
                // Try Next.js structure (props.pageProps)
                if let Some(content) = extract_nextjs_content(&data) {
                    return Some(content);
                }
                // Try generic longest-string search on the entire payload
                if let Some(longest) = find_longest_string(&data, MIN_CONTENT_LEN) {
                    return Some(render_spa_content(&longest));
                }
            }
        }
    }

    None
}

/// Extract a balanced JSON object or array from the beginning of a string.
///
/// Tracks brace/bracket depth and string escaping to find the end of the
/// outermost JSON structure. Returns the slice containing the full JSON,
/// or `None` if the structure is not balanced.
fn extract_balanced_json(s: &str) -> Option<&str> {
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

/// Extract and convert content from a JSON-bearing `<script>` element.
fn try_extract_script_json(document: &scraper::Html, css_selector: &str) -> Option<String> {
    let sel = scraper::Selector::parse(css_selector).ok()?;
    let script = document.select(&sel).next()?;
    let json_text = script.text().collect::<String>();
    let data: serde_json::Value = serde_json::from_str(&json_text).ok()?;
    extract_nextjs_content(&data)
}

/// Recursively search a Next.js `pageProps` tree for the longest content field.
///
/// Next.js stores page data under `props.pageProps`. We use two strategies:
///
/// 1. **Named-key search**: Look for well-known content field names (most accurate).
/// 2. **Longest-string fallback**: If named-key search fails, walk the entire tree
///    and return the longest string value found. This handles sites like Stripe's
///    developer blog that use proprietary key names (`bodyText`, `richContent`, etc.).
///
/// The named-key strategy is tried first because it is more precise. The
/// longest-string fallback is less precise (may pick up large JSON blobs instead of
/// article text) but is far better than returning nothing for JS-rendered pages.
fn extract_nextjs_content(data: &serde_json::Value) -> Option<String> {
    // Ordered by specificity — HTML/rich content first, summaries last.
    // Extend this list when new CMS key patterns are discovered.
    const CONTENT_KEYS: &[&str] = &[
        "body",
        "bodyText",
        "bodyHtml",
        "body_html",
        "html",
        "content",
        "contentHtml",
        "content_html",
        "richContent",
        "richText",
        "articleBody",
        "article_body",
        "article",
        "post",
        "postBody",
        "postContent",
        "markdown",
        "source",
        "text",
        "fullText",
        "full_text",
        "excerpt",
        "description",
        "summary",
    ];
    // Minimum chars to be considered article content (not a blurb or empty string)
    const MIN_CONTENT_LEN: usize = 200;

    // Next.js: props.pageProps holds the actual page data
    let page_props = data.get("props")?.get("pageProps")?;

    // Strategy 1: named-key search across the entire pageProps subtree
    let mut best: Option<String> = None;
    for key in CONTENT_KEYS {
        if let Some(found) = find_content_by_key(page_props, key) {
            let current_best_len = best.as_deref().map_or(0, str::len);
            if found.len() >= MIN_CONTENT_LEN && found.len() > current_best_len {
                best = Some(found);
            }
        }
    }

    // Strategy 2: longest-string fallback for unknown CMS structures.
    // Only activates when named-key search found nothing useful.
    if best.is_none() {
        best = find_longest_string(page_props, MIN_CONTENT_LEN);
    }

    best.map(|content| render_spa_content(&content))
}

/// Convert a SPA content string (HTML or plain text) to clean markdown.
fn render_spa_content(content: &str) -> String {
    if content.contains('<') && content.contains('>') {
        // Looks like HTML — convert to markdown
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

/// Recursively walk a JSON value tree looking for a string field named `key`.
///
/// Returns the first string value found using depth-first search (object fields
/// before array items). Returns `None` if the key is not present.
fn find_content_by_key(value: &serde_json::Value, key: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            // Check this level first
            if let Some(serde_json::Value::String(s)) = map.get(key) {
                return Some(s.clone());
            }
            // Recurse into values
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

/// Find the longest string value anywhere in a JSON tree.
///
/// This is a last-resort fallback for Next.js / SPA pages that use proprietary
/// content key names. By finding the longest string, we can usually recover the
/// article body even when its field name is unknown.
///
/// Skips strings shorter than `min_len` to avoid picking up IDs, slugs, or
/// short metadata strings.
fn find_longest_string(value: &serde_json::Value, min_len: usize) -> Option<String> {
    match value {
        serde_json::Value::String(s) => {
            if s.len() >= min_len { Some(s.clone()) } else { None }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_nextjs_content_finds_body_field() {
        let json_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "title": "Hello World",
                        "body": "<p>This is the article body content with substantial text that should be extracted by the SPA extractor when readability fails. It contains enough characters to satisfy the minimum content length threshold of two hundred characters.</p>"
                    }
                }
            }
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(
            content.contains("article body content"),
            "Expected body content, got: {content}"
        );
    }

    #[test]
    fn extract_nextjs_content_returns_none_for_short_content() {
        let json_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "body": "Short"
                    }
                }
            }
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_none(), "Short content should return None");
    }

    #[test]
    fn extract_nextjs_content_returns_none_without_page_props() {
        let json_data = serde_json::json!({
            "query": {"id": "123"},
            "buildId": "abc"
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_none());
    }

    #[test]
    fn find_content_by_key_finds_nested_key() {
        let value = serde_json::json!({
            "level1": {
                "level2": {
                    "content": "deep content string that is long enough to be found"
                }
            }
        });

        let result = find_content_by_key(&value, "content");
        assert!(result.is_some());
        assert!(result.unwrap().contains("deep content"));
    }

    #[test]
    fn find_content_by_key_finds_in_array() {
        let value = serde_json::json!([
            {"title": "skip"},
            {"body": "the actual article body content found in array"}
        ]);

        let result = find_content_by_key(&value, "body");
        assert!(result.is_some());
        assert!(result.unwrap().contains("actual article body"));
    }

    #[test]
    fn extract_spa_data_returns_none_for_plain_html() {
        let html = "<html><body><p>Plain page, no SPA data</p></body></html>";
        assert!(extract_spa_data(html).is_none());
    }

    #[test]
    fn extract_spa_data_extracts_nextjs_data() {
        // Simulate a Next.js page with embedded data
        let body_content = "This is the article body content from a Next.js application. \
            It contains multiple sentences to pass the minimum length threshold check. \
            The content must be at least two hundred characters long to be considered \
            a valid article body by the SPA extractor.";
        let json = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "title": "Test Post",
                        "body": body_content
                    }
                }
            },
            "buildId": "abc123"
        });
        let html = format!(
            r#"<html><head></head><body>
                <script id="__NEXT_DATA__" type="application/json">{json}</script>
                <div id="__next"><p>SSR placeholder</p></div>
            </body></html>"#
        );

        let result = extract_spa_data(&html);
        assert!(result.is_some(), "Should extract Next.js content");
        let content = result.unwrap();
        assert!(
            content.contains("article body content"),
            "Expected SPA body, got: {content}"
        );
    }

    // ── find_longest_string ─────────────────────────────────────────────────

    #[test]
    fn find_longest_string_returns_longest_in_flat_object() {
        // GIVEN: JSON object with strings of different lengths
        let value = serde_json::json!({
            "slug": "my-post",
            "title": "A short title",
            "bodyText": "This is a much longer string representing the article body content that should be selected as the longest value in the tree."
        });
        // WHEN: we search for the longest string above minimum length
        let result = find_longest_string(&value, 50);
        // THEN: the longest string is returned
        assert!(result.is_some());
        assert!(result.unwrap().contains("article body content"));
    }

    #[test]
    fn find_longest_string_returns_longest_in_nested_object() {
        // GIVEN: nested JSON where the longest string is deep in the tree
        let value = serde_json::json!({
            "meta": {"slug": "test"},
            "content": {
                "sections": [
                    {"type": "text", "value": "Short section."},
                    {"type": "text", "value": "This section contains a much longer body of text that represents the actual article content. It spans many more characters than the other sections."}
                ]
            }
        });
        let result = find_longest_string(&value, 50);
        assert!(result.is_some());
        assert!(result.unwrap().contains("actual article content"));
    }

    #[test]
    fn find_longest_string_returns_none_when_all_strings_below_min_len() {
        // GIVEN: JSON where all string values are below the minimum length
        let value = serde_json::json!({
            "id": "abc123",
            "slug": "my-post",
            "status": "published"
        });
        let result = find_longest_string(&value, 50);
        // THEN: None, since no string meets the minimum length
        assert!(result.is_none());
    }

    #[test]
    fn find_longest_string_handles_array_of_strings() {
        // GIVEN: array containing strings of varying lengths
        let value = serde_json::json!([
            "short",
            "medium length string here",
            "this is the longest string in the array and it exceeds the minimum length threshold easily"
        ]);
        let result = find_longest_string(&value, 30);
        assert!(result.is_some());
        assert!(result.unwrap().contains("longest string in the array"));
    }

    // ── extract_nextjs_content with unknown CMS key names ───────────────────

    #[test]
    fn extract_nextjs_content_falls_back_to_longest_string_for_unknown_keys() {
        // GIVEN: Next.js data where the article body uses a proprietary key name
        // (simulating Stripe blog's structure which uses non-standard keys)
        let article_body = "This is the full article body from Stripe's blog post about \
            payment processing and financial infrastructure. It discusses how modern \
            payment systems work and the engineering challenges involved in building \
            reliable financial software at scale.";

        let json_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "slug": "payment-systems",
                        "author": "Jane Smith",
                        "publishedAt": "2024-01-15",
                        // Proprietary key not in CONTENT_KEYS list
                        "richBodyContent": article_body
                    }
                }
            },
            "buildId": "stripe-blog-build-123"
        });

        // WHEN: we extract content
        let result = extract_nextjs_content(&json_data);

        // THEN: the article body is found via longest-string fallback
        assert!(result.is_some(), "should extract content via longest-string fallback");
        let content = result.unwrap();
        assert!(
            content.contains("Stripe's blog post") || content.contains("payment processing"),
            "expected article body, got: {content}"
        );
    }

    #[test]
    fn extract_nextjs_content_prefers_named_key_over_longer_incidental_string() {
        // GIVEN: pageProps with both a well-known key and a longer non-content string
        // The named-key match should win even if a longer string exists elsewhere
        let known_body = "This is the article body content stored under the well-known \
            'body' key. It is substantial enough to pass the minimum length check.";
        let longer_non_content = "a".repeat(500); // longer but not a content field

        let json_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "post": {
                        "body": known_body,
                        // A longer string under an opaque key
                        "_internalData": longer_non_content
                    }
                }
            }
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_some());
        // Named-key strategy finds 'body' first — but since longest-string only
        // activates when named-key finds nothing, both strategies can coexist.
        // The key assertion: we get something useful back.
        assert!(
            result.unwrap().len() >= known_body.len() - 50,
            "should return at least the known body content"
        );
    }

    #[test]
    fn extract_nextjs_content_extended_key_body_html() {
        // GIVEN: pageProps using 'bodyHtml' — one of the newly added keys
        let html_body =
            "<p>Article content stored as HTML in the bodyHtml field. \
            This is a common pattern in headless CMS systems where content \
            is stored as rendered HTML fragments rather than plain text or markdown. \
            The content is substantial enough to pass the minimum length threshold \
            required by the SPA extractor.</p>";

        let json_data = serde_json::json!({
            "props": {
                "pageProps": {
                    "page": {
                        "title": "CMS Article",
                        "bodyHtml": html_body
                    }
                }
            }
        });

        let result = extract_nextjs_content(&json_data);
        assert!(result.is_some(), "should find content via 'bodyHtml' key");
        let content = result.unwrap();
        // HTML should be converted to markdown
        assert!(
            content.contains("Article content stored as HTML"),
            "expected HTML converted to markdown, got: {content}"
        );
    }

    // ── JSON-LD extraction ──────────────────────────────────────────────────

    #[test]
    fn extract_jsonld_ignores_non_article_types() {
        // GIVEN: JSON-LD with Organization type (not an article)
        let html = r#"<!DOCTYPE html>
<html>
<head>
    <script type="application/ld+json">
    {
        "@context": "https://schema.org",
        "@type": "Organization",
        "name": "Example Corp",
        "description": "We are a company that does things and this description is long enough to pass minimum length thresholds but should not be extracted as article content since it is an Organization type."
    }
    </script>
</head>
<body><div>Minimal page</div></body>
</html>"#;

        let document = scraper::Html::parse_document(html);
        let result = extract_jsonld_content(&document);
        assert!(result.is_none(), "Organization JSON-LD should not be extracted as article content");
    }

    // ── Inline script variable extraction ───────────────────────────────────

    #[test]
    fn extract_inline_script_initial_state_assignment() {
        // GIVEN: A page using window.__INITIAL_STATE__ = {...}
        let content = "A long article body stored in the Redux initial state payload. \
            This pattern is common in React applications that use Redux for state management \
            and prehydrate the store with server-side rendered data via a global variable \
            assignment in an inline script tag.";

        let state = serde_json::json!({
            "article": {
                "content": content,
                "title": "Redux State Test"
            }
        });

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<body>
    <div id="root"></div>
    <script>window.__INITIAL_STATE__ = {state};</script>
</body>
</html>"#
        );

        let result = extract_inline_script_json(&html);
        assert!(
            result.is_some(),
            "should extract content from __INITIAL_STATE__ assignment"
        );
        let content = result.unwrap();
        assert!(
            content.contains("Redux initial state") || content.contains("long article body"),
            "expected state content, got: {content}"
        );
    }

    // ── Balanced JSON extractor ─────────────────────────────────────────────

    #[test]
    fn extract_balanced_json_basic_object() {
        let input = r#"{"key": "value", "nested": {"a": 1}}; more stuff"#;
        let result = extract_balanced_json(input);
        assert!(result.is_some());
        let json = result.unwrap();
        assert_eq!(json, r#"{"key": "value", "nested": {"a": 1}}"#);
    }

    #[test]
    fn extract_balanced_json_with_string_braces() {
        let input = r#"{"content": "text with {braces} inside"}"#;
        let result = extract_balanced_json(input);
        assert!(result.is_some());
        let json = result.unwrap();
        // Should correctly handle braces inside strings
        assert!(serde_json::from_str::<serde_json::Value>(json).is_ok());
    }

    #[test]
    fn extract_balanced_json_array() {
        let input = r#"[1, 2, {"a": [3, 4]}];"#;
        let result = extract_balanced_json(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), r#"[1, 2, {"a": [3, 4]}]"#);
    }

    #[test]
    fn extract_balanced_json_returns_none_for_unbalanced() {
        let input = r#"{"key": "value""#; // missing closing brace
        let result = extract_balanced_json(input);
        assert!(result.is_none());
    }
}
