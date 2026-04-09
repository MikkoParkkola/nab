//! Next.js SSR data extraction, JSX text extraction, and metadata detection.

use super::helpers::{find_content_by_key, find_longest_string, render_spa_content};

// Re-export webpack chunk functions so callers can use `nextjs::discover_*`.
pub use super::webpack::{
    discover_nextjs_content_chunks, resolve_content_chunk_urls, resolve_content_chunk_urls_for_slug,
};

// ── Core extraction ─────────────────────────────────────────────────────────

/// Extract and convert content from a JSON-bearing `<script>` element.
pub(crate) fn try_extract_script_json(
    document: &scraper::Html,
    css_selector: &str,
) -> Option<String> {
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
pub fn extract_nextjs_content(data: &serde_json::Value) -> Option<String> {
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
    const MIN_CONTENT_LEN: usize = 200;

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
    if best.is_none() {
        best = find_longest_string(page_props, MIN_CONTENT_LEN);
    }

    best.map(|content| render_spa_content(&content))
}

// ── Metadata-only detection ─────────────────────────────────────────────────

/// Check if the page has `__NEXT_DATA__` with only metadata (no article body).
///
/// Returns `true` if this is a Next.js page where `__NEXT_DATA__` exists but
/// `pageProps` contains no string longer than 200 characters.
/// Used to decide whether content chunk recovery is needed.
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

    if let Some(page_props) = next_data.get("props").and_then(|p| p.get("pageProps")) {
        find_longest_string(page_props, 200).is_none()
    } else {
        true
    }
}

// ── JSX text extraction ─────────────────────────────────────────────────────

/// Extract readable text content from a compiled JSX/MDX webpack chunk.
///
/// Next.js MDX blogs compile article content into webpack chunks containing
/// React JSX calls like:
/// ```text
/// (0,t.jsx)(s.p,{children:"Article text here."})
/// (0,t.jsxs)(s.h2,{id:"section",children:"Section Title"})
/// ```
///
/// Returns `Some(markdown)` if substantial content was extracted (>200 chars),
/// `None` otherwise.
pub fn extract_jsx_text_content(js_source: &str) -> Option<String> {
    const MIN_CONTENT_LEN: usize = 200;

    let mut paragraphs: Vec<String> = Vec::new();
    let mut search_from = 0;

    while search_from < js_source.len() {
        while search_from < js_source.len() && !js_source.is_char_boundary(search_from) {
            search_from += 1;
        }
        if search_from >= js_source.len() {
            break;
        }

        let Some(children_idx) = js_source[search_from..].find("children:\"") else {
            break;
        };
        let abs_idx = search_from + children_idx + 10; // skip past children:"

        if abs_idx >= js_source.len() || !js_source.is_char_boundary(abs_idx) {
            search_from = abs_idx.saturating_add(1);
            continue;
        }

        if let Some(text) = extract_js_string_value(&js_source[abs_idx..]) {
            if is_substantial_jsx_text(&text) {
                let mut context_start = abs_idx.saturating_sub(200);
                while context_start < abs_idx && !js_source.is_char_boundary(context_start) {
                    context_start += 1;
                }
                let context = &js_source[context_start..abs_idx];
                paragraphs.push(format_jsx_text(text, context));
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

fn is_substantial_jsx_text(text: &str) -> bool {
    text.len() >= 15
        && !text.starts_with("http")
        && !text.starts_with("data:")
        && !text.starts_with("text-")
        && !text.contains("className")
        && !text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn format_jsx_text(text: String, context: &str) -> String {
    if is_heading_context(context) {
        let level = detect_heading_level(context);
        format!("{} {text}", "#".repeat(level))
    } else if is_list_context(context) {
        format!("- {text}")
    } else if is_blockquote_context(context) {
        format!("> {text}")
    } else if is_code_context(context) {
        if text.len() > 30 {
            format!("```\n{text}\n```")
        } else {
            format!("`{text}`")
        }
    } else {
        text
    }
}

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
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(code) = u32::from_str_radix(&hex, 16)
                        && let Some(ch) = char::from_u32(code)
                    {
                        result.push(ch);
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
                '"' => return Some(result),
                '\\' => escape_next = true,
                _ => result.push(c),
            }
        }
    }

    None
}

fn context_tail(context: &str, n: usize) -> &str {
    if context.len() > n {
        &context[context.len() - n..]
    } else {
        context
    }
}

fn is_heading_context(context: &str) -> bool {
    let tail = context_tail(context, 100);
    (1..=6).any(|n| tail.contains(&format!("s.h{n}")))
}

fn detect_heading_level(context: &str) -> usize {
    let tail = context_tail(context, 100);
    (1..=6)
        .find(|&n| tail.contains(&format!("s.h{n}")))
        .unwrap_or(2)
}

fn is_list_context(context: &str) -> bool {
    context_tail(context, 100).contains("s.li")
}

fn is_blockquote_context(context: &str) -> bool {
    context_tail(context, 100).contains("s.blockquote")
}

fn is_code_context(context: &str) -> bool {
    let tail = context_tail(context, 100);
    tail.contains("s.pre") || tail.contains("s.code")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let html = r"<html><body><p>Regular page</p></body></html>";
        assert!(!is_nextjs_metadata_only(html));
    }
}
