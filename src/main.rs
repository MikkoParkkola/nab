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
#[command(about = "Fetch any URL as clean markdown — optimized for LLM context windows")]
#[command(version)]
#[command(after_help = "Examples:\n  \
    nab fetch https://example.com          Fetch as markdown\n  \
    nab fetch URL --cookies brave          Use browser cookies\n  \
    nab context URL1 URL2 URL3             Combine multiple URLs\n  \
    nab rules list                         Show active site rules")]
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
    /// Human-friendly with emoji status indicators
    Full,
    /// Minimal one-line: STATUS SIZE TIME (pipe-friendly)
    Compact,
    /// Structured JSON with metadata and markdown body
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

        /// Disable remote thin-content fallback via `r.jina.ai`
        #[arg(long)]
        no_fallback: bool,

        /// Batch fetch URLs from file (one per line, # comments allowed)
        #[arg(long)]
        batch: Option<String>,

        /// Max concurrent requests for batch mode (default: 5)
        #[arg(long, default_value = "5")]
        parallel: usize,

        /// Proxy URL (SOCKS5 or HTTP). Also checks `HTTP_PROXY/HTTPS_PROXY/ALL_PROXY` env vars.
        #[arg(long)]
        proxy: Option<String>,

        /// Show what changed since the last fetch (stores snapshots in ~/.nab/snapshots/)
        #[arg(long)]
        diff: bool,

        /// Do not save the fetch result to hebb kv store (default: save when hebb is available)
        #[arg(long)]
        no_save: bool,

        /// Do not OCR images in the fetched HTML (default: OCR when Apple Vision is available)
        #[arg(long)]
        no_ocr: bool,

        /// Do not auto-transcribe media URLs (YouTube, SoundCloud, direct .mp3/.mp4, etc.)
        ///
        /// By default, when nab detects a media URL it downloads the audio via yt-dlp,
        /// transcribes it via FluidAudio/sherpa-onnx, and returns the transcript as markdown.
        /// Pass this flag to disable that behaviour and fetch the page as plain HTML instead.
        #[arg(long)]
        no_transcribe: bool,

        /// BCP-47 language hint for transcription (e.g. "fi", "en-US", "de").
        /// Defaults to auto-detection when omitted.
        #[arg(long)]
        language: Option<String>,
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

        /// API endpoint URL fragments to look for (comma-separated)
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

        /// Enable active reading — identify and look up references in the transcript.
        ///
        /// Only available via the nab MCP server (nab-mcp); the CLI will print a
        /// notice and continue with passive transcription.
        #[arg(long)]
        active_reading: bool,
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

        /// Use browser automation for SPA login and CAPTCHA handling (requires --features browser)
        #[cfg(feature = "browser")]
        #[arg(long)]
        browser: bool,
    },

    /// Fetch multiple URLs in parallel and combine into LLM-ready markdown
    ///
    /// Output goes to stdout; progress goes to stderr.  Pipe to a file or
    /// clipboard for use as LLM context.
    Context {
        /// One or more URLs to fetch
        #[arg(required = true)]
        urls: Vec<String>,

        /// Use cookies from browser (auto, brave, chrome, firefox, safari, edge). Use 'none' to disable.
        #[arg(short, long, default_value = "auto")]
        cookies: String,

        /// Enable verbose debug logging
        #[arg(short, long)]
        verbose: bool,

        /// Approximate token budget for the combined output (default: 8000)
        #[arg(long, default_value = "8000")]
        max_tokens: usize,
    },

    /// Export or manage browser cookies
    Cookies {
        #[command(subcommand)]
        action: CookiesAction,
    },

    /// Export embedded default site rules to ~/.config/nab/sites/
    ///
    /// Writes each built-in TOML rule file to the user config directory so it
    /// can be inspected and customised.  Existing files are never overwritten.
    Rules {
        #[command(subcommand)]
        action: RulesAction,
    },

    /// Manage WASM provider marketplace (install, list, remove)
    ///
    /// WASM providers extend nab with custom site extractors compiled from any
    /// language (Rust, Go, `AssemblyScript`, etc.) and run in a sandboxed
    /// environment with no filesystem or network access.
    ///
    /// Requires the `wasm-providers` feature flag.
    Provider {
        #[command(subcommand)]
        action: ProviderAction,
    },

    /// Monitor URLs for content changes (RSS for the entire web)
    Watch {
        #[command(subcommand)]
        action: WatchAction,
    },

    /// Manage locally-built inference model binaries (FluidAudio, Whisper, …)
    ///
    /// Clones, builds, and symlinks model binaries into a persistent location so
    /// they survive reboots. Install location: `~/.local/share/nab/models/`
    /// Binary symlinks: `~/.local/share/nab/bin/`
    Models {
        #[command(subcommand)]
        action: ModelsAction,
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

#[derive(Subcommand)]
enum RulesAction {
    /// Export all embedded default TOML rules to ~/.config/nab/sites/
    ///
    /// Existing files are skipped so user customisations are preserved.
    Export,

    /// List all active site rules and their sources
    ///
    /// Shows embedded rules, user overrides, and user-only rules in a table.
    /// Embedded rules appear first (in definition order); user-only rules
    /// follow alphabetically.
    List,
}

#[derive(Subcommand)]
enum ProviderAction {
    /// List all installed WASM providers
    List,

    /// Install a WASM provider from a local path or URL
    ///
    /// `src` may be:
    ///   - A local directory containing manifest.toml + provider.wasm
    ///   - A local .wasm file with a sidecar <name>.manifest.toml
    ///   - An HTTP/HTTPS URL pointing to a .wasm file
    Install {
        /// Local path (directory or .wasm file) or HTTP/HTTPS URL
        src: String,
    },

    /// Remove an installed WASM provider by name
    Remove {
        /// Provider name (as shown in `nab provider list`)
        name: String,
    },
}

#[derive(Subcommand)]
enum WatchAction {
    /// Add a new URL watch (does an initial fetch to seed the first snapshot)
    Add {
        /// URL to watch
        url: String,

        /// Polling interval: `30s`, `5m`, `1h`, `24h` (default: `1h`)
        #[arg(short, long)]
        interval: Option<String>,

        /// CSS selector — only the matched element is watched
        #[arg(short, long)]
        selector: Option<String>,

        /// Diff algorithm: `text` | `semantic` | `dom` (default: `text`)
        #[arg(long = "diff-kind", default_value = "text")]
        diff_kind: String,
    },

    /// List all active watches
    List {
        /// Output format: `table` | `json`
        #[arg(short, long, default_value = "table")]
        format: String,
    },

    /// Remove a watch by ID
    Remove {
        /// Watch ID (as shown in `nab watch list`)
        id: String,
    },

    /// Show snapshot history for a watch
    Logs {
        /// Watch ID
        id: String,
    },
}

#[derive(Subcommand)]
enum ModelsAction {
    /// Clone + build + symlink a model binary into a persistent location
    ///
    /// Supported: `fluidaudio` (macOS only). Phase 3: `whisper`, `sherpa-onnx`.
    Fetch {
        /// Model name: `fluidaudio`, `whisper`, `sherpa-onnx`
        name: String,
    },

    /// List all managed models with install status and version
    List,

    /// Pull latest changes and rebuild an installed model
    Update {
        /// Model name (must already be installed)
        name: String,
    },

    /// Verify every installed model binary runs without error
    Verify,
}

#[tokio::main]
#[allow(clippy::too_many_lines)] // Main function dispatches to all commands; cannot be split
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging. `nab context` defaults to ERROR-only for clean
    // stdout piping; other commands use INFO (or DEBUG with --verbose).
    let is_quiet_context = matches!(&cli.command, Commands::Context { verbose: false, .. });
    let log_level = if cli.verbose {
        Level::DEBUG
    } else if is_quiet_context {
        Level::ERROR
    } else {
        Level::INFO
    };

    FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_target(false)
        .with_writer(std::io::stderr)
        .compact()
        .init();

    let result: Result<()> = async {
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
                no_fallback,
                batch,
                parallel,
                proxy,
                diff,
                no_save,
                no_ocr,
                no_transcribe,
                language,
                ..
            } => {
                let cfg = cmd::FetchConfig {
                    url,
                    show_headers: headers,
                    show_body: body,
                    format,
                    output_file: output,
                    cookies,
                    use_1password,
                    raw_html,
                    links,
                    max_body,
                    custom_headers: add_headers,
                    auto_referer,
                    warmup_url,
                    method,
                    data,
                    capture_cookies,
                    no_redirect,
                    batch_file: batch,
                    parallel,
                    proxy,
                    show_diff: diff,
                    no_save,
                    no_ocr,
                    no_transcribe,
                    language,
                    html_options: nab::content::html::HtmlConversionOptions {
                        allow_spa_extraction: !no_spa,
                        allow_jina_fallback: !no_fallback,
                    },
                };
                cmd::cmd_fetch(&cfg).await?;
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
                ..
            } => {
                let cfg = cmd::SpaConfig {
                    url,
                    cookies,
                    show_html: html,
                    show_console: console,
                    wait_ms: wait,
                    endpoint_hints: patterns
                        .into_iter()
                        .flat_map(|value| {
                            value
                                .split(',')
                                .map(str::trim)
                                .filter(|part| !part.is_empty())
                                .map(ToOwned::to_owned)
                                .collect::<Vec<_>>()
                        })
                        .collect(),
                    output,
                    extract_path: extract,
                    summary,
                    minify,
                    max_array,
                    max_depth,
                    force_http1: http1,
                };
                cmd::cmd_spa(&cfg).await?;
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
                let cfg = cmd::StreamCmdConfig {
                    source,
                    id,
                    output,
                    quality,
                    force_native: native,
                    force_ffmpeg: ffmpeg,
                    info_only: info,
                    list_episodes: list,
                    cookies,
                    duration,
                    ffmpeg_opts,
                    player,
                };
                cmd::cmd_stream(&cfg).await?;
            }
            Commands::Analyze {
                video,
                audio_only,
                diarize,
                format,
                output,
                dgx,
                api_key,
                active_reading,
            } => {
                let cfg = cmd::AnalyzeConfig {
                    video,
                    audio_only,
                    diarize,
                    format,
                    output,
                    dgx,
                    api_key,
                    language: None,
                    active_reading,
                };
                cmd::cmd_analyze(&cfg).await?;
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
                cmd::cmd_annotate(&cmd::AnnotateConfig {
                    video,
                    output,
                    subtitles,
                    speaker_labels,
                    analysis,
                    style,
                    hwaccel,
                })
                .await?;
            }
            Commands::Submit {
                url,
                fields,
                csrf_from,
                headers,
                ..
            } => {
                cmd::cmd_submit(&cmd::SubmitConfig {
                    url,
                    field_args: fields,
                    csrf_from,
                    show_headers: headers,
                })
                .await?;
            }
            Commands::Login {
                url,
                use_1password,
                save_session,
                cookies,
                format,
                #[cfg(feature = "browser")]
                browser,
                ..
            } => {
                cmd::cmd_login(&cmd::LoginConfig {
                    url,
                    use_1password,
                    save_session,
                    cookies,
                    format,
                    #[cfg(feature = "browser")]
                    use_browser: browser,
                })
                .await?;
            }
            Commands::Context {
                urls,
                cookies,
                max_tokens,
                ..
            } => {
                cmd::cmd_context(&urls, &cookies, max_tokens).await?;
            }
            Commands::Cookies { action } => match action {
                CookiesAction::Export { domain, cookies } => {
                    cmd::cmd_cookies("export", &domain, &cookies).await?;
                }
            },
            Commands::Rules { action } => match action {
                RulesAction::Export => {
                    cmd::cmd_export_rules()?;
                }
                RulesAction::List => {
                    cmd::cmd_list_rules()?;
                }
            },
            Commands::Provider { action } => match action {
                ProviderAction::List => {
                    cmd::cmd_provider_list()?;
                }
                ProviderAction::Install { src } => {
                    cmd::cmd_provider_install(&src).await?;
                }
                ProviderAction::Remove { name } => {
                    cmd::cmd_provider_remove(&name)?;
                }
            },
            Commands::Watch { action } => match action {
                WatchAction::Add {
                    url,
                    interval,
                    selector,
                    diff_kind,
                } => {
                    cmd::cmd_watch_add(&cmd::WatchAddConfig {
                        url,
                        interval,
                        selector,
                        diff_kind: Some(diff_kind),
                    })
                    .await?;
                }
                WatchAction::List { format } => {
                    let fmt = if format.eq_ignore_ascii_case("json") {
                        cmd::WatchListFormat::Json
                    } else {
                        cmd::WatchListFormat::Table
                    };
                    cmd::cmd_watch_list(&cmd::WatchListConfig { format: fmt }).await?;
                }
                WatchAction::Remove { id } => {
                    cmd::cmd_watch_remove(&id).await?;
                }
                WatchAction::Logs { id } => {
                    cmd::cmd_watch_logs(&cmd::WatchLogsConfig { id }).await?;
                }
            },
            Commands::Models { action } => match action {
                ModelsAction::Fetch { name } => {
                    cmd::cmd_models_fetch(&name).await?;
                }
                ModelsAction::List => {
                    cmd::cmd_models_list().await?;
                }
                ModelsAction::Update { name } => {
                    cmd::cmd_models_update(&name).await?;
                }
                ModelsAction::Verify => {
                    cmd::cmd_models_verify().await?;
                }
            },
        }

        Ok(())
    }
    .await;

    match result {
        Ok(()) => Ok(()),
        Err(err) if is_broken_pipe(&err) => Ok(()),
        Err(err) => Err(err),
    }
}

fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_err| io_err.kind() == std::io::ErrorKind::BrokenPipe)
    })
}
