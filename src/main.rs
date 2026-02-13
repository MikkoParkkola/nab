//! `MicroFetch` CLI - Token-optimized HTTP client with SPA extraction
//!
//! Designed for LLM consumption: minimal tokens, maximum information.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

mod cmd;

#[derive(Parser)]
#[command(name = "nab")]
#[command(about = "Token-optimized HTTP client with SPA extraction")]
#[command(version)]
struct Cli {
    /// Enable verbose debug logging
    #[arg(short, long, global = true)]
    verbose: bool,

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

        /// Use cookies from browser (auto, brave, chrome, firefox, safari, edge). Use 'none' to disable.
        #[arg(short, long, default_value = "auto")]
        cookies: String,

        /// Use 1Password credentials for this URL
        #[arg(long = "1password", visible_alias = "op")]
        use_1password: bool,

        /// Output raw HTML instead of markdown
        #[arg(long)]
        raw_html: bool,

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

        /// Disable automatic SPA data extraction (Next.js, Nuxt, Redux, etc.)
        #[arg(long)]
        no_spa: bool,

        /// Batch fetch URLs from file (one per line, # comments allowed)
        #[arg(long)]
        batch: Option<String>,

        /// Max concurrent requests for batch mode (default: 5)
        #[arg(long, default_value = "5")]
        parallel: usize,

        /// Proxy URL (SOCKS5 or HTTP). Also checks HTTP_PROXY/HTTPS_PROXY/ALL_PROXY env vars.
        #[arg(long)]
        proxy: Option<String>,
    },

    /// Extract data from JavaScript-heavy SPA pages
    Spa {
        /// URL to extract data from
        url: String,

        /// Use cookies from browser (auto, brave, chrome, firefox, safari, edge). Use 'none' to disable.
        #[arg(short, long, default_value = "auto")]
        cookies: String,

        /// Show raw HTML
        #[arg(long)]
        html: bool,

        /// Show console output from JS execution
        #[arg(long)]
        console: bool,

        /// Wait time in milliseconds after page load for AJAX/setTimeout to complete
        #[arg(long, default_value = "5000")]
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

        /// Use cookies from browser (auto, brave, chrome, firefox, safari, edge). Use 'none' to disable.
        #[arg(short, long, default_value = "auto")]
        cookies: String,

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

    /// Submit a form with smart field extraction (hidden fields, CSRF tokens)
    Submit {
        /// URL of the form page
        url: String,

        /// Form fields as "name=value" pairs (can be repeated)
        #[arg(short, long = "field", action = clap::ArgAction::Append)]
        fields: Vec<String>,

        /// Extract CSRF token from specific selector (e.g., "input[name=_token]")
        #[arg(long)]
        csrf_from: Option<String>,

        /// Use cookies from browser (auto, brave, chrome, firefox, safari, edge). Use 'none' to disable.
        #[arg(short, long, default_value = "auto")]
        cookies: String,

        /// Use 1Password credentials
        #[arg(long = "1password", visible_alias = "op")]
        use_1password: bool,

        /// Show response headers
        #[arg(short = 'H', long)]
        headers: bool,

        /// Output format: full, compact, json
        #[arg(short = 'f', long, default_value = "full")]
        format: OutputFormat,
    },

    /// Auto-login to a website using 1Password credentials
    Login {
        /// URL of the login page or target page (will find login form)
        url: String,

        /// Use 1Password credentials (required)
        #[arg(long = "1password", visible_alias = "op", default_value = "true")]
        use_1password: bool,

        /// Save session cookies for future requests
        #[arg(long)]
        save_session: bool,

        /// Use cookies from browser (auto, brave, chrome, firefox, safari, edge). Use 'none' to disable.
        #[arg(short, long, default_value = "auto")]
        cookies: String,

        /// Show response headers
        #[arg(short = 'H', long)]
        headers: bool,

        /// Output format: full, compact, json
        #[arg(short = 'f', long, default_value = "full")]
        format: OutputFormat,
    },

    /// Export or manage browser cookies
    Cookies {
        #[command(subcommand)]
        action: CookiesAction,
    },
}

#[derive(Subcommand)]
enum CookiesAction {
    /// Export cookies for a domain in Netscape format
    Export {
        /// Domain to export cookies for (e.g., "github.com")
        domain: String,

        /// Browser to export from (auto, brave, chrome, firefox, safari, edge)
        #[arg(short, long, default_value = "auto")]
        cookies: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging based on --verbose flag
    let log_level = if cli.verbose { Level::DEBUG } else { Level::INFO };

    FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_target(false)
        .compact()
        .init();

    match cli.command {
        Commands::Fetch {
            url,
            headers,
            body,
            format,
            output,
            cookies,
            use_1password,
            raw_html,
            links,
            max_body,
            add_headers,
            auto_referer,
            warmup_url,
            method,
            data,
            capture_cookies,
            no_redirect,
            no_spa,
            batch,
            parallel,
            proxy,
        } => {
            cmd::cmd_fetch(
                &url,
                headers,
                body,
                format,
                output,
                &cookies,
                use_1password,
                raw_html,
                links,
                max_body,
                &add_headers,
                auto_referer,
                warmup_url.as_deref(),
                &method,
                data.as_deref(),
                capture_cookies,
                no_redirect,
                no_spa,
                batch.as_deref(),
                parallel,
                proxy.as_deref(),
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
            cmd::cmd_spa(
                &url,
                &cookies,
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
            cmd::cmd_bench(&urls, iterations).await?;
        }
        Commands::Fingerprint { count } => {
            cmd::cmd_fingerprint(count);
        }
        Commands::Auth { url } => {
            cmd::cmd_auth(&url)?;
        }
        Commands::Validate => {
            cmd::cmd_validate().await?;
        }
        Commands::Otp { domain } => {
            cmd::cmd_otp(&domain)?;
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
            cmd::cmd_stream(
                &source,
                &id,
                &output,
                &quality,
                native,
                ffmpeg,
                info,
                list,
                &cookies,
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
            cmd::cmd_analyze(
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
            cmd::cmd_annotate(
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
        Commands::Submit {
            url,
            fields,
            csrf_from,
            cookies,
            use_1password,
            headers,
            format,
        } => {
            cmd::cmd_submit(
                &url,
                &fields,
                csrf_from.as_deref(),
                &cookies,
                use_1password,
                headers,
                format,
            )
            .await?;
        }
        Commands::Login {
            url,
            use_1password,
            save_session,
            cookies,
            headers,
            format,
        } => {
            cmd::cmd_login(
                &url,
                use_1password,
                save_session,
                &cookies,
                headers,
                format,
            )
            .await?;
        }
        Commands::Cookies { action } => match action {
            CookiesAction::Export { domain, cookies } => {
                cmd::cmd_cookies("export", &domain, &cookies).await?;
            }
        },
    }

    Ok(())
}
