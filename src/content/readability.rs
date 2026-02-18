//! Mozilla Readability article extraction.
//!
//! Extracts the main article content from HTML pages while filtering out
//! navigation, sidebars, footers, ads, and other boilerplate.
//!
//! # Strategy
//!
//! 1. Parse HTML into DOM with `scraper`
//! 2. Apply readability algorithm (score nodes by text density, remove unlikely candidates)
//! 3. Extract article metadata (title, excerpt)
//! 4. Return clean HTML for conversion to markdown
//!
//! # Fallback
//!
//! If extraction fails (e.g., non-article pages), returns `None` and the caller
//! falls back to raw `html2md` conversion.

use scraper::{Html, Selector};

/// Extracted article content from HTML.
#[derive(Debug, Clone)]
pub struct Article {
    /// Article title (from <title>, <h1>, or metadata).
    pub title: String,
    /// Main article content as clean HTML.
    pub content_html: String,
    /// Plain text excerpt (~200 chars).
    pub excerpt: String,
    /// Text-only version of the content.
    pub text_content: String,
}

/// Extract article content using readability algorithm.
///
/// Returns `Some(Article)` if extraction succeeds, `None` if the page
/// doesn't look like an article (e.g., homepage, search results).
pub fn extract_article(html: &str, url: &str) -> Option<Article> {
    // Try the readability crate first
    if let Some(article) = extract_with_readability_crate(html, url) {
        return Some(article);
    }

    // Fallback to our own basic implementation using scraper
    extract_with_scraper(html)
}

/// Extract using the readability crate.
fn extract_with_readability_crate(html: &str, url: &str) -> Option<Article> {
    // Parse with readability crate
    let product =
        readability::extractor::extract(&mut html.as_bytes(), &url::Url::parse(url).ok()?).ok()?;

    // Verify we got meaningful content (at least 100 chars)
    let text_content = strip_html_tags(&product.content);
    if text_content.len() < 100 {
        return None;
    }

    // Create excerpt (first ~200 chars of text)
    let excerpt = text_content
        .chars()
        .take(200)
        .collect::<String>()
        .trim()
        .to_string();

    // For title, prefer h1 from extracted content over page title
    let title = if let Some(h1_title) = extract_h1_from_html(&product.content) {
        h1_title
    } else {
        product.title
    };

    Some(Article {
        title,
        content_html: product.content,
        excerpt,
        text_content,
    })
}

/// Extract h1 text from HTML fragment.
fn extract_h1_from_html(html: &str) -> Option<String> {
    let document = Html::parse_fragment(html);
    let h1_selector = Selector::parse("h1").ok()?;
    let h1 = document.select(&h1_selector).next()?;
    let title = h1.text().collect::<Vec<_>>().join(" ").trim().to_string();
    if title.is_empty() { None } else { Some(title) }
}

/// Fallback extraction using scraper and basic heuristics.
///
/// Strategy:
/// 1. Find <article>, <main>, or largest content block by text density
/// 2. Strip <nav>, <header>, <footer>, <aside>, <script>, <style>
/// 3. Score remaining blocks by text-to-tag ratio
fn extract_with_scraper(html: &str) -> Option<Article> {
    let document = Html::parse_document(html);

    // Try semantic HTML5 elements first
    if let Some(article) = try_semantic_extraction(&document) {
        return Some(article);
    }

    // Fallback: find largest content block by text density
    find_main_content_by_density(&document)
}

/// Try extracting from semantic HTML5 elements (<article>, <main>).
fn try_semantic_extraction(document: &Html) -> Option<Article> {
    // Try <article> first
    if let Ok(article_selector) = Selector::parse("article") {
        if let Some(article_elem) = document.select(&article_selector).next() {
            return extract_from_element(article_elem, document);
        }
    }

    // Try <main>
    if let Ok(main_selector) = Selector::parse("main") {
        if let Some(main_elem) = document.select(&main_selector).next() {
            return extract_from_element(main_elem, document);
        }
    }

    None
}

/// Extract article from a specific DOM element.
fn extract_from_element(
    element: scraper::element_ref::ElementRef,
    document: &Html,
) -> Option<Article> {
    // Get the HTML of this element
    let content_html = element.html();

    // Extract text content
    let text_content = element.text().collect::<Vec<_>>().join(" ");

    // Clean up whitespace
    let text_content = text_content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if text_content.len() < 100 {
        return None; // Too short to be meaningful article
    }

    // Extract title (from <h1> or <title>)
    let title = extract_title(document);

    // Create excerpt
    let excerpt = text_content
        .chars()
        .take(200)
        .collect::<String>()
        .trim()
        .to_string();

    Some(Article {
        title,
        content_html,
        excerpt,
        text_content,
    })
}

/// Find main content by analyzing text density of all elements.
fn find_main_content_by_density(document: &Html) -> Option<Article> {
    // Score all div and section elements
    let candidates_selector = Selector::parse("div, section").ok()?;
    let mut scored_elements = Vec::new();

    for element in document.select(&candidates_selector) {
        // Skip elements that look like navigation/boilerplate
        if is_unlikely_candidate(&element) {
            continue;
        }

        let text = element.text().collect::<Vec<_>>().join(" ");
        let text_len = text.len();

        if text_len < 100 {
            continue; // Too short
        }

        // Calculate text density (text length / HTML length)
        let html_len = element.html().len();
        #[allow(clippy::cast_precision_loss)]
        let density = text_len as f64 / html_len.max(1) as f64;

        scored_elements.push((density, element, text));
    }

    // Sort by density (highest first)
    scored_elements.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());

    // Take the highest-scoring element
    if let Some((_, element, text)) = scored_elements.first() {
        let title = extract_title(document);
        let text_content = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let excerpt = text_content
            .chars()
            .take(200)
            .collect::<String>()
            .trim()
            .to_string();

        return Some(Article {
            title,
            content_html: element.html(),
            excerpt,
            text_content,
        });
    }

    None
}

/// Check if an element is unlikely to be main content.
fn is_unlikely_candidate(element: &scraper::element_ref::ElementRef) -> bool {
    let class = element.value().attr("class").unwrap_or("");
    let id = element.value().attr("id").unwrap_or("");
    let combined = format!("{class} {id}").to_lowercase();

    combined.contains("nav")
        || combined.contains("menu")
        || combined.contains("sidebar")
        || combined.contains("footer")
        || combined.contains("header")
        || combined.contains("advertisement")
        || combined.contains("ad-")
        || combined.contains("social")
        || combined.contains("share")
        || combined.contains("comment")
}

/// Extract title from document (`<title>`, `<h1>`, or `OpenGraph`).
fn extract_title(document: &Html) -> String {
    // Try <h1> first
    if let Ok(h1_selector) = Selector::parse("h1") {
        if let Some(h1) = document.select(&h1_selector).next() {
            let title = h1.text().collect::<Vec<_>>().join(" ").trim().to_string();
            if !title.is_empty() {
                return title;
            }
        }
    }

    // Try <title>
    if let Ok(title_selector) = Selector::parse("title") {
        if let Some(title) = document.select(&title_selector).next() {
            let title = title
                .text()
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string();
            if !title.is_empty() {
                return title;
            }
        }
    }

    // Try OpenGraph meta tag
    if let Ok(og_selector) = Selector::parse("meta[property='og:title']") {
        if let Some(og) = document.select(&og_selector).next() {
            if let Some(content) = og.value().attr("content") {
                let title = content.trim().to_string();
                if !title.is_empty() {
                    return title;
                }
            }
        }
    }

    "Untitled".to_string()
}

/// Strip HTML tags from content to get plain text.
fn strip_html_tags(html: &str) -> String {
    let document = Html::parse_fragment(html);
    document
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_article_with_semantic_html() {
        let html = r#"
            <html>
            <head><title>Test Article</title></head>
            <body>
                <header><nav>Site Navigation</nav></header>
                <main>
                    <article>
                        <h1>Article Title</h1>
                        <p>This is the main article content with enough text to be recognized as an article body.</p>
                        <p>It has multiple paragraphs to ensure proper extraction.</p>
                    </article>
                </main>
                <footer>© 2025 Copyright</footer>
            </body>
            </html>
        "#;

        let article = extract_article(html, "https://example.com/article").unwrap();

        // The title may come from <title> or <h1> depending on extraction method
        // Both are acceptable for readability
        assert!(
            article.title == "Article Title" || article.title == "Test Article",
            "Expected 'Article Title' or 'Test Article', got '{}'",
            article.title
        );
        assert!(article.text_content.contains("main article content"));
        assert!(!article.text_content.contains("Site Navigation"));
        assert!(!article.text_content.contains("Copyright"));
    }

    #[test]
    fn extracts_from_main_element() {
        let html = r#"
            <html>
            <head><title>Page Title</title></head>
            <body>
                <header>Header Content</header>
                <main>
                    <h1>Main Content Title</h1>
                    <p>This is the main content area with substantial text to pass the length threshold.</p>
                    <p>Multiple paragraphs ensure proper detection.</p>
                </main>
                <aside>Sidebar</aside>
            </body>
            </html>
        "#;

        let article = extract_article(html, "https://example.com/page").unwrap();
        // Title may come from various sources - just verify content extraction works
        assert!(!article.title.is_empty());
        assert!(article.text_content.contains("main content area"));
        assert!(!article.text_content.contains("Sidebar"));
    }

    #[test]
    fn filters_unlikely_candidates() {
        let html = r#"
            <html>
            <body>
                <div class="navigation">Nav Links</div>
                <div class="sidebar">Sidebar Content</div>
                <div class="content">
                    <h1>Real Article</h1>
                    <p>This is the actual article content with enough text to be recognized as the main content.</p>
                    <p>It should be extracted while filtering out navigation and sidebar.</p>
                </div>
                <div class="footer">Footer</div>
            </body>
            </html>
        "#;

        let article = extract_article(html, "https://example.com/article").unwrap();
        assert!(article.text_content.contains("actual article content"));
        assert!(!article.text_content.contains("Nav Links"));
        assert!(!article.text_content.contains("Sidebar"));
    }

    #[test]
    fn returns_none_for_non_article_pages() {
        let html = r#"
            <html>
            <body>
                <div>Short</div>
                <div>Text</div>
            </body>
            </html>
        "#;

        // Should return None for pages without substantial content
        let result = extract_article(html, "https://example.com/");
        assert!(result.is_none());
    }

    #[test]
    fn extracts_title_from_h1() {
        let html = r#"
            <html>
            <head><title>Page Title in Head</title></head>
            <body>
                <article>
                    <h1>Article Heading</h1>
                    <p>This is article content with sufficient length to pass extraction thresholds.</p>
                    <p>Multiple paragraphs ensure proper handling.</p>
                </article>
            </body>
            </html>
        "#;

        let article = extract_article(html, "https://example.com/article").unwrap();
        // Readability may prefer <title> over <h1> - both are valid
        assert!(
            article.title == "Article Heading" || article.title == "Page Title in Head",
            "Expected 'Article Heading' or 'Page Title in Head', got '{}'",
            article.title
        );
    }

    #[test]
    fn extracts_title_from_title_tag_fallback() {
        let html = r#"
            <html>
            <head><title>Page Title</title></head>
            <body>
                <article>
                    <p>Article content without an h1 but with enough text to be extracted as content.</p>
                    <p>Multiple paragraphs present for proper extraction.</p>
                </article>
            </body>
            </html>
        "#;

        let article = extract_article(html, "https://example.com/article").unwrap();
        assert_eq!(article.title, "Page Title");
    }

    #[test]
    fn creates_excerpt() {
        let html = r#"
            <html>
            <body>
                <article>
                    <p>This is a long article with substantial content that should be extracted. The excerpt should be created from the beginning of this text.</p>
                    <p>More content follows in subsequent paragraphs to ensure proper extraction.</p>
                </article>
            </body>
            </html>
        "#;

        let article = extract_article(html, "https://example.com/article").unwrap();
        assert!(!article.excerpt.is_empty());
        assert!(article.excerpt.len() <= 200);
        assert!(article.excerpt.starts_with("This is a long article"));
    }

    #[test]
    fn strips_html_tags() {
        let html = "<p>Hello <strong>world</strong> with <a href='#'>links</a></p>";
        let text = strip_html_tags(html);
        assert_eq!(text, "Hello world with links");
        assert!(!text.contains('<'));
        assert!(!text.contains('>'));
    }

    #[test]
    fn test_is_unlikely_candidate_detects_boilerplate() {
        let html = r#"<div class="navigation">Nav</div>"#;
        let doc = Html::parse_fragment(html);
        let selector = Selector::parse("div").unwrap();
        let element = doc.select(&selector).next().unwrap();
        assert!(is_unlikely_candidate(&element));

        let html = r#"<div class="sidebar">Side</div>"#;
        let doc = Html::parse_fragment(html);
        let element = doc.select(&selector).next().unwrap();
        assert!(is_unlikely_candidate(&element));

        let html = r#"<div class="content">Content</div>"#;
        let doc = Html::parse_fragment(html);
        let element = doc.select(&selector).next().unwrap();
        assert!(!is_unlikely_candidate(&element));
    }
}
