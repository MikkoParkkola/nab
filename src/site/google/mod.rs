//! Google Workspace content extraction (Docs, Sheets, Slides).
//!
//! Exports documents via the Google export API using browser cookies for
//! authentication. Supports:
//! - Google Docs → HTML export (converted to markdown) + OOXML for comments/suggestions
//!   - Note: Multi-tab discovery is not supported — Google renders tab metadata via
//!     JavaScript so it is absent from the initial HTML. The export API's `&tab=t.X`
//!     parameter requires the opaque tab ID which is only available after JS execution.
//!     Sequential IDs (`t.0`, `t.1`) do not exist; PDF/HTML without a tab parameter
//!     exports only the default tab. The single-export path returns the first/default tab.
//! - Google Sheets → XLSX export parsed to extract all sheets with proper names + cell data
//!   - Falls back to CSV export if XLSX parsing fails
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

pub mod ooxml;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;

use super::{SiteContent, SiteMetadata, SiteProvider};
use crate::http_client::AcceleratedClient;
use ooxml::{
    append_xlsx_comments_from_bytes, csv_to_markdown, parse_docx_comments, parse_pptx_comments,
    xlsx_to_all_sheets_markdown,
};


// ─── Document kind ─────────────────────────────────────────────────────────────

/// Document kind as inferred from the URL path segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocKind {
    Doc,
    Sheet,
    Slide,
}

impl DocKind {
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
fn parse_google_url(url: &str) -> Option<GoogleDocUrl> {
    let lower = url.to_lowercase();
    let base = lower.split('?').next().unwrap_or(&lower);

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

    let after_d = url.split(segment).nth(1)?;
    let id = after_d
        .split('/')
        .next()
        .filter(|s| !s.is_empty())?
        .to_string();

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
        let cookie_header = match cookies {
            Some(c) if !c.is_empty() => c,
            _ => bail!(
                "Google Workspace provider requires browser cookies. \
                 Use --cookies brave (or chrome/firefox/safari)."
            ),
        };

        let parsed = parse_google_url(url).context("Failed to parse Google Workspace URL")?;

        match parsed.kind {
            DocKind::Doc => extract_doc(&parsed.id, url, cookie_header).await,
            DocKind::Sheet => extract_sheet(&parsed.id, url, cookie_header).await,
            DocKind::Slide => extract_slide(&parsed.id, url, cookie_header).await,
        }
    }
}

// ─── HTTP helper ──────────────────────────────────────────────────────────────

/// Fetch a Google export URL and return the raw bytes.
async fn fetch_export(export_url: &str, cookie_header: &str) -> Result<bytes::Bytes> {
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

async fn extract_doc(id: &str, canonical_url: &str, cookie_header: &str) -> Result<SiteContent> {
    let (markdown_content, title) = export_doc_single(id, cookie_header).await?;
    let mut markdown = markdown_content;
    append_doc_comments(id, cookie_header, &mut markdown).await;

    Ok(SiteContent {
        markdown,
        metadata: SiteMetadata {
            author: None,
            title,
            published: None,
            platform: DocKind::Doc.platform_label().to_string(),
            canonical_url: canonical_url.to_string(),
            media_urls: vec![],
            engagement: None,
        },
    })
}

/// Export a single-tab Google Doc as HTML markdown. Returns `(markdown, title)`.
async fn export_doc_single(id: &str, cookie_header: &str) -> Result<(String, Option<String>)> {
    let html_url = format!("https://docs.google.com/document/d/{id}/export?format=html");
    let html_bytes = fetch_export(&html_url, cookie_header).await?;
    let html = String::from_utf8_lossy(&html_bytes);
    let title = extract_html_title(&html);

    let content_router = crate::content::ContentRouter::new();
    let converted = content_router
        .convert(html_bytes.as_ref(), "text/html")
        .context("Failed to convert Google Doc HTML to markdown")?;

    Ok((converted.markdown, title))
}

/// Fetch `.docx` and append parsed comments/suggestions to `markdown`.
async fn append_doc_comments(id: &str, cookie_header: &str, markdown: &mut String) {
    let docx_url = format!("https://docs.google.com/document/d/{id}/export?format=docx");
    match fetch_export(&docx_url, cookie_header).await {
        Ok(docx_bytes) => match parse_docx_comments(&docx_bytes) {
            Ok(annotations) if !annotations.is_empty() => {
                markdown.push_str("\n\n---\n\n## Comments & Suggestions\n\n");
                for annotation in &annotations {
                    markdown.push_str(annotation);
                    markdown.push('\n');
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("Failed to parse .docx comments: {e}"),
        },
        Err(e) => tracing::debug!("Skipping .docx comments: {e}"),
    }
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
    let xlsx_url = format!("https://docs.google.com/spreadsheets/d/{id}/export?format=xlsx");
    let xlsx_bytes = fetch_export(&xlsx_url, cookie_header).await?;

    let markdown_content = match xlsx_to_all_sheets_markdown(&xlsx_bytes) {
        Ok(md) if !md.is_empty() => md,
        Ok(_) | Err(_) => {
            tracing::debug!("xlsx parsing produced no content, falling back to CSV");
            let csv_url =
                format!("https://docs.google.com/spreadsheets/d/{id}/export?format=csv");
            let csv_bytes = fetch_export(&csv_url, cookie_header).await?;
            csv_to_markdown(&String::from_utf8_lossy(&csv_bytes))
        }
    };

    let mut markdown = markdown_content;
    append_xlsx_comments_from_bytes(&xlsx_bytes, &mut markdown);

    Ok(SiteContent {
        markdown,
        metadata: SiteMetadata {
            author: None,
            title: None,
            published: None,
            platform: DocKind::Sheet.platform_label().to_string(),
            canonical_url: canonical_url.to_string(),
            media_urls: vec![],
            engagement: None,
        },
    })
}

// ─── Google Slides ────────────────────────────────────────────────────────────

async fn extract_slide(
    id: &str,
    canonical_url: &str,
    cookie_header: &str,
) -> Result<SiteContent> {
    let txt_url = format!("https://docs.google.com/presentation/d/{id}/export?format=txt");
    let txt_bytes = fetch_export(&txt_url, cookie_header).await?;
    let slide_text = String::from_utf8_lossy(&txt_bytes).into_owned();

    let mut markdown = format!("## Presentation Notes\n\n{slide_text}");

    let pptx_url = format!("https://docs.google.com/presentation/d/{id}/export?format=pptx");
    match fetch_export(&pptx_url, cookie_header).await {
        Ok(pptx_bytes) => match parse_pptx_comments(&pptx_bytes) {
            Ok(comments) if !comments.is_empty() => {
                markdown.push_str("\n\n---\n\n## Comments\n\n");
                for comment in &comments {
                    markdown.push_str(comment);
                    markdown.push('\n');
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("Failed to parse .pptx comments: {e}"),
        },
        Err(e) => tracing::debug!("Skipping .pptx comments: {e}"),
    }

    Ok(SiteContent {
        markdown,
        metadata: SiteMetadata {
            author: None,
            title: None,
            published: None,
            platform: DocKind::Slide.platform_label().to_string(),
            canonical_url: canonical_url.to_string(),
            media_urls: vec![],
            engagement: None,
        },
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
        let parsed =
            parse_google_url("https://docs.google.com/document/d/DOCID123/export?format=html")
                .unwrap();
        assert_eq!(parsed.id, "DOCID123");
    }

    #[test]
    fn parse_google_url_returns_none_for_non_workspace_urls() {
        assert!(parse_google_url("https://drive.google.com/file/d/1ABC").is_none());
        assert!(parse_google_url("https://google.com/document/d/1ABC").is_none());
    }

    #[test]
    fn doc_kind_platform_labels_are_correct() {
        assert_eq!(DocKind::Doc.platform_label(), "Google Docs");
        assert_eq!(DocKind::Sheet.platform_label(), "Google Sheets");
        assert_eq!(DocKind::Slide.platform_label(), "Google Slides");
    }

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
}
