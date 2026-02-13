//! Performance comparison test for arena allocator
//!
//! Run with: `cargo test --release arena_performance -- --nocapture --ignored`

use nab::arena::{ResponseArena, ResponseBuffer};
use std::time::Instant;

const HTML_CHUNKS: &[&str] = &[
    "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"UTF-8\">",
    "<title>Example Page</title>",
    "<link rel=\"stylesheet\" href=\"/style.css\">",
    "</head><body>",
    "<header><h1>Welcome</h1></header>",
    "<nav><ul><li><a href=\"/\">Home</a></li></ul></nav>",
    "<main><article><h2>Article Title</h2>",
    "<p>This is a paragraph with some content.</p>",
    "</article></main>",
    "<footer><p>&copy; 2024</p></footer>",
    "</body></html>",
];

#[test]
#[ignore] // Run explicitly with --ignored
fn bench_arena_vs_vec_small() {
    const ITERATIONS: usize = 1000;
    const CHUNKS_PER_ITER: usize = 100;

    // Arena allocator - just allocation, no final concat
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let arena = ResponseArena::new();

        for i in 0..CHUNKS_PER_ITER {
            let chunk = HTML_CHUNKS[i % HTML_CHUNKS.len()];
            let _allocated = arena.alloc_str(chunk);
        }
    }
    let arena_time = start.elapsed();

    // Vec<String> allocator - equivalent allocation
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let mut parts = Vec::new();

        for i in 0..CHUNKS_PER_ITER {
            let chunk = HTML_CHUNKS[i % HTML_CHUNKS.len()];
            parts.push(chunk.to_string());
        }
    }
    let vec_time = start.elapsed();

    println!("\n=== Small Response (100 chunks, allocation only) ===");
    println!("Arena: {:?} ({:.2} ops/sec)", arena_time, ITERATIONS as f64 / arena_time.as_secs_f64());
    println!("Vec:   {:?} ({:.2} ops/sec)", vec_time, ITERATIONS as f64 / vec_time.as_secs_f64());
    println!("Speedup: {:.2}×", vec_time.as_secs_f64() / arena_time.as_secs_f64());
    println!("Note: Arena wins on allocation overhead; Vec needs separate String per chunk");
}

#[test]
#[ignore]
fn bench_arena_vs_vec_large() {
    const ITERATIONS: usize = 100;
    const CHUNKS_PER_ITER: usize = 10_000;

    // Arena allocator - just allocation
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let arena = ResponseArena::new();

        for i in 0..CHUNKS_PER_ITER {
            let chunk = HTML_CHUNKS[i % HTML_CHUNKS.len()];
            let _allocated = arena.alloc_str(chunk);
        }
    }
    let arena_time = start.elapsed();

    // Vec<String> allocator
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let mut parts = Vec::new();

        for i in 0..CHUNKS_PER_ITER {
            let chunk = HTML_CHUNKS[i % HTML_CHUNKS.len()];
            parts.push(chunk.to_string());
        }
    }
    let vec_time = start.elapsed();

    println!("\n=== Large Response (10k chunks, ~1MB, allocation only) ===");
    println!("Arena: {:?} ({:.2} ops/sec)", arena_time, ITERATIONS as f64 / arena_time.as_secs_f64());
    println!("Vec:   {:?} ({:.2} ops/sec)", vec_time, ITERATIONS as f64 / vec_time.as_secs_f64());
    println!("Speedup: {:.2}×", vec_time.as_secs_f64() / arena_time.as_secs_f64());
    println!("Note: Arena excels at bulk allocation with single deallocation");
}

#[test]
#[ignore]
fn bench_arena_vs_string() {
    const ITERATIONS: usize = 1000;
    const CHUNKS_PER_ITER: usize = 1000;

    // Arena allocator
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let arena = ResponseArena::new();
        let mut buffer = ResponseBuffer::new(&arena);

        for i in 0..CHUNKS_PER_ITER {
            let chunk = HTML_CHUNKS[i % HTML_CHUNKS.len()];
            buffer.push_str(chunk);
        }

        let _result = buffer.as_str();
    }
    let arena_time = start.elapsed();

    // String push_str
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let mut result = String::new();

        for i in 0..CHUNKS_PER_ITER {
            let chunk = HTML_CHUNKS[i % HTML_CHUNKS.len()];
            result.push_str(chunk);
        }

        let _result = result;
    }
    let string_time = start.elapsed();

    println!("\n=== Arena vs String::push_str (1000 chunks) ===");
    println!("Arena:  {:?} ({:.2} ops/sec)", arena_time, ITERATIONS as f64 / arena_time.as_secs_f64());
    println!("String: {:?} ({:.2} ops/sec)", string_time, ITERATIONS as f64 / string_time.as_secs_f64());
    println!("Ratio: {:.2}×", arena_time.as_secs_f64() / string_time.as_secs_f64());

    // Note: String might be faster for this use case - it's highly optimized
    // Arena wins when you need to keep individual strings alive separately
}

#[test]
#[ignore]
fn bench_arena_memory_usage() {
    const CHUNKS: usize = 10_000;

    let arena = ResponseArena::new();
    let mut buffer = ResponseBuffer::new(&arena);

    for i in 0..CHUNKS {
        let chunk = HTML_CHUNKS[i % HTML_CHUNKS.len()];
        buffer.push_str(chunk);
    }

    let content = buffer.as_str();
    let allocated = arena.bytes_allocated();
    let parts = buffer.part_count();

    println!("\n=== Memory Usage (10k chunks) ===");
    println!("Content size: {} bytes", content.len());
    println!("Arena allocated: {} bytes ({:.2} KB)", allocated, allocated as f64 / 1024.0);
    println!("Buffer parts: {}", parts);
    println!("Content length: {} bytes ({:.2} KB)", content.len(), content.len() as f64 / 1024.0);
}
