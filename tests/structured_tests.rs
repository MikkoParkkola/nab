//! Integration tests for schema-guided structured data extraction.
//!
//! Tests cover all extraction sources (JSON-LD, Open Graph, Twitter Card,
//! meta tags, microdata, CSS selectors) and type coercion.

use nab::content::structured::{
    DataSource, ExtractionError, ExtractionSchema, FieldType, extract_structured,
};

// ── Schema parsing ───────────────────────────────────────────────────────────

#[test]
fn schema_simple_types() {
    let schema = ExtractionSchema::from_json(
        r#"{"title": "string", "price": "number", "active": "boolean", "tags": "array"}"#,
    )
    .unwrap();
    assert_eq!(schema.fields.len(), 4);
    assert_eq!(schema.fields["title"].field_type, FieldType::String);
    assert_eq!(schema.fields["price"].field_type, FieldType::Number);
    assert_eq!(schema.fields["active"].field_type, FieldType::Boolean);
    assert_eq!(schema.fields["tags"].field_type, FieldType::Array);
}

#[test]
fn schema_advanced_with_selectors() {
    let schema = ExtractionSchema::from_json(
        r#"{
            "title": {"type": "string", "selector": "h1.product-name"},
            "price": {"type": "number", "selector": ".price-tag", "attribute": "data-value"}
        }"#,
    )
    .unwrap();

    assert_eq!(
        schema.fields["title"].selector.as_deref(),
        Some("h1.product-name")
    );
    assert!(schema.fields["title"].attribute.is_none());
    assert_eq!(
        schema.fields["price"].selector.as_deref(),
        Some(".price-tag")
    );
    assert_eq!(
        schema.fields["price"].attribute.as_deref(),
        Some("data-value")
    );
}

#[test]
fn schema_type_aliases() {
    let schema = ExtractionSchema::from_json(
        r#"{"a": "int", "b": "float", "c": "bool", "d": "list", "e": "dict"}"#,
    )
    .unwrap();
    assert_eq!(schema.fields["a"].field_type, FieldType::Number);
    assert_eq!(schema.fields["b"].field_type, FieldType::Number);
    assert_eq!(schema.fields["c"].field_type, FieldType::Boolean);
    assert_eq!(schema.fields["d"].field_type, FieldType::Array);
    assert_eq!(schema.fields["e"].field_type, FieldType::Object);
}

#[test]
fn schema_rejects_non_object() {
    let result = ExtractionSchema::from_json("[1, 2, 3]");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, ExtractionError::InvalidSchema(_)));
}

#[test]
fn schema_rejects_invalid_field_value() {
    let result = ExtractionSchema::from_json(r#"{"title": 42}"#);
    assert!(result.is_err());
}

// ── JSON-LD extraction ───────────────────────────────────────────────────────

#[test]
fn extract_from_jsonld_product() {
    let html = r#"
        <html><head>
            <script type="application/ld+json">
            {
                "@context": "https://schema.org",
                "@type": "Product",
                "name": "Widget Pro",
                "description": "The best widget money can buy",
                "offers": {
                    "@type": "Offer",
                    "price": 29.99,
                    "priceCurrency": "USD",
                    "availability": "https://schema.org/InStock"
                },
                "aggregateRating": {
                    "@type": "AggregateRating",
                    "ratingValue": 4.7,
                    "reviewCount": 128
                }
            }
            </script>
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(
        r#"{"name": "string", "price": "number", "rating": "number", "description": "string"}"#,
    )
    .unwrap();

    let result = extract_structured(html, &schema);

    assert_eq!(result.fields["name"].as_str(), Some("Widget Pro"));
    assert_eq!(result.fields["price"].as_f64(), Some(29.99));
    assert_eq!(result.fields["rating"].as_f64(), Some(4.7));
    assert!(
        result.fields["description"]
            .as_str()
            .unwrap()
            .contains("best widget")
    );
    assert_eq!(result.sources["name"], DataSource::JsonLd);
    assert!(result.missing.is_empty());
}

#[test]
fn extract_from_jsonld_article() {
    let html = r#"
        <html><head>
            <script type="application/ld+json">
            {
                "@context": "https://schema.org",
                "@type": "BlogPosting",
                "headline": "How to Build Widgets",
                "author": {"@type": "Person", "name": "Jane Smith"},
                "datePublished": "2026-01-15",
                "articleBody": "Widgets are fundamental components in modern engineering..."
            }
            </script>
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(
        r#"{"title": "string", "author": "string", "published": "string"}"#,
    )
    .unwrap();

    let result = extract_structured(html, &schema);

    // "title" should find "headline" via alias
    assert_eq!(
        result.fields["title"].as_str(),
        Some("How to Build Widgets")
    );
    // "author" should find nested author.name
    assert_eq!(result.fields["author"].as_str(), Some("Jane Smith"));
    // "published" should find "datePublished" via alias
    assert_eq!(result.fields["published"].as_str(), Some("2026-01-15"));
}

#[test]
fn extract_from_jsonld_array() {
    let html = r#"
        <html><head>
            <script type="application/ld+json">
            [
                {"@context": "https://schema.org", "@type": "WebSite", "name": "Example"},
                {"@context": "https://schema.org", "@type": "Product", "name": "Widget", "offers": {"price": 9.99}}
            ]
            </script>
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(r#"{"name": "string", "price": "number"}"#).unwrap();
    let result = extract_structured(html, &schema);

    // Should find "name" from the first object and "price" from the second
    assert!(result.fields.contains_key("name"));
    assert_eq!(result.fields["price"].as_f64(), Some(9.99));
}

// ── Open Graph extraction ────────────────────────────────────────────────────

#[test]
fn extract_from_og_tags() {
    let html = r#"
        <html><head>
            <meta property="og:title" content="Widget Pro - Best Widget">
            <meta property="og:description" content="Premium widget for professionals">
            <meta property="og:image" content="https://example.com/widget.jpg">
            <meta property="og:url" content="https://example.com/widget-pro">
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(
        r#"{"title": "string", "description": "string", "image": "string", "url": "string"}"#,
    )
    .unwrap();

    let result = extract_structured(html, &schema);

    assert_eq!(
        result.fields["title"].as_str(),
        Some("Widget Pro - Best Widget")
    );
    assert!(
        result.fields["description"]
            .as_str()
            .unwrap()
            .contains("Premium widget")
    );
    assert_eq!(
        result.fields["image"].as_str(),
        Some("https://example.com/widget.jpg")
    );
    assert_eq!(result.sources["title"], DataSource::OpenGraph);
}

// ── Twitter Card extraction ──────────────────────────────────────────────────

#[test]
fn extract_from_twitter_tags() {
    let html = r#"
        <html><head>
            <meta name="twitter:title" content="Widget Pro on Twitter">
            <meta name="twitter:description" content="Get the best widget today">
            <meta name="twitter:image" content="https://example.com/tw-widget.jpg">
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(
        r#"{"title": "string", "description": "string", "image": "string"}"#,
    )
    .unwrap();

    let result = extract_structured(html, &schema);

    assert_eq!(
        result.fields["title"].as_str(),
        Some("Widget Pro on Twitter")
    );
    assert_eq!(result.sources["title"], DataSource::TwitterCard);
}

// ── Meta tag extraction ──────────────────────────────────────────────────────

#[test]
fn extract_from_meta_tags() {
    let html = r#"
        <html><head>
            <meta name="description" content="A fantastic product for everyone">
            <meta name="author" content="John Doe">
            <meta name="keywords" content="widget, product, tool">
        </head><body></body></html>
    "#;

    let schema =
        ExtractionSchema::from_json(r#"{"description": "string", "author": "string"}"#).unwrap();

    let result = extract_structured(html, &schema);

    assert!(
        result.fields["description"]
            .as_str()
            .unwrap()
            .contains("fantastic product")
    );
    assert_eq!(result.fields["author"].as_str(), Some("John Doe"));
    assert_eq!(result.sources["description"], DataSource::MetaTag);
    assert_eq!(result.sources["author"], DataSource::MetaTag);
}

// ── Microdata extraction ─────────────────────────────────────────────────────

#[test]
fn extract_from_microdata() {
    let html = r#"
        <html><body>
            <div itemscope itemtype="https://schema.org/Product">
                <h1 itemprop="name">Widget Deluxe</h1>
                <span itemprop="price" content="49.99">$49.99</span>
                <meta itemprop="priceCurrency" content="USD">
                <span itemprop="description">Deluxe edition of our popular widget</span>
            </div>
        </body></html>
    "#;

    let schema = ExtractionSchema::from_json(
        r#"{"name": "string", "price": "number", "description": "string"}"#,
    )
    .unwrap();

    let result = extract_structured(html, &schema);

    assert_eq!(result.fields["name"].as_str(), Some("Widget Deluxe"));
    assert_eq!(result.fields["price"].as_f64(), Some(49.99));
    assert_eq!(result.sources["name"], DataSource::Microdata);
}

// ── CSS selector extraction (explicit) ───────────────────────────────────────

#[test]
fn extract_with_explicit_css_selectors() {
    let html = r#"
        <html><body>
            <h1 class="product-title">Custom Widget</h1>
            <span class="price-tag" data-amount="19.99">$19.99</span>
            <div class="product-desc">A custom widget for custom needs</div>
        </body></html>
    "#;

    let schema = ExtractionSchema::from_json(
        r#"{
            "title": {"type": "string", "selector": "h1.product-title"},
            "price": {"type": "number", "selector": ".price-tag", "attribute": "data-amount"},
            "description": {"type": "string", "selector": ".product-desc"}
        }"#,
    )
    .unwrap();

    let result = extract_structured(html, &schema);

    assert_eq!(result.fields["title"].as_str(), Some("Custom Widget"));
    assert_eq!(result.fields["price"].as_f64(), Some(19.99));
    assert_eq!(
        result.fields["description"].as_str(),
        Some("A custom widget for custom needs")
    );
    assert_eq!(result.sources["title"], DataSource::CssSelector);
    assert_eq!(result.sources["price"], DataSource::CssSelector);
}

// ── CSS selector heuristic fallback ──────────────────────────────────────────

#[test]
fn extract_title_from_h1_heuristic() {
    let html = r"
        <html><body>
            <h1>My Great Product</h1>
            <p>Some description text here</p>
        </body></html>
    ";

    let schema = ExtractionSchema::from_json(r#"{"title": "string"}"#).unwrap();
    let result = extract_structured(html, &schema);

    assert_eq!(result.fields["title"].as_str(), Some("My Great Product"));
    assert_eq!(result.sources["title"], DataSource::CssSelector);
}

// ── Priority ordering ────────────────────────────────────────────────────────

#[test]
fn jsonld_takes_priority_over_og_tags() {
    let html = r#"
        <html><head>
            <meta property="og:title" content="OG Title">
            <script type="application/ld+json">
            {
                "@context": "https://schema.org",
                "@type": "Product",
                "name": "JSON-LD Title"
            }
            </script>
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(r#"{"title": "string"}"#).unwrap();
    let result = extract_structured(html, &schema);

    // JSON-LD should win (alias: title -> name)
    assert_eq!(result.fields["title"].as_str(), Some("JSON-LD Title"));
    assert_eq!(result.sources["title"], DataSource::JsonLd);
}

#[test]
fn og_takes_priority_over_meta_tags() {
    let html = r#"
        <html><head>
            <meta name="description" content="Meta description">
            <meta property="og:description" content="OG description">
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(r#"{"description": "string"}"#).unwrap();
    let result = extract_structured(html, &schema);

    assert_eq!(
        result.fields["description"].as_str(),
        Some("OG description")
    );
    assert_eq!(result.sources["description"], DataSource::OpenGraph);
}

#[test]
fn css_selector_override_takes_highest_priority() {
    let html = r#"
        <html><head>
            <meta property="og:title" content="OG Title">
            <script type="application/ld+json">
            {"@context": "https://schema.org", "@type": "Product", "name": "JSON-LD Name"}
            </script>
        </head><body>
            <span class="custom-title">CSS Title</span>
        </body></html>
    "#;

    let schema = ExtractionSchema::from_json(
        r#"{"title": {"type": "string", "selector": ".custom-title"}}"#,
    )
    .unwrap();
    let result = extract_structured(html, &schema);

    assert_eq!(result.fields["title"].as_str(), Some("CSS Title"));
    assert_eq!(result.sources["title"], DataSource::CssSelector);
}

// ── Missing fields ───────────────────────────────────────────────────────────

#[test]
fn reports_missing_fields() {
    let html = r#"
        <html><head>
            <meta property="og:title" content="Product Name">
        </head><body></body></html>
    "#;

    let schema =
        ExtractionSchema::from_json(r#"{"title": "string", "price": "number", "sku": "string"}"#)
            .unwrap();

    let result = extract_structured(html, &schema);

    assert!(result.fields.contains_key("title"));
    assert!(!result.fields.contains_key("price"));
    // Missing should contain price and sku
    assert!(result.missing.contains(&"price".to_string()));
    assert!(result.missing.contains(&"sku".to_string()));
}

// ── Type coercion ────────────────────────────────────────────────────────────

#[test]
fn coerce_jsonld_number_string_to_number() {
    let html = r#"
        <html><head>
            <script type="application/ld+json">
            {"@type": "Product", "name": "Widget", "offers": {"price": "24.95"}}
            </script>
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(r#"{"price": "number"}"#).unwrap();
    let result = extract_structured(html, &schema);

    assert_eq!(result.fields["price"].as_f64(), Some(24.95));
}

#[test]
fn coerce_to_boolean() {
    let html = r#"
        <html><head>
            <script type="application/ld+json">
            {"@type": "Product", "name": "Widget", "offers": {"availability": "true"}}
            </script>
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(r#"{"availability": "boolean"}"#).unwrap();
    let result = extract_structured(html, &schema);

    assert_eq!(result.fields["availability"].as_bool(), Some(true));
}

// ── JSON serialization ───────────────────────────────────────────────────────

#[test]
fn result_to_json() {
    let html = r#"
        <html><head>
            <meta property="og:title" content="Test Product">
            <meta property="og:description" content="A test product">
        </head><body></body></html>
    "#;

    let schema =
        ExtractionSchema::from_json(r#"{"title": "string", "description": "string"}"#).unwrap();

    let result = extract_structured(html, &schema);
    let json_str = result.to_json().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    assert_eq!(parsed["title"].as_str(), Some("Test Product"));
    assert_eq!(parsed["description"].as_str(), Some("A test product"));
}

#[test]
fn result_to_json_full_includes_sources() {
    let html = r#"
        <html><head>
            <meta property="og:title" content="Test">
        </head><body></body></html>
    "#;

    let schema =
        ExtractionSchema::from_json(r#"{"title": "string", "missing_field": "string"}"#).unwrap();

    let result = extract_structured(html, &schema);
    let json_str = result.to_json_full().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    assert!(parsed["fields"]["title"].is_string());
    assert!(parsed["sources"]["title"].is_string());
    assert!(parsed["missing"].as_array().unwrap().len() == 1);
}

// ── Complex real-world-like pages ────────────────────────────────────────────

#[test]
fn extract_from_e_commerce_product_page() {
    let html = r#"
        <html>
        <head>
            <title>Widget Pro - Premium Widget | WidgetStore</title>
            <meta property="og:title" content="Widget Pro - Premium Widget">
            <meta property="og:description" content="The ultimate widget for professionals">
            <meta property="og:image" content="https://widgetstore.com/img/widget-pro.jpg">
            <meta property="og:url" content="https://widgetstore.com/products/widget-pro">
            <meta name="twitter:title" content="Widget Pro">
            <script type="application/ld+json">
            {
                "@context": "https://schema.org",
                "@type": "Product",
                "name": "Widget Pro",
                "description": "The ultimate widget for professionals. Built with aerospace-grade materials.",
                "image": "https://widgetstore.com/img/widget-pro.jpg",
                "sku": "WP-2026",
                "brand": {"@type": "Brand", "name": "WidgetCo"},
                "offers": {
                    "@type": "Offer",
                    "price": 149.99,
                    "priceCurrency": "USD",
                    "availability": "https://schema.org/InStock"
                },
                "aggregateRating": {
                    "@type": "AggregateRating",
                    "ratingValue": 4.8,
                    "reviewCount": 2048
                }
            }
            </script>
        </head>
        <body>
            <div itemscope itemtype="https://schema.org/Product">
                <h1 itemprop="name">Widget Pro</h1>
                <span itemprop="price" content="149.99">$149.99</span>
            </div>
        </body>
        </html>
    "#;

    let schema = ExtractionSchema::from_json(
        r#"{
            "name": "string",
            "price": "number",
            "rating": "number",
            "description": "string",
            "image": "string",
            "sku": "string",
            "brand": "string",
            "currency": "string"
        }"#,
    )
    .unwrap();

    let result = extract_structured(html, &schema);

    assert_eq!(result.fields["name"].as_str(), Some("Widget Pro"));
    assert_eq!(result.fields["price"].as_f64(), Some(149.99));
    assert_eq!(result.fields["rating"].as_f64(), Some(4.8));
    assert!(
        result.fields["description"]
            .as_str()
            .unwrap()
            .contains("ultimate widget")
    );
    assert_eq!(
        result.fields["image"].as_str(),
        Some("https://widgetstore.com/img/widget-pro.jpg")
    );
    assert_eq!(result.fields["sku"].as_str(), Some("WP-2026"));
    assert_eq!(result.fields["brand"].as_str(), Some("WidgetCo"));
    assert_eq!(result.fields["currency"].as_str(), Some("USD"));
    assert!(
        result.missing.is_empty(),
        "no fields should be missing: {:?}",
        result.missing
    );
}

#[test]
fn extract_from_blog_post_page() {
    let html = r#"
        <html>
        <head>
            <meta property="og:title" content="Understanding Rust Lifetimes">
            <meta property="og:description" content="A deep dive into Rust's lifetime system">
            <meta property="og:type" content="article">
            <meta property="article:published_time" content="2026-03-01T10:00:00Z">
            <meta name="author" content="Alice Rustacean">
            <script type="application/ld+json">
            {
                "@context": "https://schema.org",
                "@type": "BlogPosting",
                "headline": "Understanding Rust Lifetimes",
                "author": {"@type": "Person", "name": "Alice Rustacean"},
                "datePublished": "2026-03-01",
                "articleBody": "Lifetimes are one of Rust's most distinctive features..."
            }
            </script>
        </head>
        <body>
            <article>
                <h1>Understanding Rust Lifetimes</h1>
                <time datetime="2026-03-01">March 1, 2026</time>
                <p>Lifetimes are one of Rust's most distinctive features...</p>
            </article>
        </body>
        </html>
    "#;

    let schema = ExtractionSchema::from_json(
        r#"{"title": "string", "author": "string", "published": "string"}"#,
    )
    .unwrap();

    let result = extract_structured(html, &schema);

    assert_eq!(
        result.fields["title"].as_str(),
        Some("Understanding Rust Lifetimes")
    );
    assert_eq!(result.fields["author"].as_str(), Some("Alice Rustacean"));
    assert!(result.fields.contains_key("published"));
}

#[test]
fn extract_empty_page_reports_all_missing() {
    let html = "<html><head></head><body></body></html>";
    let schema = ExtractionSchema::from_json(r#"{"title": "string", "price": "number"}"#).unwrap();

    let result = extract_structured(html, &schema);

    assert!(result.fields.is_empty());
    assert_eq!(result.missing.len(), 2);
}

// ── Edge cases ───────────────────────────────────────────────────────────────

#[test]
fn handles_malformed_jsonld_gracefully() {
    let html = r#"
        <html><head>
            <script type="application/ld+json">
            { this is not valid json
            </script>
            <meta property="og:title" content="Fallback Title">
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(r#"{"title": "string"}"#).unwrap();
    let result = extract_structured(html, &schema);

    // Should fall through to OG tag
    assert_eq!(result.fields["title"].as_str(), Some("Fallback Title"));
    assert_eq!(result.sources["title"], DataSource::OpenGraph);
}

#[test]
fn handles_multiple_jsonld_blocks() {
    let html = r#"
        <html><head>
            <script type="application/ld+json">
            {"@type": "WebSite", "name": "Example Site"}
            </script>
            <script type="application/ld+json">
            {"@type": "Product", "name": "Actual Product", "offers": {"price": 42}}
            </script>
        </head><body></body></html>
    "#;

    let schema = ExtractionSchema::from_json(r#"{"name": "string", "price": "number"}"#).unwrap();
    let result = extract_structured(html, &schema);

    assert!(result.fields.contains_key("name"));
    assert_eq!(result.fields["price"].as_f64(), Some(42.0));
}

#[test]
fn handles_empty_schema() {
    let html =
        r#"<html><head><meta property="og:title" content="Title"></head><body></body></html>"#;
    let schema = ExtractionSchema::from_json("{}").unwrap();
    let result = extract_structured(html, &schema);

    assert!(result.fields.is_empty());
    assert!(result.missing.is_empty());
}
