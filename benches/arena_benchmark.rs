//! Benchmarks comparing bumpalo arena allocator vs standard allocation
//!
//! Run with: `cargo bench --bench arena_benchmark`

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nab::arena::{ArenaResponse, ResponseArena, ResponseBuffer, StringInterner};

/// Simulate typical HTTP response headers (10 headers, ~500 bytes)
const TYPICAL_HEADERS: &[&str] = &[
    "HTTP/1.1 200 OK",
    "Date: Mon, 27 Jul 2024 12:28:53 GMT",
    "Content-Type: text/html; charset=UTF-8",
    "Content-Length: 12345",
    "Connection: keep-alive",
    "Server: nginx/1.18.0",
    "Set-Cookie: session=abc123; Path=/; HttpOnly",
    "Cache-Control: private, max-age=0",
    "Vary: Accept-Encoding",
    "X-Frame-Options: SAMEORIGIN",
];

/// Simulate HTML fragments (average 100-500 bytes per chunk)
const HTML_CHUNKS: &[&str] = &[
    "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"UTF-8\">",
    "<title>Example Page</title>",
    "<link rel=\"stylesheet\" href=\"/style.css\">",
    "</head><body>",
    "<header><h1>Welcome</h1></header>",
    "<nav><ul><li><a href=\"/\">Home</a></li><li><a href=\"/about\">About</a></li></ul></nav>",
    "<main><article><h2>Article Title</h2>",
    "<p>This is a paragraph with some content that represents typical HTML text.</p>",
    "<p>Another paragraph with more content to simulate realistic HTML structure.</p>",
    "</article></main>",
    "<footer><p>&copy; 2024 Example Corp</p></footer>",
    "</body></html>",
];

/// Simulate markdown conversion output (smaller chunks after cleanup)
const MARKDOWN_CHUNKS: &[&str] = &[
    "# Welcome\n\n",
    "## Article Title\n\n",
    "This is a paragraph with some content that represents typical HTML text.\n\n",
    "Another paragraph with more content to simulate realistic HTML structure.\n\n",
    "## Links\n\n",
    "- [Home](/)\n",
    "- [About](/about)\n",
    "\n\n",
    "© 2024 Example Corp\n",
];

fn bench_headers_arena(c: &mut Criterion) {
    let mut group = c.benchmark_group("headers");

    for size in [10, 50, 100].iter() {
        group.throughput(Throughput::Elements(*size as u64));

        group.bench_with_input(BenchmarkId::new("bumpalo_arena", size), size, |b, &size| {
            b.iter(|| {
                let arena = ResponseArena::new();
                let mut buffer = ResponseBuffer::new(&arena);

                for _ in 0..size {
                    for header in TYPICAL_HEADERS {
                        buffer.push_str(black_box(header));
                        buffer.push_str("\r\n");
                    }
                }

                black_box(buffer.as_str())
            });
        });

        group.bench_with_input(BenchmarkId::new("vec", size), size, |b, &size| {
            b.iter(|| {
                let mut parts = Vec::new();

                for _ in 0..size {
                    for header in TYPICAL_HEADERS {
                        parts.push(black_box(header).to_string());
                        parts.push("\r\n".to_string());
                    }
                }

                black_box(parts.concat())
            });
        });
    }

    group.finish();
}

fn bench_html_buffering(c: &mut Criterion) {
    let mut group = c.benchmark_group("html_buffering");

    for size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*size as u64));

        group.bench_with_input(BenchmarkId::new("bumpalo_arena", size), size, |b, &size| {
            b.iter(|| {
                let arena = ResponseArena::new();
                let mut buffer = ResponseBuffer::new(&arena);

                for i in 0..size {
                    let chunk = HTML_CHUNKS[i % HTML_CHUNKS.len()];
                    buffer.push_str(black_box(chunk));
                }

                black_box(buffer.as_str())
            });
        });

        group.bench_with_input(BenchmarkId::new("vec", size), size, |b, &size| {
            b.iter(|| {
                let mut parts = Vec::new();

                for i in 0..size {
                    let chunk = HTML_CHUNKS[i % HTML_CHUNKS.len()];
                    parts.push(black_box(chunk).to_string());
                }

                black_box(parts.concat())
            });
        });

        group.bench_with_input(BenchmarkId::new("string", size), size, |b, &size| {
            b.iter(|| {
                let mut result = String::new();

                for i in 0..size {
                    let chunk = HTML_CHUNKS[i % HTML_CHUNKS.len()];
                    result.push_str(black_box(chunk));
                }

                black_box(result)
            });
        });
    }

    group.finish();
}

fn bench_markdown_conversion(c: &mut Criterion) {
    let mut group = c.benchmark_group("markdown_conversion");

    for size in [10, 100, 500].iter() {
        group.throughput(Throughput::Elements(*size as u64));

        group.bench_with_input(BenchmarkId::new("bumpalo_arena", size), size, |b, &size| {
            b.iter(|| {
                let arena = ResponseArena::new();
                let mut buffer = ResponseBuffer::new(&arena);

                for i in 0..size {
                    let chunk = MARKDOWN_CHUNKS[i % MARKDOWN_CHUNKS.len()];
                    buffer.push_str(black_box(chunk));
                }

                black_box(buffer.as_str())
            });
        });

        group.bench_with_input(BenchmarkId::new("vec", size), size, |b, &size| {
            b.iter(|| {
                let mut parts = Vec::new();

                for i in 0..size {
                    let chunk = MARKDOWN_CHUNKS[i % MARKDOWN_CHUNKS.len()];
                    parts.push(black_box(chunk).to_string());
                }

                black_box(parts.concat())
            });
        });
    }

    group.finish();
}

fn bench_realistic_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("realistic_response");

    group.bench_function("bumpalo_arena", |b| {
        b.iter(|| {
            let arena = ResponseArena::new();
            let mut buffer = ResponseBuffer::new(&arena);

            // Headers
            for header in TYPICAL_HEADERS {
                buffer.push_str(black_box(header));
                buffer.push_str("\r\n");
            }
            buffer.push_str("\r\n");

            // HTML body (simulating 10KB response)
            for _ in 0..100 {
                for chunk in HTML_CHUNKS {
                    buffer.push_str(black_box(chunk));
                }
            }

            black_box(buffer.as_str())
        });
    });

    group.bench_function("vec", |b| {
        b.iter(|| {
            let mut parts = Vec::new();

            // Headers
            for header in TYPICAL_HEADERS {
                parts.push(black_box(header).to_string());
                parts.push("\r\n".to_string());
            }
            parts.push("\r\n".to_string());

            // HTML body
            for _ in 0..100 {
                for chunk in HTML_CHUNKS {
                    parts.push(black_box(chunk).to_string());
                }
            }

            black_box(parts.concat())
        });
    });

    group.finish();
}

fn bench_large_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_response");
    group.sample_size(20); // Fewer samples for large benchmarks

    // Simulate 1MB response
    group.bench_function("bumpalo_arena_1mb", |b| {
        b.iter(|| {
            let arena = ResponseArena::new();
            let mut buffer = ResponseBuffer::new(&arena);

            for _ in 0..10_000 {
                for chunk in HTML_CHUNKS {
                    buffer.push_str(black_box(chunk));
                }
            }

            black_box(buffer.as_str())
        });
    });

    group.bench_function("vec_1mb", |b| {
        b.iter(|| {
            let mut parts = Vec::new();

            for _ in 0..10_000 {
                for chunk in HTML_CHUNKS {
                    parts.push(black_box(chunk).to_string());
                }
            }

            black_box(parts.concat())
        });
    });

    group.finish();
}

fn bench_arena_reuse(c: &mut Criterion) {
    c.bench_function("bumpalo_arena_reuse", |b| {
        b.iter(|| {
            let mut arena = ResponseArena::new();
            {
                let mut buffer = ResponseBuffer::new(&arena);

                for chunk in HTML_CHUNKS {
                    buffer.push_str(black_box(chunk));
                }

                black_box(buffer.as_str())
            } // buffer dropped here
              // Reset for reuse (zero-cost with bumpalo)
              // Note: In real usage, arena.reset() would be called between requests
        });
    });
}

fn bench_small_allocations(c: &mut Criterion) {
    let mut group = c.benchmark_group("small_allocations");

    // Simulate many tiny strings (common in header parsing)
    group.bench_function("bumpalo_arena_many_small", |b| {
        b.iter(|| {
            let arena = ResponseArena::new();
            let mut buffer = ResponseBuffer::with_capacity(&arena, 1000);

            for i in 0..1000 {
                let s = format!("h{i}");
                buffer.push_str(black_box(&s));
            }

            black_box(buffer.as_str())
        });
    });

    group.bench_function("vec_many_small", |b| {
        b.iter(|| {
            let mut parts = Vec::with_capacity(1000);

            for i in 0..1000 {
                parts.push(black_box(format!("h{i}")));
            }

            black_box(parts.concat())
        });
    });

    group.finish();
}

fn bench_arena_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("arena_response");

    group.bench_function("arena_response_building", |b| {
        b.iter(|| {
            let arena = ResponseArena::new();
            let mut response = ArenaResponse::new(&arena);

            // Status line
            response.set_status(&arena, 200, "OK");

            // Headers (10 typical headers)
            for header in TYPICAL_HEADERS {
                let parts: Vec<&str> = header.split(": ").collect();
                if parts.len() == 2 {
                    response.add_header(&arena, parts[0], parts[1]);
                }
            }

            // Body chunks
            for chunk in HTML_CHUNKS {
                response.add_body_chunk(&arena, chunk.as_bytes());
            }

            black_box(response.body_text())
        });
    });

    group.bench_function("standard_response_building", |b| {
        #[derive(Debug)]
        struct StandardResponse {
            status: u16,
            status_text: String,
            headers: Vec<(String, String)>,
            body_chunks: Vec<Vec<u8>>,
        }

        b.iter(|| {
            let mut response = StandardResponse {
                status: 200,
                status_text: "OK".to_string(),
                headers: Vec::with_capacity(20),
                body_chunks: Vec::with_capacity(10),
            };

            // Headers
            for header in TYPICAL_HEADERS {
                let parts: Vec<&str> = header.split(": ").collect();
                if parts.len() == 2 {
                    response
                        .headers
                        .push((parts[0].to_string(), parts[1].to_string()));
                }
            }

            // Body chunks
            for chunk in HTML_CHUNKS {
                response.body_chunks.push(chunk.as_bytes().to_vec());
            }

            // Concatenate body
            let body: Vec<u8> = response
                .body_chunks
                .iter()
                .flat_map(|c| c.iter().copied())
                .collect();

            black_box(String::from_utf8(body).ok())
        });
    });

    group.finish();
}

fn bench_string_interning(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_interning");

    group.bench_function("with_interning", |b| {
        let interner = StringInterner::new();

        b.iter(|| {
            let arena = ResponseArena::new();
            let mut response = ArenaResponse::new(&arena);

            // Common header names benefit from interning
            response.add_header_interned(&arena, &interner, "content-type", "text/html");
            response.add_header_interned(&arena, &interner, "server", "nginx");
            response.add_header_interned(&arena, &interner, "cache-control", "max-age=3600");
            response.add_header_interned(&arena, &interner, "x-custom", "value");

            black_box(response.headers.len())
        });
    });

    group.bench_function("without_interning", |b| {
        b.iter(|| {
            let arena = ResponseArena::new();
            let mut response = ArenaResponse::new(&arena);

            response.add_header(&arena, "content-type", "text/html");
            response.add_header(&arena, "server", "nginx");
            response.add_header(&arena, "cache-control", "max-age=3600");
            response.add_header(&arena, "x-custom", "value");

            black_box(response.headers.len())
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_headers_arena,
    bench_html_buffering,
    bench_markdown_conversion,
    bench_realistic_response,
    bench_large_response,
    bench_arena_reuse,
    bench_small_allocations,
    bench_arena_response,
    bench_string_interning,
);

criterion_main!(benches);
