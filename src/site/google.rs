//! Google Workspace content extraction (Docs, Sheets, Slides).
//!
//! Exports documents via the Google export API using browser cookies for
//! authentication. Supports:
//! - Google Docs → HTML export (converted to markdown) + OOXML for comments/suggestions
//! - Google Sheets → CSV export (formatted as markdown table) + OOXML for comments
//! - Google Slides → plain-text export + OOXML for comments
//!
//! Requires browser cookies (`--cookies brave` etc.). Without cookies the export
//! endpoint returns an HTTP 302 redirect to Google's login page, and the provider
//! falls through to the normal HTML fetch path.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, google::GoogleWorkspaceProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = GoogleWorkspaceProvider;
//!
//! let content = provider.extract(
//!     "https://docs.google.com/document/d/DOCID/edit",
//!     &client,
//!     Some("SID=abc; HSID=def"),
//! ).await?;
//!
//! println!("{}", content.markdown);
//! # Ok(())
//! # }
//! ```

use std::io::{Cursor, Read};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use super::{SiteContent, SiteMetadata, SiteProvider};
use crate::http_client::AcceleratedClient;

// ─── OOXML Namespace URIs ──────────────────────────────────────────────────────

/// `WordprocessingML` namespace (used in `.docx` files).
const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

// ─── URL Patterns ─────────────────────────────────────────────────────────────

/// Document kind as inferred from the URL path segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocKind {
    Doc,
    Sheet,
    Slide,
}

impl DocKind {
    /// Human-readable platform label.
    fn platform_label(self) -> &'static str {
        match self {
            Self::Doc => "Google Docs",
            Self::Sheet => "Google Sheets",
            Self::Slide => "Google Slides",
        }
    }
}

/// Parsed components from a Google Workspace URL.
#[derive(Debug)]
struct GoogleDocUrl {
    id: String,
    kind: DocKind,
}

/// Parse a Google Workspace URL into its components.
///
/// Handles the three document kinds:
/// - `docs.google.com/document/d/{ID}/...`
/// - `docs.google.com/spreadsheets/d/{ID}/...`
/// - `docs.google.com/presentation/d/{ID}/...`
fn parse_google_url(url: &str) -> Option<GoogleDocUrl> {
    let lower = url.to_lowercase();
    let base = lower.split('?').next().unwrap_or(&lower);

    // Must be on docs.google.com to avoid false matches on other domains
    // that happen to contain the same path segments.
    if !base.contains("docs.google.com/") {
        return None;
    }

    let (kind, segment) = if base.contains("/document/d/") {
        (DocKind::Doc, "/document/d/")
    } else if base.contains("/spreadsheets/d/") {
        (DocKind::Sheet, "/spreadsheets/d/")
    } else if base.contains("/presentation/d/") {
        (DocKind::Slide, "/presentation/d/")
    } else {
        return None;
    };

    // Extract the ID: the path segment after `/d/`
    let after_d = url.split(segment).nth(1)?;
    let id = after_d.split('/').next().filter(|s| !s.is_empty())?.to_string();

    Some(GoogleDocUrl { id, kind })
}

// ─── Provider ─────────────────────────────────────────────────────────────────

/// Google Workspace content provider (Docs, Sheets, Slides).
pub struct GoogleWorkspaceProvider;

#[async_trait]
impl SiteProvider for GoogleWorkspaceProvider {
    fn name(&self) -> &'static str {
        "google-workspace"
    }

    fn matches(&self, url: &str) -> bool {
        let lower = url.to_lowercase();
        lower.contains("docs.google.com/")
            && (lower.contains("/document/d/")
                || lower.contains("/spreadsheets/d/")
                || lower.contains("/presentation/d/"))
    }

    async fn extract(
        &self,
        url: &str,
        _client: &AcceleratedClient,
        cookies: Option<&str>,
    ) -> Result<SiteContent> {
        // Require cookies — without them Google redirects to the login page.
        let cookie_header = match cookies {
            Some(c) if !c.is_empty() => c,
            _ => bail!(
                "Google Workspace provider requires browser cookies. \
                 Use --cookies brave (or chrome/firefox/safari)."
            ),
        };

        let parsed = parse_google_url(url)
            .context("Failed to parse Google Workspace URL")?;

        match parsed.kind {
            DocKind::Doc => extract_doc(&parsed.id, url, cookie_header).await,
            DocKind::Sheet => extract_sheet(&parsed.id, url, cookie_header).await,
            DocKind::Slide => extract_slide(&parsed.id, url, cookie_header).await,
        }
    }
}

// ─── HTTP helper ──────────────────────────────────────────────────────────────

/// Fetch a Google export URL and return the raw bytes.
///
/// Builds a dedicated client without `http2_prior_knowledge` to allow TLS ALPN
/// negotiation — required by Google's export endpoints.
///
/// Google export endpoints return 307 redirects to `googleusercontent.com` for
/// the actual file download. We follow these redirects but detect login redirects
/// (to `accounts.google.com`) which indicate expired or invalid cookies.
async fn fetch_export(export_url: &str, cookie_header: &str) -> Result<bytes::Bytes> {
    // Custom redirect policy: follow redirects to Google CDN
    // (`googleusercontent.com`) but reject redirects to login pages.
    let google_client = reqwest::Client::builder()
        .use_rustls_tls()
        .gzip(true)
        .brotli(true)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url().to_string();
            if url.contains("accounts.google.com") || url.contains("/signin") {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .context("Failed to build Google export client")?;

    let response = google_client
        .get(export_url)
        .header("Cookie", cookie_header)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
             AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        )
        .send()
        .await
        .context("Failed to fetch Google export URL")?;

    let status = response.status();
    // After following redirects, a 3xx means we hit the login wall.
    if status.is_redirection() {
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown");
        bail!(
            "Google export redirected to login at {location} (HTTP {status}). \
             Check that cookies are valid and not expired."
        );
    }
    if !status.is_success() {
        bail!("Google export returned HTTP {status} for {export_url}");
    }

    response
        .bytes()
        .await
        .context("Failed to read Google export response body")
}

// ─── Google Docs ──────────────────────────────────────────────────────────────

async fn extract_doc(
    id: &str,
    canonical_url: &str,
    cookie_header: &str,
) -> Result<SiteContent> {
    // Fetch HTML export for main content
    let html_url = format!(
        "https://docs.google.com/document/d/{id}/export?format=html"
    );
    let html_bytes = fetch_export(&html_url, cookie_header).await?;
    let html = String::from_utf8_lossy(&html_bytes);

    // Convert HTML to markdown using nab's ContentRouter
    let content_router = crate::content::ContentRouter::new();
    let converted = content_router
        .convert(html_bytes.as_ref(), "text/html")
        .context("Failed to convert Google Doc HTML to markdown")?;
    let mut markdown = converted.markdown;

    // Fetch OOXML for comments and suggested edits
    let docx_url = format!(
        "https://docs.google.com/document/d/{id}/export?format=docx"
    );
    match fetch_export(&docx_url, cookie_header).await {
        Ok(docx_bytes) => {
            match parse_docx_comments(&docx_bytes) {
                Ok(annotations) if !annotations.is_empty() => {
                    markdown.push_str("\n\n---\n\n## Comments & Suggestions\n\n");
                    for annotation in &annotations {
                        markdown.push_str(annotation);
                        markdown.push('\n');
                    }
                }
                Ok(_) => {} // no comments found
                Err(e) => {
                    tracing::warn!("Failed to parse .docx comments: {e}");
                }
            }
        }
        Err(e) => {
            tracing::debug!("Skipping .docx comments: {e}");
        }
    }

    // Extract title from HTML <title> tag
    let title = extract_html_title(&html);

    let metadata = SiteMetadata {
        author: None,
        title: title.clone(),
        published: None,
        platform: DocKind::Doc.platform_label().to_string(),
        canonical_url: canonical_url.to_string(),
        media_urls: vec![],
        engagement: None,
    };

    Ok(SiteContent { markdown, metadata })
}

/// Extract `<title>` content from HTML.
fn extract_html_title(html: &str) -> Option<String> {
    let start = html.find("<title")?;
    let open_end = html[start..].find('>')? + start + 1;
    let close = html[open_end..].find("</title>")? + open_end;
    let raw = html[open_end..close].trim().to_string();
    if raw.is_empty() { None } else { Some(raw) }
}

// ─── Google Sheets ────────────────────────────────────────────────────────────

async fn extract_sheet(
    id: &str,
    canonical_url: &str,
    cookie_header: &str,
) -> Result<SiteContent> {
    // Fetch CSV for the first (default) sheet
    let csv_url = format!(
        "https://docs.google.com/spreadsheets/d/{id}/export?format=csv"
    );
    let csv_bytes = fetch_export(&csv_url, cookie_header).await?;
    let csv_text = String::from_utf8_lossy(&csv_bytes).into_owned();

    let mut markdown = csv_to_markdown(&csv_text);

    // Fetch OOXML for comments
    let xlsx_url = format!(
        "https://docs.google.com/spreadsheets/d/{id}/export?format=xlsx"
    );
    match fetch_export(&xlsx_url, cookie_header).await {
        Ok(xlsx_bytes) => {
            match parse_xlsx_comments(&xlsx_bytes) {
                Ok(comments) if !comments.is_empty() => {
                    markdown.push_str("\n\n---\n\n## Comments\n\n");
                    for comment in &comments {
                        markdown.push_str(comment);
                        markdown.push('\n');
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Failed to parse .xlsx comments: {e}");
                }
            }
        }
        Err(e) => {
            tracing::debug!("Skipping .xlsx comments: {e}");
        }
    }

    let metadata = SiteMetadata {
        author: None,
        title: None,
        published: None,
        platform: DocKind::Sheet.platform_label().to_string(),
        canonical_url: canonical_url.to_string(),
        media_urls: vec![],
        engagement: None,
    };

    Ok(SiteContent { markdown, metadata })
}

/// Convert CSV text to a markdown table.
///
/// Produces a GFM-style pipe table. The first row is treated as headers.
fn csv_to_markdown(csv: &str) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();

    for line in csv.lines() {
        let cells = split_csv_line(line);
        rows.push(cells);
    }

    if rows.is_empty() {
        return String::new();
    }

    let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if col_count == 0 {
        return String::new();
    }

    let mut md = String::new();

    // Header row
    let header = &rows[0];
    md.push('|');
    for i in 0..col_count {
        let cell = header.get(i).map_or("", String::as_str);
        md.push(' ');
        md.push_str(&cell.replace('|', "\\|"));
        md.push_str(" |");
    }
    md.push('\n');

    // Separator
    md.push('|');
    for _ in 0..col_count {
        md.push_str(" --- |");
    }
    md.push('\n');

    // Data rows
    for row in rows.iter().skip(1) {
        md.push('|');
        for i in 0..col_count {
            let cell = row.get(i).map_or("", String::as_str);
            md.push(' ');
            md.push_str(&cell.replace('|', "\\|"));
            md.push_str(" |");
        }
        md.push('\n');
    }

    md
}

/// Split a single CSV line into cells, respecting double-quoted fields.
fn split_csv_line(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    // Escaped double-quote
                    chars.next();
                    current.push('"');
                } else {
                    in_quotes = false;
                }
            }
            '"' => {
                in_quotes = true;
            }
            ',' if !in_quotes => {
                cells.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    cells.push(current.trim().to_string());
    cells
}

// ─── Google Slides ────────────────────────────────────────────────────────────

async fn extract_slide(
    id: &str,
    canonical_url: &str,
    cookie_header: &str,
) -> Result<SiteContent> {
    let txt_url = format!(
        "https://docs.google.com/presentation/d/{id}/export?format=txt"
    );
    let txt_bytes = fetch_export(&txt_url, cookie_header).await?;
    let slide_text = String::from_utf8_lossy(&txt_bytes).into_owned();

    let mut markdown = format!("## Presentation Notes\n\n{slide_text}");

    // Fetch OOXML for comments
    let pptx_url = format!(
        "https://docs.google.com/presentation/d/{id}/export?format=pptx"
    );
    match fetch_export(&pptx_url, cookie_header).await {
        Ok(pptx_bytes) => {
            match parse_pptx_comments(&pptx_bytes) {
                Ok(comments) if !comments.is_empty() => {
                    markdown.push_str("\n\n---\n\n## Comments\n\n");
                    for comment in &comments {
                        markdown.push_str(comment);
                        markdown.push('\n');
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Failed to parse .pptx comments: {e}");
                }
            }
        }
        Err(e) => {
            tracing::debug!("Skipping .pptx comments: {e}");
        }
    }

    let metadata = SiteMetadata {
        author: None,
        title: None,
        published: None,
        platform: DocKind::Slide.platform_label().to_string(),
        canonical_url: canonical_url.to_string(),
        media_urls: vec![],
        engagement: None,
    };

    Ok(SiteContent { markdown, metadata })
}

// ─── OOXML Parsing ────────────────────────────────────────────────────────────

/// Open a ZIP archive from raw bytes and return an in-memory [`zip::ZipArchive`].
fn open_zip(bytes: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>> {
    zip::ZipArchive::new(Cursor::new(bytes)).context("Failed to open ZIP/OOXML archive")
}

/// Read a named entry from a ZIP archive as a UTF-8 string.
fn read_zip_entry(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<String> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("ZIP entry '{name}' not found"))?;
    let mut buf = String::new();
    entry
        .read_to_string(&mut buf)
        .with_context(|| format!("Failed to read ZIP entry '{name}'"))?;
    Ok(buf)
}

// ─── .docx comment & suggestion parsing ──────────────────────────────────────

/// Extract comments and suggested edits from a `.docx` file.
///
/// Returns formatted annotation strings like:
/// - `"💬 Author (date): "text""`
/// - `"✏️ suggestion by Author: delete "old" → insert "new""`
pub(crate) fn parse_docx_comments(bytes: &[u8]) -> Result<Vec<String>> {
    let mut archive = open_zip(bytes)?;

    let mut results = Vec::new();

    // Parse comments from word/comments.xml
    if let Ok(xml) = read_zip_entry(&mut archive, "word/comments.xml") {
        results.extend(parse_docx_comment_xml(&xml));
    }

    // Parse suggested edits from word/document.xml
    if let Ok(xml) = read_zip_entry(&mut archive, "word/document.xml") {
        results.extend(parse_docx_suggestions(&xml));
    }

    Ok(results)
}

/// Parse `word/comments.xml` and return formatted comment strings.
fn parse_docx_comment_xml(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    doc.descendants()
        .filter(|n| n.has_tag_name("comment"))
        .filter_map(|comment| {
            // In roxmltree, namespace-prefixed attributes are accessed via (namespace_uri, local_name).
            let author = comment
                .attribute((W_NS, "author"))
                .or_else(|| comment.attribute("author"))
                .unwrap_or("Unknown");
            let date = comment
                .attribute((W_NS, "date"))
                .or_else(|| comment.attribute("date"))
                .unwrap_or("");
            let date_short = date.get(..10).unwrap_or(date); // yyyy-mm-dd

            let text = collect_text_nodes(&comment);
            if text.is_empty() {
                return None;
            }

            Some(format!("💬 **{author}** ({date_short}): \"{text}\""))
        })
        .collect()
}

/// Collect all `w:t` text node contents under a node, joined with spaces.
fn collect_text_nodes(node: &roxmltree::Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.has_tag_name("t"))
        .filter_map(|n| n.text())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Parse `word/document.xml` for `<w:ins>` and `<w:del>` tracked changes.
fn parse_docx_suggestions(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    let mut results = Vec::new();

    for node in doc.descendants() {
        if node.has_tag_name("ins") {
            let author = node
                .attribute((W_NS, "author"))
                .or_else(|| node.attribute("author"))
                .unwrap_or("Unknown");
            let inserted = collect_text_nodes(&node);
            if !inserted.is_empty() {
                results.push(format!(
                    "✏️ suggestion by **{author}**: insert \"{inserted}\""
                ));
            }
        } else if node.has_tag_name("del") {
            let author = node
                .attribute((W_NS, "author"))
                .or_else(|| node.attribute("author"))
                .unwrap_or("Unknown");
            let deleted = collect_del_text(&node);
            if !deleted.is_empty() {
                results.push(format!(
                    "✏️ suggestion by **{author}**: delete \"{deleted}\""
                ));
            }
        }
    }

    results
}

/// Collect `w:delText` nodes (deleted text in tracked changes).
fn collect_del_text(node: &roxmltree::Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.has_tag_name("delText"))
        .filter_map(|n| n.text())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

// ─── .xlsx comment parsing ────────────────────────────────────────────────────

/// Extract comments from a `.xlsx` file.
///
/// Checks both modern (`xl/threadedComments/`) and legacy (`xl/comments*.xml`) formats.
pub(crate) fn parse_xlsx_comments(bytes: &[u8]) -> Result<Vec<String>> {
    let mut archive = open_zip(bytes)?;

    let names: Vec<String> = archive.file_names().map(String::from).collect();

    let mut results = Vec::new();

    // Modern threaded comments
    let threaded: Vec<String> = names
        .iter()
        .filter(|n| n.starts_with("xl/threadedComments/"))
        .cloned()
        .collect();

    for name in &threaded {
        let xml = read_zip_entry(&mut archive, name)?;
        results.extend(parse_xlsx_threaded_xml(&xml));
    }

    // Legacy comments (xl/comments1.xml etc.)
    if results.is_empty() {
        let legacy: Vec<String> = names
            .iter()
            .filter(|n| {
                n.starts_with("xl/comments")
                    && std::path::Path::new(n.as_str())
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
            })
            .cloned()
            .collect();

        for name in &legacy {
            let xml = read_zip_entry(&mut archive, name)?;
            results.extend(parse_xlsx_legacy_xml(&xml));
        }
    }

    Ok(results)
}

/// Parse `xl/threadedComments/threadedComment*.xml`.
fn parse_xlsx_threaded_xml(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    doc.descendants()
        .filter(|n| n.has_tag_name("threadedComment"))
        .filter_map(|comment| {
            let text = comment
                .descendants()
                .find(|n| n.has_tag_name("text"))
                .and_then(|n| n.text())
                .unwrap_or("");

            if text.is_empty() {
                return None;
            }

            let ref_cell = comment.attribute("ref").unwrap_or("");
            let author_id = comment.attribute("personId").unwrap_or("");

            Some(format!("💬 [{ref_cell}] (author={author_id}): \"{text}\""))
        })
        .collect()
}

/// Parse legacy `xl/comments*.xml` format.
fn parse_xlsx_legacy_xml(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    doc.descendants()
        .filter(|n| n.has_tag_name("comment"))
        .filter_map(|comment| {
            let ref_cell = comment.attribute("ref").unwrap_or("");
            let author = comment.attribute("authorId").unwrap_or("0");

            let text: String = comment
                .descendants()
                .filter(|n| n.has_tag_name("t"))
                .filter_map(|n| n.text())
                .collect::<Vec<_>>()
                .join(" ");

            if text.is_empty() {
                return None;
            }

            Some(format!("💬 [{ref_cell}] (author={author}): \"{text}\""))
        })
        .collect()
}

// ─── .pptx comment parsing ───────────────────────────────────────────────────

/// Extract comments from a `.pptx` file.
pub(crate) fn parse_pptx_comments(bytes: &[u8]) -> Result<Vec<String>> {
    let mut archive = open_zip(bytes)?;

    let names: Vec<String> = archive.file_names().map(String::from).collect();

    let comment_files: Vec<String> = names
        .iter()
        .filter(|n| {
            n.starts_with("ppt/comments/")
                && std::path::Path::new(n.as_str())
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
        })
        .cloned()
        .collect();

    let mut results = Vec::new();

    for name in &comment_files {
        let xml = read_zip_entry(&mut archive, name)?;
        results.extend(parse_pptx_comment_xml(&xml));
    }

    Ok(results)
}

/// Parse a `ppt/comments/comment*.xml` file.
fn parse_pptx_comment_xml(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    doc.descendants()
        .filter(|n| n.has_tag_name("cm") || n.has_tag_name("comment"))
        .filter_map(|comment| {
            let author = comment.attribute("authorId").unwrap_or("Unknown");
            let created = comment.attribute("created").unwrap_or("");
            let date_short = created.get(..10).unwrap_or(created);

            let text: String = comment
                .descendants()
                .filter(|n| n.has_tag_name("t"))
                .filter_map(|n| n.text())
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string();

            if text.is_empty() {
                return None;
            }

            Some(format!("💬 **{author}** ({date_short}): \"{text}\""))
        })
        .collect()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── URL matching ──────────────────────────────────────────────────────────

    #[test]
    fn matches_google_docs_urls() {
        let p = GoogleWorkspaceProvider;
        assert!(p.matches("https://docs.google.com/document/d/1ABC123/edit"));
        assert!(p.matches("https://docs.google.com/document/d/1ABC123/view"));
        assert!(p.matches("https://DOCS.GOOGLE.COM/document/d/1ABC123/edit"));
    }

    #[test]
    fn matches_google_sheets_urls() {
        let p = GoogleWorkspaceProvider;
        assert!(p.matches("https://docs.google.com/spreadsheets/d/1XYZ/edit#gid=0"));
        assert!(p.matches("https://docs.google.com/spreadsheets/d/1XYZ/view"));
    }

    #[test]
    fn matches_google_slides_urls() {
        let p = GoogleWorkspaceProvider;
        assert!(p.matches("https://docs.google.com/presentation/d/1PQR/edit"));
        assert!(p.matches("https://docs.google.com/presentation/d/1PQR/present"));
    }

    #[test]
    fn does_not_match_non_google_docs_urls() {
        let p = GoogleWorkspaceProvider;
        assert!(!p.matches("https://google.com/document/d/1ABC"));
        assert!(!p.matches("https://drive.google.com/file/d/1ABC"));
        assert!(!p.matches("https://docs.google.com/forms/d/1ABC"));
        assert!(!p.matches("https://example.com/document/d/1ABC"));
    }

    // ── URL parsing ───────────────────────────────────────────────────────────

    #[test]
    fn parse_google_doc_url_extracts_id_and_kind() {
        let parsed = parse_google_url(
            "https://docs.google.com/document/d/1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms/edit",
        )
        .unwrap();
        assert_eq!(parsed.id, "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms");
        assert_eq!(parsed.kind, DocKind::Doc);
    }

    #[test]
    fn parse_google_sheet_url_extracts_id_and_kind() {
        let parsed =
            parse_google_url("https://docs.google.com/spreadsheets/d/1abc_XYZ/edit#gid=0")
                .unwrap();
        assert_eq!(parsed.id, "1abc_XYZ");
        assert_eq!(parsed.kind, DocKind::Sheet);
    }

    #[test]
    fn parse_google_slide_url_extracts_id_and_kind() {
        let parsed =
            parse_google_url("https://docs.google.com/presentation/d/1pptID/present").unwrap();
        assert_eq!(parsed.id, "1pptID");
        assert_eq!(parsed.kind, DocKind::Slide);
    }

    #[test]
    fn parse_google_url_strips_query_params_from_id() {
        // ID should not include query parameters
        let parsed = parse_google_url(
            "https://docs.google.com/document/d/DOCID123/export?format=html",
        )
        .unwrap();
        assert_eq!(parsed.id, "DOCID123");
    }

    #[test]
    fn parse_google_url_returns_none_for_non_workspace_urls() {
        assert!(parse_google_url("https://drive.google.com/file/d/1ABC").is_none());
        assert!(parse_google_url("https://google.com/document/d/1ABC").is_none());
    }

    // ── DocKind labels ────────────────────────────────────────────────────────

    #[test]
    fn doc_kind_platform_labels_are_correct() {
        assert_eq!(DocKind::Doc.platform_label(), "Google Docs");
        assert_eq!(DocKind::Sheet.platform_label(), "Google Sheets");
        assert_eq!(DocKind::Slide.platform_label(), "Google Slides");
    }

    // ── CSV → Markdown ────────────────────────────────────────────────────────

    #[test]
    fn csv_to_markdown_produces_gfm_pipe_table() {
        let csv = "Name,Age,City\nAlice,30,Helsinki\nBob,25,Tampere";
        let md = csv_to_markdown(csv);
        assert!(md.contains("| Name | Age | City |"), "header row missing");
        assert!(md.contains("| --- | --- | --- |"), "separator missing");
        assert!(md.contains("| Alice | 30 | Helsinki |"), "data row missing");
        assert!(md.contains("| Bob | 25 | Tampere |"), "data row missing");
    }

    #[test]
    fn csv_to_markdown_handles_quoted_fields_with_commas() {
        let csv = "Name,Notes\nAlice,\"Hello, world\"";
        let md = csv_to_markdown(csv);
        assert!(md.contains("Hello, world"), "quoted comma field should be preserved");
    }

    #[test]
    fn csv_to_markdown_escapes_pipe_characters() {
        let csv = "Col1,Col2\nA|B,C";
        let md = csv_to_markdown(csv);
        assert!(md.contains("A\\|B"), "pipe in cell must be escaped");
    }

    #[test]
    fn csv_to_markdown_empty_input_returns_empty() {
        assert_eq!(csv_to_markdown(""), "");
    }

    // ── HTML title extraction ─────────────────────────────────────────────────

    #[test]
    fn extract_html_title_finds_title_tag() {
        let html = "<html><head><title>My Document - Google Docs</title></head></html>";
        assert_eq!(
            extract_html_title(html),
            Some("My Document - Google Docs".to_string())
        );
    }

    #[test]
    fn extract_html_title_returns_none_when_missing() {
        assert!(extract_html_title("<html><body>no title</body></html>").is_none());
    }

    // ── OOXML stubs (using minimal valid ZIP/XML fixtures) ────────────────────

    #[test]
    fn parse_docx_comments_returns_empty_for_no_comments_entry() {
        // A minimal .docx (ZIP) with no word/comments.xml → returns empty, no panic
        let bytes = create_minimal_zip(&[("word/document.xml", "<document/>")]);
        let result = parse_docx_comments(&bytes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_docx_comment_xml_extracts_author_and_text() {
        let xml = r#"<?xml version="1.0"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:comment w:author="Alice" w:date="2025-01-15T10:00:00Z" w:id="1">
    <w:p><w:r><w:t>This needs revision</w:t></w:r></w:p>
  </w:comment>
</w:comments>"#;
        let comments = parse_docx_comment_xml(xml);
        assert_eq!(comments.len(), 1);
        assert!(comments[0].contains("Alice"));
        assert!(comments[0].contains("2025-01-15"));
        assert!(comments[0].contains("This needs revision"));
    }

    #[test]
    fn parse_docx_suggestions_extracts_insertions() {
        let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:ins w:author="Bob" w:date="2025-01-16T09:00:00Z" w:id="2">
      <w:r><w:t>new text</w:t></w:r>
    </w:ins>
  </w:body>
</w:document>"#;
        let suggestions = parse_docx_suggestions(xml);
        assert_eq!(suggestions.len(), 1);
        assert!(suggestions[0].contains("Bob"));
        assert!(suggestions[0].contains("insert"));
        assert!(suggestions[0].contains("new text"));
    }

    #[test]
    fn parse_docx_suggestions_extracts_deletions() {
        let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:del w:author="Carol" w:date="2025-01-17T08:00:00Z" w:id="3">
      <w:r><w:delText>old text</w:delText></w:r>
    </w:del>
  </w:body>
</w:document>"#;
        let suggestions = parse_docx_suggestions(xml);
        assert_eq!(suggestions.len(), 1);
        assert!(suggestions[0].contains("Carol"));
        assert!(suggestions[0].contains("delete"));
        assert!(suggestions[0].contains("old text"));
    }

    #[test]
    fn parse_xlsx_comments_returns_empty_for_minimal_xlsx() {
        let bytes = create_minimal_zip(&[("[Content_Types].xml", "<Types/>")]);
        let result = parse_xlsx_comments(&bytes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_xlsx_legacy_xml_extracts_comments() {
        let xml = r#"<?xml version="1.0"?>
<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <authors><author>Alice</author></authors>
  <commentList>
    <comment ref="B5" authorId="0">
      <text><r><t>Check this value</t></r></text>
    </comment>
  </commentList>
</comments>"#;
        let comments = parse_xlsx_legacy_xml(xml);
        assert_eq!(comments.len(), 1);
        assert!(comments[0].contains("B5"));
        assert!(comments[0].contains("Check this value"));
    }

    #[test]
    fn parse_pptx_comments_returns_empty_for_minimal_pptx() {
        let bytes = create_minimal_zip(&[("[Content_Types].xml", "<Types/>")]);
        let result = parse_pptx_comments(&bytes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_pptx_comment_xml_extracts_comments() {
        let xml = r#"<?xml version="1.0"?>
<cmLst xmlns="http://schemas.openxmlformats.org/presentationml/2006/main">
  <cm authorId="0" created="2025-01-20T12:00:00Z">
    <pos x="1524000" y="1524000"/>
    <text><r><t>Nice slide!</t></r></text>
  </cm>
</cmLst>"#;
        let comments = parse_pptx_comment_xml(xml);
        assert_eq!(comments.len(), 1);
        assert!(comments[0].contains("Nice slide!"));
    }

    // ── split_csv_line ────────────────────────────────────────────────────────

    #[test]
    fn split_csv_line_handles_simple_fields() {
        let cells = split_csv_line("a,b,c");
        assert_eq!(cells, vec!["a", "b", "c"]);
    }

    #[test]
    fn split_csv_line_handles_quoted_fields() {
        let cells = split_csv_line("\"hello, world\",b");
        assert_eq!(cells, vec!["hello, world", "b"]);
    }

    #[test]
    fn split_csv_line_handles_escaped_quotes() {
        let cells = split_csv_line("\"say \"\"hi\"\"\",b");
        assert_eq!(cells, vec!["say \"hi\"", "b"]);
    }

    // ── Helper ────────────────────────────────────────────────────────────────

    /// Create a minimal in-memory ZIP archive for testing.
    fn create_minimal_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write as _;
        let buf = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, content) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
}
