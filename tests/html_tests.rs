//! Integration tests for HTML-to-markdown conversion (`nab::content::html`).
//!
//! Extracted from inline tests in `src/content/html.rs`.

use nab::content::ContentHandler;
use nab::content::html::{
    HtmlHandler, detect_thin_content, html_to_markdown, html_to_markdown_with_readability,
    html_to_markdown_with_url, is_boilerplate, strip_comment_sections,
};

#[test]
fn converts_basic_html() {
    let html = "<html><body><h1>Title</h1><p>Paragraph</p></body></html>";
    let md = html_to_markdown(html);
    assert!(md.contains("Title"));
    assert!(md.contains("Paragraph"));
}

#[test]
fn filters_boilerplate() {
    let html = "<html><body>\
        <p>Skip to content</p>\
        <h1>Real Content</h1>\
        <p>\u{00a9} 2025 Company</p>\
        </body></html>";
    let md = html_to_markdown(html);
    assert!(md.contains("Real Content"));
    assert!(!md.contains("Skip to content"));
    assert!(!md.contains("2025 Company"));
}

#[test]
fn preserves_markdown_links() {
    let html = r#"<html><body><a href="https://example.com">Link text</a></body></html>"#;
    let md = html_to_markdown(html);
    assert!(md.contains("]("));
}

#[test]
fn handler_returns_conversion_result() {
    let handler = HtmlHandler;
    let html = b"<html><body><p>Test</p></body></html>";
    let result = handler.to_markdown(html, "text/html").unwrap();
    assert!(result.markdown.contains("Test"));
    assert_eq!(result.content_type, "text/html");
    assert!(result.page_count.is_none());
    assert!(result.elapsed_ms >= 0.0);
}

#[test]
fn handler_with_url_improves_extraction() {
    let handler = HtmlHandler;
    let html = b"<html><body>\
        <article><h1>Article</h1><p>Content here.</p></article>\
        </body></html>";
    let result = handler
        .to_markdown_with_url(html, "text/html", Some("https://example.com/article"))
        .unwrap();
    assert!(result.markdown.contains("Article") || result.markdown.contains("Content"));
}

#[test]
fn handles_non_utf8_gracefully() {
    let handler = HtmlHandler;
    // Latin-1 encoded text (invalid UTF-8 byte 0xe9 for 'e')
    let bytes: &[u8] = b"<html><body>caf\xe9</body></html>";
    let result = handler.to_markdown(bytes, "text/html; charset=iso-8859-1");
    assert!(result.is_ok());
}

#[test]
fn test_is_boilerplate_detects_common_patterns() {
    assert!(is_boilerplate("Skip to content"));
    assert!(is_boilerplate("Cookie Policy"));
    assert!(is_boilerplate("Privacy Policy"));
    assert!(is_boilerplate("Terms of Service"));
    assert!(is_boilerplate("\u{00a9} 2025 Company"));
    assert!(is_boilerplate("Copyright 2025"));
}

#[test]
fn test_is_boilerplate_preserves_content() {
    assert!(!is_boilerplate("This is actual content"));
    assert!(!is_boilerplate("Welcome to our site"));
    assert!(!is_boilerplate("[Link](https://example.com)"));
}

#[test]
fn test_html_to_markdown_removes_excess_whitespace() {
    let html = "<html><body><p>Line 1</p>\n\n\n<p>Line 2</p></body></html>";
    let md = html_to_markdown(html);
    // Should collapse multiple newlines
    assert!(!md.contains("\n\n\n"));
}

#[test]
fn test_handler_supported_types() {
    let handler = HtmlHandler;
    let types = handler.supported_types();
    assert!(types.contains(&"text/html"));
    assert!(types.contains(&"application/xhtml+xml"));
}

#[test]
fn test_readability_extraction_removes_boilerplate() {
    let html = r#"
        <html>
        <head><title>Article Title</title></head>
        <body>
            <nav>
                <a href="/">Home</a>
                <a href="/about">About</a>
            </nav>
            <main>
                <article>
                    <h1>Main Article</h1>
                    <p>This is the main article content that should be extracted.</p>
                    <p>It contains important information for the reader.</p>
                </article>
            </main>
            <aside class="sidebar">
                <h3>Related Articles</h3>
                <ul><li>Related 1</li></ul>
            </aside>
            <footer>
                <p>&copy; 2025 Company</p>
                <p>Privacy Policy | Terms of Service</p>
            </footer>
        </body>
        </html>
    "#;

    let markdown = html_to_markdown_with_url(html, Some("https://example.com/article"));

    // Should contain main article content (case-insensitive check)
    let markdown_lower = markdown.to_lowercase();
    assert!(
        markdown_lower.contains("main article") || markdown_lower.contains("article content"),
        "Expected article content, got: {markdown}"
    );
    assert!(markdown.contains("main article content"));

    // Should NOT contain navigation or footer boilerplate
    assert!(!markdown.contains("Home") || !markdown.contains("About"));
    assert!(!markdown.contains("2025 Company"));
    assert!(!markdown.contains("Privacy Policy"));
}

#[test]
fn test_readability_fallback_for_non_article_pages() {
    let html = "<html><body><div>Simple page</div></body></html>";
    let markdown = html_to_markdown_with_url(html, None);

    // Should still convert to markdown (fallback path)
    assert!(markdown.contains("Simple page"));
}

#[test]
fn test_readability_with_semantic_html() {
    let html = r"
        <html>
        <body>
            <header>Header content</header>
            <main>
                <h1>Article Title</h1>
                <p>This is the main content area with substantial text.</p>
                <p>Multiple paragraphs ensure proper extraction.</p>
            </main>
            <footer>Footer content</footer>
        </body>
        </html>
    ";

    let markdown = html_to_markdown_with_url(html, Some("https://example.com/article"));

    assert!(markdown.contains("Article Title"));
    assert!(markdown.contains("main content area"));
    assert!(!markdown.contains("Header content"));
    assert!(!markdown.contains("Footer content"));
}

#[test]
fn test_readability_with_real_url_extracts_article() {
    // Verifies BUG 1 fix: real URL passed to readability, not "https://example.com"
    let html = r#"
        <html>
        <head><title>LessWrong Post</title></head>
        <body>
            <article>
                <h1>Rationalist Argument</h1>
                <p>This post contains substantial content about reasoning under uncertainty.</p>
                <p>Multiple paragraphs to ensure proper article detection by readability.</p>
            </article>
            <div class="comment-section">
                <div class="comment">User comment one</div>
                <div class="comment">User comment two</div>
            </div>
        </body>
        </html>
    "#;

    let markdown =
        html_to_markdown_with_url(html, Some("https://www.lesswrong.com/posts/abc/title"));

    assert!(markdown.contains("Rationalist Argument") || markdown.contains("reasoning"));
}

#[test]
fn strip_comment_sections_removes_comment_containers() {
    let html = r#"
        <html><body>
            <article>
                <h1>Post Title</h1>
                <p>Real article content here with enough text.</p>
            </article>
            <div class="comment-section">
                <div>First comment: Lorem ipsum</div>
                <div>Second comment: dolor sit amet</div>
            </div>
        </body></html>
    "#;

    let stripped = strip_comment_sections(html);

    // The stripped HTML should not contain the comment-section div
    assert!(
        !stripped.contains("comment-section"),
        "comment-section class should be removed, got: {}",
        &stripped[..stripped.len().min(500)]
    );
    // But should retain article content
    assert!(stripped.contains("Post Title") || stripped.contains("article"));
}

#[test]
fn strip_comment_sections_preserves_article_content() {
    let html = r"
        <html><body>
            <article>
                <h1>The Real Post</h1>
                <p>Substantive article body that we must not lose.</p>
            </article>
        </body></html>
    ";

    let stripped = strip_comment_sections(html);

    assert!(stripped.contains("The Real Post"));
    assert!(stripped.contains("Substantive article body"));
}

// Backward-compatibility: old callers still work
#[test]
fn html_to_markdown_with_readability_still_works() {
    let html = "<html><body><article>\
        <h1>Compat Test</h1>\
        <p>Backward compatible call path works fine.</p>\
        </article></body></html>";
    let md = html_to_markdown_with_readability(html);
    assert!(md.contains("Compat Test") || md.contains("Backward compatible"));
}

// ── detect_thin_content ─────────────────────────────────────────────────────

#[test]
fn detect_thin_content_warns_when_ratio_below_threshold() {
    // GIVEN: 10 KB HTML producing only 50 chars of markdown (0.5% ratio)
    let warning = detect_thin_content(10_000, 50);
    // THEN: a warning is returned
    assert!(warning.is_some(), "should warn for 0.5% ratio");
    let msg = warning.unwrap();
    assert!(
        msg.contains("suspiciously thin"),
        "message should describe the problem"
    );
    assert!(
        msg.contains("JavaScript rendering"),
        "message should explain likely cause"
    );
    assert!(
        msg.contains("--cookies"),
        "message should suggest a workaround"
    );
    assert!(
        msg.contains("nab spa"),
        "message should suggest nab spa as alternative"
    );
}

#[test]
fn detect_thin_content_no_warning_for_normal_ratio() {
    // GIVEN: 10 KB HTML producing 1 KB of markdown (10% ratio -- normal article)
    let warning = detect_thin_content(10_000, 1_000);
    // THEN: no warning
    assert!(warning.is_none(), "should not warn for healthy 10% ratio");
}

#[test]
fn detect_thin_content_no_warning_for_tiny_html() {
    // GIVEN: HTML body below the minimum size threshold (4 KB)
    // WHEN: markdown is also very small
    let warning = detect_thin_content(4_000, 10);
    // THEN: no warning -- too small to be reliable signal
    assert!(
        warning.is_none(),
        "should not warn for HTML below 5 KB minimum"
    );
}

#[test]
fn detect_thin_content_no_warning_at_exact_threshold() {
    // GIVEN: ratio exactly at the 2% threshold (boundary condition)
    let html_len = 10_000;
    let markdown_len = 200; // 2% exactly
    let warning = detect_thin_content(html_len, markdown_len);
    // THEN: no warning (2% is at the boundary, not below it)
    assert!(
        warning.is_none(),
        "exact threshold should not trigger warning"
    );
}

#[test]
fn detect_thin_content_warns_just_below_threshold() {
    // GIVEN: ratio just below the 2% threshold (boundary condition)
    let html_len = 10_000;
    let markdown_len = 199; // 1.99% -- just below
    let warning = detect_thin_content(html_len, markdown_len);
    // THEN: warning is returned
    assert!(
        warning.is_some(),
        "just-below-threshold should trigger warning"
    );
}

#[test]
fn detect_thin_content_no_warning_for_empty_markdown_on_small_html() {
    // GIVEN: small page that produces empty markdown -- not a JS rendering issue
    let warning = detect_thin_content(100, 0);
    // THEN: no warning -- HTML is below minimum size
    assert!(warning.is_none(), "tiny HTML should never warn");
}

// ── Integration tests: SPA extraction via html_to_markdown_with_url ─────────

#[test]
fn js_rendered_page_with_next_data_extracts_article_body() {
    let article_body = "Minions & Stripe's One-Shot End-to-End Coding Agents: \
        In this post we explore how we built autonomous coding agents capable of \
        completing entire engineering tasks from specification to pull request. \
        We discuss the architecture, the challenges of tool use, and the lessons \
        learned from running thousands of agent sessions in production. \
        The system combines LLM planning with deterministic execution steps, \
        enabling reliable automation of complex software engineering workflows.";

    let next_data = serde_json::json!({
        "props": {
            "pageProps": {
                "post": {
                    "title": "Minions & Stripe's One-Shot End-to-End Coding Agents",
                    "slug": "minions-stripes-one-shot-end-to-end-coding-agents",
                    "author": {"name": "Stripe Engineering"},
                    "bodyText": article_body
                }
            }
        },
        "buildId": "abc123",
        "page": "/blog/[slug]"
    });

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Minions &amp; Stripe's One-Shot End-to-End Coding Agents - Stripe Blog</title>
    <meta name="description" content="How we built autonomous coding agents.">
</head>
<body>
    <div id="__next">
        <header><nav><a href="/">Stripe</a></nav></header>
        <main>
            <p class="author-bio">Stripe Engineering Team</p>
        </main>
    </div>
    <script id="__NEXT_DATA__" type="application/json">{next_data}</script>
</body>
</html>"#
    );

    let markdown = html_to_markdown_with_url(
        &html,
        Some("https://stripe.dev/blog/minions-stripes-one-shot-end-to-end-coding-agents"),
    );

    assert!(
        markdown.contains("autonomous coding agents") || markdown.contains("Minions"),
        "expected article body in markdown, got only: {markdown}"
    );
    assert!(
        markdown.len() > 200,
        "output should be substantially longer than a bio, got {} chars",
        markdown.len()
    );
}

#[test]
fn detect_thin_content_fires_for_js_rendered_page_shell() {
    let html_len = 34_936;
    // 200 is the NOT-thin boundary (>= 200 chars = adequate).
    // Use 199 to test the thin-content detection path.
    let markdown_len = 199;

    let warning = detect_thin_content(html_len, markdown_len);

    assert!(
        warning.is_some(),
        "34 KB HTML -> 199 char markdown must trigger thin-content warning"
    );
    let msg = warning.unwrap();
    assert!(
        msg.contains("199"),
        "warning should include actual markdown length"
    );
    assert!(
        msg.contains("34936") || msg.contains("34,936") || msg.contains("bytes"),
        "warning should include HTML size"
    );
    assert!(
        msg.contains("nab browser <url>"),
        "warning should include explicit browser-rendering hint"
    );
}

#[test]
fn extract_jsonld_article_body() {
    let article_body = "This is the full article body extracted from JSON-LD structured \
        data. Modern blogs increasingly embed Schema.org Article markup which contains \
        the complete article text in the articleBody field, making it possible to \
        extract content even when the HTML shell is empty.";

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Test Blog Post</title>
    <script type="application/ld+json">
    {{
        "@context": "https://schema.org",
        "@type": "BlogPosting",
        "headline": "Test Blog Post",
        "author": {{"@type": "Person", "name": "Test Author"}},
        "articleBody": "{article_body}"
    }}
    </script>
</head>
<body>
    <div id="root"><p>Loading...</p></div>
</body>
</html>"#
    );

    let markdown = html_to_markdown_with_url(&html, Some("https://example.com/blog/test"));

    assert!(
        markdown.contains("JSON-LD structured data") || markdown.contains("full article body"),
        "expected JSON-LD article body in markdown, got: {markdown}"
    );
    assert!(
        markdown.len() > 200,
        "output should be substantial, got {} chars",
        markdown.len()
    );
}

#[test]
fn extract_jsonld_handles_array_of_schemas() {
    let article_body = "The complete article body from a page that uses an array of \
        JSON-LD schemas. This is a common pattern where the page includes both a \
        WebSite schema and a BlogPosting schema in a single script tag. The \
        extraction should find the article content from the BlogPosting entry.";

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <script type="application/ld+json">
    [
        {{
            "@context": "https://schema.org",
            "@type": "WebSite",
            "name": "Example Blog",
            "url": "https://example.com"
        }},
        {{
            "@context": "https://schema.org",
            "@type": "BlogPosting",
            "headline": "Array Test",
            "articleBody": "{article_body}"
        }}
    ]
    </script>
</head>
<body><div id="root"></div></body>
</html>"#
    );

    let markdown = html_to_markdown_with_url(&html, Some("https://example.com/blog/test"));
    assert!(
        markdown.contains("array of JSON-LD schemas") || markdown.contains("complete article body"),
        "expected article body from JSON-LD array, got: {markdown}"
    );
}

#[test]
fn extract_jsonld_uses_description_fallback() {
    let description = "A detailed description of the article that serves as a fallback \
        when the articleBody field is not present. This is a common pattern in some CMS \
        systems that only populate the description field in their JSON-LD markup. The \
        description should be extracted as content when no better field is available.";

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <script type="application/ld+json">
    {{
        "@context": "https://schema.org",
        "@type": "Article",
        "headline": "Description Fallback Test",
        "description": "{description}"
    }}
    </script>
</head>
<body><div id="root"></div></body>
</html>"#
    );

    let markdown = html_to_markdown_with_url(&html, Some("https://example.com/article"));
    assert!(
        markdown.contains("detailed description") || markdown.contains("fallback"),
        "expected description content as fallback, got: {markdown}"
    );
}

#[test]
fn extract_inline_script_next_data_assignment() {
    let article_body = "This is the article body from a page that uses inline script \
        assignment for Next.js data instead of a tagged script element. This pattern \
        is used by some Next.js deployments and custom frameworks that inject the \
        hydration data via window.__NEXT_DATA__ = {...} in an inline script.";

    let next_data = serde_json::json!({
        "props": {
            "pageProps": {
                "post": {
                    "title": "Inline Assignment Test",
                    "body": article_body
                }
            }
        },
        "buildId": "test123"
    });

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head><title>Inline Next.js</title></head>
<body>
    <div id="__next"><p>Loading...</p></div>
    <script>window.__NEXT_DATA__ = {next_data};</script>
</body>
</html>"#
    );

    let markdown = html_to_markdown_with_url(&html, Some("https://example.com/blog/test"));
    assert!(
        markdown.contains("inline script assignment")
            || markdown.contains("article body from a page"),
        "expected article body from inline script, got: {markdown}"
    );
}

// ── Issue #32 end-to-end regression: JS-rendered page with JSON-LD ──────────

#[test]
fn js_rendered_page_with_jsonld_extracts_article_body() {
    let article_body = "Minions and autonomous coding agents represent a new paradigm in \
        software engineering automation. In this comprehensive post we explore the \
        architecture decisions that went into building Stripe's end-to-end coding \
        agent system. We discuss the challenges of tool use, context management, \
        and the critical importance of verification in autonomous code generation. \
        The system achieved remarkable results in production, completing complex \
        engineering tasks from specification to pull request.";

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Minions - Stripe Blog</title>
    <script type="application/ld+json">
    {{
        "@context": "https://schema.org",
        "@type": "BlogPosting",
        "headline": "Minions: Stripe's One-Shot End-to-End Coding Agents",
        "author": {{"@type": "Person", "name": "Alistair Gray"}},
        "datePublished": "2025-03-01",
        "articleBody": "{article_body}"
    }}
    </script>
</head>
<body>
    <div id="__next">
        <header><nav><a href="/">Stripe</a></nav></header>
        <main>
            <p class="author-bio">Alistair is a software engineer on the Leverage team</p>
        </main>
    </div>
</body>
</html>"#
    );

    let markdown = html_to_markdown_with_url(
        &html,
        Some("https://stripe.dev/blog/minions-stripes-one-shot-end-to-end-coding-agents"),
    );

    assert!(
        markdown.contains("autonomous coding agents") || markdown.contains("Minions"),
        "expected article body from JSON-LD in markdown, got only: {markdown}"
    );
    assert!(
        markdown.len() > 200,
        "output should be substantially longer than a bio, got {} chars",
        markdown.len()
    );
    assert!(
        !markdown.starts_with("Alistair is a software engineer"),
        "should extract article body, not just author bio"
    );
}
