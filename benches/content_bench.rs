//! Benchmarks for HTML-to-markdown conversion at varying payload sizes.
//!
//! Run with: `cargo bench --bench content_bench`

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nab::content::html::html_to_markdown;
use nab::content::ContentRouter;

/// Generate a realistic HTML document of approximately `target_bytes`.
///
/// Produces a well-formed document with headings, paragraphs, links, lists,
/// and boilerplate elements that exercise both the html2md parser and the
/// post-processing boilerplate filter.
fn generate_html(target_bytes: usize) -> String {
    let boilerplate_header = r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="UTF-8"><title>Benchmark Page</title></head>
<body>
<nav>
  <ul><li><a href="/">Home</a></li><li><a href="/about">About</a></li></ul>
</nav>
<div class="cookie-banner"><p>We use cookies to improve your experience.</p></div>
<main>
"#;

    let boilerplate_footer = r#"
</main>
<footer>
  <p>Skip to content</p>
  <p>&copy; 2025 Benchmark Corp. All rights reserved.</p>
  <p>Privacy Policy | Terms of Service</p>
</footer>
</body>
</html>"#;

    let paragraph = "<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
        Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
        Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.</p>\n";

    let heading = "<h2>Section Heading</h2>\n";

    let link_block =
        r#"<p>See <a href="https://example.com/article">this article</a> for more details.</p>"#;

    let list_block = "<ul>\n\
        <li>First item with some text</li>\n\
        <li>Second item with more text</li>\n\
        <li>Third item closing out the list</li>\n\
        </ul>\n";

    let mut html = String::with_capacity(target_bytes + 1024);
    html.push_str(boilerplate_header);

    let blocks = [
        heading, paragraph, paragraph, link_block, "\n", list_block, paragraph,
    ];
    let mut block_idx = 0;

    while html.len() < target_bytes {
        html.push_str(blocks[block_idx % blocks.len()]);
        block_idx += 1;
    }

    html.push_str(boilerplate_footer);
    html
}

fn bench_html_to_markdown(c: &mut Criterion) {
    let mut group = c.benchmark_group("html_to_markdown");

    let sizes: &[(usize, &str)] = &[
        (1_024, "1KB"),
        (10_240, "10KB"),
        (51_200, "50KB"),
        (204_800, "200KB"),
    ];

    for &(size, label) in sizes {
        let html = generate_html(size);

        group.throughput(Throughput::Bytes(html.len() as u64));
        group.bench_with_input(BenchmarkId::new("convert", label), &html, |b, html| {
            b.iter(|| black_box(html_to_markdown(black_box(html))));
        });
    }

    group.finish();
}

fn bench_content_router_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("content_router_dispatch");

    // Small HTML payload — measures router overhead + conversion
    let small_html = generate_html(1_024);
    let small_bytes = small_html.as_bytes();

    group.bench_function("html_1kb", |b| {
        let router = ContentRouter::new();
        b.iter(|| black_box(router.convert(black_box(small_bytes), "text/html").unwrap()));
    });

    // Medium HTML payload
    let medium_html = generate_html(51_200);
    let medium_bytes = medium_html.as_bytes();

    group.bench_function("html_50kb", |b| {
        let router = ContentRouter::new();
        b.iter(|| {
            black_box(
                router
                    .convert(black_box(medium_bytes), "text/html")
                    .unwrap(),
            )
        });
    });

    // Plain text (passthrough, baseline)
    let plain = "Hello, world! This is plain text content.".repeat(25);
    let plain_bytes = plain.as_bytes();

    group.bench_function("plain_text_1kb", |b| {
        let router = ContentRouter::new();
        b.iter(|| {
            black_box(
                router
                    .convert(black_box(plain_bytes), "text/plain")
                    .unwrap(),
            )
        });
    });

    // JSON passthrough
    let json = r#"{"key": "value", "items": [1, 2, 3, 4, 5]}"#.repeat(25);
    let json_bytes = json.as_bytes();

    group.bench_function("json_1kb", |b| {
        let router = ContentRouter::new();
        b.iter(|| {
            black_box(
                router
                    .convert(black_box(json_bytes), "application/json")
                    .unwrap(),
            )
        });
    });

    // Content-type with charset parameter (exercises mime parsing)
    group.bench_function("html_with_charset", |b| {
        let router = ContentRouter::new();
        b.iter(|| {
            black_box(
                router
                    .convert(black_box(small_bytes), "text/html; charset=utf-8")
                    .unwrap(),
            )
        });
    });

    // Fallback path: unknown content type but HTML bytes
    group.bench_function("html_fallback_detection", |b| {
        let router = ContentRouter::new();
        b.iter(|| {
            black_box(
                router
                    .convert(black_box(small_bytes), "application/octet-stream")
                    .unwrap(),
            )
        });
    });

    group.finish();
}

fn bench_boilerplate_filter(c: &mut Criterion) {
    let mut group = c.benchmark_group("boilerplate_filter");

    // HTML with heavy boilerplate — measures filtering efficiency
    let boilerplate_heavy = r#"<!DOCTYPE html>
<html><body>
<p>Skip to content</p>
<p>We use cookies on this site.</p>
<p>Privacy Policy agreement required.</p>
<p>Terms of Service apply.</p>
<p>&copy; 2025 Company Name</p>
<p>Copyright holder information</p>
<h1>Real Content Title</h1>
<p>This is the actual content that should be preserved in the output.</p>
<p>More real content with <a href="https://example.com">a useful link</a>.</p>
</body></html>"#;

    group.bench_function("heavy_boilerplate", |b| {
        b.iter(|| black_box(html_to_markdown(black_box(boilerplate_heavy))));
    });

    // HTML with minimal boilerplate
    let clean_html = r#"<!DOCTYPE html>
<html><body>
<h1>Title</h1>
<p>First paragraph of clean content.</p>
<p>Second paragraph with <a href="/link">a link</a>.</p>
<ul><li>Item one</li><li>Item two</li></ul>
</body></html>"#;

    group.bench_function("minimal_boilerplate", |b| {
        b.iter(|| black_box(html_to_markdown(black_box(clean_html))));
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_html_to_markdown,
    bench_content_router_dispatch,
    bench_boilerplate_filter,
);

criterion_main!(benches);
