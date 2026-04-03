//! `fetch` tool — single-URL fetch with diff, focus, and budget support.

use std::fmt::Write as FmtWrite;
use std::time::Instant;

use rust_mcp_sdk::macros::{JsonSchema, mcp_tool};
use rust_mcp_sdk::schema::{CallToolResult, TextContent, schema_utils::CallToolError};
use serde::{Deserialize, Serialize};

use nab::content::budget::truncate_to_budget;
use nab::content::diff::{ContentSnapshot, compute_diff};
use nab::content::diff_format::format_diff_markdown;
use nab::content::focus::extract_focused;
use nab::content::response_classifier::{classify_http_response, classify_thin_content};
use nab::content::snapshot_store::SnapshotStore;
use nab::{AcceleratedClient, SafeFetchConfig};

use crate::helpers::{
    convert_body_async, fetch_safe_response, fetch_with_cookies, fetch_with_session_response,
    resolve_cookie_header, write_body_info, write_response_summary,
};
use crate::structured::{FetchStructuredParams, build_fetch_structured_v2, truncate_markdown};
use crate::tools::client::{get_client, resolve_session_client};

// ─── Tool definition ─────────────────────────────────────────────────────────

#[mcp_tool(
    name = "fetch",
    description = "Fetch a URL and convert to clean markdown for LLM consumption.

Content conversion (automatic by Content-Type):
- HTML → clean markdown (boilerplate removed, links preserved)
- PDF → markdown with headings and table detection (requires pdf feature)
- JSON/plain text → passthrough
- SPA data auto-extracted (__NEXT_DATA__, __NUXT__, __APOLLO_STATE__, etc.)

Network features:
- HTTP/2 multiplexing, HTTP/3 (QUIC) with 0-RTT
- TLS 1.3, Brotli/Zstd/Gzip decompression
- Realistic browser fingerprints (Chrome/Firefox/Safari)
- Browser cookie injection (Brave/Chrome/Firefox/Safari)

Diff mode (diff: true):
- Compares current content against the previous snapshot for this URL
- Returns only the changed sections (token-efficient for monitoring tasks)
- First fetch caches the page; subsequent fetches return semantic diffs
- Unchanged content returns a 5-token confirmation instead of full body

Focus mode (focus: query):
- Keeps only sections relevant to the query (BM25 scoring)
- Replaces dropped sections with '[N sections omitted]' markers
- Diff markers are always preserved regardless of relevance

Token budget (max_tokens: N):
- Structure-aware truncation preserving headings, code, and tables
- Priority: title > code/tables > headings (30% cap) > body > blockquotes

Returns: Markdown-converted body with timing info (or diff when diff: true).",
    read_only_hint = true,
    open_world_hint = true
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FetchTool {
    url: String,
    #[serde(default)]
    headers: bool,
    #[serde(default)]
    body: bool,
    /// Browser cookie source.
    ///
    /// Omit or use `"auto"` to use the default browser for this domain.
    /// Use `"none"` to disable cookies, or pass an explicit browser name such
    /// as `"brave"`, `"chrome"`, `"firefox"`, `"safari"`, or `"edge"`.
    #[serde(default)]
    cookies: Option<String>,
    /// When true, return only changed content vs the previous snapshot.
    ///
    /// On first fetch the page is cached and full content is returned.
    /// On subsequent fetches only the semantic diff is returned, saving
    /// tokens for monitoring or change-detection workflows.
    #[serde(default)]
    diff: bool,
    /// Natural-language query to focus extraction on relevant sections.
    ///
    /// When set, uses BM25 scoring to keep only the sections most relevant
    /// to the query, replacing omitted sections with count markers.
    /// Dramatically reduces token count for large documents when you know
    /// what you're looking for.
    #[serde(default)]
    focus: Option<String>,
    /// Maximum token budget for the returned content.
    ///
    /// When set, performs structure-aware truncation that preserves
    /// headings, code blocks, and tables before trimming body text.
    /// Uses priority scoring: title/summary first, then code/tables,
    /// then headings (capped at 30% of budget), then body text.
    #[serde(default)]
    max_tokens: Option<u64>,
    /// Named session for cookie persistence across calls.
    ///
    /// When set, nab uses an isolated per-session cookie jar so that
    /// `Set-Cookie` response headers from one call are automatically included
    /// on the next call with the same session name.  Use this to maintain
    /// authenticated state across multiple `fetch` calls after a `login`.
    ///
    /// Session names: 1-64 chars, alphanumeric + hyphens + underscores.
    /// Sessions are created implicitly on first use and live for the
    /// process lifetime.  Absent = stateless global client (no change).
    #[serde(default)]
    session: Option<String>,
}

impl FetchTool {
    #[allow(clippy::too_many_lines)]
    pub async fn run(&self) -> Result<CallToolResult, CallToolError> {
        let url_host = url::Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| "<invalid>".to_owned());
        tracing::info!(
            url_host = %url_host,
            has_focus = self.focus.is_some(),
            has_budget = self.max_tokens.is_some(),
            has_session = self.session.is_some(),
            diff = self.diff,
            "fetch start"
        );

        let start = Instant::now();
        let client: &AcceleratedClient = get_client().await;
        let profile = client.profile().await;

        let mut output = format!("🌐 Fetching: {}\n", self.url);
        let _ = writeln!(
            output,
            "🎭 Profile: {}",
            profile.user_agent.split('/').next().unwrap_or("Unknown")
        );

        let cookie_header = resolve_cookie_header(&self.url, self.cookies.as_deref());

        // ── Session path: use isolated cookie jar client ──────────────────────
        // When a named session is requested the session's client handles cookie
        // persistence automatically through its baked-in reqwest::Jar.  We skip
        // the SiteRouter (which requires AcceleratedClient) and go straight to
        // the HTTP fetch so the session jar receives all Set-Cookie responses.
        if let Some(ref session_name) = self.session {
            let session_client =
                resolve_session_client(session_name, Some(&cookie_header), &self.url).await?;
            if let Some(ref sn) = self.session {
                let _ = writeln!(output, "🔑 Session: {sn}");
            }

            let (status, content_type, response_headers, body_bytes, elapsed) =
                fetch_with_session_response(&session_client, &self.url, start).await?;
            let raw_text = String::from_utf8_lossy(&body_bytes).into_owned();

            write_response_summary(
                &mut output,
                status,
                elapsed,
                self.headers,
                &response_headers,
            );
            write_body_info(&mut output, body_bytes.len());

            let conversion = convert_body_async(&body_bytes, &content_type, &self.url).await?;
            trace_fetch_classification(
                status.as_u16(),
                &content_type,
                &raw_text,
                body_bytes.len(),
                &conversion.markdown,
                conversion.quality.as_ref(),
            );

            if let Some(pages) = conversion.page_count {
                let _ = writeln!(
                    output,
                    "📑 Pages: {} | Conversion: {:.1}ms",
                    pages, conversion.elapsed_ms
                );
            }

            let markdown = conversion.markdown;
            let status_u16 = status.as_u16();
            let elapsed_ms = elapsed.as_secs_f64() * 1000.0;

            return Ok(self.finish_fetch(output, markdown, status_u16, &content_type, elapsed_ms));
        }

        // ── Standard path (no session) ────────────────────────────────────────

        let site_router = nab::site::SiteRouter::new();
        let cookie_opt = if cookie_header.is_empty() {
            None
        } else {
            Some(cookie_header.as_str())
        };

        // Determine markdown, status, content_type, and elapsed_ms from either
        // a specialized site provider or the standard HTTP fetch path.  Both
        // paths converge below into the single diff + structured_content pipeline.
        let (markdown, status_u16, content_type, elapsed_ms) = if let Some(site_content) =
            site_router.try_extract(&self.url, client, cookie_opt).await
        {
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            output.push_str("\n📄 Content (from specialized provider):\n\n");
            (
                site_content.markdown,
                200u16,
                "text/html".to_owned(),
                elapsed_ms,
            )
        } else {
            let config = SafeFetchConfig::default();

            let (status, content_type, response_headers, body_bytes, elapsed) =
                if cookie_header.is_empty() {
                    fetch_safe_response(client, &self.url, &config, start).await?
                } else {
                    fetch_with_cookies(client, &self.url, &cookie_header, &profile, start).await?
                };
            let raw_text = String::from_utf8_lossy(&body_bytes).into_owned();

            write_response_summary(
                &mut output,
                status,
                elapsed,
                self.headers,
                &response_headers,
            );
            write_body_info(&mut output, body_bytes.len());

            let conversion = convert_body_async(&body_bytes, &content_type, &self.url).await?;
            trace_fetch_classification(
                status.as_u16(),
                &content_type,
                &raw_text,
                body_bytes.len(),
                &conversion.markdown,
                conversion.quality.as_ref(),
            );

            if let Some(pages) = conversion.page_count {
                let _ = writeln!(
                    output,
                    "📑 Pages: {} | Conversion: {:.1}ms",
                    pages, conversion.elapsed_ms
                );
            }

            // Attempt Next.js content chunk recovery when extraction is thin.
            // The readability extractor often captures 300-600 chars of
            // nav/header/footer even when the article body is empty, so we
            // use a generous threshold (800) combined with a low quality
            // confidence score to trigger recovery.
            let quality_is_low = conversion
                .quality
                .as_ref()
                .is_some_and(|q| q.confidence < 0.5);
            let final_markdown = if content_type.contains("html")
                && (conversion.markdown.len() < 800 || quality_is_low)
                && body_bytes.len() > 5_000
            {
                let raw_html = String::from_utf8_lossy(&body_bytes);
                if let Some(recovered) =
                    crate::helpers::recover_nextjs_chunks(client, &raw_html, &self.url).await
                {
                    let _ = writeln!(
                        output,
                        "   Recovered {} chars from Next.js content chunk",
                        recovered.len()
                    );
                    recovered
                } else {
                    conversion.markdown
                }
            } else {
                conversion.markdown
            };

            (
                final_markdown,
                status.as_u16(),
                content_type,
                elapsed.as_secs_f64() * 1000.0,
            )
        };

        Ok(self.finish_fetch(output, markdown, status_u16, &content_type, elapsed_ms))
    }

    /// Unified post-processing pipeline shared by both the session and the
    /// standard fetch paths: diff → body preview → focus → budget → structured.
    fn finish_fetch(
        &self,
        mut output: String,
        markdown: String,
        status_u16: u16,
        content_type: &str,
        elapsed_ms: f64,
    ) -> CallToolResult {
        // Unified post-processing pipeline: diff → focus → budget
        let has_diff = if self.diff {
            let (diff_output, had_diff) = apply_diff(&self.url, &markdown);
            output.push('\n');
            output.push_str(&diff_output);
            had_diff
        } else {
            if self.body {
                let truncated = truncate_markdown(&markdown, 4000);
                let _ = write!(output, "\n{truncated}");
            }
            false
        };

        // Focus: keep only sections relevant to the query (BM25 scoring).
        // Diff markers are automatically exempt from filtering.
        let (processed_markdown, omitted_sections, total_sections) =
            if let Some(ref query) = self.focus {
                let focus_result = extract_focused(&markdown, query);
                (
                    focus_result.markdown,
                    focus_result.omitted_sections,
                    focus_result.total_sections,
                )
            } else {
                (markdown, 0, 0)
            };

        // Budget: structure-aware truncation with priority scoring.
        let max_tok = self
            .max_tokens
            .map(|t| usize::try_from(t).unwrap_or(usize::MAX));
        let budget_result = truncate_to_budget(&processed_markdown, max_tok);

        let structured = build_fetch_structured_v2(&FetchStructuredParams {
            url: &self.url,
            status: status_u16,
            content_type,
            markdown: &budget_result.markdown,
            timing_ms: elapsed_ms,
            has_diff,
            omitted_sections,
            total_sections,
            truncated: budget_result.truncated,
            full_tokens: budget_result.total_tokens,
        });

        let mut result = CallToolResult::text_content(vec![TextContent::from(output)]);
        result.structured_content = Some(structured);
        result
    }
}

// ─── Diff helpers ─────────────────────────────────────────────────────────────

/// Load previous snapshot, compute diff, save new snapshot.
///
/// Returns `(formatted_output, has_diff)` where `has_diff` is `true` when
/// content changed since the last snapshot.  Always saves a fresh snapshot
/// regardless of whether content changed.
fn apply_diff(url: &str, markdown: &str) -> (String, bool) {
    apply_diff_with_store(&SnapshotStore::new(), url, markdown)
}

fn trace_fetch_classification(
    status: u16,
    content_type: &str,
    raw_text: &str,
    body_len: usize,
    markdown: &str,
    quality: Option<&nab::content::quality::QualityScore>,
) {
    if let Some(primary) = classify_http_response(status, raw_text) {
        tracing::warn!(
            status,
            class = primary.code(),
            summary = primary.summary(),
            "fetch response classified"
        );
    }

    if classify_thin_content(Some(content_type), body_len, markdown.len(), quality).is_some() {
        tracing::warn!(
            status,
            markdown_len = markdown.len(),
            body_len,
            "fetch response classified as thin content"
        );
    }
}

/// Testable variant: same logic as [`apply_diff`] but uses an explicit store.
pub(crate) fn apply_diff_with_store(
    store: &SnapshotStore,
    url: &str,
    markdown: &str,
) -> (String, bool) {
    let new_snap = ContentSnapshot::new(url, markdown, std::time::SystemTime::now());

    let output = match store.load_latest_snapshot(url) {
        Some(old_snap) if old_snap.content_unchanged(&new_snap) => {
            let _ = store.save_snapshot(url, &new_snap);
            "No changes since last fetch".to_owned()
        }
        Some(old_snap) => {
            let _ = store.save_snapshot(url, &new_snap);
            let diff = compute_diff(&old_snap, &new_snap);
            format!(
                "Changed since last fetch:\n\n{}",
                format_diff_markdown(&diff)
            )
        }
        None => {
            let _ = store.save_snapshot(url, &new_snap);
            format!(
                "First fetch (cached for future diff):\n\n{}",
                truncate_markdown(markdown, 4000)
            )
        }
    };

    let has_diff = !output.starts_with("No changes") && !output.starts_with("First fetch");
    (output, has_diff)
}
