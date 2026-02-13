//! Benchmarks for `SiteRouter` and `ContentRouter` dispatch.
//!
//! Measures the cost of URL pattern matching across site providers
//! and content-type dispatch for the content router.
//!
//! Run with: `cargo bench --bench router_bench`

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nab::content::ContentRouter;
use nab::site::github::GitHubProvider;
use nab::site::hackernews::HackerNewsProvider;
use nab::site::instagram::InstagramProvider;
use nab::site::reddit::RedditProvider;
use nab::site::youtube::YouTubeProvider;
use nab::site::{SiteProvider, SiteRouter};

// ---------------------------------------------------------------------------
// URL datasets
// ---------------------------------------------------------------------------

/// URLs that match specific providers.
const REDDIT_URLS: &[&str] = &[
    "https://reddit.com/r/rust/comments/abc123/my_post_title",
    "https://old.reddit.com/r/programming/comments/xyz789/title",
    "https://www.reddit.com/r/linux/comments/def456/discussion",
];

const HN_URLS: &[&str] = &[
    "https://news.ycombinator.com/item?id=38471822",
    "https://NEWS.YCOMBINATOR.COM/ITEM?ID=999",
    "https://news.ycombinator.com/item?id=12345&foo=bar",
];

const GITHUB_URLS: &[&str] = &[
    "https://github.com/rust-lang/rust/issues/12345",
    "https://github.com/tokio-rs/tokio/pull/67890",
    "https://GITHUB.COM/owner/repo/ISSUES/999",
];

const INSTAGRAM_URLS: &[&str] = &[
    "https://instagram.com/p/ABC123xyz",
    "https://www.instagram.com/reel/XYZ789abc",
    "https://INSTAGRAM.COM/P/test123",
];

const YOUTUBE_URLS: &[&str] = &[
    "https://youtube.com/watch?v=dQw4w9WgXcQ",
    "https://youtu.be/dQw4w9WgXcQ",
    "https://www.youtube.com/watch?v=ABC123",
];

/// URLs that should NOT match any provider.
const NON_MATCHING_URLS: &[&str] = &[
    "https://example.com/page",
    "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "https://docs.rs/tokio/latest/tokio/",
    "https://stackoverflow.com/questions/12345",
    "https://linkedin.com/in/someone",
    "https://medium.com/@user/article-title-123abc",
    "https://nytimes.com/2025/01/01/technology/ai.html",
    "https://arxiv.org/abs/2301.12345",
];

/// URLs with patterns that could cause false positives.
const EDGE_CASE_URLS: &[&str] = &[
    "https://x.com/naval",                         // Twitter profile, not status
    "https://reddit.com/r/rust",                    // Subreddit, not comments
    "https://github.com/rust-lang/rust",            // Repo root, not issue
    "https://instagram.com/username",               // Profile, not post
    "https://youtube.com/channel/UCxyz",            // Channel, not video
    "https://news.ycombinator.com/",                // HN front page, not item
    "https://example.com/status/fake",              // "status" in wrong context
    "https://not-github.com/owner/repo/issues/1",  // Similar path, wrong domain
];

// ---------------------------------------------------------------------------
// Individual provider matching benchmarks
// ---------------------------------------------------------------------------

fn bench_reddit_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("provider_match_reddit");
    let provider = RedditProvider;

    group.bench_function("hit", |b| {
        b.iter(|| {
            for url in REDDIT_URLS {
                black_box(provider.matches(black_box(url)));
            }
        });
    });

    group.bench_function("miss", |b| {
        b.iter(|| {
            for url in NON_MATCHING_URLS {
                black_box(provider.matches(black_box(url)));
            }
        });
    });

    group.finish();
}

fn bench_github_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("provider_match_github");
    let provider = GitHubProvider;

    group.bench_function("hit", |b| {
        b.iter(|| {
            for url in GITHUB_URLS {
                black_box(provider.matches(black_box(url)));
            }
        });
    });

    group.bench_function("miss", |b| {
        b.iter(|| {
            for url in NON_MATCHING_URLS {
                black_box(provider.matches(black_box(url)));
            }
        });
    });

    group.finish();
}

fn bench_hackernews_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("provider_match_hackernews");
    let provider = HackerNewsProvider;

    group.bench_function("hit", |b| {
        b.iter(|| {
            for url in HN_URLS {
                black_box(provider.matches(black_box(url)));
            }
        });
    });

    group.bench_function("miss", |b| {
        b.iter(|| {
            for url in NON_MATCHING_URLS {
                black_box(provider.matches(black_box(url)));
            }
        });
    });

    group.finish();
}

fn bench_instagram_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("provider_match_instagram");
    let provider = InstagramProvider;

    group.bench_function("hit", |b| {
        b.iter(|| {
            for url in INSTAGRAM_URLS {
                black_box(provider.matches(black_box(url)));
            }
        });
    });

    group.bench_function("miss", |b| {
        b.iter(|| {
            for url in NON_MATCHING_URLS {
                black_box(provider.matches(black_box(url)));
            }
        });
    });

    group.finish();
}

fn bench_youtube_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("provider_match_youtube");
    let provider = YouTubeProvider;

    group.bench_function("hit", |b| {
        b.iter(|| {
            for url in YOUTUBE_URLS {
                black_box(provider.matches(black_box(url)));
            }
        });
    });

    group.bench_function("miss", |b| {
        b.iter(|| {
            for url in NON_MATCHING_URLS {
                black_box(provider.matches(black_box(url)));
            }
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Full router benchmarks (all providers iterated)
// ---------------------------------------------------------------------------

fn bench_all_providers_miss(c: &mut Criterion) {
    let mut group = c.benchmark_group("all_providers");

    // Create all public providers (mirrors SiteRouter order minus Twitter)
    let providers: Vec<Box<dyn SiteProvider>> = vec![
        Box::new(RedditProvider),
        Box::new(HackerNewsProvider),
        Box::new(GitHubProvider),
        Box::new(InstagramProvider),
        Box::new(YouTubeProvider),
    ];

    // Worst case: URL matches no provider, all 5 checked
    group.bench_function("full_miss_scan", |b| {
        b.iter(|| {
            for url in NON_MATCHING_URLS {
                let mut matched = false;
                for provider in &providers {
                    if provider.matches(black_box(url)) {
                        matched = true;
                        break;
                    }
                }
                black_box(matched);
            }
        });
    });

    // Mixed workload: some hits, some misses
    let mixed_urls: Vec<&str> = REDDIT_URLS
        .iter()
        .chain(GITHUB_URLS.iter())
        .chain(NON_MATCHING_URLS.iter())
        .chain(EDGE_CASE_URLS.iter())
        .copied()
        .collect();

    group.bench_function("mixed_workload", |b| {
        b.iter(|| {
            let mut matches = 0u32;
            for url in &mixed_urls {
                for provider in &providers {
                    if provider.matches(black_box(url)) {
                        matches += 1;
                        break;
                    }
                }
            }
            black_box(matches)
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Router construction benchmarks
// ---------------------------------------------------------------------------

fn bench_content_router_creation(c: &mut Criterion) {
    c.bench_function("content_router_new", |b| {
        b.iter(|| black_box(ContentRouter::new()));
    });
}

fn bench_site_router_creation(c: &mut Criterion) {
    c.bench_function("site_router_new", |b| {
        b.iter(|| black_box(SiteRouter::new()));
    });
}

// ---------------------------------------------------------------------------
// Content router dispatch overhead
// ---------------------------------------------------------------------------

fn bench_content_dispatch_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("content_dispatch");

    // Measure dispatch overhead by comparing direct handler vs router
    let small_html = b"<html><body><h1>Title</h1><p>Body text here.</p></body></html>";
    let plain_text = b"Simple plain text content for benchmarking purposes.";

    group.bench_function("router_html_dispatch", |b| {
        let router = ContentRouter::new();
        b.iter(|| black_box(router.convert(black_box(small_html.as_slice()), "text/html").unwrap()));
    });

    group.bench_function("router_plain_dispatch", |b| {
        let router = ContentRouter::new();
        b.iter(|| {
            black_box(
                router
                    .convert(black_box(plain_text.as_slice()), "text/plain")
                    .unwrap(),
            )
        });
    });

    // Mime type parsing overhead: simple vs with parameters
    group.bench_function("mime_simple", |b| {
        let router = ContentRouter::new();
        b.iter(|| black_box(router.convert(black_box(small_html.as_slice()), "text/html").unwrap()));
    });

    group.bench_function("mime_with_params", |b| {
        let router = ContentRouter::new();
        b.iter(|| {
            black_box(
                router
                    .convert(
                        black_box(small_html.as_slice()),
                        "text/html; charset=utf-8; boundary=something",
                    )
                    .unwrap(),
            )
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_reddit_match,
    bench_github_match,
    bench_hackernews_match,
    bench_instagram_match,
    bench_youtube_match,
    bench_all_providers_miss,
    bench_content_router_creation,
    bench_site_router_creation,
    bench_content_dispatch_overhead,
);

criterion_main!(benches);
