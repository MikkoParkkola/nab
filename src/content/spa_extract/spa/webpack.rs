//! Next.js webpack chunk discovery and URL resolution.

use super::helpers::find_longest_string;
use super::inline::extract_balanced_json;

/// Discover Next.js content chunk URLs from the HTML page.
///
/// When a Next.js site uses MDX or lazy-loaded content, `__NEXT_DATA__` contains
/// only metadata (title, description, slug) but no article body.  The actual
/// content is compiled into a webpack chunk that is loaded dynamically.
///
/// Returns absolute chunk URLs that should be fetched and passed through
/// `extract_jsx_text_content` to recover the article body.
pub fn discover_nextjs_content_chunks(html: &str, page_url: &str) -> Vec<String> {
    let document = scraper::Html::parse_document(html);

    let next_data = {
        let sel = scraper::Selector::parse("script#__NEXT_DATA__").ok();
        sel.and_then(|s| document.select(&s).next())
            .and_then(|script| {
                let json_text = script.text().collect::<String>();
                serde_json::from_str::<serde_json::Value>(&json_text).ok()
            })
    };

    let Some(next_data) = next_data else {
        return Vec::new();
    };

    // If __NEXT_DATA__ already has substantial content, no need for chunks
    if let Some(page_props) = next_data.get("props").and_then(|p| p.get("pageProps"))
        && find_longest_string(page_props, 200).is_some()
    {
        return Vec::new();
    }

    let build_id = next_data
        .get("buildId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let origin = url::Url::parse(page_url)
        .ok()
        .map(|u| u.origin().unicode_serialization())
        .unwrap_or_default();

    let script_sel = scraper::Selector::parse("script[src]").ok();
    let script_srcs: Vec<String> = script_sel
        .iter()
        .flat_map(|sel| document.select(sel))
        .filter_map(|el| el.value().attr("src").map(String::from))
        .collect();

    let webpack_src = script_srcs
        .iter()
        .find(|s| s.contains("webpack-") && s.contains(".js"));

    let page_path = url::Url::parse(page_url)
        .ok()
        .map(|u| u.path().to_string())
        .unwrap_or_default();

    let page_src = find_page_script(&script_srcs, &page_path);

    let mut result = Vec::new();

    if let (Some(webpack), Some(page)) = (webpack_src, page_src) {
        let webpack_url = resolve_script_url(webpack, &origin);
        let page_url_resolved = resolve_script_url(page, &origin);
        result.push(webpack_url);
        result.push(page_url_resolved);
    }

    if !build_id.is_empty() && !origin.is_empty() {
        result.push(build_id.to_string());
    }

    result
}

/// Select the page-specific script from the list of script src attributes.
fn find_page_script<'a>(srcs: &'a [String], page_path: &str) -> Option<&'a String> {
    let is_framework = |s: &&String| -> bool {
        s.contains("/_app") || s.contains("/_error") || s.contains("/_document")
    };

    let path_segments: Vec<&str> = page_path.split('/').filter(|s| !s.is_empty()).collect();

    srcs.iter()
        .find(|s| {
            if !s.contains("/pages/") || !s.contains(".js") || is_framework(s) {
                return false;
            }
            if let Some(first_segment) = path_segments.first() {
                s.contains(&format!("/pages/{first_segment}/"))
                    || s.contains(&format!("/pages/{first_segment}-"))
            } else {
                s.contains("/pages/index")
            }
        })
        .or_else(|| {
            srcs.iter()
                .find(|s| s.contains("/pages/") && s.contains(".js") && !is_framework(s))
        })
}

/// Parse the webpack runtime JS to extract chunk-ID-to-filename map,
/// then parse the page component JS to find lazy content chunk IDs,
/// and return the resolved content chunk URLs.
pub fn resolve_content_chunk_urls(webpack_js: &str, page_js: &str, origin: &str) -> Vec<String> {
    resolve_content_chunk_urls_for_slug(webpack_js, page_js, origin, None)
}

/// Like `resolve_content_chunk_urls` but filters to a specific page slug.
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
            chunk_hashes
                .get(&id_str)
                .map(|hash| format!("{origin}/_next/static/chunks/{id}.{hash}.js"))
        })
        .collect()
}

/// Parse chunk hash map from webpack runtime JS.
///
/// Looks for the pattern:
/// ```text
/// r.u=e=>"static/chunks/"+e+"."+({11:"hash1",264:"hash2"})[e]+".js"
/// ```
fn parse_webpack_chunk_hashes(js: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();

    let Some(chunks_idx) = js.find("static/chunks/") else {
        return map;
    };

    let remaining = &js[chunks_idx..];
    let Some(map_start) = remaining.find("({") else {
        return map;
    };
    let obj_start = chunks_idx + map_start + 1; // skip the '('

    let Some(json_str) = extract_balanced_json(&js[obj_start..]) else {
        return map;
    };

    let json_fixed = quote_numeric_keys(json_str);

    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&json_fixed)
        && let Some(obj_map) = obj.as_object()
    {
        for (k, v) in obj_map {
            if let Some(hash) = v.as_str() {
                map.insert(k.clone(), hash.to_string());
            }
        }
    }

    map
}

/// Quote unquoted numeric object keys in a JS object literal to make it valid JSON.
///
/// Converts `{11:"abc",264:"def"}` to `{"11":"abc","264":"def"}`.
fn quote_numeric_keys(js_obj: &str) -> String {
    let mut result = String::with_capacity(js_obj.len() + 20);
    let mut chars = js_obj.chars().peekable();
    let mut in_string = false;
    let mut escape_next = false;
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
fn parse_lazy_chunk_ids_for_slug(js: &str, slug: &str) -> Vec<u64> {
    let mut ids = Vec::new();

    for ext in &[".mdx", ".md"] {
        let pattern = format!("./{slug}{ext}\":[");
        if let Some(idx) = js.find(&pattern) {
            let start = idx + pattern.len();
            if let Some(bracket_end) = js[start..].find(']') {
                let nums_str = &js[start..start + bracket_end];
                let parts: Vec<&str> = nums_str.split(',').collect();
                if parts.len() == 2
                    && let Ok(chunk_id) = parts[1].trim().parse::<u64>()
                {
                    ids.push(chunk_id);
                }
            }
        }
    }

    ids
}

/// Parse lazy chunk IDs from a Next.js page component JS.
///
/// Returns the chunk IDs (second number in each `[moduleId, chunkId]` pair).
fn parse_lazy_chunk_ids(js: &str) -> Vec<u64> {
    let mut ids = Vec::new();

    let mut search_from = 0;
    while let Some(mdx_idx) = js[search_from..].find(".mdx\":[") {
        let abs_idx = search_from + mdx_idx + 7;
        if let Some(bracket_end) = js[abs_idx..].find(']') {
            let nums_str = &js[abs_idx..abs_idx + bracket_end];
            let parts: Vec<&str> = nums_str.split(',').collect();
            if parts.len() == 2
                && let Ok(chunk_id) = parts[1].trim().parse::<u64>()
            {
                ids.push(chunk_id);
            }
        }
        search_from = abs_idx;
    }

    search_from = 0;
    while let Some(md_idx) = js[search_from..].find(".md\":[") {
        let abs_idx = search_from + md_idx + 6;
        if let Some(bracket_end) = js[abs_idx..].find(']') {
            let nums_str = &js[abs_idx..abs_idx + bracket_end];
            let parts: Vec<&str> = nums_str.split(',').collect();
            if parts.len() == 2
                && let Ok(chunk_id) = parts[1].trim().parse::<u64>()
            {
                ids.push(chunk_id);
            }
        }
        search_from = abs_idx;
    }

    ids.dedup();
    ids
}

/// Resolve a script src attribute to an absolute URL, stripping query parameters.
pub(super) fn resolve_script_url(src: &str, origin: &str) -> String {
    let path = src.split('?').next().unwrap_or(src);

    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else if path.starts_with('/') {
        format!("{origin}{path}")
    } else {
        format!("{origin}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(urls.is_empty());
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
        assert!(
            chunks.len() >= 2,
            "Expected at least 2 URLs, got: {chunks:?}"
        );
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
        let html = r"<html><body><p>Regular page</p></body></html>";
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
