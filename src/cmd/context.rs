//! `nab context URL1 URL2 ...` — fetch multiple URLs in parallel and produce
//! a combined markdown document optimised for LLM context windows.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use nab::content::ContentRouter;
use nab::rate_limit::DomainRateLimiter;
use nab::site::SiteRouter;

use super::fetch::{build_client, non_empty};

/// Maximum concurrent in-flight requests.
const DEFAULT_CONCURRENCY: usize = 10;

/// Minimum delay between requests to the same domain (milliseconds).
const DOMAIN_DELAY_MS: u64 = 200;

/// Result of fetching a single source URL.
struct FetchedSource {
    /// The URL that was fetched.
    url: String,
    /// Human-readable title extracted from the page (falls back to URL).
    title: String,
    /// Converted markdown body, possibly truncated.
    body: String,
    /// Whether the fetch succeeded.
    ok: bool,
}

/// Fetch multiple URLs in parallel and print combined markdown to stdout.
///
/// Each URL is fetched concurrently (up to `DEFAULT_CONCURRENCY` at a time).
/// Results are emitted in the **original argument order** regardless of which
/// responses arrive first, so the output is stable and deterministic.
pub async fn cmd_context(urls: &[String], cookies: &str, max_tokens: usize) -> Result<()> {
    anyhow::ensure!(!urls.is_empty(), "at least one URL is required");

    let total_start = Instant::now();
    let n = urls.len();

    eprintln!("Fetching {n} URL(s) …");

    let semaphore = Arc::new(Semaphore::new(DEFAULT_CONCURRENCY));
    let limiter = Arc::new(DomainRateLimiter::new(Duration::from_millis(
        DOMAIN_DELAY_MS,
    )));
    let cookies_owned = cookies.to_owned();
    let char_budget = budget_per_source(max_tokens, n);

    // Spawn all tasks; collect results in order via indexed slots.
    let mut set: JoinSet<(usize, FetchedSource)> = JoinSet::new();

    for (idx, url) in urls.iter().enumerate() {
        let url = url.clone();
        let sem = semaphore.clone();
        let lim = limiter.clone();
        let cookies = cookies_owned.clone();

        set.spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");

            // Per-domain rate limiting.
            let domain = super::extract_domain(&url);
            lim.wait(&domain).await;

            let source = fetch_one(&url, &cookies, char_budget).await;
            (idx, source)
        });
    }

    // Collect into a fixed-size vec so we can re-sort by index.
    let mut ordered: Vec<Option<FetchedSource>> = (0..n).map(|_| None).collect();
    let mut completed = 0usize;
    while let Some(res) = set.join_next().await {
        completed += 1;
        match res {
            Ok((idx, source)) => {
                let status = if source.ok { "ok" } else { "err" };
                eprintln!("  [{completed}/{n}] {status}: {}", source.url);
                ordered[idx] = Some(source);
            }
            Err(e) => eprintln!("  [{completed}/{n}] panic: {e}"),
        }
    }

    let elapsed = total_start.elapsed();
    emit_combined_markdown(ordered, n, elapsed);

    eprintln!("Done in {:.2}s", elapsed.as_secs_f64());

    Ok(())
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Compute a per-source character budget from the total token budget.
///
/// A conservative LLM token ≈ 4 chars; we divide evenly across sources.
fn budget_per_source(max_tokens: usize, n_sources: usize) -> usize {
    let total_chars = max_tokens.saturating_mul(4);
    total_chars / n_sources.max(1)
}

/// Fetch a single URL, convert to markdown, and apply the character budget.
async fn fetch_one(url: &str, cookies: &str, char_budget: usize) -> FetchedSource {
    match fetch_one_inner(url, cookies).await {
        Ok((title, body)) => {
            let truncated = truncate_to_budget(body, char_budget);
            FetchedSource {
                url: url.to_owned(),
                title,
                body: truncated,
                ok: true,
            }
        }
        Err(e) => FetchedSource {
            url: url.to_owned(),
            title: url.to_owned(),
            body: format!("*Error fetching this source: {e}*"),
            ok: false,
        },
    }
}

/// Inner fetch: try site rule providers first, then fall back to generic HTML.
///
/// Returns `(title, markdown_body)`.
async fn fetch_one_inner(url: &str, cookies: &str) -> Result<(String, String)> {
    let client = build_client(false, None)?;

    let domain = super::extract_domain(url);

    let cookie_header = super::resolve_cookie_header(cookies, &domain);

    let cookie_opt = non_empty(&cookie_header);

    // Try site rule providers first (Wikipedia, YouTube, Twitter, etc.).
    let site_router = SiteRouter::new();
    if let Some(content) = site_router.try_extract(url, &client, cookie_opt).await {
        let title = content
            .metadata
            .title
            .clone()
            .or_else(|| extract_title_from_markdown(&content.markdown))
            .unwrap_or_else(|| url.to_owned());
        return Ok((title, content.markdown));
    }

    // No rule matched — fall back to generic HTTP fetch + content conversion.
    let mut request = client
        .inner()
        .get(url)
        .headers(client.profile().await.to_headers());
    if let Some(cv) = cookie_opt {
        request = request.header("Cookie", cv);
    }

    let response = request.send().await?;
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html")
        .to_owned();

    let bytes = response.bytes().await?;

    let content_router = ContentRouter::new();
    let fetch_url = url.to_owned();
    let ct = content_type.clone();

    let result = tokio::task::spawn_blocking(move || {
        content_router.convert_with_url(&bytes, &ct, Some(&fetch_url))
    })
    .await??;

    let title = extract_title_from_markdown(&result.markdown).unwrap_or_else(|| url.to_owned());

    Ok((title, result.markdown))
}

/// Truncate markdown at the character budget, keeping whole lines.
fn truncate_to_budget(body: String, budget: usize) -> String {
    if budget == 0 || body.len() <= budget {
        return body;
    }

    // Walk backwards from `budget` to the previous newline for a clean cut.
    let safe_budget = body.floor_char_boundary(budget);
    let cut = body[..safe_budget]
        .rfind('\n')
        .map_or(safe_budget, |pos| pos + 1);

    let mut out = body[..cut].to_owned();
    out.push_str("\n\n*[content truncated for context budget]*");
    out
}

/// Extract the first heading or `<title>`-level line from markdown.
fn extract_title_from_markdown(md: &str) -> Option<String> {
    md.lines()
        .find(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches('#').trim().to_owned())
}

/// Write the combined markdown document to stdout.
fn emit_combined_markdown(
    sources: Vec<Option<FetchedSource>>,
    n: usize,
    elapsed: std::time::Duration,
) {
    println!("# Context: {n} source{}", if n == 1 { "" } else { "s" });
    println!();
    println!(
        "*Assembled in {:.2}s from {n} URL{}*",
        elapsed.as_secs_f64(),
        if n == 1 { "" } else { "s" }
    );

    for (i, slot) in sources.into_iter().enumerate() {
        println!("\n---\n");
        let source_num = i + 1;

        let Some(src) = slot else {
            println!("## Source {source_num}: (unavailable)");
            println!();
            println!("*This source could not be retrieved.*");
            continue;
        };

        let status = if src.ok { "" } else { " [ERROR]" };
        let label = if src.title == src.url {
            src.url.clone()
        } else {
            format!("{} ({})", src.title, src.url)
        };

        println!("## Source {source_num}{status}: {label}");
        println!();
        println!("{}", src.body);
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── budget_per_source ────────────────────────────────────────────────────

    #[test]
    fn budget_per_source_divides_evenly_across_sources() {
        // GIVEN: 8000 tokens, 4 sources
        // WHEN: compute per-source budget
        // THEN: each gets (8000 * 4) / 4 = 8000 chars
        assert_eq!(budget_per_source(8_000, 4), 8_000);
    }

    #[test]
    fn budget_per_source_single_source_gets_full_budget() {
        assert_eq!(budget_per_source(8_000, 1), 32_000);
    }

    #[test]
    fn budget_per_source_zero_sources_avoids_division_by_zero() {
        let result = budget_per_source(8_000, 0);
        assert_eq!(
            result, 32_000,
            "zero sources treated as 1 to avoid div-by-zero"
        );
    }

    #[test]
    fn budget_per_source_zero_max_tokens_returns_zero() {
        assert_eq!(budget_per_source(0, 4), 0);
    }

    // ── truncate_to_budget ───────────────────────────────────────────────────

    #[test]
    fn truncate_to_budget_short_body_unchanged() {
        // GIVEN: body shorter than budget
        let body = "short".to_owned();
        // WHEN: truncate with large budget
        let result = truncate_to_budget(body.clone(), 1_000);
        // THEN: body unchanged
        assert_eq!(result, body);
    }

    #[test]
    fn truncate_to_budget_zero_budget_passes_through() {
        let body = "x".repeat(100_000);
        let result = truncate_to_budget(body.clone(), 0);
        assert_eq!(result, body, "budget=0 means unlimited");
    }

    #[test]
    fn truncate_to_budget_appends_notice_when_truncated() {
        // GIVEN: body longer than budget
        let body = "line one\nline two\nline three\n".to_owned();
        // WHEN: truncate at 15 chars (after "line one\nline t")
        let result = truncate_to_budget(body, 15);
        // THEN: truncation notice appended
        assert!(
            result.contains("[content truncated"),
            "should contain truncation notice, got: {result}"
        );
    }

    #[test]
    fn truncate_to_budget_cuts_at_newline_boundary() {
        // GIVEN: body with clear line boundaries
        let body = "aaaa\nbbbb\ncccc\n".to_owned();
        // WHEN: budget is 8 (halfway through second word)
        let result = truncate_to_budget(body, 8);
        // THEN: cut happens at newline after "aaaa", not mid-word
        assert!(result.starts_with("aaaa\n"), "should cut at line boundary");
        assert!(!result.contains("bbbb"), "should not include next line");
    }

    // ── extract_title_from_markdown ──────────────────────────────────────────

    #[test]
    fn extract_title_finds_h1_heading() {
        let md = "# My Page Title\n\nSome paragraph text.";
        assert_eq!(
            extract_title_from_markdown(md),
            Some("My Page Title".to_owned())
        );
    }

    #[test]
    fn extract_title_ignores_h2_and_lower() {
        let md = "## Subtitle\n\nNo h1 here.";
        assert_eq!(extract_title_from_markdown(md), None);
    }

    #[test]
    fn extract_title_returns_none_for_empty_markdown() {
        assert_eq!(extract_title_from_markdown(""), None);
    }

    #[test]
    fn extract_title_strips_leading_hashes_and_whitespace() {
        let md = "#   Padded Title  \n\nBody.";
        // Note: trim() handles both leading (after #) and trailing spaces
        assert_eq!(
            extract_title_from_markdown(md),
            Some("Padded Title".to_owned())
        );
    }

    // ── cmd_context error handling ────────────────────────────────────────────

    #[tokio::test]
    async fn cmd_context_empty_urls_returns_error() {
        // GIVEN: no URLs provided
        // WHEN: cmd_context called with empty slice
        // THEN: returns an error immediately
        let result = cmd_context(&[], "none", 8_000).await;
        assert!(result.is_err(), "empty URL list should be an error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("at least one URL"),
            "error message should be descriptive, got: {msg}"
        );
    }
}
