use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use anyhow::Result;
use reqwest::header::{CONTENT_TYPE, COOKIE, HeaderMap, HeaderName, HeaderValue, REFERER};
use serde_json::json;

use nab::content::diff::ContentSnapshot;
use nab::content::diff_format::format_diff_terminal;
use nab::content::ocr::fetch_integration::FetchOcrEnricher;
use nab::content::response_classifier::{
    ResponseAnalysis, ResponseClass, ThinContentDiagnostic, classify_response,
    classify_thin_content,
};
use nab::content::snapshot_store::SnapshotStore;
use nab::{AcceleratedClient, OnePasswordAuth, SafeFetchConfig, SafeRequestOptions};

use super::output::{output_body, write_stdout, write_stdout_line};
use crate::OutputFormat;

/// All parameters for a `nab fetch` invocation.
///
/// Constructed from CLI arguments in `main.rs` and threaded through the
/// fetch pipeline, replacing the 22-positional-parameter function signature.
#[allow(clippy::struct_excessive_bools)] // 1:1 map of CLI boolean flags
pub struct FetchConfig {
    pub url: String,
    pub show_headers: bool,
    pub show_body: bool,
    pub format: OutputFormat,
    pub output_file: Option<PathBuf>,
    pub cookies: String,
    pub use_1password: bool,
    pub raw_html: bool,
    pub links: bool,
    pub max_body: usize,
    pub custom_headers: Vec<String>,
    pub auto_referer: bool,
    pub warmup_url: Option<String>,
    pub method: String,
    pub data: Option<String>,
    pub capture_cookies: bool,
    pub no_redirect: bool,
    pub batch_file: Option<String>,
    pub parallel: usize,
    pub proxy: Option<String>,
    /// When `true`, route all requests through the Tor SOCKS5 proxy at
    /// `localhost:9050`.  DNS resolution is also proxied (`socks5h://`) to
    /// prevent DNS leaks.  If Tor is not running the request falls back to a
    /// direct connection with a warning printed to stderr.
    pub tor: bool,
    pub show_diff: bool,
    pub html_options: nab::content::html::HtmlConversionOptions,
    /// When `true`, skip saving the fetch result to hebb's kv store.
    pub no_save: bool,
    /// When `true`, skip OCR-enriching images in the fetched HTML.
    pub no_ocr: bool,
    /// When `true`, do not auto-transcribe media URLs; fall back to normal HTML fetch.
    pub no_transcribe: bool,
    /// Optional BCP-47 language hint for transcription (e.g. `"fi"`, `"en-US"`).
    pub language: Option<String>,
    /// When `true`, run the Cloudflare AI Labyrinth detector on the
    /// fetched HTML body and refuse to return content classified as a
    /// trap. See [`nab::detect::labyrinth`].
    pub detect_labyrinth: bool,
    /// WAF challenge handling strategy. See [`WafMode`].
    pub waf_mode: WafMode,
}

/// Strategy for handling detected WAF challenges (AWS WAF, Cloudflare
/// Turnstile, `DataDome`, …).
///
/// * `Off`  — never attempt to solve; surface the challenge as-is.
/// * `Auto` — detect and pick the cheapest tier that works
///   (replay → js → browser). Default.
/// * `Replay` — replay-mode only; fail fast if replay cannot solve it.
/// * `Js`   — force the JS interpreter path (requires `js-dom-full`).
/// * `Browser` — open the default browser and wait for user solve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WafMode {
    Off,
    #[default]
    Auto,
    Replay,
    Js,
    Browser,
}

impl WafMode {
    /// Parse the CLI flag value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "off" | "none" | "disabled" => Some(Self::Off),
            "auto" | "" => Some(Self::Auto),
            "replay" => Some(Self::Replay),
            "js" | "javascript" => Some(Self::Js),
            "browser" | "open" => Some(Self::Browser),
            _ => None,
        }
    }

    /// Return the canonical CLI string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Replay => "replay",
            Self::Js => "js",
            Self::Browser => "browser",
        }
    }
}

impl std::str::FromStr for WafMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| format!("unknown waf-mode: {s}"))
    }
}

#[allow(clippy::too_many_lines)] // Orchestration function; splitting would hurt readability
pub async fn cmd_fetch(cfg: &FetchConfig) -> Result<()> {
    // Handle batch mode
    if cfg.batch_file.is_some() {
        return super::fetch_batch::cmd_fetch_batch(cfg).await;
    }

    let client = build_client(cfg.no_redirect, cfg.proxy.as_deref(), cfg.tor)?;
    let profile = client.profile().await;

    let domain = super::extract_domain(&cfg.url);

    let cookie_header = super::resolve_cookie_header(&cfg.cookies, &domain);
    if !cookie_header.is_empty() && matches!(cfg.format, OutputFormat::Full) {
        write_stdout_line(&format!("🍪 Loading cookies for {domain}"))?;
    }

    // ── Media URL auto-transcription ──────────────────────────────────────
    if !cfg.no_transcribe && !cfg.raw_html && nab::content::media::is_media_url(&cfg.url) {
        tracing::info!(url = %cfg.url, "detected media URL — transcribing");
        match nab::content::media::fetch_media_as_markdown(&cfg.url, cfg.language.as_deref(), false)
            .await
        {
            Ok(result) => {
                let markdown = nab::security::guard_fetch_output(
                    &result.markdown,
                    "cli_fetch_media",
                    &cfg.url,
                )?;
                if !cfg.no_save {
                    save_to_hebb(&cfg.url, &markdown, "").await;
                }
                output_body(
                    &markdown,
                    cfg.output_file.as_deref(),
                    cfg.links,
                    cfg.max_body,
                )?;
                return Ok(());
            }
            Err(e) => {
                tracing::warn!("media transcription failed ({e:#}), falling back to normal fetch");
            }
        }
    }

    // Site providers produce structured markdown, which is incompatible with
    // --raw-html (the user is asking for the wire HTML). Skip the site router
    // entirely when raw HTML is requested so that the generic fetch path runs
    // and emits the unmodified HTML body. Cookies + impersonation still apply
    // via the lower-level fetch_safe path.
    if !cfg.raw_html {
        let site_router = nab::site::SiteRouter::new();
        let cookie_opt = non_empty(&cookie_header);
        if let Some(site_content) = site_router.try_extract(&cfg.url, &client, cookie_opt).await {
            let markdown = nab::security::guard_fetch_output(
                &site_content.markdown,
                "cli_fetch_site_provider",
                &cfg.url,
            )?;
            output_body(
                &markdown,
                cfg.output_file.as_deref(),
                cfg.links,
                cfg.max_body,
            )?;
            return Ok(());
        }
    }

    let markdown = !cfg.raw_html;

    if cfg.use_1password && OnePasswordAuth::is_available() {
        let auth = OnePasswordAuth::new(None);
        if let Ok(Some(cred)) = auth.get_credential_for_url(&cfg.url)
            && matches!(cfg.format, OutputFormat::Full)
        {
            write_stdout_line(&format!("🔐 Found 1Password: {}", cred.title))?;
        }
    }

    if let Some(warmup) = &cfg.warmup_url {
        if matches!(cfg.format, OutputFormat::Full) {
            write_stdout_line(&format!("🔥 Warming up session: {warmup}"))?;
        }
        let headers = build_safe_request_headers(cfg, &profile, &cookie_header, warmup, false)?;
        let _ = client
            .request_safe(
                warmup,
                SafeRequestOptions {
                    headers,
                    config: SafeFetchConfig::default(),
                    ..SafeRequestOptions::default()
                },
            )
            .await;
    }

    let start = Instant::now();

    let is_simple_get = cfg.method.eq_ignore_ascii_case("GET")
        && cookie_header.is_empty()
        && cfg.custom_headers.is_empty()
        && cfg.data.is_none()
        && !cfg.auto_referer
        && !cfg.no_redirect;

    let (status, version, set_cookies, content_type, response_headers, body_bytes) =
        if is_simple_get {
            execute_safe_get(&client, &cfg.url, cfg.show_headers).await?
        } else {
            execute_manual_request(&client, cfg, &profile, &cookie_header).await?
        };

    let elapsed = start.elapsed();
    let raw_text = String::from_utf8_lossy(&body_bytes).to_string();

    // ── WAF challenge detection + tiered solver ───────────────────────────
    // The CLI flag defaults to Auto: when a challenge is detected we
    // attempt replay-mode first (cheap, native), then optionally the JS
    // interpreter (local), and finally the default-browser escape hatch.
    // This runs before any content conversion so no WAF interstitial
    // leaks into the caller's output.
    if !matches!(cfg.waf_mode, WafMode::Off) && content_type.contains("html") {
        let header_slice: Vec<(&str, &str)> = response_headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        if let Some(kind) = nab::waf::detect_challenge(&raw_text, header_slice.iter().copied()) {
            let vendor = match &kind {
                nab::waf::ChallengeKind::AwsWaf(_) => "AWS WAF",
                nab::waf::ChallengeKind::Cloudflare => "Cloudflare",
                nab::waf::ChallengeKind::DataDome => "DataDome",
            };
            eprintln!(
                "⚠️  WAF challenge detected ({vendor}), mode={}…",
                cfg.waf_mode.as_str()
            );
            // Replay tier.
            if matches!(cfg.waf_mode, WafMode::Auto | WafMode::Replay) {
                match nab::waf::solve_replay(&kind) {
                    Ok(solved) => {
                        eprintln!(
                            "   replay solver returned algo={} iterations={}",
                            solved.algo, solved.iterations
                        );
                    }
                    Err(e) => {
                        eprintln!("   replay solver failed: {e}");
                        if matches!(cfg.waf_mode, WafMode::Replay) {
                            return Err(anyhow::anyhow!("waf replay failed: {e}"));
                        }
                    }
                }
            }
            // JS tier (placeholder — full Promise-driven executor lives in
            // the js-engine module; the CLI defers running it until the
            // replay tier cannot proceed).
            if matches!(cfg.waf_mode, WafMode::Js) {
                eprintln!("   js solver not yet wired in CLI — falling through");
            }
            // Browser escape hatch.
            if matches!(cfg.waf_mode, WafMode::Browser) {
                eprintln!("   opening default browser for manual solve (60s)…");
                #[cfg(any(feature = "browser", feature = "browser-launcher"))]
                {
                    let _ = nab::browser::open_and_wait(
                        &cfg.url,
                        std::time::Duration::from_mins(1),
                        None,
                    );
                }
                #[cfg(not(any(feature = "browser", feature = "browser-launcher")))]
                {
                    eprintln!("   (build lacks browser-launcher feature — cannot open browser)");
                }
            }
        }
    }

    // ── Cloudflare AI Labyrinth (and similar) bot-trap detection ──────────
    // Run *before* any conversion / save / OCR step so we never persist or
    // emit content that came from a trap. The detector is opt-in via
    // `--detect-labyrinth` because it adds a few ms per fetch.
    if cfg.detect_labyrinth
        && content_type.contains("html")
        && let Ok(parsed_url) = url::Url::parse(&cfg.url)
    {
        let score = nab::detect::detect_labyrinth(&raw_text, &parsed_url);
        tracing::debug!(
            url = %cfg.url,
            total = score.total,
            verdict = ?score.verdict,
            "labyrinth scan complete"
        );
        if score.is_trap() {
            for sig in &score.signals {
                tracing::warn!(
                    signal = sig.name,
                    score = sig.score,
                    detail = %sig.detail,
                    "labyrinth signal"
                );
            }
            tracing::warn!(
                url = %cfg.url,
                total = score.total,
                "AI Labyrinth detected — refusing to return content"
            );
            return Err(nab::NabError::LabyrinthDetected {
                score: score.total,
                verdict: format!("{:?}", score.verdict),
            }
            .into());
        }
        if score.is_suspicious() {
            tracing::warn!(
                url = %cfg.url,
                total = score.total,
                "page looks suspicious (labyrinth score in warning band)"
            );
        }
    }

    if cfg.capture_cookies && !set_cookies.is_empty() {
        write_stdout_line("🍪 Set-Cookie:")?;
        for cookie in &set_cookies {
            if let Some(name_value) = cookie.split(';').next() {
                write_stdout_line(&format!("   {name_value}"))?;
            }
        }
    }

    let body_len = body_bytes.len();

    let (body_text, quality) = if markdown && !cfg.links {
        let converted = convert_body_to_markdown(
            &body_bytes,
            &content_type,
            &cfg.url,
            cfg.format,
            cfg.html_options,
        )
        .await?;

        // Attempt Next.js content chunk recovery when extraction yields thin content.
        // The readability extractor may capture 300-600 chars of nav/header/footer
        // even when the article body is empty, so we use a higher threshold (800)
        // combined with a low quality confidence score (<0.5) to detect this case.
        let quality_is_low = converted
            .quality
            .as_ref()
            .is_some_and(|q| q.confidence < 0.5);
        let is_nextjs_meta = content_type.contains("html")
            && body_len > 5_000
            && nab::content::spa_extract::is_nextjs_metadata_only(&raw_text);
        let (final_markdown, final_quality) = if is_nextjs_meta
            && (converted.markdown.len() < 800 || quality_is_low)
        {
            match recover_nextjs_content_chunks(&client, &raw_text, &cfg.url, cfg.format).await {
                Some(recovered) => (recovered, converted.quality),
                None => (converted.markdown, converted.quality),
            }
        } else {
            (converted.markdown, converted.quality)
        };

        (final_markdown, final_quality)
    } else {
        (raw_text.clone(), None)
    };

    // ── Apple Vision OCR enrichment ────────────────────────────────────────
    let body_text = if !cfg.no_ocr && !cfg.raw_html && content_type.contains("html") {
        enrich_with_ocr(&body_text, &raw_text, &cfg.url, &client).await
    } else {
        body_text
    };
    let body_text = nab::security::guard_fetch_output(&body_text, "cli_fetch", &cfg.url)?;

    // ── Auto-save to hebb kv:urls ──────────────────────────────────────────
    if !cfg.no_save {
        save_to_hebb(&cfg.url, &body_text, &raw_text).await;
    }

    for warning in build_fetch_diagnostics(
        status.as_u16(),
        &raw_text,
        Some(&content_type),
        body_len,
        &body_text,
        quality.as_ref(),
        cfg.html_options.allow_jina_fallback,
    ) {
        eprintln!("⚠️  {warning}");
    }

    if cfg.show_diff {
        emit_diff(&cfg.url, &body_text, cfg.format)?;
    }

    print_output(
        cfg,
        &FetchResponse {
            profile: &profile,
            cookie_header: &cookie_header,
            status,
            version: &version,
            elapsed,
            response_headers: &response_headers,
            body_len,
            body_text: &body_text,
            raw_text: &raw_text,
            content_type: &content_type,
            quality: quality.as_ref(),
        },
    )?;

    Ok(())
}

/// Execute a safe GET request via `fetch_safe`.
async fn execute_safe_get(
    client: &AcceleratedClient,
    url: &str,
    show_headers: bool,
) -> Result<(
    reqwest::StatusCode,
    String,
    Vec<String>,
    String,
    Vec<(String, String)>,
    bytes::Bytes,
)> {
    let config = SafeFetchConfig::default();
    let safe_resp = client.fetch_safe(url, &config).await?;

    let set_cookies: Vec<String> = safe_resp
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.clone())
        .collect();

    let resp_headers: Vec<(String, String)> = if show_headers {
        safe_resp.headers.clone()
    } else {
        Vec::new()
    };

    Ok((
        safe_resp.status,
        format!("{:?}", safe_resp.version),
        set_cookies,
        safe_resp.content_type.clone(),
        resp_headers,
        safe_resp.body,
    ))
}

fn build_safe_request_headers(
    cfg: &FetchConfig,
    profile: &nab::fingerprint::BrowserProfile,
    cookie_header: &str,
    url: &str,
    include_default_content_type: bool,
) -> Result<HeaderMap> {
    let mut headers = profile.to_headers();

    if !cookie_header.is_empty() {
        headers.insert(COOKIE, HeaderValue::from_str(cookie_header)?);
    }

    if cfg.auto_referer
        && let Some(referer) = super::build_referer(url)
    {
        headers.insert(REFERER, HeaderValue::from_str(&referer)?);
    }

    if include_default_content_type
        && cfg.data.is_some()
        && !cfg
            .custom_headers
            .iter()
            .any(|h| h.to_lowercase().starts_with("content-type"))
    {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }

    for header_str in &cfg.custom_headers {
        let Some((name, value)) = header_str.split_once(':') else {
            continue;
        };
        let name = HeaderName::from_bytes(name.trim().as_bytes())?;
        let value = HeaderValue::from_str(value.trim())?;
        headers.insert(name, value);
    }

    Ok(headers)
}

/// Execute a manually-built request (non-GET, cookies, custom headers, etc.).
async fn execute_manual_request(
    client: &AcceleratedClient,
    cfg: &FetchConfig,
    profile: &nab::fingerprint::BrowserProfile,
    cookie_header: &str,
) -> Result<(
    reqwest::StatusCode,
    String,
    Vec<String>,
    String,
    Vec<(String, String)>,
    bytes::Bytes,
)> {
    let url = &cfg.url;
    let method = cfg.method.parse::<reqwest::Method>()?;
    let headers = build_safe_request_headers(cfg, profile, cookie_header, url, true)?;
    let config = if cfg.no_redirect {
        SafeFetchConfig {
            max_redirects: 0,
            ..SafeFetchConfig::default()
        }
    } else {
        SafeFetchConfig::default()
    };
    let safe_resp = client
        .request_safe(
            url,
            SafeRequestOptions {
                method,
                headers,
                body: cfg.data.clone().map(bytes::Bytes::from),
                config,
            },
        )
        .await?;

    let set_cookies: Vec<String> = safe_resp
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.clone())
        .collect();

    let resp_headers: Vec<(String, String)> = if cfg.show_headers {
        safe_resp.headers.clone()
    } else {
        Vec::new()
    };

    Ok((
        safe_resp.status,
        format!("{:?}", safe_resp.version),
        set_cookies,
        safe_resp.content_type,
        resp_headers,
        safe_resp.body,
    ))
}

/// Attempt to recover article content from Next.js webpack content chunks.
///
/// When a Next.js page has `__NEXT_DATA__` with only metadata (no article body),
/// the content is often compiled into a lazy-loaded webpack chunk (common for MDX
/// blogs).  This function:
///
/// 1. Discovers the webpack runtime and page component script URLs from the HTML
/// 2. Fetches them to find the content chunk filename
/// 3. Fetches the content chunk
/// 4. Extracts readable text from the compiled JSX
///
/// Returns `Some(markdown)` on success, `None` if recovery fails at any step.
async fn recover_nextjs_content_chunks(
    client: &nab::AcceleratedClient,
    html: &str,
    page_url: &str,
    format: crate::OutputFormat,
) -> Option<String> {
    if matches!(format, crate::OutputFormat::Full) {
        eprintln!("   Attempting Next.js content chunk recovery...");
    }
    let recovered = nab::util::recover_nextjs_chunks(client, html, page_url).await;
    if matches!(format, crate::OutputFormat::Full)
        && let Some(content) = recovered.as_ref()
    {
        eprintln!("   Recovered {} chars from content chunk", content.len());
    }
    recovered
}

/// Conversion result bundled with quality metadata for the output layer.
struct ConvertedBody {
    markdown: String,
    quality: Option<nab::content::quality::QualityScore>,
}

/// Convert body bytes to markdown via `ContentRouter`.
async fn convert_body_to_markdown(
    body_bytes: &bytes::Bytes,
    content_type: &str,
    url: &str,
    format: OutputFormat,
    html_options: nab::content::html::HtmlConversionOptions,
) -> Result<ConvertedBody> {
    let router = nab::content::ContentRouter::with_html_options(html_options);
    let bytes = body_bytes.to_vec();
    let ct = content_type.to_string();
    let fetch_url = url.to_string();

    let result = tokio::time::timeout(
        std::time::Duration::from_mins(1),
        tokio::task::spawn_blocking(move || router.convert_with_url(&bytes, &ct, Some(&fetch_url))),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Content conversion timed out after 60s"))???;

    if matches!(format, OutputFormat::Full)
        && let Some(pages) = result.page_count
    {
        write_stdout_line(&format!("   Pages: {pages}"))?;
        write_stdout_line(&format!("   Conversion: {:.1}ms", result.elapsed_ms))?;
    }

    Ok(ConvertedBody {
        quality: result.quality,
        markdown: result.markdown,
    })
}

/// Response data collected after the HTTP request completes.
struct FetchResponse<'a> {
    profile: &'a nab::fingerprint::BrowserProfile,
    cookie_header: &'a str,
    status: reqwest::StatusCode,
    version: &'a str,
    elapsed: std::time::Duration,
    response_headers: &'a [(String, String)],
    body_len: usize,
    body_text: &'a str,
    raw_text: &'a str,
    content_type: &'a str,
    /// Extraction quality score — present for HTML, absent for raw/binary content.
    quality: Option<&'a nab::content::quality::QualityScore>,
}

/// Print the response according to the requested output format.
fn print_output(cfg: &FetchConfig, resp: &FetchResponse<'_>) -> Result<()> {
    let markdown = !cfg.raw_html;
    let out_path = cfg.output_file.as_deref();

    match cfg.format {
        OutputFormat::Compact => {
            write_stdout_line(&format!(
                "{} {}B {:.0}ms",
                resp.status.as_u16(),
                resp.body_len,
                resp.elapsed.as_secs_f64() * 1000.0
            ))?;
            if cfg.show_body || out_path.is_some() || markdown || cfg.links {
                output_body(resp.body_text, out_path, cfg.links, cfg.max_body)?;
            }
        }
        OutputFormat::Json => {
            let metadata = serde_json::json!({
                "title": extract_title(resp.raw_text),
                "content_length": resp.body_len,
                "content_type": resp.content_type,
            });
            let mut output = serde_json::json!({
                "url": cfg.url,
                "status": resp.status.as_u16(),
                "content_type": resp.content_type,
                "markdown": resp.body_text,
                "metadata": metadata,
                "elapsed_ms": (resp.elapsed.as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
            });
            if let Some(q) = resp.quality {
                output["confidence"] = serde_json::json!(q.confidence);
                output["quality"] = serde_json::to_value(q)?;
            }
            write_stdout_line(&serde_json::to_string(&output)?)?;
            if let Some(path) = out_path {
                let mut file = File::create(path)?;
                file.write_all(resp.body_text.as_bytes())?;
            }
        }
        OutputFormat::Full => {
            write_stdout_line(&format!("🌐 Fetching: {}", cfg.url))?;
            write_stdout_line(&format!("🎭 User-Agent: {}", resp.profile.user_agent))?;
            if !resp.cookie_header.is_empty() {
                write_stdout_line(&format!(
                    "🍪 Loaded {} cookies from {}",
                    resp.cookie_header.matches('=').count(),
                    if cfg.cookies == "auto" {
                        "browser (auto-detected)"
                    } else {
                        &cfg.cookies
                    }
                ))?;
            }
            write_stdout_line("\n📊 Response:")?;
            write_stdout_line(&format!("   Status: {}", resp.status))?;
            write_stdout_line(&format!("   Version: {}", resp.version))?;
            write_stdout_line(&format!(
                "   Time: {:.2}ms",
                resp.elapsed.as_secs_f64() * 1000.0
            ))?;
            if cfg.show_headers {
                write_stdout_line("\n📋 Headers:")?;
                for (name, value) in resp.response_headers {
                    write_stdout_line(&format!("   {name}: {value}"))?;
                }
            }
            write_stdout_line(&format!("\n📄 Body: {} bytes", resp.body_len))?;
            if cfg.show_body || out_path.is_some() || markdown || cfg.links {
                output_body(resp.body_text, out_path, cfg.links, cfg.max_body)?;
            }
        }
    }
    Ok(())
}

/// Load the previous snapshot, compute diff, print it, then save new snapshot.
fn emit_diff(url: &str, current_text: &str, format: OutputFormat) -> Result<()> {
    let store = SnapshotStore::default();
    let new_snap = ContentSnapshot::new(url, current_text, SystemTime::now());

    if let Some(old_snap) = store.load_latest_snapshot(url) {
        let diff = nab::content::diff::compute_diff(&old_snap, &new_snap);
        let output = format_diff_terminal(&diff);
        match format {
            OutputFormat::Full | OutputFormat::Compact => write_stdout(&output)?,
            OutputFormat::Json => eprint!("{output}"),
        }
    } else if matches!(format, OutputFormat::Full) {
        write_stdout_line("(no previous snapshot — storing baseline for future --diff runs)")?;
    }

    let _ = store.save_snapshot(url, &new_snap);
    Ok(())
}

/// Extract `<title>` from HTML.
fn extract_title(html: &str) -> Option<String> {
    let doc = scraper::Html::parse_document(html);
    let sel = scraper::Selector::parse("title").ok()?;
    doc.select(&sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
}

fn build_fetch_diagnostics(
    status: u16,
    raw_text: &str,
    content_type: Option<&str>,
    html_len: usize,
    body_text: &str,
    quality: Option<&nab::content::quality::QualityScore>,
    allow_jina_fallback: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let markdown_len = body_text.len();

    let classification = classify_response(ResponseAnalysis {
        status,
        body: raw_text,
        content_type,
        html_bytes: content_type
            .is_some_and(|value| value.contains("html"))
            .then_some(html_len),
        markdown: content_type
            .is_some_and(|value| value.contains("html"))
            .then_some(body_text),
        markdown_chars: content_type
            .is_some_and(|value| value.contains("html"))
            .then_some(markdown_len),
        quality,
    });

    if let Some(primary) = classification.primary() {
        let warning = match primary.class {
            ResponseClass::BotChallenge => format!(
                "Bot or browser challenge detected (HTTP {status}). Browser challenge likely requires cookies or JavaScript.\nTry:\n1. Visit the URL in a browser first\n2. Let nab reuse your default browser cookies automatically unless you intentionally disabled them\n3. Use --cookies brave|chrome|firefox|safari only to override the default browser profile"
            ),
            ResponseClass::RateLimited => format!(
                "Rate limiting detected (HTTP {status}).\nRetry later, or use an authenticated browser/session path if the site rate-limits anonymous traffic."
            ),
            ResponseClass::Unauthorized => format!(
                "Authenticated access appears to be required (HTTP {status}).\nSign in in your browser first, then retry with the default browser cookies or a named session. If you explicitly disabled cookies, re-enable them."
            ),
            ResponseClass::LoginRequired => "The response looks like a login page.\nSign in in your browser first, then retry. nab already uses your default browser cookies automatically unless you disabled them."
                .to_string(),
            ResponseClass::Forbidden => {
                if status == 999 {
                    "Nonstandard block status HTTP 999 detected.\nSome sites use this as an anti-automation or access-control response. Retry with the default browser cookies, or override the browser profile with --cookies brave if the authenticated session lives outside your default browser."
                        .to_string()
                } else {
                    format!(
                        "Forbidden response (HTTP {status}).\nAccess may require authentication, an allowed browser session, or different permissions."
                    )
                }
            }
            ResponseClass::ObfuscatedContent => {
                obfuscated_content_message(status, markdown_len, html_len)
            }
            ResponseClass::ThinContent => String::new(),
        };
        if !warning.is_empty() {
            warnings.push(warning);
        }
    }

    if let Some(thin) = classify_thin_content(content_type, html_len, markdown_len, quality) {
        warnings.push(thin_content_message(thin));
        if !allow_jina_fallback {
            warnings.push(
                "Remote reader fallback is disabled. Pass --remote-fallback to opt in.".to_string(),
            );
        }
    }

    warnings
}

fn obfuscated_content_message(status: u16, markdown_len: usize, html_len: usize) -> String {
    format!(
        "The extracted page content looks encoded or obfuscated rather than readable text (HTTP {status}).\nThis often happens on protected or paywalled pages that return an opaque payload instead of article content.\nObserved output: {markdown_len} chars extracted from {html_len} bytes of HTML.\nTry:\n1. Sign in in your browser first and retry with the default browser cookies\n2. Use a named login session if the site requires an authenticated flow\n3. If the page still returns an opaque blob, the site is likely withholding readable content from non-browser automation"
    )
}

fn thin_content_message(diagnostic: ThinContentDiagnostic) -> String {
    if let Some(message) =
        nab::content::html::detect_thin_content(diagnostic.html_bytes, diagnostic.markdown_chars)
    {
        return message;
    }

    format!(
        "Output is suspiciously thin ({} chars from {} bytes of HTML). \
         Extraction confidence is low, so the main content may be missing.\n  \
         1. nab spa <url>              (extract embedded SPA data)\n  \
         2. nab fetch <url>            (uses default browser cookies automatically)\n  \
         3. nab fetch --cookies brave <url>  (override the browser profile if needed)",
        diagnostic.markdown_chars, diagnostic.html_bytes
    )
}

// ─── OCR enrichment ───────────────────────────────────────────────────────────

/// Run OCR on images in `html` and annotate `markdown` with recognized text.
///
/// Returns the original markdown unchanged when the OCR engine is unavailable
/// or when no thin-alt images are found.  Per-image errors are silently skipped.
async fn enrich_with_ocr(
    markdown: &str,
    html: &str,
    url: &str,
    client: &AcceleratedClient,
) -> String {
    let enricher = match FetchOcrEnricher::new() {
        Ok(e) if e.is_available() => e,
        _ => return markdown.to_string(),
    };

    let http = client.inner().clone();
    let ocr_map = enricher.enrich_images(html, url, &http).await;
    if ocr_map.is_empty() {
        return markdown.to_string();
    }
    enricher.annotate_markdown(markdown, &ocr_map)
}

// ─── hebb kv save ─────────────────────────────────────────────────────────────

/// Save the fetch result to hebb's `kv:urls` namespace for future retrieval.
///
/// Spawns `hebb-mcp` as a one-shot child process and sends a single
/// `tools/call kv_set` request over its stdio.  Silently no-ops when
/// `hebb-mcp` is not installed.  All errors are logged as `debug`.
async fn save_to_hebb(url: &str, markdown: &str, html: &str) {
    if !hebb_is_available() {
        return;
    }
    let key = url_key(url);
    let title = extract_title(html).unwrap_or_default();
    if let Err(e) = hebb_kv_set_oneshot("urls", &key, url, &title, markdown).await {
        tracing::debug!("hebb kv_set skipped: {e}");
    }
}

/// Return `true` when `hebb-mcp` is locatable on this system.
fn hebb_is_available() -> bool {
    if which::which("hebb-mcp").is_ok() {
        return true;
    }
    dirs::data_local_dir().is_some_and(|d| d.join("hebb/bin/hebb-mcp").exists())
}

/// Spawn `hebb-mcp` for a single `kv_set` call then let the process exit.
///
/// Uses the same MCP JSON-RPC stdio protocol as `HebbClient` but without
/// maintaining a long-lived subprocess.
async fn hebb_kv_set_oneshot(
    namespace: &str,
    key: &str,
    url: &str,
    title: &str,
    markdown: &str,
) -> Result<()> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let binary = if let Ok(p) = which::which("hebb-mcp") {
        p
    } else if let Some(managed) = dirs::data_local_dir().map(|d| d.join("hebb/bin/hebb-mcp")) {
        if managed.exists() {
            managed
        } else {
            return Err(anyhow::anyhow!("hebb-mcp not found"));
        }
    } else {
        return Err(anyhow::anyhow!("hebb-mcp not found"));
    };

    let mut child = tokio::process::Command::new(&binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn hebb-mcp: {e}"))?;

    let mut stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let mut reader = BufReader::new(stdout);

    // ── initialize ──────────────────────────────────────────────────────
    let init_req = serde_json::to_string(&json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": { "sampling": {} },
            "clientInfo": { "name": "nab-cli", "version": env!("CARGO_PKG_VERSION") }
        }
    }))?;
    stdin.write_all(format!("{init_req}\n").as_bytes()).await?;
    stdin.flush().await?;

    // Wait for initialize response.
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let _init_resp: serde_json::Value = serde_json::from_str(line.trim())?;

    // initialized notification
    let notif = serde_json::to_string(&json!({
        "jsonrpc": "2.0", "method": "notifications/initialized", "params": {}
    }))?;
    stdin.write_all(format!("{notif}\n").as_bytes()).await?;
    stdin.flush().await?;

    // ── kv_set ──────────────────────────────────────────────────────────
    let call_req = serde_json::to_string(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "kv_set",
            "arguments": {
                "namespace": namespace,
                "key": key,
                "value": { "url": url, "title": title },
                "content_text": markdown,
            }
        }
    }))?;
    stdin.write_all(format!("{call_req}\n").as_bytes()).await?;
    stdin.flush().await?;
    drop(stdin);

    // Read kv_set response (best-effort; don't block indefinitely).
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut resp_line = String::new();
        let _ = reader.read_line(&mut resp_line).await;
    })
    .await;

    let _ = child.kill().await;
    Ok(())
}

/// Derive a short, stable key from a URL using the first 16 hex chars of
/// its SHA-256 hash.
fn url_key(url: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(url.as_bytes());
    hex::encode(&digest[..8]) // 8 bytes = 16 hex chars
}

/// Local alias for the canonical Tor proxy URL defined in the library crate.
///
/// The `socks5h` scheme routes DNS through the proxy, preventing leaks to the
/// local resolver that would reveal the destination to the ISP.
const TOR_PROXY_URL: &str = nab::TOR_PROXY_URL;

/// Build HTTP client with optional proxy and redirect settings.
///
/// When `tor` is `true` the client is configured to route all traffic through
/// the Tor SOCKS5 proxy at `127.0.0.1:9050`.  An explicit `proxy` value takes
/// precedence over `tor` — they are mutually exclusive.  If Tor is unavailable
/// a warning is emitted and the request proceeds without a proxy.
pub(super) fn build_client(
    no_redirect: bool,
    proxy: Option<&str>,
    tor: bool,
) -> Result<AcceleratedClient> {
    let proxy_url = proxy
        .map(String::from)
        .or_else(|| tor.then(|| TOR_PROXY_URL.to_owned()))
        .or_else(|| std::env::var("HTTPS_PROXY").ok())
        .or_else(|| std::env::var("HTTP_PROXY").ok())
        .or_else(|| std::env::var("ALL_PROXY").ok())
        .or_else(|| std::env::var("https_proxy").ok())
        .or_else(|| std::env::var("http_proxy").ok())
        .or_else(|| std::env::var("all_proxy").ok());

    if let Some(ref purl) = proxy_url {
        match build_client_with_proxy(purl, no_redirect) {
            Ok(client) => return Ok(client),
            Err(e) if tor && proxy.is_none() => {
                // Tor was requested but the daemon is not running; warn and fall
                // back to a direct connection so the caller still gets a result.
                eprintln!("⚠️  Tor proxy unavailable ({e:#}); falling back to direct connection");
            }
            Err(e) => return Err(e),
        }
    }

    if no_redirect {
        AcceleratedClient::new_no_redirect()
    } else {
        AcceleratedClient::new()
    }
}

/// Build a `reqwest` client that routes all traffic through the given proxy URL.
fn build_client_with_proxy(proxy_url: &str, no_redirect: bool) -> Result<AcceleratedClient> {
    let proxy = reqwest::Proxy::all(proxy_url)
        .map_err(|e| anyhow::anyhow!("Invalid proxy URL '{proxy_url}': {e}"))?;
    let no_redirect_proxy = reqwest::Proxy::all(proxy_url)
        .map_err(|e| anyhow::anyhow!("Invalid proxy URL '{proxy_url}': {e}"))?;
    let profile = nab::random_profile();
    let headers = profile.to_headers();

    let mut builder = reqwest::Client::builder()
        .proxy(proxy)
        .default_headers(headers.clone());
    if no_redirect {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    }

    let inner_client = builder.build()?;
    let no_redirect_client = reqwest::Client::builder()
        .proxy(no_redirect_proxy)
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    AcceleratedClient::from_clients_with_profile(inner_client, no_redirect_client, profile)
}

// Re-export from mod.rs for internal use.
pub(super) use super::non_empty;

#[cfg(test)]
mod tests {
    use super::{TOR_PROXY_URL, build_client, build_fetch_diagnostics};

    #[test]
    fn build_fetch_diagnostics_for_bot_challenge_mentions_cookies() {
        let warning = build_fetch_diagnostics(
            429,
            "<html><body>Vercel Security Checkpoint</body></html>",
            Some("text/html"),
            0,
            "",
            None,
            true,
        )
        .into_iter()
        .next()
        .expect("expected challenge warning");
        assert!(
            warning.contains("challenge"),
            "warning should mention challenge, got: {warning}"
        );
        assert!(
            warning.contains("--cookies"),
            "warning should suggest --cookies workaround, got: {warning}"
        );
    }

    #[test]
    fn build_fetch_diagnostics_for_rate_limit_is_not_bot_specific() {
        let warning = build_fetch_diagnostics(
            429,
            "Rate limit exceeded. Please slow down.",
            Some("text/html"),
            0,
            "",
            None,
            true,
        )
        .into_iter()
        .next()
        .expect("expected rate-limit warning");
        assert!(
            warning.contains("Rate limiting"),
            "warning should mention rate limiting, got: {warning}"
        );
    }

    #[test]
    fn build_fetch_diagnostics_for_http_401_mentions_authenticated_access() {
        let warning =
            build_fetch_diagnostics(401, "Unauthorized", Some("text/html"), 0, "", None, true)
                .into_iter()
                .next()
                .expect("expected unauthorized warning");
        assert!(
            warning.contains("Authenticated access appears to be required"),
            "warning should mention authenticated access, got: {warning}"
        );
        assert!(
            !warning.contains("login page"),
            "warning should not pretend a bare 401 is a login page, got: {warning}"
        );
    }

    #[test]
    fn build_fetch_diagnostics_for_thin_content_includes_remote_fallback_hint() {
        let thin_markdown = "x".repeat(100);
        let warnings = build_fetch_diagnostics(
            200,
            "<html></html>",
            Some("text/html"),
            20_000,
            &thin_markdown,
            None,
            false,
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("suspiciously thin")),
            "expected thin-content warning, got: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("--remote-fallback")),
            "expected remote-fallback hint, got: {warnings:?}"
        );
    }

    #[test]
    fn build_fetch_diagnostics_for_obfuscated_content_mentions_paywall_behavior() {
        let blob = format!("Title: Protected article\n\n{}", "AbC123+/".repeat(700));
        let warning = build_fetch_diagnostics(
            200,
            "<html><body><script>protected payload</script></body></html>",
            Some("text/html"),
            40_000,
            &blob,
            None,
            true,
        )
        .into_iter()
        .next()
        .expect("expected obfuscated-content warning");
        assert!(
            warning.contains("encoded or obfuscated"),
            "warning should explain the blob-like output, got: {warning}"
        );
        assert!(
            warning.contains("paywalled"),
            "warning should mention protected/paywalled pages, got: {warning}"
        );
    }

    // ── Tor / proxy routing tests ─────────────────────────────────────────────

    #[test]
    fn tor_proxy_url_uses_socks5h_scheme() {
        // GIVEN: the canonical Tor proxy constant
        // WHEN: we inspect its scheme prefix
        // THEN: it uses `socks5h` (DNS via proxy) not plain `socks5`
        assert!(
            TOR_PROXY_URL.starts_with("socks5h://"),
            "Tor proxy must use socks5h:// for DNS-via-proxy; got: {TOR_PROXY_URL}"
        );
    }

    #[test]
    fn tor_proxy_url_targets_localhost_9050() {
        // GIVEN: the canonical Tor proxy constant
        // WHEN: we inspect the host and port
        // THEN: it targets the standard Tor daemon address
        assert!(
            TOR_PROXY_URL.contains("127.0.0.1:9050"),
            "Tor proxy must target 127.0.0.1:9050; got: {TOR_PROXY_URL}"
        );
    }

    #[test]
    fn build_client_without_tor_succeeds() {
        // GIVEN: no proxy, no Tor flag
        // WHEN: we build a client
        // THEN: it succeeds and uses the default configuration
        let result = build_client(false, None, false);
        assert!(
            result.is_ok(),
            "build_client(no_redirect=false, proxy=None, tor=false) must succeed"
        );
    }

    #[test]
    fn build_client_with_explicit_proxy_takes_precedence_over_tor() {
        // GIVEN: an explicit HTTP proxy and tor=true
        // WHEN: we build the client
        // THEN: the explicit proxy is used (no error from bad SOCKS5h address)
        //       The proxy parse step validates URL syntax only; a real TCP
        //       connection is not made, so this succeeds even without a proxy daemon.
        let result = build_client(false, Some("http://127.0.0.1:8080"), true);
        assert!(
            result.is_ok(),
            "explicit proxy must override tor flag without error"
        );
    }

    #[test]
    fn build_client_with_invalid_proxy_url_returns_error() {
        // GIVEN: a syntactically invalid proxy URL
        // WHEN: we build the client
        // THEN: an error is returned (not a panic)
        let result = build_client(false, Some("not-a-valid-url:::"), false);
        assert!(
            result.is_err(),
            "invalid proxy URL must return an error, not panic"
        );
    }
}
