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
pub fn extract_spa_data(html: &str) -> Option<String> {
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
pub fn extract_jsonld_content(document: &scraper::Html) -> Option<String> {
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
pub fn extract_inline_script_json(html: &str) -> Option<String> {
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
pub fn extract_nextjs_content(data: &serde_json::Value) -> Option<String> {
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
pub fn find_content_by_key(value: &serde_json::Value, key: &str) -> Option<String> {
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
pub fn find_longest_string(value: &serde_json::Value, min_len: usize) -> Option<String> {
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

