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

use std::sync::LazyLock;

use scraper::{Html, Selector};

static H1_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("h1").expect("static h1 selector"));
static ARTICLE_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("article").expect("static article selector"));
static MAIN_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("main").expect("static main selector"));
static DENSITY_CANDIDATE_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("div, section").expect("static density selector"));
static TITLE_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("title").expect("static title selector"));
static OG_TITLE_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("meta[property='og:title']").expect("static og:title selector")
});
static SUBSTACK_TITLE_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("h1.post-title, .post-title").expect("static substack title selector")
});
static SUBSTACK_SUBTITLE_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".subtitle").expect("static substack subtitle selector"));
static SUBSTACK_BODY_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse(".available-content .body.markup").expect("static substack body selector")
});
static SUBSTACK_NOISE_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse(
        ".subscription-widget-wrap, \
         .subscription-widget-wrap-editor, \
         .subscription-widget, \
         .subscribe-widget, \
         .subscription-widget-subscribe, \
         .post-ufi, \
         .byline-wrapper, \
         form, input, button, iframe, svg, picture, source, img",
    )
    .expect("static substack noise selector")
});

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
    if let Some(substack_result) = extract_substack_article(html) {
        tracing::debug!(
            "substack extraction: {} chars for {}",
            substack_result.text_content.len(),
            url
        );
        return Some(substack_result);
    }

    let readability_result = extract_with_readability_crate(html, url);
    let scraper_result = extract_with_scraper(html);

    // Pick the result with more content.  The readability crate sometimes
    // truncates list-heavy articles (e.g. Ghost blogs with <ol>/<li>), so
    // compare its output with our semantic <article>/<main> extraction and
    // use whichever captured more text.
    match (readability_result, scraper_result) {
        (Some(r), Some(s)) => {
            tracing::debug!(
                "readability: {} chars, scraper: {} chars",
                r.text_content.len(),
                s.text_content.len()
            );
            if s.text_content.len() > r.text_content.len() {
                Some(s)
            } else {
                Some(r)
            }
        }
        (Some(r), None) => Some(r),
        (None, Some(s)) => Some(s),
        (None, None) => None,
    }
}

/// Extract the authored body from Substack's static post DOM.
///
/// Substack pages include the complete post body in `.available-content
/// .body.markup`, but the surrounding `<article>` also contains header UI,
/// reaction buttons, subscribe forms, image `srcset` payloads, recommendations,
/// and comments. Generic readability scoring can therefore pick a node that is
/// technically longer but much less useful to an LLM.
fn extract_substack_article(html: &str) -> Option<Article> {
    let document = Html::parse_document(html);
    let body = document.select(&SUBSTACK_BODY_SELECTOR).next()?;
    let title = extract_substack_title(&document)?;
    let subtitle = extract_substack_subtitle(&document);

    let body_html = strip_substack_noise(&body.html());
    let mut content_html = String::from("<article>");
    content_html.push_str("<h1>");
    content_html.push_str(&escape_html_text(&title));
    content_html.push_str("</h1>");

    if let Some(subtitle) = subtitle.as_ref() {
        content_html.push_str("<p><em>");
        content_html.push_str(&escape_html_text(subtitle));
        content_html.push_str("</em></p>");
    }

    content_html.push_str(&body_html);
    content_html.push_str("</article>");

    let text_content = strip_html_tags(&content_html);
    if text_content.len() < 100 {
        return None;
    }

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

fn extract_substack_title(document: &Html) -> Option<String> {
    document
        .select(&SUBSTACK_TITLE_SELECTOR)
        .find_map(|title| {
            let text = title.text().collect::<Vec<_>>().join(" ");
            let text = normalize_whitespace(&text);
            if text.is_empty() { None } else { Some(text) }
        })
        .or_else(|| extract_og_title(document))
}

fn extract_substack_subtitle(document: &Html) -> Option<String> {
    document
        .select(&SUBSTACK_SUBTITLE_SELECTOR)
        .find_map(|title| {
            let text = title.text().collect::<Vec<_>>().join(" ");
            let text = normalize_whitespace(&text);
            if text.is_empty() { None } else { Some(text) }
        })
}

fn extract_og_title(document: &Html) -> Option<String> {
    document.select(&OG_TITLE_SELECTOR).find_map(|og| {
        og.value().attr("content").and_then(|content| {
            let title = content.trim().to_string();
            if title.is_empty() { None } else { Some(title) }
        })
    })
}

fn strip_substack_noise(html: &str) -> String {
    let document = Html::parse_fragment(html);
    let excluded_ids = document
        .select(&SUBSTACK_NOISE_SELECTOR)
        .map(|el| el.id())
        .collect::<std::collections::HashSet<_>>();

    serialize_children_excluding(&document, document.root_element().id(), &excluded_ids)
}

fn serialize_children_excluding(
    document: &Html,
    parent_id: ego_tree::NodeId,
    exclude: &std::collections::HashSet<ego_tree::NodeId>,
) -> String {
    let Some(node) = document.tree.get(parent_id) else {
        return String::new();
    };
    let mut out = String::new();

    for child in node.children() {
        if exclude.contains(&child.id()) {
            continue;
        }
        match child.value() {
            scraper::Node::Element(el) => {
                out.push('<');
                out.push_str(el.name());
                for (k, v) in el.attrs() {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&v.replace('"', "&quot;"));
                    out.push('"');
                }
                out.push('>');
                out.push_str(&serialize_children_excluding(document, child.id(), exclude));
                if !is_void_element(el.name()) {
                    out.push_str("</");
                    out.push_str(el.name());
                    out.push('>');
                }
            }
            scraper::Node::Text(text) => out.push_str(text),
            _ => {}
        }
    }

    out
}

fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn escape_html_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
    let h1 = document.select(&H1_SELECTOR).next()?;
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
    if let Some(article_elem) = document.select(&ARTICLE_SELECTOR).next() {
        return extract_from_element(article_elem, document);
    }

    // Try <main>
    if let Some(main_elem) = document.select(&MAIN_SELECTOR).next() {
        return extract_from_element(main_elem, document);
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
    let mut scored_elements = Vec::new();

    for element in document.select(&DENSITY_CANDIDATE_SELECTOR) {
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
    scored_elements.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

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
    if let Some(h1) = document.select(&H1_SELECTOR).next() {
        let title = h1.text().collect::<Vec<_>>().join(" ").trim().to_string();
        if !title.is_empty() {
            return title;
        }
    }

    // Try <title>
    if let Some(title) = document.select(&TITLE_SELECTOR).next() {
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

    // Try OpenGraph meta tag
    if let Some(og) = document.select(&OG_TITLE_SELECTOR).next()
        && let Some(content) = og.value().attr("content")
    {
        let title = content.trim().to_string();
        if !title.is_empty() {
            return title;
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

    static DIV_SELECTOR: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("div").expect("static div selector"));

    #[test]
    fn extracts_article_with_semantic_html() {
        let html = r"
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
        ";

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
        let html = r"
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
        ";

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
        let html = r"
            <html>
            <body>
                <div>Short</div>
                <div>Text</div>
            </body>
            </html>
        ";

        // Should return None for pages without substantial content
        let result = extract_article(html, "https://example.com/");
        assert!(result.is_none());
    }

    #[test]
    fn extracts_title_from_h1() {
        let html = r"
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
        ";

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
        let html = r"
            <html>
            <head><title>Page Title</title></head>
            <body>
                <article>
                    <p>Article content without an h1 but with enough text to be extracted as content.</p>
                    <p>Multiple paragraphs present for proper extraction.</p>
                </article>
            </body>
            </html>
        ";

        let article = extract_article(html, "https://example.com/article").unwrap();
        assert_eq!(article.title, "Page Title");
    }

    #[test]
    fn creates_excerpt() {
        let html = r"
            <html>
            <body>
                <article>
                    <p>This is a long article with substantial content that should be extracted. The excerpt should be created from the beginning of this text.</p>
                    <p>More content follows in subsequent paragraphs to ensure proper extraction.</p>
                </article>
            </body>
            </html>
        ";

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
    fn extracts_full_ghost_blog_with_ordered_list() {
        // Ghost CMS produces <article> with <ol><li> content.
        // html2md truncates ordered list items, so the scraper path
        // (which uses element.text()) must capture the full text.
        let html = r#"
            <html>
            <head><title>Porting Software</title></head>
            <body>
                <header><nav>Site Nav</nav></header>
                <main class="site-main">
                    <article class="gh-article post tag-ai">
                        <header class="gh-article-header">
                            <h1 class="gh-article-title">porting software has been trivial</h1>
                        </header>
                        <div class="gh-content gh-canvas">
                            <p>This one is short and sweet. if you want to port a codebase from one language to another here's the approach:</p>
                            <ol>
                                <li>Run a ralph loop which compresses all tests into specs which looks similar to study every file in tests using separate subagents and document in specs and link the implementation as citations in the specification</li>
                                <li>Then do a separate Ralph loop for all product functionality ensuring there are citations to the specification. Study every file in src using separate subagents per file and link the implementation as citations in the specification</li>
                                <li>Once you have that within the same repo run a Ralph loop to create a TODO file and then execute a classic ralph doing just one thing and the most important thing per loop. Remind the agent that it can study the specifications and follow the citations to reference source code.</li>
                                <li>For best outcomes you wanna configure your target language to have strict compilation</li>
                            </ol>
                            <p>The key theory here is usage of citations in the specifications which tease the file_read tool to study the original implementation during stage 3. Reducing stage 1 and stage 2 to specs is the precursor which transforms a code base into high level PRDs without coupling the implementation from the source language.</p>
                        </div>
                    </article>
                </main>
                <section class="newsletter-signup"><p>Subscribe</p></section>
                <footer>Copyright</footer>
            </body>
            </html>
        "#;

        let article = extract_article(html, "https://ghuntley.com/porting/").unwrap();

        // Must contain the conclusion paragraph (currently truncated by html2md)
        assert!(
            article
                .text_content
                .contains("high level PRDs without coupling"),
            "Missing conclusion paragraph. text_content ({} chars): {}",
            article.text_content.len(),
            &article.text_content[..article.text_content.len().min(500)]
        );
        // Must contain list items
        assert!(
            article.text_content.contains("Ralph loop"),
            "Missing list content"
        );
        // Should be substantial (the full article is ~1200+ chars)
        assert!(
            article.text_content.len() > 800,
            "Text too short: {} chars",
            article.text_content.len()
        );
    }

    #[test]
    fn extracts_substack_body_without_post_chrome() {
        let body_1 = "This is the first substantial article paragraph about infrastructure, institutions, and why practical constraints still matter for technological change.";
        let body_2 = "This second article paragraph continues the argument with enough detail to dominate the extraction and leave interface labels below the body ratio budget.";
        let footnote = "This footnote is part of the authored article and should remain available to downstream summarizers.";
        let html = format!(
            r#"
            <html>
            <head>
                <meta property="og:title" content="Open Graph Fallback Title">
                <title>Publisher Homepage</title>
            </head>
            <body>
                <header>
                    <h1>Publication Name</h1>
                    <nav>Home Archive About</nav>
                    <button>Subscribe</button>
                </header>
                <article class="typography newsletter-post post">
                    <div class="post-header">
                        <h1 class="post-title published">Actual Substack Post Title</h1>
                        <h3 class="subtitle">A useful subtitle for the post</h3>
                        <div class="post-ufi">451 90 Share</div>
                        <div class="byline-wrapper">Author avatar and profile chrome</div>
                    </div>
                    <div class="available-content">
                        <div dir="auto" class="body markup">
                            <div class="captioned-image-container">
                                <figure>
                                    <a class="image-link" href="https://cdn.example.com/huge-image.png">
                                        <picture>
                                            <source srcset="very-large-srcset-payload 1x, very-large-srcset-payload 2x">
                                            <img src="https://cdn.example.com/huge-image.png" data-attrs="huge image metadata">
                                        </picture>
                                    </a>
                                    <figcaption>Figure caption text stays if present.</figcaption>
                                </figure>
                            </div>
                            <p>{body_1}</p>
                            <p>{body_2}</p>
                            <div class="subscription-widget-wrap-editor">
                                <p>Subscribe now for more posts.</p>
                                <form><input value="reader@example.com"><button>Subscribe</button></form>
                            </div>
                            <div class="footnote">
                                <a id="footnote-1">1</a>
                                <div class="footnote-content"><p>{footnote}</p></div>
                            </div>
                        </div>
                    </div>
                </article>
                <section class="comments">A long comment thread should not be selected.</section>
            </body>
            </html>
        "#
        );

        let article = extract_article(
            &html,
            "https://writer.substack.com/p/actual-substack-post-title",
        )
        .unwrap();

        assert_eq!(article.title, "Actual Substack Post Title");
        assert!(article.text_content.contains(body_1));
        assert!(article.text_content.contains(body_2));
        assert!(article.text_content.contains(footnote));
        assert!(!article.text_content.contains("Publication Name"));
        assert!(!article.text_content.contains("451"));
        assert!(!article.text_content.contains("Subscribe now"));
        assert!(!article.content_html.contains("srcset"));
        assert!(!article.content_html.contains("huge image metadata"));

        let authored_chars = body_1.len() + body_2.len() + footnote.len();
        #[allow(clippy::cast_precision_loss)]
        let authored_ratio = authored_chars as f64 / article.text_content.len() as f64;
        assert!(
            authored_ratio >= 0.80,
            "authored body ratio too low: {authored_ratio:.2}; text: {}",
            article.text_content
        );
    }

    #[test]
    fn test_is_unlikely_candidate_detects_boilerplate() {
        let html = r#"<div class="navigation">Nav</div>"#;
        let doc = Html::parse_fragment(html);
        let element = doc.select(&DIV_SELECTOR).next().unwrap();
        assert!(is_unlikely_candidate(&element));

        let html = r#"<div class="sidebar">Side</div>"#;
        let doc = Html::parse_fragment(html);
        let element = doc.select(&DIV_SELECTOR).next().unwrap();
        assert!(is_unlikely_candidate(&element));

        let html = r#"<div class="content">Content</div>"#;
        let doc = Html::parse_fragment(html);
        let element = doc.select(&DIV_SELECTOR).next().unwrap();
        assert!(!is_unlikely_candidate(&element));
    }
}
