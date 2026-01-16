//! `MicroFetch` CLI - Test and benchmark the accelerated HTTP client

use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use microfetch::{AcceleratedClient, CookieSource, OnePasswordAuth, OtpRetriever};

#[derive(Parser)]
#[command(name = "microfetch")]
#[command(about = "Ultra-minimal browser engine with HTTP acceleration")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch a URL and display results
    Fetch {
        /// URL to fetch
        url: String,

        /// Show response headers
        #[arg(short = 'H', long)]
        headers: bool,

        /// Show full body (not just length)
        #[arg(short, long)]
        body: bool,

        /// Use cookies from browser (brave, chrome, firefox, safari)
        #[arg(short, long)]
        cookies: Option<String>,

        /// Use 1Password credentials for this URL
        #[arg(long = "1password", visible_alias = "op")]
        use_1password: bool,
    },

    /// Benchmark fetching multiple URLs
    Bench {
        /// URLs to benchmark (comma-separated)
        urls: String,

        /// Number of iterations per URL
        #[arg(short, long, default_value = "5")]
        iterations: usize,
    },

    /// Test browser fingerprint spoofing
    Fingerprint {
        /// Number of profiles to generate
        #[arg(short, long, default_value = "3")]
        count: usize,
    },

    /// Test 1Password integration
    Auth {
        /// URL to find credentials for
        url: String,
    },

    /// Run all validation tests against real websites
    Validate,

    /// Get OTP code from all available sources
    Otp {
        /// Domain or URL to get OTP for
        domain: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Fetch { url, headers, body, cookies, use_1password } => {
            cmd_fetch(&url, headers, body, cookies.as_deref(), use_1password).await?;
        }
        Commands::Bench { urls, iterations } => {
            cmd_bench(&urls, iterations).await?;
        }
        Commands::Fingerprint { count } => {
            cmd_fingerprint(count);
        }
        Commands::Auth { url } => {
            cmd_auth(&url)?;
        }
        Commands::Validate => {
            cmd_validate().await?;
        }
        Commands::Otp { domain } => {
            cmd_otp(&domain)?;
        }
    }

    Ok(())
}

async fn cmd_fetch(url: &str, show_headers: bool, show_body: bool, cookies: Option<&str>, use_1password: bool) -> Result<()> {
    let client = AcceleratedClient::new()?;
    let profile = client.profile().await;

    println!("🌐 Fetching: {url}");
    println!("🎭 User-Agent: {}", profile.user_agent);

    // Extract domain from URL
    let domain = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_default();

    // Get cookies if requested
    let mut cookie_header = String::new();
    if let Some(browser) = cookies {
        let source = match browser.to_lowercase().as_str() {
            "brave" => CookieSource::Brave,
            "chrome" => CookieSource::Chrome,
            "firefox" => CookieSource::Firefox,
            "safari" => CookieSource::Safari,
            _ => {
                println!("⚠️  Unknown browser: {browser}, using Brave");
                CookieSource::Brave
            }
        };
        cookie_header = source.get_cookie_header(&domain).unwrap_or_default();
        if !cookie_header.is_empty() {
            println!("🍪 Loaded {} cookies from {browser}", cookie_header.matches('=').count());
        }
    }

    // Get 1Password credentials if requested
    if use_1password {
        if OnePasswordAuth::is_available() {
            let auth = OnePasswordAuth::new(None);
            if let Ok(Some(cred)) = auth.get_credential_for_url(url) {
                println!("🔐 Found 1Password: {}", cred.title);
                if cred.has_totp {
                    println!("   TOTP available");
                }
            }
        } else {
            println!("⚠️  1Password CLI not available");
        }
    }

    let start = Instant::now();

    // Build request with cookies
    let response = if cookie_header.is_empty() {
        client.fetch(url).await?
    } else {
        client.inner()
            .get(url)
            .header("Cookie", &cookie_header)
            .headers(profile.to_headers())
            .send()
            .await?
    };

    let elapsed = start.elapsed();

    println!("\n📊 Response:");
    println!("   Status: {}", response.status());
    println!("   Version: {:?}", response.version());
    println!("   Time: {:.2}ms", elapsed.as_secs_f64() * 1000.0);

    if show_headers {
        println!("\n📋 Headers:");
        for (name, value) in response.headers() {
            println!("   {}: {}", name, value.to_str().unwrap_or("<binary>"));
        }
    }

    let body = response.text().await?;
    println!("\n📄 Body: {} bytes", body.len());

    if show_body {
        println!("\n{}", &body[..body.len().min(2000)]);
        if body.len() > 2000 {
            println!("\n... [truncated]");
        }
    }

    Ok(())
}

async fn cmd_bench(urls: &str, iterations: usize) -> Result<()> {
    let client = AcceleratedClient::new()?;
    let urls: Vec<&str> = urls.split(',').map(str::trim).collect();

    println!(
        "🚀 Benchmarking {} URLs, {} iterations each\n",
        urls.len(),
        iterations
    );

    for url in urls {
        let mut times = Vec::with_capacity(iterations);

        for i in 0..iterations {
            let start = Instant::now();
            let response = client.fetch(url).await?;
            let _ = response.text().await?;
            let elapsed = start.elapsed();
            times.push(elapsed.as_secs_f64() * 1000.0);

            print!(".");
            if i == iterations - 1 {
                println!();
            }
        }

        let avg = times.iter().sum::<f64>() / times.len() as f64;
        let min = times.iter().copied().fold(f64::INFINITY, f64::min);
        let max = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);

        println!("📊 {url}");
        println!(
            "   Avg: {avg:.2}ms | Min: {min:.2}ms | Max: {max:.2}ms\n"
        );
    }

    Ok(())
}

fn cmd_fingerprint(count: usize) {
    println!("🎭 Generating {count} browser fingerprints:\n");

    for i in 0..count {
        let profile = microfetch::random_profile();
        println!("Profile {}:", i + 1);
        println!("   UA: {}", profile.user_agent);
        println!("   Accept-Language: {}", profile.accept_language);
        if !profile.sec_ch_ua.is_empty() {
            println!("   Sec-CH-UA: {}", profile.sec_ch_ua);
        }
        println!();
    }
}

fn cmd_auth(url: &str) -> Result<()> {
    if !OnePasswordAuth::is_available() {
        println!("❌ 1Password CLI not available or not authenticated");
        println!("   Run: op signin");
        return Ok(());
    }

    println!("🔐 Searching 1Password for: {url}");

    let auth = OnePasswordAuth::new(None);
    match auth.get_credential_for_url(url)? {
        Some(cred) => {
            println!("\n✅ Found credential:");
            println!("   Title: {}", cred.title);
            if let Some(ref username) = cred.username {
                println!("   Username: {username}");
            }
            if cred.password.is_some() {
                println!("   Password: [present]");
            }
            if let Some(ref totp) = cred.totp {
                println!("   TOTP: {totp}");
            }
            if let Some(ref passkey) = cred.passkey_credential_id {
                println!("   Passkey: {passkey}");
            }
        }
        None => {
            println!("\n❌ No credential found for this URL");
        }
    }

    Ok(())
}

async fn cmd_validate() -> Result<()> {
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

fn cmd_otp(domain: &str) -> Result<()> {
    println!("🔐 Searching for OTP codes for: {domain}\n");

    // Extract domain from URL if needed
    let clean_domain = url::Url::parse(domain)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_else(|| domain.to_string());

    if let Some(otp) = OtpRetriever::get_otp_for_domain(&clean_domain)? {
        println!("✅ Found OTP code!");
        println!("   Code: {}", otp.code);
        println!("   Source: {}", otp.source);
        if let Some(expires) = otp.expires_in_seconds {
            println!("   Expires in: {expires}s");
        }
    } else {
        println!("❌ No OTP code found from any source");
        println!("\nSearched:");
        println!("   1. 1Password TOTP");
        println!("   2. SMS via Beeper");
        println!("   3. Email via Gmail");
    }

    Ok(())
}
