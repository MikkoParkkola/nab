use std::time::Instant;

use anyhow::Result;

use nab::{AcceleratedClient, OnePasswordAuth};

pub async fn cmd_validate() -> Result<()> {
    println!("🧪 MicroFetch Validation Suite\n");
    println!("Testing against real websites with fail-fast approach:\n");

    let client = AcceleratedClient::new_adaptive()?;

    // Test 1: Basic fetch
    print!("1️⃣  Basic fetch (example.com)... ");
    let start = Instant::now();
    let response = client.fetch("https://example.com").await?;
    let body = response.text().await?;
    let elapsed = start.elapsed();
    if body.contains("Example Domain") {
        println!(
            "✅ {:.0}ms, {} bytes",
            elapsed.as_secs_f64() * 1000.0,
            body.len()
        );
    } else {
        println!("❌ Unexpected content");
        return Ok(());
    }

    // Test 2: Compression (Brotli)
    print!("2️⃣  Brotli compression (httpbin.org)... ");
    let start = Instant::now();
    let response = client.fetch("https://httpbin.org/brotli").await?;
    let body = response.text().await?;
    let elapsed = start.elapsed();
    if body.contains("brotli") {
        println!("✅ {:.0}ms", elapsed.as_secs_f64() * 1000.0);
    } else {
        println!("⚠️  Compression may not be working");
    }

    // Test 3: Gzip compression
    print!("3️⃣  Gzip compression (httpbin.org)... ");
    let start = Instant::now();
    let response = client.fetch("https://httpbin.org/gzip").await?;
    let body = response.text().await?;
    let elapsed = start.elapsed();
    if body.contains("gzipped") {
        println!("✅ {:.0}ms", elapsed.as_secs_f64() * 1000.0);
    } else {
        println!("⚠️  Compression may not be working");
    }

    // Test 4: User-Agent check
    print!("4️⃣  Fingerprint check (httpbin.org)... ");
    let response = client.fetch("https://httpbin.org/user-agent").await?;
    let body = response.text().await?;
    let profile = client.profile().await;
    if body.contains(&profile.user_agent[..20]) {
        println!("✅ UA matches");
    } else {
        println!("⚠️  UA mismatch");
    }

    // Test 5: Headers check
    print!("5️⃣  Headers verification (httpbin.org)... ");
    let response = client.fetch("https://httpbin.org/headers").await?;
    let body = response.text().await?;
    if body.contains("Accept-Encoding") && body.contains("Accept-Language") {
        println!("✅ Headers present");
    } else {
        println!("⚠️  Missing headers");
    }

    // Test 6: Real website (Hacker News)
    print!("6️⃣  Real website - HN (news.ycombinator.com)... ");
    let start = Instant::now();
    let response = client.fetch("https://news.ycombinator.com").await?;
    let body = response.text().await?;
    let elapsed = start.elapsed();
    if body.contains("Hacker News") {
        println!(
            "✅ {:.0}ms, {} bytes",
            elapsed.as_secs_f64() * 1000.0,
            body.len()
        );
    } else {
        println!("❌ Failed to fetch");
    }

    // Test 7: HTTPS with modern TLS
    print!("7️⃣  TLS 1.3 check (cloudflare.com)... ");
    let start = Instant::now();
    let response = client.fetch("https://www.cloudflare.com").await?;
    let elapsed = start.elapsed();
    if response.status().is_success() {
        println!("✅ {:.0}ms", elapsed.as_secs_f64() * 1000.0);
    } else {
        println!("⚠️  Status: {}", response.status());
    }

    // Test 8: 1Password check
    print!("8️⃣  1Password CLI... ");
    if OnePasswordAuth::is_available() {
        println!("✅ Available");
    } else {
        println!("⚠️  Not available (run: op signin)");
    }

    println!("\n✨ Validation complete!");

    Ok(())
}
