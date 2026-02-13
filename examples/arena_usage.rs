//! Example showing how to use Arena allocator for response buffering
//!
//! Run with: `cargo run --example arena_usage`

use nab::arena::{ArenaResponse, ResponseArena, ResponseBuffer, StringInterner};

fn main() {
    example_header_parsing();
    example_html_chunks();
    example_arena_reuse();
    example_memory_stats();
    example_http_response();
    example_string_interning();
}

/// Simulate HTTP header parsing
fn example_header_parsing() {
    println!("=== HTTP Header Parsing ===");

    let arena = ResponseArena::new();
    let mut buffer = ResponseBuffer::new(&arena);

    // Simulate parsing headers one at a time
    let headers = vec![
        "HTTP/1.1 200 OK",
        "Content-Type: text/html; charset=utf-8",
        "Content-Length: 12345",
        "Cache-Control: max-age=3600",
        "Set-Cookie: session=abc123",
    ];

    for header in headers {
        buffer.push_str(header);
        buffer.push_str("\r\n");
    }

    buffer.push_str("\r\n"); // Empty line separating headers from body

    let result = buffer.as_str();
    println!(
        "Parsed {} bytes in {} parts",
        result.len(),
        buffer.part_count()
    );
    println!("First 100 chars: {:?}\n", &result[..100.min(result.len())]);
}

/// Simulate HTML chunk processing
fn example_html_chunks() {
    println!("=== HTML Chunk Processing ===");

    let arena = ResponseArena::new();
    let mut buffer = ResponseBuffer::new(&arena);

    // Simulate receiving HTML in chunks from network
    let chunks = vec![
        "<!DOCTYPE html><html>",
        "<head><title>Example</title></head>",
        "<body>",
        "<h1>Welcome</h1>",
        "<p>Content paragraph</p>",
        "</body></html>",
    ];

    for chunk in chunks {
        buffer.push_str(chunk);
    }

    let html = buffer.as_str();
    println!(
        "Assembled {} bytes from {} chunks",
        html.len(),
        buffer.part_count()
    );
    println!("HTML: {}\n", html);
}

/// Demonstrate arena reuse for multiple requests
fn example_arena_reuse() {
    println!("=== Arena Reuse Pattern ===");

    let mut arena = ResponseArena::new();

    for request_num in 1..=3 {
        {
            let mut buffer = ResponseBuffer::new(&arena);

            // Simulate processing different requests
            buffer.push_str(&format!("Request #{request_num}\n"));
            buffer.push_str("Status: 200 OK\n");
            buffer.push_str("Body: Some response data\n");

            let result = buffer.as_str();
            println!("Request {}: {} bytes", request_num, result.len());
        } // buffer dropped here, releasing borrow

        // Reset arena for next request (reuses memory, zero-cost with bumpalo)
        arena.reset();
    }
    println!();
}

/// Show memory statistics
fn example_memory_stats() {
    println!("=== Memory Statistics ===");

    let arena = ResponseArena::with_capacity(8192); // 8KB initial capacity
    let mut buffer = ResponseBuffer::new(&arena);

    // Allocate some data
    for i in 0..100 {
        buffer.push_str(&format!("Line {i}: Some data here\n"));
    }

    println!("Bytes allocated: {} KB", arena.bytes_allocated() / 1024);
    println!("Content size: {} bytes", buffer.len());
    println!(
        "Overhead: {:.1}%",
        (arena.bytes_allocated() - buffer.len()) as f64 / arena.bytes_allocated() as f64 * 100.0
    );
    println!();
}

/// Demonstrate building HTTP response with ArenaResponse
fn example_http_response() {
    println!("=== HTTP Response Building ===");

    let arena = ResponseArena::new();
    let mut response = ArenaResponse::new(&arena);

    // Set status
    response.set_status(&arena, 200, "OK");

    // Add headers
    response.add_header(&arena, "Content-Type", "text/html; charset=utf-8");
    response.add_header(&arena, "Content-Length", "55");
    response.add_header(&arena, "Cache-Control", "max-age=3600");

    // Add body in chunks (simulating streaming response)
    response.add_body_chunk(&arena, b"<!DOCTYPE html><html>");
    response.add_body_chunk(&arena, b"<body>Hello, World!</body>");
    response.add_body_chunk(&arena, b"</html>");

    println!("Status: {} {}", response.status, response.status_text);
    println!("Headers: {} headers", response.headers.len());
    for (name, value) in &response.headers {
        println!("  {}: {}", name, value);
    }

    let body_text = response.body_text().unwrap();
    println!("Body: {} bytes", body_text.len());
    println!("Content: {}\n", body_text);
}

/// Demonstrate string interning for common headers
fn example_string_interning() {
    println!("=== String Interning ===");

    let arena = ResponseArena::new();
    let interner = StringInterner::new();
    let mut response = ArenaResponse::new(&arena);

    // Common headers benefit from interning (reuse same pointer)
    response.add_header_interned(&arena, &interner, "content-type", "text/html");
    response.add_header_interned(&arena, &interner, "server", "nginx");
    response.add_header_interned(&arena, &interner, "cache-control", "max-age=3600");

    // Custom headers fall back to arena allocation
    response.add_header_interned(&arena, &interner, "x-custom-header", "custom-value");

    println!("Headers with interning:");
    for (i, (name, value)) in response.headers.iter().enumerate() {
        let name_ptr = *name as *const str;
        println!(
            "  {}: {} = {} (ptr: {:p})",
            i + 1,
            name,
            value,
            name_ptr
        );
    }

    // Verify that common headers share the same pointer
    let content_type_ptr1 = response.headers[0].0 as *const str;
    response.add_header_interned(&arena, &interner, "content-type", "text/plain");
    let content_type_ptr2 = response.headers[4].0 as *const str;

    println!(
        "\nString interning verification: content-type pointers {} ({})",
        if std::ptr::addr_eq(content_type_ptr1, content_type_ptr2) {
            "MATCH"
        } else {
            "DIFFER"
        },
        if std::ptr::addr_eq(content_type_ptr1, content_type_ptr2) {
            "interned successfully"
        } else {
            "not interned"
        }
    );
}
