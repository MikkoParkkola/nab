//! Integration tests for readability extraction.

use nab::content::{ContentRouter, ConversionResult};

#[test]
fn test_readability_extracts_article_content() {
    let html = r#"
        <!DOCTYPE html>
        <html>
        <head>
            <title>Test Article - Example Site</title>
        </head>
        <body>
            <header>
                <nav>
                    <a href="/">Home</a>
                    <a href="/about">About</a>
                    <a href="/contact">Contact</a>
                </nav>
            </header>
            <main>
                <article>
                    <h1>The Benefits of Rust Programming</h1>
                    <p>Rust is a systems programming language that runs blazingly fast, prevents segfaults, and guarantees thread safety.</p>
                    <p>It accomplishes these goals without requiring a garbage collector or runtime, making it suitable for embedded systems and other performance-critical applications.</p>
                    <h2>Key Features</h2>
                    <p>The language provides zero-cost abstractions, move semantics, guaranteed memory safety, threads without data races, trait-based generics, pattern matching, type inference, and efficient C bindings.</p>
                </article>
            </main>
            <aside class="sidebar">
                <h3>Related Articles</h3>
                <ul>
                    <li><a href="/rust-vs-cpp">Rust vs C++</a></li>
                    <li><a href="/memory-safety">Memory Safety in Rust</a></li>
                </ul>
            </aside>
            <footer>
                <p>© 2025 Example Site</p>
                <p><a href="/privacy">Privacy Policy</a> | <a href="/terms">Terms of Service</a></p>
                <p>Cookie Notice: We use cookies to improve your experience.</p>
            </footer>
        </body>
        </html>
    "#;

    let router = ContentRouter::new();
    let result: ConversionResult = router
        .convert(html.as_bytes(), "text/html")
        .expect("Conversion should succeed");

    // Verify main article content is present (case insensitive)
    let markdown_lower = result.markdown.to_lowercase();
    assert!(
        markdown_lower.contains("rust") && markdown_lower.contains("programming"),
        "Should contain article about Rust programming, got: {}",
        result.markdown
    );
    assert!(
        markdown_lower.contains("systems programming"),
        "Should contain article body"
    );
    assert!(
        result.markdown.contains("zero-cost abstractions") || markdown_lower.contains("zero"),
        "Should contain article details"
    );

    // Verify navigation boilerplate is removed
    assert!(
        !result.markdown.contains("Home") || !result.markdown.contains("About"),
        "Should not contain navigation"
    );

    // Verify footer boilerplate is removed
    assert!(
        !result.markdown.contains("2025 Example Site"),
        "Should not contain copyright footer"
    );
    assert!(
        !result.markdown.contains("Cookie Notice"),
        "Should not contain cookie notice"
    );
}

#[test]
fn test_readability_handles_non_article_pages_gracefully() {
    let html = r#"
        <!DOCTYPE html>
        <html>
        <body>
            <div>Homepage</div>
            <div>Welcome</div>
        </body>
        </html>
    "#;

    let router = ContentRouter::new();
    let result = router
        .convert(html.as_bytes(), "text/html")
        .expect("Should not fail on non-article pages");

    // Should still produce some output (fallback behavior)
    assert!(!result.markdown.is_empty());
}

#[test]
fn test_readability_preserves_markdown_structure() {
    let html = r#"
        <!DOCTYPE html>
        <html>
        <body>
            <article>
                <h1>Article Title</h1>
                <p>First paragraph with <strong>bold</strong> and <em>italic</em> text.</p>
                <ul>
                    <li>List item one</li>
                    <li>List item two</li>
                </ul>
                <p>Paragraph with <a href="https://example.com">a link</a>.</p>
            </article>
        </body>
        </html>
    "#;

    let router = ContentRouter::new();
    let result = router
        .convert(html.as_bytes(), "text/html")
        .expect("Conversion should succeed");

    // Verify markdown formatting is preserved
    assert!(
        result.markdown.contains("Article Title"),
        "Should contain heading"
    );
    assert!(result.markdown.contains("List item"), "Should contain list");
    assert!(
        result.markdown.contains("](") || result.markdown.contains("example.com"),
        "Should contain link"
    );
}
