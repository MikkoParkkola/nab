//! Integration tests for SPA / structured-data extraction (`nab::content::spa_extract`).
//!
//! Extracted from inline tests in `src/content/spa_extract.rs`.

use nab::content::spa_extract::{
    extract_balanced_json, extract_inline_script_json, extract_jsonld_content,
    extract_nextjs_content, extract_spa_data, find_content_by_key, find_longest_string,
};

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

// ── find_longest_string ─────────────────────────────────────────────────────

#[test]
fn find_longest_string_returns_longest_in_flat_object() {
    let value = serde_json::json!({
        "slug": "my-post",
        "title": "A short title",
        "bodyText": "This is a much longer string representing the article body content that should be selected as the longest value in the tree."
    });
    let result = find_longest_string(&value, 50);
    assert!(result.is_some());
    assert!(result.unwrap().contains("article body content"));
}

#[test]
fn find_longest_string_returns_longest_in_nested_object() {
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
    let value = serde_json::json!({
        "id": "abc123",
        "slug": "my-post",
        "status": "published"
    });
    let result = find_longest_string(&value, 50);
    assert!(result.is_none());
}

#[test]
fn find_longest_string_handles_array_of_strings() {
    let value = serde_json::json!([
        "short",
        "medium length string here",
        "this is the longest string in the array and it exceeds the minimum length threshold easily"
    ]);
    let result = find_longest_string(&value, 30);
    assert!(result.is_some());
    assert!(result.unwrap().contains("longest string in the array"));
}

// ── extract_nextjs_content with unknown CMS key names ───────────────────────

#[test]
fn extract_nextjs_content_falls_back_to_longest_string_for_unknown_keys() {
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
                    "richBodyContent": article_body
                }
            }
        },
        "buildId": "stripe-blog-build-123"
    });

    let result = extract_nextjs_content(&json_data);

    assert!(
        result.is_some(),
        "should extract content via longest-string fallback"
    );
    let content = result.unwrap();
    assert!(
        content.contains("Stripe's blog post") || content.contains("payment processing"),
        "expected article body, got: {content}"
    );
}

#[test]
fn extract_nextjs_content_prefers_named_key_over_longer_incidental_string() {
    let known_body = "This is the article body content stored under the well-known \
        'body' key. It is substantial enough to pass the minimum length check.";
    let longer_non_content = "a".repeat(500);

    let json_data = serde_json::json!({
        "props": {
            "pageProps": {
                "post": {
                    "body": known_body,
                    "_internalData": longer_non_content
                }
            }
        }
    });

    let result = extract_nextjs_content(&json_data);
    assert!(result.is_some());
    assert!(
        result.unwrap().len() >= known_body.len() - 50,
        "should return at least the known body content"
    );
}

#[test]
fn extract_nextjs_content_extended_key_body_html() {
    let html_body = "<p>Article content stored as HTML in the bodyHtml field. \
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
    assert!(
        content.contains("Article content stored as HTML"),
        "expected HTML converted to markdown, got: {content}"
    );
}

// ── JSON-LD extraction ──────────────────────────────────────────────────────

#[test]
fn extract_jsonld_ignores_non_article_types() {
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
    assert!(
        result.is_none(),
        "Organization JSON-LD should not be extracted as article content"
    );
}

// ── Inline script variable extraction ───────────────────────────────────────

#[test]
fn extract_inline_script_initial_state_assignment() {
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

#[test]
fn extract_inline_script_custom_bootstrap_assignment_uses_generic_scan() {
    let content = "A long article body embedded in a custom SPA bootstrap payload. \
        The framework does not use one of nab's built-in global names, but the \
        inline script still contains a valid JSON assignment with substantial \
        article content that should be recovered by the generic scan.";

    let state = serde_json::json!({
        "bootstrap": {
            "richContent": content,
            "title": "Custom Bootstrap Test"
        }
    });

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<body>
    <div id="app"></div>
    <script>window.__ICE_APP_CONTEXT__ = {state};</script>
</body>
</html>"#
    );

    let result = extract_inline_script_json(&html);
    assert!(
        result.is_some(),
        "should extract content from an unknown inline bootstrap assignment"
    );
    let content = result.unwrap();
    assert!(
        content.contains("custom SPA bootstrap payload") || content.contains("generic scan"),
        "expected custom bootstrap content, got: {content}"
    );
}

#[test]
fn extract_inline_script_generic_scan_ignores_csr_shell_without_content() {
    let html = r#"<!DOCTYPE html>
<html>
<body>
    <div id="ice-container"></div>
    <script>
        !(function () {
            var a = window.__ICE_APP_CONTEXT__ || {};
            var b = {"appData":null,"loaderData":{"layout":{"pageConfig":{}},"home":{"pageConfig":{}}},"routePath":"/home","matchedIds":["layout","home"],"documentOnly":true,"renderMode":"CSR"};
            for (var k in a) { b[k] = a[k]; }
            window.__ICE_APP_CONTEXT__ = b;
        })();
    </script>
</body>
</html>"#;

    let result = extract_inline_script_json(html);
    assert!(
        result.is_none(),
        "qwen-style CSR shell should not be mistaken for article content"
    );
}

// ── Balanced JSON extractor ─────────────────────────────────────────────────

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
