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
// `Commands` is a parse-once CLI singleton: built from argv, destructured
// immediately in `main`, never stored in a collection or hot path. The size
// disparity the lint guards against (small variants paying for a large sibling
// in a `Vec`/`Box`) does not apply here, and clippy's `Box<Vec<_>>` suggestion
// would add a pointless indirection over an already heap-backed `Vec`.
#[allow(clippy::large_enum_variant)]
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

        /// Force readability extraction for HTML pages
        #[arg(long)]
        readability: bool,

        /// Maximum output token envelope; returned markdown uses 80% for headroom
        #[arg(long)]
        max_output_tokens: Option<usize>,

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

        /// Allow remote thin-content fallback via `r.jina.ai` (may disclose the URL to a third party)
        #[arg(long)]
        remote_fallback: bool,

        /// Deprecated no-op: remote fallback is disabled unless --remote-fallback is set
        #[arg(long, hide = true)]
        no_fallback: bool,

        /// Render through an explicitly configured external CDP browser endpoint.
        ///
        /// Disabled by default; set `NAB_BROWSER_CDP_WS` or pass `--browser-cdp-url`.
        #[arg(long)]
        render: bool,

        /// Alias for --render for JS-heavy pages that need browser interaction or DOM execution.
        #[arg(long)]
        interactive: bool,

        /// CDP WebSocket endpoint for --render/--interactive.
        #[arg(long)]
        browser_cdp_url: Option<String>,

        /// Environment variable containing CDP header overrides as JSON or `Name: value` lines.
        #[arg(long, default_value = "NAB_BROWSER_CDP_HEADERS")]
        browser_headers_env: String,

        /// Extra wait after browser load event before extracting DOM, in milliseconds.
        #[arg(long, default_value = "1000")]
        browser_wait_ms: u64,

        /// Batch fetch URLs from file (one per line, # comments allowed)
        #[arg(long)]
        batch: Option<String>,

        /// Max concurrent requests for batch mode (default: 5)
        #[arg(long, default_value = "5")]
        parallel: usize,

        /// Proxy URL (SOCKS5 or HTTP). Also checks `HTTP_PROXY/HTTPS_PROXY/ALL_PROXY` env vars.
        #[arg(long)]
        proxy: Option<String>,

        /// Route through Tor (requires Tor daemon on localhost:9050).
        ///
        /// DNS resolution is also proxied (`socks5h://`) to prevent leaks.
        /// Falls back to a direct connection with a warning if Tor is unavailable.
        #[arg(long)]
        tor: bool,

        /// Show what changed since the last fetch (stores snapshots in ~/.nab/snapshots/)
        #[arg(long)]
        diff: bool,

        /// Do not save the fetch result to hebb kv store (default: save when hebb is available)
        #[arg(long)]
        no_save: bool,

        /// Do not OCR images in the fetched HTML (default: OCR when Apple Vision is available)
        #[arg(long)]
        no_ocr: bool,

        /// Do not auto-transcribe media URLs (`YouTube`, `SoundCloud`, direct
        /// `.mp3`/`.mp4`, etc.)
        ///
        /// By default, when nab detects a media URL it downloads the audio via
        /// `yt-dlp`, transcribes it via `FluidAudio`/`sherpa-onnx`, and returns
        /// the transcript as markdown.
        /// Pass this flag to disable that behaviour and fetch the page as plain HTML instead.
        #[arg(long)]
        no_transcribe: bool,

        /// BCP-47 language hint for transcription (e.g. "fi", "en-US", "de").
        /// Defaults to auto-detection when omitted.
        #[arg(long)]
        language: Option<String>,

        /// Run the Cloudflare AI Labyrinth (and similar) bot-trap detector
        /// on the fetched HTML body. If the page is classified as a trap,
        /// nab logs a warning and exits with `NabError::LabyrinthDetected`
        /// instead of returning the content. See
        /// <https://blog.cloudflare.com/ai-labyrinth/>.
        #[arg(long)]
        detect_labyrinth: bool,

        /// WAF challenge handling strategy: off, auto, replay, js, browser.
        /// `auto` (default) detects WAF challenges and picks the best solver.
        #[arg(long, default_value = "auto")]
        waf_mode: String,

        /// DANGER — allow fetching private/internal IPs (RFC 1918, IPv6 ULA,
        /// CGN). OFF by default; SSRF protection stays on.
        ///
        /// nab blocks private/internal addresses by default to prevent
        /// Server-Side Request Forgery (SSRF). Enable this only on a trusted
        /// workstation where you legitimately need to reach a corporate
        /// intranet dashboard. Loopback (127.x) and cloud-metadata endpoints
        /// (169.254.169.254) stay blocked even with this flag. Prefer the
        /// scoped `--allow-private-ip <CIDR>` allowlist over this blanket flag.
        /// Can also be enabled via `NAB_SSRF_ALLOW_PRIVATE=1`.
        #[arg(long)]
        allow_private_ips: bool,

        /// Scoped allowlist entry permitting one private CIDR/IP
        /// (repeatable, e.g. `--allow-private-ip 10.252.0.0/16`).
        ///
        /// Preferred over `--allow-private-ips`: only addresses inside a listed
        /// range bypass the private-IP block. Listing a dangerous address
        /// (loopback, cloud metadata) has no effect. Can also be supplied via
        /// `NAB_SSRF_ALLOWLIST=10.252.0.0/16,192.168.1.5`.
        #[arg(long = "allow-private-ip", action = clap::ArgAction::Append)]
        allow_private_ip: Vec<String>,
    },

    /// Render a JavaScript-heavy URL through an explicitly configured CDP browser endpoint
    Browser {
        /// URL to render
        url: String,

        /// Output format: full, compact, json
        #[arg(short, long, default_value = "full")]
        format: OutputFormat,

        /// Save markdown body to file
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Maximum body chars to display (0=unlimited)
        #[arg(long, default_value = "0")]
        max_body: usize,

        /// Maximum output token envelope; returned markdown uses 80% for headroom
        #[arg(long)]
        max_output_tokens: Option<usize>,

        /// CDP WebSocket endpoint. If omitted, `NAB_BROWSER_CDP_WS` is used.
        #[arg(long)]
        cdp_url: Option<String>,

        /// Environment variable containing CDP header overrides as JSON or `Name: value` lines.
        #[arg(long, default_value = "NAB_BROWSER_CDP_HEADERS")]
        headers_env: String,

        /// Extra wait after browser load event before extracting DOM, in milliseconds.
        #[arg(long, default_value = "1000")]
        wait_ms: u64,

        /// Force readability extraction for HTML pages
        #[arg(long)]
        readability: bool,

        /// Disable automatic SPA data extraction (Next.js, Nuxt, Redux, etc.)
        #[arg(long)]
        no_spa: bool,
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

    /// Diagnose the environment (e.g. multiple nab installs shadowing each other on PATH)
    Doctor,

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

    /// Manage locally-built inference model binaries (`FluidAudio`, `Whisper`, …)
    ///
    /// Clones, builds, and symlinks model binaries into a persistent location so
    /// they survive reboots. Install location: `~/.local/share/nab/models/`
    /// Binary symlinks: `~/.local/share/nab/bin/`
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },

    /// Run post-install migrations and print what's new
    ///
    /// Compares the version stamp at `~/.nab/version.stamp` to the running
    /// binary and applies any pending data migrations.  Also hints at installed
    /// models that have newer versions available.
    Upgrade {
        /// Show what would happen without making any changes
        #[arg(long)]
        dry_run: bool,

        /// Suppress informational output
        #[arg(short, long)]
        quiet: bool,
    },

    /// LinkedIn-specific tooling. Currently: `nab linkedin export`
    /// initiates and downloads the official LinkedIn data archive
    /// (Settings → Data Privacy → Get a copy of your data).
    #[allow(clippy::doc_markdown)]
    Linkedin {
        #[command(subcommand)]
        action: LinkedinAction,
    },

    /// MCP server management (serve, install)
    ///
    /// `nab mcp` starts the MCP server on stdio (same as `nab-mcp`).
    /// `nab mcp install` auto-configures your AI client to use nab.
    Mcp {
        #[command(subcommand)]
        action: Option<McpAction>,

        /// Bind address for Streamable HTTP transport (e.g. "127.0.0.1:8765").
        /// Omit to run in stdio mode (default).
        #[arg(long, value_name = "HOST:PORT")]
        http: Option<String>,

        /// Allowed CORS origin for HTTP mode.
        #[arg(long, value_name = "ORIGIN")]
        http_allow_origin: Option<String>,
    },

    /// Complete a web task (experimental API-first web-task engine).
    ///
    /// `nab task "<goal>" <url>` fetches the seed URL through the moat
    /// (browser cookies, fingerprint, HTTP/3), YARA-screens it, and returns the
    /// shaped markdown. Slice 1 implements rung 0 (fetch); build with
    /// `--features task`.
    #[cfg(feature = "task")]
    Task {
        /// Natural-language goal for the task.
        goal: String,
        /// Seed URL to start from.
        url: String,
        /// Output format for the fetched content.
        #[arg(short, long, default_value = "full")]
        format: OutputFormat,
        /// Emit the full `TaskOutcome` as JSON instead of just the content.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
#[allow(clippy::doc_markdown)]
enum LinkedinAction {
    /// Request and download a LinkedIn data archive.
    ///
    /// LinkedIn lets every user request a ZIP of their own data
    /// (Settings → Data Privacy → "Get a copy of your data"). FAST archives
    /// (connections + account history) take ~10 minutes; FULL archives
    /// (posts, articles, messages, reactions) take up to 24 hours.
    ///
    ///   nab linkedin export --kind fast --wait
    ///   nab linkedin export --kind full           # request only, exit
    ///   nab linkedin export --poll-only --wait    # resume polling later
    Export {
        /// Browser to read cookies from (auto, brave, chrome, firefox, safari, edge).
        #[arg(short, long, default_value = "auto")]
        cookies: String,

        /// Archive kind: `fast` (~10 min, contacts/history) or `full` (~24 h, all content).
        #[arg(long, default_value = "full", value_parser = ["fast", "full"])]
        kind: String,

        /// Where to write the ZIP. Defaults to `~/Downloads/linkedin-export-<ts>.zip`.
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Block until the archive is ready and download it. Otherwise, just
        /// fire the request and exit.
        #[arg(long)]
        wait: bool,

        /// Skip the request POST. Useful when the request was fired earlier
        /// (web UI or prior `--no-wait` invocation) and you only want to poll.
        #[arg(long)]
        poll_only: bool,

        /// Initial poll interval in seconds (exponential backoff up to --poll-max).
        #[arg(long, default_value = "60")]
        poll_base: u64,

        /// Cap on the polling interval, in seconds.
        #[arg(long, default_value = "600")]
        poll_max: u64,

        /// Total wallclock cap in seconds. Default 26 h.
        #[arg(long, default_value = "93600")]
        max_wait: u64,

        /// Override the status-page URL (escape hatch when LinkedIn rotates the path).
        #[arg(long)]
        form_url: Option<String>,

        /// Override the JSON request URL (escape hatch when LinkedIn rotates
        /// the internal mysettings-api path).
        #[arg(long)]
        request_url: Option<String>,

        /// Override the JSON body (escape hatch when the field name rotates).
        /// Capture from Chrome DevTools → Network → click the "Get a copy of
        /// your data" button → Request Payload.
        #[arg(long)]
        body_override: Option<String>,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// Start the MCP server on stdio (default) or Streamable HTTP
    Serve {
        /// Bind address for Streamable HTTP transport (e.g. "127.0.0.1:8765").
        #[arg(long, value_name = "HOST:PORT")]
        http: Option<String>,

        /// Allowed CORS origin for HTTP mode.
        #[arg(long, value_name = "ORIGIN")]
        http_allow_origin: Option<String>,
    },

    /// Install nab as an MCP server in your AI client's config
    ///
    /// No JSON editing, no file-path hunting. Run this command and restart
    /// your client.
    ///
    ///   nab mcp install                        # Claude Desktop (default)
    ///   nab mcp install --client cursor         # Cursor
    ///   nab mcp install --client claude-code    # Claude Code
    ///   nab mcp install --client windsurf       # Windsurf
    ///   nab mcp install --dry-run               # show what would change
    Install {
        /// MCP client to configure: claude-desktop, cursor, claude-code, windsurf
        #[arg(long, default_value = "claude-desktop")]
        client: String,

        /// Overwrite existing nab entry without asking
        #[arg(long)]
        force: bool,

        /// Print the planned change without writing the file
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum CookiesAction {
    /// Export cookies for a domain (Netscape by default; Playwright `storage_state` with --format)
    Export {
        /// Domain to export cookies for (e.g., "github.com"). Bare domains also include www. variants.
        domain: String,

        /// Browser to export from (auto, brave, chrome, firefox, safari, edge)
        #[arg(short, long, default_value = "auto")]
        cookies: String,

        /// Output format: "netscape" (default) or "playwright" (CDP `storage_state` JSON)
        #[arg(long, default_value = "netscape")]
        format: String,

        /// Write to this path instead of stdout
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
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

    // Run silent migration check on every invocation except `nab upgrade`
    // (which manages the stamp itself via cmd_upgrade).
    if !matches!(&cli.command, Commands::Upgrade { .. })
        && let Err(e) = cmd::check_upgrade()
    {
        // Non-fatal: a broken stamp file must never prevent normal use.
        tracing::debug!("upgrade check failed (non-fatal): {e:#}");
    }

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
                readability,
                max_output_tokens,
                add_headers,
                auto_referer,
                warmup_url,
                method,
                data,
                capture_cookies,
                no_redirect,
                no_spa,
                remote_fallback,
                no_fallback,
                render,
                interactive,
                browser_cdp_url,
                browser_headers_env,
                browser_wait_ms,
                batch,
                parallel,
                proxy,
                tor,
                diff,
                no_save,
                no_ocr,
                no_transcribe,
                language,
                detect_labyrinth,
                waf_mode,
                allow_private_ips,
                allow_private_ip,
                ..
            } => {
                let ssrf_policy = nab::SsrfPolicy::from_env()
                    .with_allow_private(allow_private_ips)
                    .with_allowlist_entries(allow_private_ip.iter());
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
                    max_output_tokens,
                    custom_headers: add_headers,
                    auto_referer,
                    warmup_url,
                    method,
                    data,
                    capture_cookies,
                    no_redirect,
                    render,
                    interactive,
                    browser_cdp_url,
                    browser_headers_env,
                    browser_wait_ms,
                    batch_file: batch,
                    parallel,
                    proxy,
                    tor,
                    show_diff: diff,
                    no_save,
                    no_ocr,
                    no_transcribe,
                    language,
                    detect_labyrinth,
                    waf_mode: waf_mode.parse().unwrap_or_default(),
                    ssrf_policy,
                    html_options: nab::content::html::HtmlConversionOptions {
                        allow_spa_extraction: !no_spa,
                        allow_jina_fallback: remote_fallback && !no_fallback,
                        force_readability: readability,
                        max_output_tokens,
                    },
                };
                cmd::cmd_fetch(&cfg).await?;
            }
            Commands::Browser {
                url,
                format,
                output,
                max_body,
                max_output_tokens,
                cdp_url,
                headers_env,
                wait_ms,
                readability,
                no_spa,
            } => {
                let cfg = cmd::BrowserConfig {
                    url,
                    format,
                    output_file: output,
                    max_body,
                    max_output_tokens,
                    cdp_url,
                    headers_env,
                    wait_ms,
                    html_options: nab::content::html::HtmlConversionOptions {
                        allow_spa_extraction: !no_spa,
                        allow_jina_fallback: false,
                        force_readability: readability,
                        max_output_tokens,
                    },
                };
                cmd::cmd_browser(&cfg).await?;
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
            Commands::Doctor => {
                cmd::cmd_doctor()?;
            }
            Commands::Otp { domain } => {
                cmd::cmd_otp(&domain)?;
            }
            #[cfg(feature = "task")]
            Commands::Task {
                goal,
                url,
                format,
                json,
            } => {
                cmd::task::cmd_task(&goal, &url, format, json).await?;
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
                CookiesAction::Export {
                    domain,
                    cookies,
                    format,
                    output,
                } => {
                    cmd::cmd_cookies_export(&domain, &cookies, &format, output.as_deref()).await?;
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
            Commands::Upgrade { dry_run, quiet } => {
                cmd::cmd_upgrade(&cmd::UpgradeConfig { dry_run, quiet })?;
            }
            Commands::Linkedin { action } => match action {
                LinkedinAction::Export {
                    cookies,
                    kind,
                    output,
                    wait,
                    poll_only,
                    poll_base,
                    poll_max,
                    max_wait,
                    form_url,
                    request_url,
                    body_override,
                } => {
                    let kind_arg = match kind.as_str() {
                        "fast" => cmd::ArchiveKindArg::Fast,
                        _ => cmd::ArchiveKindArg::Full,
                    };
                    cmd::cmd_linkedin_export(cmd::LinkedinExportConfig {
                        cookies,
                        kind: kind_arg,
                        output,
                        wait,
                        poll_only,
                        poll_base_secs: poll_base,
                        poll_max_secs: poll_max,
                        max_wait_secs: max_wait,
                        form_url,
                        request_url,
                        body_override,
                    })
                    .await?;
                }
            },
            Commands::Mcp {
                action,
                http,
                http_allow_origin,
            } => match action {
                Some(McpAction::Install {
                    client,
                    force,
                    dry_run,
                }) => {
                    cmd::cmd_mcp_install(&cmd::McpInstallConfig {
                        client,
                        force,
                        dry_run,
                    })?;
                }
                Some(McpAction::Serve {
                    http: sub_http,
                    http_allow_origin: sub_origin,
                }) => {
                    cmd::cmd_mcp_serve(&cmd::McpServeConfig {
                        http: sub_http,
                        http_allow_origin: sub_origin,
                    })?;
                }
                // `nab mcp` with no subcommand → serve (default behavior)
                None => {
                    cmd::cmd_mcp_serve(&cmd::McpServeConfig {
                        http,
                        http_allow_origin,
                    })?;
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
