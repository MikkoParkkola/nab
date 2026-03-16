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
//! 5. Hidden `<code>` elements with JSON (LinkedIn-style SPA hydration)
//! 6. Pre-fetched API response envelopes (`{status: 200, body: "{...}"}`) in any JSON
//!
//! # Next.js MDX Content Chunk Recovery
//!
//! Some Next.js sites (e.g., blogs using MDX) embed only metadata in
//! `__NEXT_DATA__` and load article content lazily from webpack chunks.
//! [`discover_nextjs_content_chunks`] parses the webpack runtime to find
//! content chunk URLs, and [`extract_jsx_text_content`] extracts readable
//! text from the compiled JSX.  The async fetch layer can use these to
//! make secondary requests when thin content is detected.

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

    // Try hidden <code> elements with JSON (LinkedIn-style SPA hydration).
    // Many SPA frameworks embed server-fetched data in hidden <code> or <script>
    // elements for client-side hydration. This catches the pattern generically.
    if let Some(content) = extract_hidden_code_json(&document) {
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
fn extract_hidden_code_json(document: &scraper::Html) -> Option<String> {
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

        // Collect text from the JSON tree and any nested API response envelopes
        collect_text_from_json(&value, &mut all_text);
        unwrap_api_response_bodies(&value, &mut all_text);
    }

    if all_text.is_empty() {
        return None;
    }

    // Return the longest text found (most likely to be the main content)
    all_text
        .into_iter()
        .filter(|s| s.len() >= MIN_CONTENT_LEN)
        .max_by_key(std::string::String::len)
        .map(|content| render_spa_content(&content))
}

/// Strip `<!--` prefix and `-->` suffix from an HTML comment wrapper.
///
/// `LinkedIn` wraps `<code>` JSON in HTML comments: `<!--{...}-->`.
/// Returns the inner content unchanged if no wrapper is present.
fn strip_html_comment_wrapper(s: &str) -> &str {
    let s = s.strip_prefix("<!--").unwrap_or(s);
    let s = s.strip_suffix("-->").unwrap_or(s);
    s.trim()
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
            // Check if this object is an API response envelope
            if let (Some(status), Some(body_str)) = (
                map.get("status").and_then(serde_json::Value::as_u64),
                map.get("body").and_then(|v| v.as_str()),
            ) && status == 200
                && !body_str.is_empty()
                && let Ok(body_json) = serde_json::from_str::<serde_json::Value>(body_str)
            {
                collect_text_from_json(&body_json, texts);
            }
            // Recurse into all values to find nested envelopes
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

/// Recursively collect substantial text strings from a JSON value tree.
///
/// Walks the entire JSON structure and collects string values that look like
/// meaningful text content (minimum length, not URLs/IDs/hashes).
fn collect_text_from_json(value: &serde_json::Value, texts: &mut Vec<String>) {
    const MIN_TEXT_LEN: usize = 50;

    match value {
        serde_json::Value::String(s) => {
            // Skip short strings, URLs, hashes, and IDs
            if s.len() >= MIN_TEXT_LEN
                && !s.starts_with("http")
                && !s.starts_with("urn:")
                && !s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            {
                texts.push(s.clone());
            }
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

/// Discover Next.js content chunk URLs from the HTML page.
///
/// When a Next.js site uses MDX or lazy-loaded content, `__NEXT_DATA__` contains
/// only metadata (title, description, slug) but no article body.  The actual
/// content is compiled into a webpack chunk that is loaded dynamically.
///
/// This function:
/// 1. Confirms `__NEXT_DATA__` exists but has no substantial content
/// 2. Parses the `buildId` from `__NEXT_DATA__`
/// 3. Finds the webpack runtime `<script>` to extract the chunk-ID-to-hash map
/// 4. Finds the page component `<script>` to extract lazy chunk references
/// 5. Constructs absolute URLs for the content chunks
///
/// Returns absolute chunk URLs that should be fetched and passed through
/// [`extract_jsx_text_content`] to recover the article body.
///
/// # Example
///
/// ```text
/// let chunks = discover_nextjs_content_chunks(html, "https://example.com/blog/post");
/// // chunks = ["https://example.com/_next/static/chunks/264.35c2eaf588f3e425.js"]
/// ```
pub fn discover_nextjs_content_chunks(html: &str, page_url: &str) -> Vec<String> {
    let document = scraper::Html::parse_document(html);

    // Step 1: Check __NEXT_DATA__ exists but lacks content
    let next_data = {
        let sel = scraper::Selector::parse("script#__NEXT_DATA__").ok();
        sel.and_then(|s| document.select(&s).next())
            .and_then(|script| {
                let json_text = script.text().collect::<String>();
                serde_json::from_str::<serde_json::Value>(&json_text).ok()
            })
    };

    let Some(next_data) = next_data else {
        return Vec::new(); // Not a Next.js page
    };

    // If __NEXT_DATA__ already has substantial content, no need for chunks
    if let Some(page_props) = next_data.get("props").and_then(|p| p.get("pageProps")) {
        if find_longest_string(page_props, 200).is_some() {
            return Vec::new();
        }
    }

    // Step 2: Extract buildId for constructing chunk paths
    let build_id = next_data
        .get("buildId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    // Step 3: Find the origin for constructing absolute URLs
    let origin = url::Url::parse(page_url)
        .ok()
        .map(|u| u.origin().unicode_serialization())
        .unwrap_or_default();

    // Step 4: Collect all script src URLs from the page
    let script_sel = scraper::Selector::parse("script[src]").ok();
    let script_srcs: Vec<String> = script_sel
        .iter()
        .flat_map(|sel| document.select(sel))
        .filter_map(|el| el.value().attr("src").map(String::from))
        .collect();

    // Step 5: Find the webpack runtime script (contains chunk hash map)
    // Pattern: /_next/static/chunks/webpack-{hash}.js[?query]
    // Note: Vercel adds query params like ?dpl=... so we check for ".js" anywhere
    let webpack_src = script_srcs
        .iter()
        .find(|s| s.contains("webpack-") && s.contains(".js"));

    // Step 6: Find the page-specific script (contains lazy chunk references)
    // Pattern: /_next/static/chunks/pages/blog/[slug]-{hash}.js[?query]
    // We need the actual page route component, not _app or _error.
    // Strategy: extract the current page path from the URL and match it,
    // or fall back to the most specific /pages/ script (not _app, _error, index).
    let page_path = url::Url::parse(page_url)
        .ok()
        .map(|u| u.path().to_string())
        .unwrap_or_default();

    let page_src = script_srcs
        .iter()
        .find(|s| {
            if !s.contains("/pages/") || !s.contains(".js") {
                return false;
            }
            // Skip framework scripts (_app, _error)
            if s.contains("/_app") || s.contains("/_error") || s.contains("/_document") {
                return false;
            }
            // Prefer scripts matching the current page path
            // e.g., /blog/can-llms-be-computers → /pages/blog/
            let path_segments: Vec<&str> = page_path.split('/').filter(|s| !s.is_empty()).collect();
            if let Some(first_segment) = path_segments.first() {
                s.contains(&format!("/pages/{first_segment}/"))
                    || s.contains(&format!("/pages/{first_segment}-"))
            } else {
                // Root page — match /pages/index
                s.contains("/pages/index")
            }
        })
        // Fallback: any /pages/ script that isn't _app/_error
        .or_else(|| {
            script_srcs.iter().find(|s| {
                s.contains("/pages/") && s.contains(".js")
                    && !s.contains("/_app") && !s.contains("/_error") && !s.contains("/_document")
            })
        });

    // We need both the webpack runtime (for hash map) and the page script
    // (for chunk IDs). Return the info needed for the caller to fetch them.
    // For now, we embed the URLs so the async layer can fetch and parse.
    let mut result = Vec::new();

    if let (Some(webpack), Some(page)) = (webpack_src, page_src) {
        // Resolve relative URLs
        let webpack_url = resolve_script_url(webpack, &origin);
        let page_url_resolved = resolve_script_url(page, &origin);
        result.push(webpack_url);
        result.push(page_url_resolved);
    }

    // Also include the build ID for _next/data endpoint fallback
    if !build_id.is_empty() && !origin.is_empty() {
        // This is stored as the third element for the caller's convenience
        result.push(build_id.to_string());
    }

    result
}

/// Parse the webpack runtime JS to extract the chunk-ID-to-filename map,
/// then parse the page component JS to find lazy content chunk IDs,
/// and return the resolved content chunk URLs.
///
/// `webpack_js` is the content of the webpack runtime script.
/// `page_js` is the content of the page component script (e.g., `pages/blog/[slug]-*.js`).
/// `origin` is the site origin (e.g., `https://example.com`).
///
/// Returns absolute URLs to content chunks that should be fetched.
/// When all content chunks are desired (no slug filter), returns all of them.
pub fn resolve_content_chunk_urls(
    webpack_js: &str,
    page_js: &str,
    origin: &str,
) -> Vec<String> {
    resolve_content_chunk_urls_for_slug(webpack_js, page_js, origin, None)
}

/// Like [`resolve_content_chunk_urls`] but filters to a specific page slug.
///
/// When `slug` is `Some("can-llms-be-computers")`, only the chunk for
/// `./can-llms-be-computers.mdx` (or `.md`) is returned.
pub fn resolve_content_chunk_urls_for_slug(
    webpack_js: &str,
    page_js: &str,
    origin: &str,
    slug: Option<&str>,
) -> Vec<String> {
    let chunk_hashes = parse_webpack_chunk_hashes(webpack_js);
    if chunk_hashes.is_empty() {
        return Vec::new();
    }

    let lazy_chunk_ids = if let Some(slug) = slug {
        parse_lazy_chunk_ids_for_slug(page_js, slug)
    } else {
        parse_lazy_chunk_ids(page_js)
    };
    if lazy_chunk_ids.is_empty() {
        return Vec::new();
    }

    lazy_chunk_ids
        .into_iter()
        .filter_map(|id| {
            let id_str = id.to_string();
            chunk_hashes.get(&id_str).map(|hash| {
                format!("{origin}/_next/static/chunks/{id}.{hash}.js")
            })
        })
        .collect()
}

/// Parse chunk hash map from webpack runtime JS.
///
/// Looks for the pattern:
/// ```text
/// r.u=e=>"static/chunks/"+e+"."+({11:"hash1",264:"hash2",365:"hash3"})[e]+".js"
/// ```
fn parse_webpack_chunk_hashes(js: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();

    // Find the chunk hash mapping object
    // Pattern: ({digits:"hex",...})[e]+".js"
    // We look for the characteristic "static/chunks/" prefix
    let Some(chunks_idx) = js.find("static/chunks/") else {
        return map;
    };

    // Search forward for the hash map object: ({...})
    let search_start = chunks_idx;
    let remaining = &js[search_start..];

    // Find "({"  which starts the hash map
    let Some(map_start) = remaining.find("({") else {
        return map;
    };
    let obj_start = search_start + map_start + 1; // skip the '('

    // Extract the balanced JSON object
    let Some(json_str) = extract_balanced_json(&js[obj_start..]) else {
        return map;
    };

    // The chunk hash object uses unquoted numeric keys (valid JS, invalid JSON):
    //   {11:"hash1",264:"hash2"}
    // Convert to valid JSON by quoting unquoted numeric keys:
    //   {"11":"hash1","264":"hash2"}
    let json_fixed = quote_numeric_keys(json_str);

    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&json_fixed) {
        if let Some(obj_map) = obj.as_object() {
            for (k, v) in obj_map {
                if let Some(hash) = v.as_str() {
                    map.insert(k.clone(), hash.to_string());
                }
            }
        }
    }

    map
}

/// Quote unquoted numeric object keys in a JS object literal to make it valid JSON.
///
/// Converts `{11:"abc",264:"def"}` to `{"11":"abc","264":"def"}`.
/// Preserves already-quoted keys and string values.
fn quote_numeric_keys(js_obj: &str) -> String {
    let mut result = String::with_capacity(js_obj.len() + 20);
    let mut chars = js_obj.chars().peekable();
    let mut in_string = false;
    let mut escape_next = false;
    // Track whether we're at a key position (after { or ,)
    let mut expect_key = false;

    while let Some(c) = chars.next() {
        if escape_next {
            result.push(c);
            escape_next = false;
            continue;
        }

        if c == '\\' && in_string {
            result.push(c);
            escape_next = true;
            continue;
        }

        if c == '"' {
            in_string = !in_string;
            result.push(c);
            continue;
        }

        if in_string {
            result.push(c);
            continue;
        }

        match c {
            '{' | ',' => {
                result.push(c);
                expect_key = true;
            }
            c if expect_key && c.is_ascii_digit() => {
                // Unquoted numeric key — collect all digits and quote them
                let mut key = String::new();
                key.push(c);
                while chars.peek().is_some_and(|&ch| ch.is_ascii_digit()) {
                    key.push(chars.next().unwrap());
                }
                result.push('"');
                result.push_str(&key);
                result.push('"');
                expect_key = false;
            }
            _ => {
                if !c.is_whitespace() {
                    expect_key = false;
                }
                result.push(c);
            }
        }
    }

    result
}

/// Parse lazy chunk IDs for a specific slug from a Next.js page component JS.
///
/// Looks for `"./slug.mdx":[moduleId,chunkId]` or `"./slug.md":[moduleId,chunkId]`.
fn parse_lazy_chunk_ids_for_slug(js: &str, slug: &str) -> Vec<u64> {
    let mut ids = Vec::new();

    // Try .mdx extension first, then .md
    for ext in &[".mdx", ".md"] {
        let pattern = format!("./{slug}{ext}\":[");
        if let Some(idx) = js.find(&pattern) {
            let start = idx + pattern.len();
            if let Some(bracket_end) = js[start..].find(']') {
                let nums_str = &js[start..start + bracket_end];
                let parts: Vec<&str> = nums_str.split(',').collect();
                if parts.len() == 2 {
                    if let Ok(chunk_id) = parts[1].trim().parse::<u64>() {
                        ids.push(chunk_id);
                    }
                }
            }
        }
    }

    ids
}

/// Parse lazy chunk IDs from a Next.js page component JS.
///
/// Looks for patterns like:
/// ```text
/// {"./post-slug.mdx":[5264,264],"./other-post.mdx":[2011,11]}
/// ```
///
/// Returns the chunk IDs (the second number in each pair — the webpack chunk ID,
/// not the module ID which is the first number).
fn parse_lazy_chunk_ids(js: &str) -> Vec<u64> {
    let mut ids = Vec::new();

    // Find ".mdx" or ".md" references which indicate content files
    // The pattern is "filename.mdx":[moduleId,chunkId]
    let mut search_from = 0;
    while let Some(mdx_idx) = js[search_from..].find(".mdx\":[") {
        let abs_idx = search_from + mdx_idx + 7; // skip past .mdx":[
        if let Some(bracket_end) = js[abs_idx..].find(']') {
            let nums_str = &js[abs_idx..abs_idx + bracket_end];
            let parts: Vec<&str> = nums_str.split(',').collect();
            if parts.len() == 2 {
                if let Ok(chunk_id) = parts[1].trim().parse::<u64>() {
                    ids.push(chunk_id);
                }
            }
        }
        search_from = abs_idx;
    }

    // Also try .md" pattern
    search_from = 0;
    while let Some(md_idx) = js[search_from..].find(".md\":[") {
        let abs_idx = search_from + md_idx + 6; // skip past .md":[
        if let Some(bracket_end) = js[abs_idx..].find(']') {
            let nums_str = &js[abs_idx..abs_idx + bracket_end];
            let parts: Vec<&str> = nums_str.split(',').collect();
            if parts.len() == 2 {
                if let Ok(chunk_id) = parts[1].trim().parse::<u64>() {
                    ids.push(chunk_id);
                }
            }
        }
        search_from = abs_idx;
    }

    ids.dedup();
    ids
}

/// Extract readable text content from a compiled JSX/MDX webpack chunk.
///
/// Next.js MDX blogs compile article content into webpack chunks containing
/// React JSX calls like:
/// ```text
/// (0,t.jsx)(s.p,{children:"Article text here."})
/// (0,t.jsxs)(s.h2,{id:"section",children:"Section Title"})
/// ```
///
/// This function extracts the `children:"..."` string literals, filters out
/// noise (short strings, URLs, CSS classes), and reassembles them into a
/// readable markdown document.
///
/// # Returns
///
/// `Some(markdown)` if substantial content was extracted (>200 chars),
/// `None` otherwise.
pub fn extract_jsx_text_content(js_source: &str) -> Option<String> {
    const MIN_CONTENT_LEN: usize = 200;

    let mut paragraphs: Vec<String> = Vec::new();

    // Strategy: scan for children:" patterns and extract string values.
    // JSX children can be:
    //   children:"text"               — simple text node
    //   id:"section-id"               — heading anchor (captured for context)
    //
    // We also detect element types from the JSX calls:
    //   s.h2  s.h3  → headings
    //   s.p          → paragraphs
    //   s.li         → list items
    //   s.blockquote → blockquotes
    //   s.code / s.pre → code blocks

    let mut search_from = 0;
    while search_from < js_source.len() {
        // Ensure search_from is on a char boundary (multi-byte safety)
        while search_from < js_source.len() && !js_source.is_char_boundary(search_from) {
            search_from += 1;
        }
        if search_from >= js_source.len() {
            break;
        }

        // Find next children:" pattern
        let Some(children_idx) = js_source[search_from..].find("children:\"") else {
            break;
        };
        let abs_idx = search_from + children_idx + 10; // skip past children:"

        // Ensure abs_idx is on a char boundary
        if abs_idx >= js_source.len() || !js_source.is_char_boundary(abs_idx) {
            search_from = abs_idx.saturating_add(1);
            continue;
        }

        // Extract the string value (handle escaped quotes)
        if let Some(text) = extract_js_string_value(&js_source[abs_idx..]) {
            // Filter noise: skip short strings, CSS classes, element names, URLs
            if text.len() >= 15
                && !text.starts_with("http")
                && !text.starts_with("data:")
                && !text.starts_with("text-")
                && !text.contains("className")
                && !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                // Check context: look backwards for element type hints
                // Floor to a valid char boundary to avoid slicing inside multi-byte chars
                let mut context_start = abs_idx.saturating_sub(200);
                while context_start < abs_idx && !js_source.is_char_boundary(context_start) {
                    context_start += 1;
                }
                let context = &js_source[context_start..abs_idx];

                if is_heading_context(context) {
                    // Format as markdown heading
                    let level = detect_heading_level(context);
                    let prefix = "#".repeat(level);
                    paragraphs.push(format!("{prefix} {text}"));
                } else if is_list_context(context) {
                    paragraphs.push(format!("- {text}"));
                } else if is_blockquote_context(context) {
                    paragraphs.push(format!("> {text}"));
                } else if is_code_context(context) {
                    // Skip inline code that's just element names
                    if text.len() > 30 {
                        paragraphs.push(format!("```\n{text}\n```"));
                    } else {
                        paragraphs.push(format!("`{text}`"));
                    }
                } else {
                    paragraphs.push(text);
                }
            }

            search_from = abs_idx + 1;
        } else {
            search_from = abs_idx + 1;
        }
    }

    if paragraphs.is_empty() {
        return None;
    }

    let content = paragraphs.join("\n\n");
    if content.len() >= MIN_CONTENT_LEN {
        Some(content)
    } else {
        None
    }
}

/// Extract a JavaScript string value starting after the opening quote.
///
/// Handles escape sequences: `\"`, `\\`, `\n`, `\t`, `\uXXXX`.
/// Returns the unescaped string content up to the closing unescaped `"`.
fn extract_js_string_value(s: &str) -> Option<String> {
    let mut result = String::new();
    let mut chars = s.chars();
    let mut escape_next = false;

    while let Some(c) = chars.next() {
        if escape_next {
            match c {
                '"' => result.push('"'),
                '\\' => result.push('\\'),
                'n' => result.push('\n'),
                't' => result.push('\t'),
                'r' => result.push('\r'),
                'u' => {
                    // Unicode escape: \uXXXX
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            result.push(ch);
                        }
                    }
                }
                _ => {
                    result.push('\\');
                    result.push(c);
                }
            }
            escape_next = false;
        } else {
            match c {
                '"' => return Some(result), // End of string
                '\\' => escape_next = true,
                _ => result.push(c),
            }
        }
    }

    None // Unterminated string
}

/// Check if the JSX context before a `children:` indicates a heading element.
fn is_heading_context(context: &str) -> bool {
    // Look for s.h1, s.h2, s.h3, s.h4, s.h5, s.h6 in the nearby context
    let last_200 = if context.len() > 100 {
        &context[context.len() - 100..]
    } else {
        context
    };
    last_200.contains("s.h1")
        || last_200.contains("s.h2")
        || last_200.contains("s.h3")
        || last_200.contains("s.h4")
        || last_200.contains("s.h5")
        || last_200.contains("s.h6")
}

/// Detect the heading level from the JSX context.
fn detect_heading_level(context: &str) -> usize {
    let last_100 = if context.len() > 100 {
        &context[context.len() - 100..]
    } else {
        context
    };
    if last_100.contains("s.h1") {
        1
    } else if last_100.contains("s.h2") {
        2
    } else if last_100.contains("s.h3") {
        3
    } else if last_100.contains("s.h4") {
        4
    } else if last_100.contains("s.h5") {
        5
    } else if last_100.contains("s.h6") {
        6
    } else {
        2 // Default
    }
}

/// Check if the JSX context indicates a list item element.
fn is_list_context(context: &str) -> bool {
    let last_100 = if context.len() > 100 {
        &context[context.len() - 100..]
    } else {
        context
    };
    last_100.contains("s.li")
}

/// Check if the JSX context indicates a blockquote element.
fn is_blockquote_context(context: &str) -> bool {
    let last_100 = if context.len() > 100 {
        &context[context.len() - 100..]
    } else {
        context
    };
    last_100.contains("s.blockquote")
}

/// Check if the JSX context indicates a code element.
fn is_code_context(context: &str) -> bool {
    let last_100 = if context.len() > 100 {
        &context[context.len() - 100..]
    } else {
        context
    };
    last_100.contains("s.pre") || last_100.contains("s.code")
}

/// Resolve a script src attribute to an absolute URL.
///
/// Strips query parameters (e.g., `?dpl=...`) since they are not needed
/// for fetching the static chunk file and can cause issues.
fn resolve_script_url(src: &str, origin: &str) -> String {
    // Strip query parameters
    let path = src.split('?').next().unwrap_or(src);

    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else if path.starts_with('/') {
        format!("{origin}{path}")
    } else {
        format!("{origin}/{path}")
    }
}

/// Check if the page has `__NEXT_DATA__` with only metadata (no article body).
///
/// Returns `true` if this is a Next.js page where `__NEXT_DATA__` exists but
/// `pageProps` contains no string longer than `min_content_len`.
/// This is a fast check used to decide whether content chunk recovery is needed.
pub fn is_nextjs_metadata_only(html: &str) -> bool {
    let document = scraper::Html::parse_document(html);

    let sel = scraper::Selector::parse("script#__NEXT_DATA__").ok();
    let next_data = sel
        .and_then(|s| document.select(&s).next())
        .and_then(|script| {
            let json_text = script.text().collect::<String>();
            serde_json::from_str::<serde_json::Value>(&json_text).ok()
        });

    let Some(next_data) = next_data else {
        return false;
    };

    // Check if pageProps has no substantial content
    if let Some(page_props) = next_data.get("props").and_then(|p| p.get("pageProps")) {
        find_longest_string(page_props, 200).is_none()
    } else {
        true // No pageProps at all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── Next.js content chunk discovery ─────────────────────────────────

    #[test]
    fn parse_webpack_chunk_hashes_extracts_map() {
        let webpack_js = r#"r.u=e=>"static/chunks/"+e+"."+({11:"4c5bd1c96d90c00e",264:"35c2eaf588f3e425",365:"cc49d04e8ed0ee46"})[e]+".js""#;
        let hashes = parse_webpack_chunk_hashes(webpack_js);
        assert_eq!(hashes.get("264"), Some(&"35c2eaf588f3e425".to_string()));
        assert_eq!(hashes.get("11"), Some(&"4c5bd1c96d90c00e".to_string()));
        assert_eq!(hashes.len(), 3);
    }

    #[test]
    fn parse_webpack_chunk_hashes_returns_empty_for_no_match() {
        let js = r#"console.log("no webpack here")"#;
        assert!(parse_webpack_chunk_hashes(js).is_empty());
    }

    #[test]
    fn parse_lazy_chunk_ids_extracts_mdx_references() {
        let page_js = r#"var s={"./beyond-the-sandbox.mdx":[2011,11],"./can-llms-be-computers.mdx":[5264,264]};"#;
        let ids = parse_lazy_chunk_ids(page_js);
        assert!(ids.contains(&11));
        assert!(ids.contains(&264));
    }

    #[test]
    fn parse_lazy_chunk_ids_returns_empty_for_no_mdx() {
        let js = r#"var x = {"key": "value"};"#;
        assert!(parse_lazy_chunk_ids(js).is_empty());
    }

    #[test]
    fn resolve_content_chunk_urls_produces_full_urls() {
        let webpack_js = r#"r.u=e=>"static/chunks/"+e+"."+({264:"abc123"})[e]+".js""#;
        let page_js = r#"{"./post.mdx":[5264,264]}"#;
        let urls = resolve_content_chunk_urls(webpack_js, page_js, "https://example.com");
        assert_eq!(urls.len(), 1);
        assert_eq!(
            urls[0],
            "https://example.com/_next/static/chunks/264.abc123.js"
        );
    }

    #[test]
    fn resolve_content_chunk_urls_returns_empty_when_no_hash() {
        let webpack_js = r#"r.u=e=>"static/chunks/"+e+"."+({999:"abc123"})[e]+".js""#;
        let page_js = r#"{"./post.mdx":[5264,264]}"#;
        let urls = resolve_content_chunk_urls(webpack_js, page_js, "https://example.com");
        assert!(urls.is_empty()); // chunk 264 not in hash map
    }

    // ── JSX text extraction ─────────────────────────────────────────────

    #[test]
    fn extract_jsx_text_content_extracts_paragraphs() {
        let jsx = r#"(0,t.jsx)(s.p,{children:"This is a paragraph with enough text to demonstrate that the JSX extraction works correctly for typical blog post content structures in compiled Next.js MDX pages. It needs to exceed the two hundred character minimum threshold set by the extraction function."}),(0,t.jsx)(s.p,{children:"Second paragraph with additional content that helps establish this is a real article and not just metadata or navigation text from the page structure."})"#;
        let result = extract_jsx_text_content(jsx);
        assert!(result.is_some(), "Should extract content from JSX");
        let content = result.unwrap();
        assert!(content.contains("paragraph with enough text"));
        assert!(content.contains("Second paragraph"));
    }

    #[test]
    fn extract_jsx_text_content_detects_headings() {
        let jsx = r#"(0,t.jsx)(s.h2,{id:"tldr",children:"TL;DR - This Heading Is Long Enough"}),(0,t.jsx)(s.p,{children:"This is article content that follows a heading element in the JSX tree. The extraction should format the heading with markdown heading syntax and treat the paragraph as regular text content."})"#;
        let result = extract_jsx_text_content(jsx);
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(
            content.contains("## TL;DR"),
            "Should format h2 as ## heading, got: {content}"
        );
    }

    #[test]
    fn extract_jsx_text_content_handles_escaped_quotes() {
        let jsx = r#"(0,t.jsx)(s.p,{children:"This text has a \"quoted\" word inside it and needs to demonstrate that escaped quote handling works correctly in the JSX string value extraction. The parser must handle backslash-escaped double quotes without terminating the string prematurely, which would cause content truncation."})"#;
        let result = extract_jsx_text_content(jsx);
        assert!(result.is_some());
        assert!(result.unwrap().contains("\"quoted\""));
    }

    #[test]
    fn extract_jsx_text_content_skips_short_strings() {
        let jsx = r#"(0,t.jsx)(s.a,{children:"click"}),(0,t.jsx)(s.span,{children:"icon"})"#;
        // Both strings are too short (<15 chars)
        assert!(extract_jsx_text_content(jsx).is_none());
    }

    #[test]
    fn extract_jsx_text_content_returns_none_for_no_content() {
        let js = r#"console.log("no jsx here")"#;
        assert!(extract_jsx_text_content(js).is_none());
    }

    #[test]
    fn extract_js_string_value_handles_unicode_escapes() {
        let result = extract_js_string_value(r#"caf\u00e9 au lait" rest"#);
        assert_eq!(result, Some("caf\u{00e9} au lait".to_string()));
    }

    #[test]
    fn extract_js_string_value_handles_simple_string() {
        assert_eq!(
            extract_js_string_value(r#"hello world" rest"#),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn extract_js_string_value_handles_escaped_backslash() {
        assert_eq!(
            extract_js_string_value(r#"path\\to\\file" rest"#),
            Some("path\\to\\file".to_string())
        );
    }

    #[test]
    fn is_nextjs_metadata_only_true_for_metadata_only_page() {
        let html = r#"<html><body>
            <script id="__NEXT_DATA__" type="application/json">
            {"props":{"pageProps":{"slug":"test","meta":{"title":"Test","description":"Short desc"}}},"buildId":"abc123"}
            </script>
        </body></html>"#;
        assert!(is_nextjs_metadata_only(html));
    }

    #[test]
    fn is_nextjs_metadata_only_false_for_content_page() {
        let long_content = "x".repeat(300);
        let html = format!(
            r#"<html><body>
            <script id="__NEXT_DATA__" type="application/json">
            {{"props":{{"pageProps":{{"body":"{long_content}"}}}},"buildId":"abc123"}}
            </script>
        </body></html>"#
        );
        assert!(!is_nextjs_metadata_only(&html));
    }

    #[test]
    fn is_nextjs_metadata_only_false_for_non_nextjs_page() {
        let html = r#"<html><body><p>Regular page</p></body></html>"#;
        assert!(!is_nextjs_metadata_only(html));
    }

    #[test]
    fn discover_nextjs_content_chunks_finds_script_urls() {
        let html = r#"<html><head>
            <script src="/_next/static/chunks/webpack-abc123.js" defer></script>
            <script src="/_next/static/chunks/pages/blog/%5Bslug%5D-def456.js" defer></script>
            <script id="__NEXT_DATA__" type="application/json">
            {"props":{"pageProps":{"slug":"test","meta":{"title":"Test"}}},"buildId":"9aCehAyjokblLUFqGNdFr"}
            </script>
        </head><body></body></html>"#;
        let chunks = discover_nextjs_content_chunks(html, "https://example.com/blog/test");
        // Should find webpack + page script URLs + buildId
        assert!(chunks.len() >= 2, "Expected at least 2 URLs, got: {chunks:?}");
        assert!(
            chunks[0].contains("webpack-"),
            "First should be webpack: {}",
            chunks[0]
        );
        assert!(
            chunks[1].contains("/pages/"),
            "Second should be page chunk: {}",
            chunks[1]
        );
    }

    #[test]
    fn discover_nextjs_content_chunks_returns_empty_for_non_nextjs() {
        let html = r#"<html><body><p>Regular page</p></body></html>"#;
        assert!(discover_nextjs_content_chunks(html, "https://example.com").is_empty());
    }

    #[test]
    fn discover_nextjs_content_chunks_returns_empty_when_content_present() {
        let long_content = "x".repeat(300);
        let html = format!(
            r#"<html><head>
            <script src="/_next/static/chunks/webpack-abc.js" defer></script>
            <script id="__NEXT_DATA__" type="application/json">
            {{"props":{{"pageProps":{{"body":"{long_content}"}}}},"buildId":"abc"}}
            </script>
        </head><body></body></html>"#
        );
        assert!(discover_nextjs_content_chunks(&html, "https://example.com").is_empty());
    }
}
