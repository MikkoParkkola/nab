//! `MicroFetch` CLI - Token-optimized HTTP client with SPA extraction
//!
//! Designed for LLM consumption: minimal tokens, maximum information.

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use scraper::{Html, Selector};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use microfetch::{AcceleratedClient, CookieSource, OnePasswordAuth, OtpRetriever};

#[derive(Parser)]
#[command(name = "microfetch")]
#[command(about = "Token-optimized HTTP client with SPA extraction")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Default, ValueEnum)]
enum OutputFormat {
    #[default]
    /// Verbose with emojis (human-friendly)
    Full,
    /// Minimal: STATUS SIZE TIME (LLM-optimized)
    Compact,
    /// JSON output
    Json,
}

#[derive(Clone, Copy, Default, ValueEnum)]
enum AnalyzeOutputFormat {
    #[default]
    /// JSON with all analysis data
    Json,
    /// Markdown report
    Markdown,
    /// SRT subtitle format
    Srt,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum OverlayStyleArg {
    #[default]
    /// Clean subtitles only
    Minimal,
    /// Subtitles + speaker labels
    Detailed,
    /// All overlays including timestamps
    Debug,
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch a URL (token-optimized output available)
    Fetch {
        /// URL to fetch
        url: String,

        /// Show response headers
        #[arg(short = 'H', long)]
        headers: bool,

        /// Show body content
        #[arg(short, long)]
        body: bool,

        /// Output format: full, compact, json
        #[arg(short, long, default_value = "full")]
        format: OutputFormat,

        /// Save body to file (bypasses truncation)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Use cookies from browser (brave, chrome, firefox, safari)
        #[arg(short, long)]
        cookies: Option<String>,

        /// Use 1Password credentials for this URL
        #[arg(long = "1password", visible_alias = "op")]
        use_1password: bool,

        /// Convert HTML to markdown (strips clutter)
        #[arg(short, long)]
        markdown: bool,

        /// Extract links only
        #[arg(short, long)]
        links: bool,

        /// Maximum body chars to display (0=unlimited)
        #[arg(long, default_value = "0")]
        max_body: usize,

        /// Add custom request headers (can be repeated: --add-header "Accept: application/json")
        #[arg(long = "add-header", action = clap::ArgAction::Append)]
        add_headers: Vec<String>,

        /// Automatically add Referer header based on URL origin
        #[arg(long)]
        auto_referer: bool,

        /// Warmup URL to fetch first (establishes session state for APIs)
        #[arg(long)]
        warmup_url: Option<String>,

        /// HTTP method (GET, POST, PUT, DELETE, PATCH)
        #[arg(short = 'X', long, default_value = "GET")]
        method: String,

        /// Request body data (for POST/PUT/PATCH)
        #[arg(short = 'd', long)]
        data: Option<String>,

        /// Output Set-Cookie headers from response (for auth flows)
        #[arg(long)]
        capture_cookies: bool,

        /// Don't follow redirects (capture 302 response directly)
        #[arg(long)]
        no_redirect: bool,
    },

    /// Extract data from JavaScript-heavy SPA pages
    Spa {
        /// URL to extract data from
        url: String,

        /// Use cookies from browser (brave, chrome, firefox, safari)
        #[arg(short, long)]
        cookies: Option<String>,

        /// Show raw HTML
        #[arg(long)]
        html: bool,

        /// Show console output from JS execution
        #[arg(long)]
        console: bool,

        /// Wait time in milliseconds after page load for AJAX/setTimeout to complete
        #[arg(long, default_value = "2000")]
        wait: u64,

        /// API endpoint patterns to look for (comma-separated)
        #[arg(short, long)]
        patterns: Option<String>,

        /// Output format: json or text
        #[arg(short, long, default_value = "text")]
        output: String,

        /// Extract specific JSON path (e.g., 'props.pageProps.session')
        #[arg(long)]
        extract: Option<String>,

        /// Show structure summary only (95%+ token savings)
        #[arg(long)]
        summary: bool,

        /// Minify JSON output (10-30% savings)
        #[arg(long)]
        minify: bool,

        /// Limit arrays to first N items
        #[arg(long)]
        max_array: Option<usize>,

        /// Limit nesting depth
        #[arg(long)]
        max_depth: Option<usize>,

        /// Force HTTP/1.1 (for servers with HTTP/2 issues)
        #[arg(long)]
        http1: bool,
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

    /// Stream media from various providers
    Stream {
        /// Provider or URL (yle, youtube, or direct URL)
        source: String,

        /// Program/video ID or URL
        id: String,

        /// Output destination (- for stdout, path for file)
        #[arg(short, long, default_value = "-")]
        output: String,

        /// Quality: best, worst, or height (720, 1080)
        #[arg(short, long, default_value = "best")]
        quality: String,

        /// Force native backend
        #[arg(long)]
        native: bool,

        /// Force ffmpeg backend
        #[arg(long)]
        ffmpeg: bool,

        /// Show stream info only (no download)
        #[arg(long)]
        info: bool,

        /// List episodes (for series URLs)
        #[arg(long)]
        list: bool,

        /// Use cookies from browser
        #[arg(short, long)]
        cookies: Option<String>,

        /// Duration limit for live streams (e.g., "1h", "30m")
        #[arg(long)]
        duration: Option<String>,

        /// ffmpeg output options (e.g., "-c:v libx265")
        #[arg(long = "ffmpeg-opts")]
        ffmpeg_opts: Option<String>,

        /// Pipe output to media player (vlc, mpv, etc.)
        #[arg(long)]
        player: Option<String>,
    },

    /// Analyze video with multimodal pipeline (transcription + vision)
    Analyze {
        /// Video file or URL to analyze
        video: String,

        /// Skip visual analysis, transcription only
        #[arg(long)]
        audio_only: bool,

        /// Enable speaker diarization
        #[arg(long)]
        diarize: bool,

        /// Output format
        #[arg(long, short, default_value = "json")]
        format: AnalyzeOutputFormat,

        /// Output file (default: stdout)
        #[arg(long, short)]
        output: Option<PathBuf>,

        /// Offload processing to DGX Spark
        #[arg(long)]
        dgx: bool,

        /// Claude API key for vision analysis (or `ANTHROPIC_API_KEY` env)
        #[arg(long)]
        api_key: Option<String>,
    },

    /// Add overlays to video (subtitles, speaker labels, analysis)
    Annotate {
        /// Input video file
        video: String,

        /// Output video file
        output: String,

        /// Generate and burn subtitles
        #[arg(long)]
        subtitles: bool,

        /// Add speaker identification labels
        #[arg(long)]
        speaker_labels: bool,

        /// Add emotional/behavioral analysis overlay
        #[arg(long)]
        analysis: bool,

        /// Overlay style
        #[arg(long, default_value = "minimal")]
        style: OverlayStyleArg,

        /// Use hardware acceleration (`VideoToolbox` on macOS)
        #[arg(long)]
        hwaccel: bool,
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
        Commands::Fetch {
            url,
            headers,
            body,
            format,
            output,
            cookies,
            use_1password,
            markdown,
            links,
            max_body,
            add_headers,
            auto_referer,
            warmup_url,
            method,
            data,
            capture_cookies,
            no_redirect,
        } => {
            cmd_fetch(
                &url,
                headers,
                body,
                format,
                output,
                cookies.as_deref(),
                use_1password,
                markdown,
                links,
                max_body,
                &add_headers,
                auto_referer,
                warmup_url.as_deref(),
                &method,
                data.as_deref(),
                capture_cookies,
                no_redirect,
            )
            .await?;
        }
        Commands::Spa {
            url,
            cookies,
            html,
            console,
            wait,
            patterns,
            output,
            extract,
            summary,
            minify,
            max_array,
            max_depth,
            http1,
        } => {
            cmd_spa(
                &url,
                cookies.as_deref(),
                html,
                console,
                wait,
                patterns.as_deref(),
                &output,
                extract.as_deref(),
                summary,
                minify,
                max_array,
                max_depth,
                http1,
            )
            .await?;
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
        Commands::Stream {
            source,
            id,
            output,
            quality,
            native,
            ffmpeg,
            info,
            list,
            cookies,
            duration,
            ffmpeg_opts,
            player,
        } => {
            cmd_stream(
                &source,
                &id,
                &output,
                &quality,
                native,
                ffmpeg,
                info,
                list,
                cookies.as_deref(),
                duration.as_deref(),
                ffmpeg_opts.as_deref(),
                player.as_deref(),
            )
            .await?;
        }
        Commands::Analyze {
            video,
            audio_only,
            diarize,
            format,
            output,
            dgx,
            api_key,
        } => {
            cmd_analyze(
                &video,
                audio_only,
                diarize,
                format,
                output,
                dgx,
                api_key.as_deref(),
            )
            .await?;
        }
        Commands::Annotate {
            video,
            output,
            subtitles,
            speaker_labels,
            analysis,
            style,
            hwaccel,
        } => {
            cmd_annotate(
                &video,
                &output,
                subtitles,
                speaker_labels,
                analysis,
                style,
                hwaccel,
            )
            .await?;
        }
    }

    Ok(())
}

async fn cmd_fetch(
    url: &str,
    show_headers: bool,
    show_body: bool,
    format: OutputFormat,
    output_file: Option<PathBuf>,
    cookies: Option<&str>,
    use_1password: bool,
    markdown: bool,
    links: bool,
    max_body: usize,
    custom_headers: &[String],
    auto_referer: bool,
    warmup_url: Option<&str>,
    method: &str,
    data: Option<&str>,
    capture_cookies: bool,
    no_redirect: bool,
) -> Result<()> {
    // Create client - with or without redirect following
    let client = if no_redirect {
        AcceleratedClient::new_no_redirect()?
    } else {
        AcceleratedClient::new()?
    };
    let profile = client.profile().await;

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
            _ => CookieSource::Brave,
        };
        cookie_header = source.get_cookie_header(&domain).unwrap_or_default();
    }

    // Handle 1Password
    if use_1password && OnePasswordAuth::is_available() {
        let auth = OnePasswordAuth::new(None);
        if let Ok(Some(cred)) = auth.get_credential_for_url(url) {
            if matches!(format, OutputFormat::Full) {
                println!("🔐 Found 1Password: {}", cred.title);
            }
        }
    }

    // Session warmup (for APIs that require prior page load)
    if let Some(warmup) = warmup_url {
        if matches!(format, OutputFormat::Full) {
            println!("🔥 Warming up session: {warmup}");
        }
        let mut warmup_req = client.inner().get(warmup);
        warmup_req = warmup_req.headers(profile.to_headers());
        if !cookie_header.is_empty() {
            warmup_req = warmup_req.header("Cookie", &cookie_header);
        }
        let _ = warmup_req.send().await; // Ignore result, just establish session
    }

    let start = Instant::now();

    // Build request based on HTTP method
    let mut request = match method.to_uppercase().as_str() {
        "POST" => client.inner().post(url),
        "PUT" => client.inner().put(url),
        "PATCH" => client.inner().patch(url),
        "DELETE" => client.inner().delete(url),
        "HEAD" => client.inner().head(url),
        _ => client.inner().get(url),
    };

    // Add request body for methods that support it
    if let Some(body_data) = data {
        request = request.body(body_data.to_owned());
        // Default to JSON content type if not specified
        if !custom_headers.iter().any(|h| h.to_lowercase().starts_with("content-type")) {
            request = request.header("Content-Type", "application/json");
        }
    }

    // Add fingerprint headers
    request = request.headers(profile.to_headers());

    // Add cookies if present
    if !cookie_header.is_empty() {
        request = request.header("Cookie", &cookie_header);
    }

    // Add auto-referer if requested (domain origin)
    if auto_referer {
        if let Ok(parsed) = url::Url::parse(url) {
            let referer = format!("{}://{}/", parsed.scheme(), parsed.host_str().unwrap_or(""));
            request = request.header("Referer", referer);
        }
    }

    // Add custom headers (--add-header "Name: Value")
    for header_str in custom_headers {
        let parts: Vec<&str> = header_str.splitn(2, ':').collect();
        if parts.len() == 2 {
            request = request.header(parts[0].trim(), parts[1].trim());
        }
    }

    let response = request.send().await?;

    let elapsed = start.elapsed();
    let status = response.status();
    let version = response.version();

    // Extract Set-Cookie headers before consuming response
    let set_cookies: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(String::from))
        .collect();

    // Output Set-Cookie headers if requested (for auth flows)
    if capture_cookies && !set_cookies.is_empty() {
        println!("🍪 Set-Cookie:");
        for cookie in &set_cookies {
            // Parse cookie to extract name=value
            if let Some(name_value) = cookie.split(';').next() {
                println!("   {name_value}");
            }
        }
    }

    // Output based on format
    match format {
        OutputFormat::Compact => {
            // Minimal: STATUS SIZE TIME
            let body_text = response.text().await?;
            let body_len = body_text.len();
            println!("{} {}B {:.0}ms", status.as_u16(), body_len, elapsed.as_secs_f64() * 1000.0);

            if show_body || output_file.is_some() || markdown || links {
                output_body(&body_text, output_file, markdown, links, max_body)?;
            }
        }
        OutputFormat::Json => {
            let body_text = response.text().await?;
            let output = serde_json::json!({
                "status": status.as_u16(),
                "size": body_text.len(),
                "time_ms": elapsed.as_secs_f64() * 1000.0,
                "url": url,
            });
            println!("{}", serde_json::to_string(&output)?);

            if let Some(path) = output_file {
                let mut file = File::create(&path)?;
                file.write_all(body_text.as_bytes())?;
            }
        }
        OutputFormat::Full => {
            println!("🌐 Fetching: {url}");
            println!("🎭 User-Agent: {}", profile.user_agent);

            if !cookie_header.is_empty() {
                println!(
                    "🍪 Loaded {} cookies from {}",
                    cookie_header.matches('=').count(),
                    cookies.unwrap_or("browser")
                );
            }

            println!("\n📊 Response:");
            println!("   Status: {status}");
            println!("   Version: {version:?}");
            println!("   Time: {:.2}ms", elapsed.as_secs_f64() * 1000.0);

            if show_headers {
                println!("\n📋 Headers:");
                for (name, value) in response.headers() {
                    println!("   {}: {}", name, value.to_str().unwrap_or("<binary>"));
                }
            }

            let body_text = response.text().await?;
            println!("\n📄 Body: {} bytes", body_text.len());

            if show_body || output_file.is_some() || markdown || links {
                output_body(&body_text, output_file, markdown, links, max_body)?;
            }
        }
    }

    Ok(())
}

fn output_body(
    body: &str,
    output_file: Option<PathBuf>,
    markdown: bool,
    links: bool,
    max_body: usize,
) -> Result<()> {
    // Save to file if requested (always full, no truncation)
    if let Some(path) = output_file {
        let mut file = File::create(&path)?;
        if markdown {
            let md = html_to_markdown(body);
            file.write_all(md.as_bytes())?;
        } else {
            file.write_all(body.as_bytes())?;
        }
        println!("💾 Saved {} bytes to {}", body.len(), path.display());
        return Ok(());
    }

    // Extract links if requested
    if links {
        let extracted = extract_links(body);
        for (text, href) in &extracted {
            if text.is_empty() {
                println!("{href}");
            } else {
                println!("[{}]({href})", truncate_text(text, 50));
            }
        }
        println!("\n({} links)", extracted.len());
        return Ok(());
    }

    // Convert to markdown if requested
    let output = if markdown {
        html_to_markdown(body)
    } else {
        body.to_string()
    };

    // Display with optional limit
    let limit = if max_body == 0 { output.len() } else { max_body };
    if output.len() > limit {
        println!("\n{}", &output[..limit]);
        println!("\n... [{} more bytes]", output.len() - limit);
    } else {
        println!("\n{output}");
    }

    Ok(())
}

fn html_to_markdown(html: &str) -> String {
    // Use html2md for conversion
    let md = html2md::parse_html(html);

    // Post-process: remove excessive whitespace and clutter
    let lines: Vec<&str> = md
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !is_boilerplate(l))
        .collect();

    lines.join("\n")
}

fn is_boilerplate(line: &str) -> bool {
    let lower = line.to_lowercase();
    // Skip common navigation/boilerplate patterns
    lower.contains("skip to content")
        || lower.contains("cookie")
        || lower.contains("privacy policy")
        || lower.contains("terms of service")
        || lower.starts_with("©")
        || lower.starts_with("copyright")
        || (lower.len() < 3 && !lower.chars().any(char::is_alphanumeric))
}

fn extract_links(html: &str) -> Vec<(String, String)> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();

    let mut links = Vec::new();
    let mut seen = HashSet::new();

    for element in document.select(&selector) {
        if let Some(href) = element.value().attr("href") {
            // Skip anchors, javascript, and duplicates
            if href.starts_with('#') || href.starts_with("javascript:") || seen.contains(href) {
                continue;
            }
            seen.insert(href.to_string());

            let text = element
                .text()
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string();

            links.push((text, href.to_string()));
        }
    }

    links
}

fn truncate_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else {
        format!("{}...", &text[..max - 3])
    }
}

async fn cmd_spa(
    url: &str,
    cookies: Option<&str>,
    _show_html: bool,
    _show_console: bool,
    _wait_ms: u64,
    _patterns: Option<&str>,
    output: &str,
    extract_path: Option<&str>,
    summary: bool,
    minify: bool,
    max_array: Option<usize>,
    max_depth: Option<usize>,
    _http1: bool,
) -> Result<()> {
    let client = AcceleratedClient::new()?;

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
            _ => CookieSource::Brave,
        };
        cookie_header = source.get_cookie_header(&domain).unwrap_or_default();
        if !cookie_header.is_empty() {
            println!(
                "🍪 Loading {} cookies for {domain}",
                browser.to_lowercase()
            );
        }
    }

    let profile = client.profile().await;
    let start = Instant::now();

    let response = if cookie_header.is_empty() {
        client.fetch(url).await?
    } else {
        client
            .inner()
            .get(url)
            .header("Cookie", &cookie_header)
            .headers(profile.to_headers())
            .send()
            .await?
    };

    let html = response.text().await?;
    let elapsed = start.elapsed();

    println!("🕸️  Extracting SPA data from: {url}");

    // Look for common SPA data patterns
    let mut found_data = false;

    // __NEXT_DATA__ (Next.js)
    if let Some(data) = extract_script_json(&html, "__NEXT_DATA__") {
        println!("\n📊 Extraction complete in {:.2}ms", elapsed.as_secs_f64() * 1000.0);
        println!("\n✅ __NEXT_DATA__ found:");
        output_spa_data(&data, output, extract_path, summary, minify, max_array, max_depth)?;
        found_data = true;
    }

    // __INITIAL_STATE__ (Redux, Vuex)
    if let Some(data) = extract_script_json(&html, "__INITIAL_STATE__") {
        if !found_data {
            println!("\n📊 Extraction complete in {:.2}ms", elapsed.as_secs_f64() * 1000.0);
        }
        println!("\n✅ __INITIAL_STATE__ found:");
        output_spa_data(&data, output, extract_path, summary, minify, max_array, max_depth)?;
        found_data = true;
    }

    // __NUXT__ (Nuxt.js)
    if let Some(data) = extract_script_json(&html, "__NUXT__") {
        if !found_data {
            println!("\n📊 Extraction complete in {:.2}ms", elapsed.as_secs_f64() * 1000.0);
        }
        println!("\n✅ __NUXT__ found:");
        output_spa_data(&data, output, extract_path, summary, minify, max_array, max_depth)?;
        found_data = true;
    }

    // __PRELOADED_STATE__ (common Redux pattern)
    if let Some(data) = extract_script_json(&html, "__PRELOADED_STATE__") {
        if !found_data {
            println!("\n📊 Extraction complete in {:.2}ms", elapsed.as_secs_f64() * 1000.0);
        }
        println!("\n✅ __PRELOADED_STATE__ found:");
        output_spa_data(&data, output, extract_path, summary, minify, max_array, max_depth)?;
        found_data = true;
    }

    if !found_data {
        println!("\n❌ No SPA data found (__NEXT_DATA__, __INITIAL_STATE__, etc.)");
        println!("   HTML size: {} bytes", html.len());
        println!("   This may be a server-rendered page or data loads via AJAX.");
    }

    Ok(())
}

fn extract_script_json(html: &str, var_name: &str) -> Option<serde_json::Value> {
    // Pattern: window.__VAR__ = {...} or <script id="__VAR__">...</script>
    let document = Html::parse_document(html);

    // Try script tag with id
    let id_selector = Selector::parse(&format!("script#{var_name}")).ok()?;
    if let Some(script) = document.select(&id_selector).next() {
        let content = script.text().collect::<String>();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            return Some(json);
        }
    }

    // Try window.__VAR__ = pattern
    let pattern = format!("window.{var_name}");
    if let Some(start_idx) = html.find(&pattern) {
        let after_eq = html[start_idx..].find('=')? + start_idx + 1;
        let json_start = html[after_eq..].chars().position(|c| c == '{' || c == '[')? + after_eq;

        // Find matching bracket
        let json_str = extract_json_object(&html[json_start..])?;
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
            return Some(json);
        }
    }

    // Try self.__VAR__ pattern (some frameworks)
    let self_pattern = format!("self.{var_name}");
    if let Some(start_idx) = html.find(&self_pattern) {
        let after_eq = html[start_idx..].find('=')? + start_idx + 1;
        let json_start = html[after_eq..].chars().position(|c| c == '{' || c == '[')? + after_eq;
        let json_str = extract_json_object(&html[json_start..])?;
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
            return Some(json);
        }
    }

    None
}

fn extract_json_object(s: &str) -> Option<&str> {
    let first_char = s.chars().next()?;
    let (open, close) = match first_char {
        '{' => ('{', '}'),
        '[' => ('[', ']'),
        _ => return None,
    };

    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, c) in s.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match c {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            _ if in_string => {}
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
        }
    }

    None
}

fn output_spa_data(
    data: &serde_json::Value,
    output: &str,
    extract_path: Option<&str>,
    summary: bool,
    minify: bool,
    max_array: Option<usize>,
    max_depth: Option<usize>,
) -> Result<()> {
    // Extract specific path if requested
    let target = if let Some(path) = extract_path {
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = data;
        for part in parts {
            current = current.get(part).unwrap_or(&serde_json::Value::Null);
        }
        current.clone()
    } else {
        data.clone()
    };

    // Apply transformations
    let transformed = if max_array.is_some() || max_depth.is_some() {
        transform_json(&target, max_array.unwrap_or(usize::MAX), max_depth.unwrap_or(usize::MAX), 0)
    } else {
        target
    };

    // Output
    if summary {
        println!("   {} bytes", serde_json::to_string(&transformed)?.len());
        print_structure(&transformed, 3, 0);
    } else if output == "json" || minify {
        if minify {
            println!("{}", serde_json::to_string(&transformed)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&transformed)?);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&transformed)?);
    }

    Ok(())
}

fn transform_json(value: &serde_json::Value, max_array: usize, max_depth: usize, depth: usize) -> serde_json::Value {
    if depth >= max_depth {
        return serde_json::Value::String("[depth limit]".to_string());
    }

    match value {
        serde_json::Value::Array(arr) => {
            let limited: Vec<serde_json::Value> = arr
                .iter()
                .take(max_array)
                .map(|v| transform_json(v, max_array, max_depth, depth + 1))
                .collect();
            if arr.len() > max_array {
                let mut result = limited;
                result.push(serde_json::Value::String(format!("... +{} more", arr.len() - max_array)));
                serde_json::Value::Array(result)
            } else {
                serde_json::Value::Array(limited)
            }
        }
        serde_json::Value::Object(obj) => {
            let transformed: serde_json::Map<String, serde_json::Value> = obj
                .iter()
                .map(|(k, v)| (k.clone(), transform_json(v, max_array, max_depth, depth + 1)))
                .collect();
            serde_json::Value::Object(transformed)
        }
        _ => value.clone(),
    }
}

fn print_structure(value: &serde_json::Value, max_depth: usize, depth: usize) {
    let indent = "  ".repeat(depth);

    if depth >= max_depth {
        println!("{indent}...");
        return;
    }

    match value {
        serde_json::Value::Object(obj) => {
            for (key, val) in obj {
                match val {
                    serde_json::Value::Object(_) => {
                        println!("{indent}{key}: {{...}}");
                        print_structure(val, max_depth, depth + 1);
                    }
                    serde_json::Value::Array(arr) => {
                        println!("{indent}{key}: [{} items]", arr.len());
                    }
                    _ => {
                        let type_name = match val {
                            serde_json::Value::String(_) => "string",
                            serde_json::Value::Number(_) => "number",
                            serde_json::Value::Bool(_) => "bool",
                            serde_json::Value::Null => "null",
                            _ => "?",
                        };
                        println!("{indent}{key}: {type_name}");
                    }
                }
            }
        }
        serde_json::Value::Array(arr) if !arr.is_empty() => {
            println!("{indent}[0]:");
            print_structure(&arr[0], max_depth, depth + 1);
        }
        _ => {}
    }
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

async fn cmd_stream(
    source: &str,
    id: &str,
    output: &str,
    quality: &str,
    force_native: bool,
    force_ffmpeg: bool,
    info_only: bool,
    list_episodes: bool,
    cookies: Option<&str>,
    duration: Option<&str>,
    ffmpeg_opts: Option<&str>,
    player: Option<&str>,
) -> Result<()> {
    use microfetch::stream::{
        StreamProvider, StreamBackend, StreamQuality,
        providers::{YleProvider, GenericHlsProvider},
        backends::{NativeHlsBackend, FfmpegBackend},
        backend::StreamConfig,
    };
    use microfetch::CookieSource;
    use std::collections::HashMap;
    use tokio::io::{stdout, AsyncWriteExt};
    use std::process::Stdio;

    // Parse quality
    let stream_quality = match quality.to_lowercase().as_str() {
        "best" => StreamQuality::Best,
        "worst" => StreamQuality::Worst,
        q => q.parse::<u32>()
            .map(StreamQuality::Specific)
            .unwrap_or(StreamQuality::Best),
    };

    // Select provider based on source
    let provider: Box<dyn StreamProvider> = match source.to_lowercase().as_str() {
        "yle" => Box::new(YleProvider::new()?),
        "generic" | "hls" | "dash" => Box::new(GenericHlsProvider::new()),
        url if url.starts_with("http") => {
            // Direct URL - use appropriate provider
            if url.contains("areena.yle.fi") || url.contains("arenan.yle.fi") {
                Box::new(YleProvider::new()?)
            } else {
                Box::new(GenericHlsProvider::new())
            }
        }
        _ => {
            // Try to detect from ID
            if id.contains("areena.yle.fi") || id.starts_with("1-") {
                Box::new(YleProvider::new()?)
            } else if id.ends_with(".m3u8") || id.ends_with(".mpd") {
                Box::new(GenericHlsProvider::new())
            } else {
                anyhow::bail!("Unknown source: {source}. Use 'yle', 'generic', or a direct URL.");
            }
        }
    };

    eprintln!("🎬 Provider: {}", provider.name());

    // List episodes mode
    if list_episodes {
        eprintln!("📋 Listing episodes for: {id}");
        let series = provider.list_series(id).await?;
        println!("Series: {}", series.title);
        println!("Episodes: {}", series.episodes.len());
        for ep in &series.episodes {
            let duration = ep.duration_seconds
                .map(|d| format!(" ({}:{:02})", d / 60, d % 60))
                .unwrap_or_default();
            let ep_num = ep.episode_number
                .map(|n| format!("E{n}"))
                .unwrap_or_default();
            let season = ep.season_number
                .map(|n| format!("S{n}"))
                .unwrap_or_default();
            println!("  {} {}{}: {}{}", ep.id, season, ep_num, ep.title, duration);
        }
        return Ok(());
    }

    // Get stream info
    eprintln!("📡 Fetching stream info for: {id}");
    let stream_info = provider.get_stream_info(id).await?;

    // Info only mode
    if info_only {
        println!("Title: {}", stream_info.title);
        if let Some(ref desc) = stream_info.description {
            println!("Description: {desc}");
        }
        if let Some(dur) = stream_info.duration_seconds {
            println!("Duration: {}:{:02}", dur / 60, dur % 60);
        }
        println!("Live: {}", stream_info.is_live);
        println!("Manifest: {}", stream_info.manifest_url);
        if let Some(ref thumb) = stream_info.thumbnail_url {
            println!("Thumbnail: {thumb}");
        }
        return Ok(());
    }

    eprintln!("📺 {}", stream_info.title);
    if stream_info.is_live {
        eprintln!("   🔴 LIVE");
    }
    if let Some(dur) = stream_info.duration_seconds {
        eprintln!("   Duration: {}:{:02}", dur / 60, dur % 60);
    }

    // Build stream config
    let mut headers = HashMap::new();
    headers.insert("Referer".to_string(), "https://areena.yle.fi".to_string());
    headers.insert("Origin".to_string(), "https://areena.yle.fi".to_string());

    // For Yle: Always add X-Forwarded-For for CDN access (required by Akamai)
    // Cookies provide session auth for premium content, but CDN still checks geo
    if provider.name() == "yle" {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        // Elisa Finland IP range: 91.152.0.0/13 (91.152.0.1 - 91.159.255.254)
        let ip = format!("91.{}.{}.{}",
            rng.gen_range(152..160),
            rng.gen_range(0..256),
            rng.gen_range(1..255));
        headers.insert("X-Forwarded-For".to_string(), ip);

        if cookies.is_some() {
            eprintln!("🔐 Using browser session + Finnish IP for Yle");
        } else {
            eprintln!("🌍 Using Finnish IP for geo access. Add --cookies brave for premium content.");
        }
    }

    // Extract cookies from browser if specified
    if let Some(browser) = cookies {
        eprintln!("🍪 Extracting cookies from {browser}...");
        let cookie_source = match browser.to_lowercase().as_str() {
            "brave" => CookieSource::Brave,
            "chrome" => CookieSource::Chrome,
            "firefox" => CookieSource::Firefox,
            "safari" => CookieSource::Safari,
            _ => CookieSource::Brave,
        };

        // Get Yle cookies
        match cookie_source.get_cookies("yle.fi") {
            Ok(cookie_map) if !cookie_map.is_empty() => {
                let cookie_str: String = cookie_map.iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                headers.insert("Cookie".to_string(), cookie_str);
                eprintln!("   ✅ Found {} cookies for yle.fi", cookie_map.len());
            }
            Ok(_) => {
                eprintln!("   ⚠️  No cookies found for yle.fi. Are you logged in?");
            }
            Err(e) => {
                eprintln!("   ⚠️  Cookie extraction failed: {e}");
            }
        }
    }

    let config = StreamConfig {
        quality: stream_quality,
        headers,
        cookies: cookies.map(String::from),
    };

    // For Yle, get fresh manifest URL via yle-dl (Akamai tokens expire quickly)
    let manifest_url = if provider.name() == "yle" {
        eprintln!("🔄 Getting fresh manifest URL via yle-dl...");
        let yle_provider = YleProvider::new()?;
        match yle_provider.get_fresh_manifest_url(id).await {
            Ok(url) => {
                eprintln!("   ✅ Got fresh URL");
                url
            }
            Err(e) => {
                eprintln!("   ⚠️  yle-dl failed: {e}");
                eprintln!("   Using preview API URL (may fail)");
                stream_info.manifest_url.clone()
            }
        }
    } else {
        stream_info.manifest_url.clone()
    };
    let manifest_url = &manifest_url;
    let is_dash = manifest_url.contains(".mpd");
    let is_encrypted = false; // Would need manifest parsing to detect

    let use_ffmpeg = force_ffmpeg || is_dash || is_encrypted || ffmpeg_opts.is_some();
    let use_native = force_native && !is_dash && !is_encrypted;

    if use_ffmpeg && !use_native {
        eprintln!("🔧 Backend: ffmpeg");
        let mut backend = FfmpegBackend::new()?;

        if let Some(opts) = ffmpeg_opts {
            backend = backend.with_transcode_opts(opts);
        }

        // Check ffmpeg availability
        if !backend.check_available().await {
            anyhow::bail!("ffmpeg not found in PATH. Install ffmpeg or use --native.");
        }

        let progress_cb = |p: microfetch::stream::backend::StreamProgress| {
            eprint!("\r   📥 {:.1} MB, {:.1}s elapsed    ",
                p.bytes_downloaded as f64 / 1_000_000.0,
                p.elapsed_seconds);
        };

        if let Some(player_cmd) = player {
            // Stream to media player
            eprintln!("🎬 Piping to: {player_cmd}");
            let player_args = get_player_stdin_args(player_cmd);
            let mut child = tokio::process::Command::new(player_cmd)
                .args(&player_args)
                .stdin(Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| anyhow::anyhow!("Failed to spawn {player_cmd}: {e}"))?;

            let mut stdin = child.stdin.take()
                .ok_or_else(|| anyhow::anyhow!("Failed to get stdin for {player_cmd}"))?;

            if let Some(dur_str) = duration {
                let secs = parse_duration(dur_str)?;
                backend.stream_with_duration(manifest_url, &config, &mut stdin, secs, Some(Box::new(progress_cb))).await?;
            } else {
                backend.stream_to(manifest_url, &config, &mut stdin, Some(Box::new(progress_cb))).await?;
            }

            drop(stdin); // Close stdin to signal EOF
            child.wait().await?;
        } else if output == "-" {
            // Stream to stdout
            let mut stdout = stdout();
            if let Some(dur_str) = duration {
                let secs = parse_duration(dur_str)?;
                backend.stream_with_duration(manifest_url, &config, &mut stdout, secs, Some(Box::new(progress_cb))).await?;
            } else {
                backend.stream_to(manifest_url, &config, &mut stdout, Some(Box::new(progress_cb))).await?;
            }
            stdout.flush().await?;
        } else {
            // Stream to file
            let path = std::path::Path::new(output);
            let duration_parsed = duration.map(parse_duration).transpose()?;
            backend.stream_to_file(manifest_url, &config, path, Some(Box::new(progress_cb)), duration_parsed).await?;
        }
    } else {
        eprintln!("🔧 Backend: native");
        let backend = NativeHlsBackend::new()?;

        if !backend.can_handle(manifest_url, is_encrypted) {
            anyhow::bail!("Native backend cannot handle this stream. Try --ffmpeg.");
        }

        let progress_cb = |p: microfetch::stream::backend::StreamProgress| {
            let total = p.segments_total.map(|t| format!("/{t}")).unwrap_or_default();
            eprint!("\r   📥 {:.1} MB, {}{} segments, {:.1}s    ",
                p.bytes_downloaded as f64 / 1_000_000.0,
                p.segments_completed,
                total,
                p.elapsed_seconds);
        };

        if let Some(player_cmd) = player {
            // Stream to media player
            eprintln!("🎬 Piping to: {player_cmd}");
            let player_args = get_player_stdin_args(player_cmd);
            let mut child = tokio::process::Command::new(player_cmd)
                .args(&player_args)
                .stdin(Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| anyhow::anyhow!("Failed to spawn {player_cmd}: {e}"))?;

            let mut stdin = child.stdin.take()
                .ok_or_else(|| anyhow::anyhow!("Failed to get stdin for {player_cmd}"))?;

            backend.stream_to(manifest_url, &config, &mut stdin, Some(Box::new(progress_cb))).await?;

            drop(stdin); // Close stdin to signal EOF
            child.wait().await?;
        } else if output == "-" {
            let mut stdout = stdout();
            backend.stream_to(manifest_url, &config, &mut stdout, Some(Box::new(progress_cb))).await?;
            stdout.flush().await?;
        } else {
            let path = std::path::Path::new(output);
            let duration_parsed = duration.map(parse_duration).transpose()?;
            backend.stream_to_file(manifest_url, &config, path, Some(Box::new(progress_cb)), duration_parsed).await?;
        }
    }

    eprintln!("\n✅ Stream complete");
    Ok(())
}

/// Get arguments for media players to read from stdin
fn get_player_stdin_args(player: &str) -> Vec<&'static str> {
    match player {
        "vlc" => vec!["-", "--intf", "dummy", "--play-and-exit"],
        "mpv" => vec!["-"],
        "ffplay" => vec!["-i", "-"],
        "mplayer" => vec!["-"],
        "iina" => vec!["--stdin"],
        _ => vec!["-"], // Default: most players accept - for stdin
    }
}

/// Parse duration string like "1h", "30m", "1h30m", "90" (seconds)
fn parse_duration(s: &str) -> Result<u64> {
    let s = s.trim().to_lowercase();

    if let Ok(secs) = s.parse::<u64>() {
        return Ok(secs);
    }

    let mut total_secs = 0u64;
    let mut current_num = String::new();

    for c in s.chars() {
        if c.is_ascii_digit() {
            current_num.push(c);
        } else {
            let num: u64 = current_num.parse().unwrap_or(0);
            current_num.clear();

            match c {
                'h' => total_secs += num * 3600,
                'm' => total_secs += num * 60,
                's' => total_secs += num,
                _ => {}
            }
        }
    }

    // Handle trailing number (assume seconds)
    if !current_num.is_empty() {
        total_secs += current_num.parse::<u64>().unwrap_or(0);
    }

    if total_secs == 0 {
        anyhow::bail!("Invalid duration: {s}. Use format like '1h', '30m', '1h30m', or seconds.");
    }

    Ok(total_secs)
}

async fn cmd_analyze(
    video: &str,
    audio_only: bool,
    diarize: bool,
    format: AnalyzeOutputFormat,
    output: Option<PathBuf>,
    dgx: bool,
    api_key: Option<&str>,
) -> Result<()> {
    use microfetch::analyze::{
        AnalysisPipeline, PipelineConfig as AnalysisConfig, VisionBackend,
        report::{AnalysisReport, ReportFormat},
    };

    eprintln!("🎬 Analyzing: {video}");

    // Auto-detect audio-only files by extension
    let is_audio_file = video.to_lowercase().ends_with(".wav")
        || video.to_lowercase().ends_with(".mp3")
        || video.to_lowercase().ends_with(".flac")
        || video.to_lowercase().ends_with(".m4a")
        || video.to_lowercase().ends_with(".aac")
        || video.to_lowercase().ends_with(".ogg");

    let audio_only = audio_only || is_audio_file;

    if is_audio_file {
        eprintln!("   Detected audio-only file, skipping video analysis");
    }

    // Build configuration
    let mut config = AnalysisConfig::default();

    // DGX offload
    if dgx {
        config.dgx_host = Some("spark".to_string());
        eprintln!("   GPU: DGX Spark (nvfp4 quantization)");
    }

    // Diarization
    config.enable_diarization = diarize;
    if diarize {
        eprintln!("   Diarization: enabled");
    }

    // Vision backend
    let _skip_vision = audio_only;
    if audio_only {
        eprintln!("   Mode: audio-only (transcription)");
    } else if let Some(key) = api_key {
        config.vision_backend = VisionBackend::ClaudeApi { api_key: key.to_string() };
        eprintln!("   Vision: Claude API");
    } else if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        config.vision_backend = VisionBackend::ClaudeApi { api_key: key };
        eprintln!("   Vision: Claude API (from ANTHROPIC_API_KEY)");
    } else {
        config.vision_backend = VisionBackend::Local;
        eprintln!("   Vision: local models");
    }

    // Create and run pipeline
    let pipeline = AnalysisPipeline::with_config(config)?;

    let start = std::time::Instant::now();
    let analysis = if audio_only {
        pipeline.analyze_audio_only(video).await?
    } else {
        pipeline.analyze(video).await?
    };
    let elapsed = start.elapsed();

    eprintln!(
        "\n✅ Analysis complete: {} segments in {:.1}s",
        analysis.segments.len(),
        elapsed.as_secs_f64()
    );

    // Generate output
    let report_format = match format {
        AnalyzeOutputFormat::Json => ReportFormat::Json,
        AnalyzeOutputFormat::Markdown => ReportFormat::Markdown,
        AnalyzeOutputFormat::Srt => ReportFormat::Srt,
    };

    let report = AnalysisReport::generate(&analysis, report_format)?;

    // Output to file or stdout
    if let Some(path) = output {
        std::fs::write(&path, &report)?;
        eprintln!("📄 Saved to: {}", path.display());
    } else {
        println!("{report}");
    }

    // Summary stats to stderr
    if let Some(ref meta) = analysis.metadata {
        eprintln!("\n📊 Video: {}x{} @ {:.1}fps, {:.1}s",
            meta.width, meta.height, meta.fps, meta.duration);
    }

    let speakers: std::collections::HashSet<_> = analysis.segments
        .iter()
        .filter_map(|s| s.speaker.as_ref())
        .collect();

    if !speakers.is_empty() {
        eprintln!("   Speakers: {}", speakers.len());
    }

    Ok(())
}

async fn cmd_annotate(
    video: &str,
    output: &str,
    subtitles: bool,
    speaker_labels: bool,
    analysis: bool,
    style: OverlayStyleArg,
    hwaccel: bool,
) -> Result<()> {
    use microfetch::annotate::{
        AnnotationPipeline,
        PipelineConfig,
        AnalysisConfig,
    };

    eprintln!("🎬 Annotating: {video}");
    eprintln!("   Output: {output}");

    // Build configuration based on style
    let mut config = match style {
        OverlayStyleArg::Minimal => PipelineConfig::default(),
        OverlayStyleArg::Detailed => PipelineConfig::high_quality()
            .with_speaker_labels(true),
        OverlayStyleArg::Debug => PipelineConfig::high_quality()
            .with_speaker_labels(true)
            .with_analysis(true),
    };

    // Override with explicit flags
    if subtitles || (!subtitles && !speaker_labels && !analysis) {
        config.subtitles = true;
        eprintln!("   Subtitles: enabled");
    }

    if speaker_labels {
        config.speaker_labels = true;
        config.transcription = config.transcription.with_diarization();
        eprintln!("   Speaker labels: enabled");
    }

    if analysis {
        config.analysis_overlay = true;
        config.analysis = AnalysisConfig::full();
        eprintln!("   Analysis overlay: enabled");
    }

    // Hardware acceleration (VideoToolbox on macOS, NVENC on Linux)
    if hwaccel {
        #[cfg(target_os = "macos")]
        {
            config.compositor = config.compositor.with_hwaccel("videotoolbox");
            eprintln!("   Hardware acceleration: VideoToolbox");
        }
        #[cfg(not(target_os = "macos"))]
        {
            config.compositor = config.compositor.with_hwaccel("nvenc");
            eprintln!("   Hardware acceleration: NVENC");
        }
    }

    eprintln!("   Style: {style:?}");

    // Create and run pipeline
    let pipeline = AnnotationPipeline::new(config)?;

    let start = std::time::Instant::now();
    let result = pipeline.process_file(video, output).await?;
    let elapsed = start.elapsed();

    eprintln!("\n✅ Annotation complete in {:.1}s", elapsed.as_secs_f64());

    if let Some(ref path) = result.output_path {
        eprintln!("   Output: {}", path.display());
    }

    eprintln!("   Subtitles: {} entries", result.subtitle_count);
    eprintln!("   Speakers detected: {}", result.speakers.len());

    if let Some(ref lang) = result.detected_language {
        eprintln!("   Language: {lang}");
    }

    Ok(())
}
