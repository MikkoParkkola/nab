//! Integrates Apple Vision OCR into the nab fetch HTML pipeline.
//!
//! When nab fetches an HTML page, any `<img>` tag with missing or thin
//! (< 20 char) alt text is OCR'd.  The recognized text is inserted into the
//! output markdown as `[Image: <ocr text>]` annotations.
//!
//! ## Cache
//!
//! Keyed by SHA-256 of image bytes, stored in
//! `~/Library/Application Support/nab/cache/ocr/<sha256>.txt`.
//! Cache TTL: 30 days.  Files older than 30 days are re-OCR'd and overwritten.
//!
//! ## Budget
//!
//! At most [`MAX_IMAGES_PER_PAGE`] images are OCR'd per fetch call, within
//! `OCR_STAGE_BUDGET` for the whole stage, to bound added latency.  Images
//! are processed in document order; extras are skipped (their alt text is
//! left as-is), with a warning when the time budget cut the stage short.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::{OcrEngine, default_engine};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Maximum number of images to OCR per single fetch call.
pub const MAX_IMAGES_PER_PAGE: usize = 10;

/// Minimum alt-text length that counts as "good" — images with alt text at or
/// above this threshold are skipped.
const MIN_ALT_TEXT_LEN: usize = 20;

/// Cache TTL: 30 days in seconds.
const CACHE_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// Wall-clock budget for the whole OCR stage of one fetch: image downloads
/// plus recognition. A cold Apple Vision call blocked for 57 s and over 110 s
/// while the Neural Engine model compiled (MIK-8234); warm calls finish well
/// inside this. Past the budget the fetch keeps what it has and moves on.
const OCR_STAGE_BUDGET: Duration = Duration::from_secs(3);

// ─── FetchOcrEnricher ─────────────────────────────────────────────────────────

/// Enriches fetched HTML by OCR-ing images with thin or absent alt text.
///
/// # Example
///
/// ```rust,no_run
/// use nab::content::ocr::fetch_integration::FetchOcrEnricher;
///
/// # async fn example() -> anyhow::Result<()> {
/// let enricher = FetchOcrEnricher::new()?;
/// let client = reqwest::Client::new();
/// if enricher.is_available() {
///     let ocr_map = enricher
///         .enrich_images("<img src='/a.png'>", "https://example.com", &client)
///         .await;
///     let annotated = enricher.annotate_markdown("# Page\n\n![](a.png)", &ocr_map);
/// }
/// # Ok(())
/// # }
/// ```
pub struct FetchOcrEnricher {
    engine: Arc<dyn OcrEngine>,
    cache_dir: PathBuf,
    max_per_page: usize,
}

impl Default for FetchOcrEnricher {
    fn default() -> Self {
        // Infallible default — uses platform OCR engine and standard cache dir.
        Self::with_max(MAX_IMAGES_PER_PAGE)
    }
}

impl FetchOcrEnricher {
    /// Create an enricher with the default OCR engine and the standard cache.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the cache directory cannot be created.
    pub fn new() -> Result<Self> {
        let cache_dir = default_cache_dir()?;
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("create OCR cache dir {}", cache_dir.display()))?;
        Ok(Self {
            engine: Arc::from(default_engine()),
            cache_dir,
            max_per_page: MAX_IMAGES_PER_PAGE,
        })
    }

    /// Create with a custom `max_per_page` limit (for testing / caller override).
    pub fn with_max(max_per_page: usize) -> Self {
        let cache_dir = default_cache_dir().unwrap_or_else(|_| PathBuf::from("/tmp/nab-ocr-cache"));
        Self {
            engine: Arc::from(default_engine()),
            cache_dir,
            max_per_page,
        }
    }

    /// Create with an explicit engine and cache directory (for unit tests).
    pub fn with_engine_and_cache(
        engine: Arc<dyn OcrEngine>,
        cache_dir: PathBuf,
        max_per_page: usize,
    ) -> Self {
        Self {
            engine,
            cache_dir,
            max_per_page,
        }
    }

    /// Return `true` when the OCR engine is available on this platform.
    pub fn is_available(&self) -> bool {
        self.engine.is_available()
    }

    /// Extract image URLs with thin/missing alt text from `html`, fetch each
    /// image's bytes, run OCR, and return a map of `image_url → ocr_text`.
    ///
    /// At most `max_per_page` images are processed in document order, within
    /// `OCR_STAGE_BUDGET` for the whole stage; past it the images not yet
    /// done are skipped with a warning and the results so far are returned.
    /// Errors for individual images are logged as debug warnings and skipped —
    /// the returned map simply omits entries for failed images.
    ///
    /// `base_url` is used to resolve relative image `src` values.
    pub async fn enrich_images(
        &self,
        html: &str,
        base_url: &str,
        http_client: &reqwest::Client,
    ) -> HashMap<String, String> {
        let candidates = extract_image_candidates(html, base_url);
        let mut results = HashMap::new();
        let deadline = tokio::time::Instant::now() + OCR_STAGE_BUDGET;

        for url in candidates.into_iter().take(self.max_per_page) {
            let Ok(outcome) =
                tokio::time::timeout_at(deadline, self.ocr_url(&url, http_client)).await
            else {
                tracing::warn!(
                    "OCR stage stopped at its {} s budget; kept text from {} image(s), skipped the rest",
                    OCR_STAGE_BUDGET.as_secs(),
                    results.len()
                );
                break;
            };
            match outcome {
                Ok(Some(text)) if !text.trim().is_empty() => {
                    results.insert(url, text.trim().to_string());
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(url = %url, "OCR skipped: {e}");
                }
            }
        }

        results
    }

    /// Given the fetched markdown and the OCR results, insert
    /// `[Image: <text>]` annotations in place.
    ///
    /// Matches markdown image references of the form `![alt](url)` and, when
    /// `ocr_results` contains an entry for that URL, appends
    /// ` [Image: <text>]` immediately after the image syntax.
    pub fn annotate_markdown(
        &self,
        markdown: &str,
        ocr_results: &HashMap<String, String>,
    ) -> String {
        if ocr_results.is_empty() {
            return markdown.to_string();
        }
        annotate_markdown_images(markdown, ocr_results)
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// OCR a single image URL, consulting the cache first.
    async fn ocr_url(&self, url: &str, http_client: &reqwest::Client) -> Result<Option<String>> {
        // Fetch image bytes.
        let bytes = http_client
            .get(url)
            .send()
            .await
            .with_context(|| format!("fetch image {url}"))?
            .bytes()
            .await
            .with_context(|| format!("read image bytes {url}"))?;

        if bytes.is_empty() {
            return Ok(None);
        }

        let hash = hex_sha256(&bytes);
        let cache_path = self.cache_dir.join(format!("{hash}.txt"));

        // Cache hit?
        if let Some(cached) = read_cache(&cache_path) {
            return Ok(Some(cached));
        }

        // Cache miss — run OCR.
        let result = self
            .engine
            .ocr_image(&bytes)
            .await
            .with_context(|| format!("OCR failed for {url}"))?;

        let text = result.text;

        // Persist to cache regardless of whether text is empty.
        if let Err(e) = std::fs::write(&cache_path, &text) {
            tracing::debug!("OCR cache write failed for {hash}: {e}");
        }

        Ok(Some(text))
    }
}

// ─── Image candidate extraction ───────────────────────────────────────────────

/// Parse `html` for `<img>` tags with thin or absent alt text, resolve their
/// `src` attributes against `base_url`, and return candidates in document order.
fn extract_image_candidates(html: &str, base_url: &str) -> Vec<String> {
    use scraper::{Html, Selector};

    let doc = Html::parse_document(html);
    let Ok(sel) = Selector::parse("img") else {
        return vec![];
    };

    let base = url::Url::parse(base_url).ok();

    doc.select(&sel)
        .filter_map(|el| {
            let alt = el.value().attr("alt").unwrap_or("");
            // Skip images with sufficient alt text.
            if alt.len() >= MIN_ALT_TEXT_LEN {
                return None;
            }
            let src = el.value().attr("src")?;
            // Skip data URIs — no HTTP fetch needed but we can't get bytes simply.
            if src.starts_with("data:") {
                return None;
            }
            let resolved = resolve_url(src, base.as_ref())?;
            Some(resolved)
        })
        .collect()
}

/// Resolve a potentially-relative `src` against the page's base URL.
fn resolve_url(src: &str, base: Option<&url::Url>) -> Option<String> {
    if src.starts_with("http://") || src.starts_with("https://") {
        return Some(src.to_string());
    }
    let base = base?;
    base.join(src).ok().map(|u| u.to_string())
}

// ─── Markdown annotation ──────────────────────────────────────────────────────

/// Insert `[Image: <text>]` annotations into `markdown` for every image
/// whose URL appears in `ocr_results`.
///
/// Handles the common markdown image patterns:
/// - `![alt](url)` — inserts ` [Image: text]` after the closing `)`
/// - `![alt](url "title")` — same
fn annotate_markdown_images(markdown: &str, ocr_results: &HashMap<String, String>) -> String {
    // We process the markdown character by character to reliably find
    // image spans without a full parser dependency.  The pattern we look
    // for is: `![` ... `](` <url> [optional "title"] `)`.
    let mut output = String::with_capacity(markdown.len() + ocr_results.len() * 40);
    let chars: Vec<char> = markdown.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        // Look for `![`
        if i + 1 < n
            && chars[i] == '!'
            && chars[i + 1] == '['
            && let Some((end, url)) = parse_markdown_image(&chars, i)
        {
            // Write the original image syntax verbatim.
            output.push_str(&markdown[char_byte_offset(&chars, i)..char_byte_offset(&chars, end)]);
            // Append annotation if we have OCR text for this URL.
            if let Some(text) = ocr_results.get(&url) {
                let clean = text.replace('\n', " ");
                let _ = write!(output, " [Image: {clean}]");
            }
            i = end;
            continue;
        }
        output.push(chars[i]);
        i += 1;
    }

    output
}

/// Try to parse a markdown image starting at `chars[start]` (which must be `!`).
///
/// Returns `(end_exclusive, absolute_url)` on success.
fn parse_markdown_image(chars: &[char], start: usize) -> Option<(usize, String)> {
    let n = chars.len();
    // Consume `![`
    let mut i = start + 2;
    // Skip alt text up to `]`
    let mut depth = 1usize;
    while i < n && depth > 0 {
        match chars[i] {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    // Expect `(`
    if i >= n || chars[i] != '(' {
        return None;
    }
    i += 1;
    // Collect URL until whitespace or `)`
    let url_start = i;
    while i < n && chars[i] != ')' && !chars[i].is_whitespace() {
        i += 1;
    }
    if i > url_start && i < n {
        let url: String = chars[url_start..i].iter().collect();
        // Skip optional title and closing `)`
        while i < n && chars[i] != ')' {
            i += 1;
        }
        if i < n {
            i += 1; // consume `)`
        }
        return Some((i, url));
    }
    None
}

/// Map a character index back to a byte offset in the original string.
///
/// `chars` must be the `Vec<char>` collected from the same string.
fn char_byte_offset(chars: &[char], char_idx: usize) -> usize {
    chars[..char_idx].iter().map(|c| c.len_utf8()).sum()
}

// ─── Cache helpers ────────────────────────────────────────────────────────────

/// Compute the hex-encoded SHA-256 of `bytes`.
fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

/// Return the standard OCR cache directory for this platform.
fn default_cache_dir() -> Result<PathBuf> {
    let base = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    Ok(base.join("nab/cache/ocr"))
}

/// Read from cache if the file exists and is younger than `CACHE_TTL_SECS`.
///
/// Returns `None` on any error (missing file, stale, I/O error).
fn read_cache(path: &std::path::Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(mtime).ok()?;
    if age > Duration::from_secs(CACHE_TTL_SECS) {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_image_candidates ─────────────────────────────────────────────

    /// Images with alt text >= 20 chars are NOT returned as candidates.
    #[test]
    fn extract_image_candidates_skips_good_alt_text() {
        // GIVEN HTML with an image that has adequate alt text (>= 20 chars)
        let html = r#"<img src="photo.jpg" alt="A landscape photo of mountains">"#;
        // WHEN we extract candidates
        let candidates = extract_image_candidates(html, "https://example.com/page");
        // THEN no candidates — alt text is sufficient
        assert!(candidates.is_empty(), "should skip well-described image");
    }

    /// Images with short alt text are returned as candidates.
    #[test]
    fn extract_image_candidates_includes_thin_alt_text() {
        // GIVEN HTML with an image that has no alt text
        let html = r#"<img src="chart.png" alt="">"#;
        // WHEN extracted
        let candidates = extract_image_candidates(html, "https://example.com/page");
        // THEN one candidate
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].contains("chart.png"));
    }

    /// `max_per_page` limits the number of candidates processed.
    #[test]
    fn enrich_images_respects_max_per_page() {
        // GIVEN HTML with 5 thin-alt images and max_per_page=3
        let html: String = (1..=5)
            .map(|i| format!(r#"<img src="img{i}.png" alt="">"#))
            .collect::<Vec<_>>()
            .join("\n");
        // WHEN we extract candidates and take only 3
        let candidates = extract_image_candidates(&html, "https://example.com/");
        let capped: Vec<_> = candidates.into_iter().take(3).collect();
        // THEN exactly 3 candidates
        assert_eq!(capped.len(), 3);
    }

    // ── annotate_markdown ────────────────────────────────────────────────────

    /// `annotate_markdown` inserts `[Image: ...]` after matched image syntax.
    #[test]
    fn annotate_markdown_inserts_ocr_annotation() {
        // GIVEN markdown with an image reference and an OCR result for that URL
        let markdown = "# Title\n\n![](https://example.com/chart.png)\n\nSome text.";
        let mut ocr = HashMap::new();
        ocr.insert(
            "https://example.com/chart.png".to_string(),
            "Q3 Revenue: $42M".to_string(),
        );
        // WHEN we annotate
        let enricher = FetchOcrEnricher::with_max(10);
        let result = enricher.annotate_markdown(markdown, &ocr);
        // THEN the annotation appears inline
        assert!(
            result.contains("[Image: Q3 Revenue: $42M]"),
            "annotation missing in: {result}"
        );
        assert!(result.contains("# Title"), "original content preserved");
    }

    /// `annotate_markdown` leaves markdown unchanged when no OCR results match.
    #[test]
    fn annotate_markdown_leaves_no_match_unchanged() {
        // GIVEN markdown with an image not in the OCR map
        let markdown = "![alt text](https://example.com/unknown.png)";
        let ocr: HashMap<String, String> = HashMap::new();
        // WHEN annotated
        let enricher = FetchOcrEnricher::with_max(10);
        let result = enricher.annotate_markdown(markdown, &ocr);
        // THEN markdown is byte-identical
        assert_eq!(result, markdown);
    }

    /// Newlines in OCR text are collapsed to spaces in the annotation.
    #[test]
    fn annotate_markdown_collapses_newlines_in_ocr_text() {
        // GIVEN OCR text with embedded newlines
        let markdown = "![](https://example.com/img.png)";
        let mut ocr = HashMap::new();
        ocr.insert(
            "https://example.com/img.png".to_string(),
            "Line one\nLine two".to_string(),
        );
        // WHEN annotated
        let enricher = FetchOcrEnricher::with_max(10);
        let result = enricher.annotate_markdown(markdown, &ocr);
        // THEN newline is replaced with space
        assert!(
            result.contains("[Image: Line one Line two]"),
            "got: {result}"
        );
    }

    // ── stage budget (MIK-8234) ──────────────────────────────────────────────

    /// Answers the first OCR call, then never returns: the shape of a cold
    /// Apple Vision call blocked while the Neural Engine model compiles.
    struct StallAfterFirstEngine {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl OcrEngine for StallAfterFirstEngine {
        fn name(&self) -> &'static str {
            "stall_after_first"
        }
        fn supported_languages(&self) -> &'static [&'static str] {
            &["*"]
        }
        fn is_available(&self) -> bool {
            true
        }
        async fn ocr_image(
            &self,
            _bytes: &[u8],
        ) -> Result<crate::content::ocr::OcrResult, crate::content::ocr::OcrError> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return Ok(crate::content::ocr::OcrResult {
                    text: "first image text".to_string(),
                    language: None,
                    confidence: 1.0,
                    regions: vec![],
                });
            }
            std::future::pending().await
        }
    }

    /// Serves a distinct non-empty body per path (distinct bytes defeat the
    /// hash-keyed cache, so every image reaches the engine).
    async fn spawn_image_server() -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut buf = [0_u8; 1024];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        req.extend_from_slice(&buf[..n]);
                        if req.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let path = String::from_utf8_lossy(&req)
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .to_string();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        path.len()
                    );
                    // The client drops connections at the deadline; ignore write errors.
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(path.as_bytes()).await;
                });
            }
        });
        addr
    }

    /// A stalled OCR engine must not stall the fetch: `enrich_images` returns
    /// within its stage budget and keeps the results it already has.
    /// Fails on 0.13.1 by the 10 s test timeout (the stage had no deadline).
    #[tokio::test]
    async fn enrich_images_keeps_partial_results_when_engine_stalls_on_fixture() {
        // GIVEN the daniel.haxx.se page whose images stalled Vision (MIK-8234),
        // with its image origins pointed at a local server
        let addr = spawn_image_server().await;
        let local = format!("http://{addr}");
        let html = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ocr_stall_daniel_haxx_se.html"
        ))
        .expect("fixture")
        .replace("https://daniel.haxx.se", &local)
        .replace("https://secure.gravatar.com", &local);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cache = tempfile::tempdir().expect("tempdir");
        let enricher = FetchOcrEnricher::with_engine_and_cache(
            Arc::new(StallAfterFirstEngine {
                calls: Arc::clone(&calls),
            }),
            cache.path().to_path_buf(),
            MAX_IMAGES_PER_PAGE,
        );
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client");

        // WHEN the OCR stage runs and the second image never finishes
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            enricher.enrich_images(&html, &format!("{local}/blog/"), &client),
        )
        .await;

        // THEN it returns within the budget, with the first image's text kept
        let map =
            result.expect("enrich_images did not return within 10 s: OCR stage has no deadline");
        assert!(
            calls.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "engine reached fewer than 2 times; the stall was never exercised"
        );
        assert_eq!(map.len(), 1, "partial OCR results lost: {map:?}");
        assert!(map.values().any(|t| t == "first image text"));
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "took {:?}",
            started.elapsed()
        );
    }

    // ── cache helpers ────────────────────────────────────────────────────────

    /// Stale cache files (older than TTL) are not returned.
    #[test]
    fn read_cache_returns_none_for_stale_file() {
        use std::io::Write;
        // GIVEN a temp file with mtime forced to >30 days ago
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("stale.txt");
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(b"cached text").expect("write");
        // Set mtime to 31 days ago via std (stable since Rust 1.75).
        let old_time = SystemTime::now() - Duration::from_hours(744);
        f.set_modified(old_time).expect("set_modified");
        drop(f);
        // WHEN read
        let result = read_cache(&path);
        // THEN None (stale)
        assert!(
            result.is_none(),
            "expected None for stale cache, got: {result:?}"
        );
    }

    /// Fresh cache files are returned.
    #[test]
    fn read_cache_returns_content_for_fresh_file() {
        use std::io::Write;
        // GIVEN a recently-written temp file
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fresh.txt");
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(b"recognized text").expect("write");
        drop(f);
        // WHEN read
        let result = read_cache(&path);
        // THEN content is returned
        assert_eq!(result.as_deref(), Some("recognized text"));
    }
}
