use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use scraper::{Html, Selector};

static LINK_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("a[href]").expect("static link selector"));

pub(crate) fn write_stdout(content: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(content.as_bytes())
        .context("Failed to write to stdout")?;
    stdout.flush().context("Failed to flush stdout")?;
    Ok(())
}

pub(crate) fn write_stdout_line(content: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(content.as_bytes())
        .context("Failed to write to stdout")?;
    stdout
        .write_all(b"\n")
        .context("Failed to write newline to stdout")?;
    stdout.flush().context("Failed to flush stdout")?;
    Ok(())
}

pub fn output_body(
    body: &str,
    output_file: Option<&Path>,
    links: bool,
    max_body: usize,
) -> Result<()> {
    // Save to file if requested (always full, no truncation)
    if let Some(path) = output_file {
        let mut file = File::create(path)?;
        // Body is already converted (via ContentRouter) when markdown mode is active
        file.write_all(body.as_bytes())?;
        write_stdout_line(&format!(
            "💾 Saved {} bytes to {}",
            body.len(),
            path.display()
        ))?;
        return Ok(());
    }

    // Extract links if requested
    if links {
        let extracted = extract_links(body);
        for (text, href) in &extracted {
            if text.is_empty() {
                write_stdout_line(href)?;
            } else {
                write_stdout_line(&format!("[{}]({href})", truncate_text(text, 50)))?;
            }
        }
        write_stdout_line(&format!("\n({} links)", extracted.len()))?;
        return Ok(());
    }

    // Display with optional truncation (UTF-8 safe via floor_char_boundary)
    if max_body > 0 && body.len() > max_body {
        let at = body.floor_char_boundary(max_body);
        write_stdout("\n")?;
        write_stdout(&body[..at])?;
        write_stdout_line("")?;
        write_stdout_line(&format!("\n... [{} more bytes]", body.len() - at))?;
    } else {
        write_stdout("\n")?;
        write_stdout(body)?;
        write_stdout_line("")?;
    }

    Ok(())
}

pub fn extract_links(html: &str) -> Vec<(String, String)> {
    let document = Html::parse_document(html);

    let mut links = Vec::new();
    let mut seen = HashSet::new();

    for element in document.select(&LINK_SELECTOR) {
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

pub fn truncate_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

/// Print response headers and markdown body for form-submission results.
pub async fn output_response(response: reqwest::Response, show_headers: bool) -> Result<()> {
    if show_headers {
        write_stdout_line("\nResponse Headers:")?;
        for (key, value) in response.headers() {
            write_stdout_line(&format!(
                "  {}: {}",
                key,
                value.to_str().unwrap_or("<binary>")
            ))?;
        }
    }

    let body_text = response.text().await?;
    let router = nab::content::ContentRouter::new();
    let markdown = router.convert(body_text.as_bytes(), "text/html")?.markdown;
    output_body(&markdown, None, false, 0)?;

    Ok(())
}

/// Write a response body to `path` unchanged.
pub(crate) fn write_file_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("Failed to write {}", path.display()))
}

/// Whether an `-o` path asks for text or for the file itself.
///
/// The final extension states the intent: `notes.md` wants the page's text,
/// `logo.png` wants the image. Names without an extension keep the text
/// behaviour nab always had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Destination {
    /// Converted (or raw) text, exactly as before.
    Text,
    /// The response body, byte for byte.
    File,
}

/// Classify an `-o` path by its final extension (ASCII case-insensitive).
pub(crate) fn destination_for(path: &Path) -> Destination {
    const TEXT_EXTENSIONS: &[&str] =
        &["md", "markdown", "txt", "html", "htm", "json", "xml", "csv"];
    match path.extension().and_then(|ext| ext.to_str()) {
        None => Destination::Text,
        Some(ext) if TEXT_EXTENSIONS.iter().any(|t| ext.eq_ignore_ascii_case(t)) => {
            Destination::Text
        }
        Some(_) => Destination::File,
    }
}

/// Whether a response body is text or a binary file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyClass {
    /// HTML, plain text, JSON, XML, JavaScript and similar.
    Text,
    /// Everything that is not text: images (including SVG), PDF, audio, video, archives.
    Binary,
}

/// Classify a response body from its `Content-Type` and, when the type is
/// missing or generic, from the bytes themselves.
pub(crate) fn body_class(content_type: &str, body: &[u8]) -> BodyClass {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let is_text_mime = mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/xhtml+xml"
                | "application/json"
                | "application/xml"
                | "application/javascript"
        )
        || ((mime.ends_with("+json") || mime.ends_with("+xml")) && mime != "image/svg+xml");
    if is_text_mime {
        return BodyClass::Text;
    }
    if mime.is_empty() || mime == "application/octet-stream" {
        return if looks_like_text(body) {
            BodyClass::Text
        } else {
            BodyClass::Binary
        };
    }
    BodyClass::Binary
}

/// Text when the body is valid UTF-8, or starts with `<` after an optional
/// UTF-8 byte-order mark and ASCII whitespace (HTML in another encoding).
fn looks_like_text(body: &[u8]) -> bool {
    let without_bom = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
    let first = without_bom.iter().find(|byte| !byte.is_ascii_whitespace());
    first == Some(&b'<') || std::str::from_utf8(body).is_ok()
}

#[cfg(test)]
mod file_output_tests {
    use super::{BodyClass, Destination, body_class, destination_for};
    use std::path::Path;

    #[test]
    fn text_extensions_and_bare_names_are_text_destinations() {
        for name in [
            "notes.md",
            "NOTES.MD",
            "a.markdown",
            "a.txt",
            "page.html",
            "page.HTM",
            "data.json",
            "feed.xml",
            "rows.csv",
            "out",
            "dir/out",
        ] {
            assert_eq!(
                destination_for(Path::new(name)),
                Destination::Text,
                "{name}"
            );
        }
    }

    #[test]
    fn other_extensions_are_file_destinations() {
        for name in [
            "logo.png",
            "paper.PDF",
            "clip.mp4",
            "tool.cmd",
            "archive.tar.gz",
            "x.svg",
        ] {
            assert_eq!(
                destination_for(Path::new(name)),
                Destination::File,
                "{name}"
            );
        }
    }

    #[test]
    fn textual_content_types_are_text() {
        for ct in [
            "text/html; charset=utf-8",
            "application/xhtml+xml",
            "text/plain",
            "application/json",
            "application/ld+json",
            "application/rss+xml",
            "text/xml",
            "application/javascript",
        ] {
            assert_eq!(body_class(ct, b"\x89\xff"), BodyClass::Text, "{ct}");
        }
    }

    #[test]
    fn file_content_types_are_binary() {
        for ct in [
            "image/png",
            "image/svg+xml",
            "application/pdf",
            "audio/mpeg",
            "video/mp4",
            "application/zip",
        ] {
            assert_eq!(body_class(ct, b"plain words"), BodyClass::Binary, "{ct}");
        }
    }

    #[test]
    fn generic_type_is_sniffed_from_the_bytes() {
        let generic = "application/octet-stream";
        assert_eq!(
            body_class(generic, b"\x89PNG\r\n\x1a\n\xff\x00"),
            BodyClass::Binary
        );
        assert_eq!(
            body_class(generic, b"\xef\xbb\xbf  <!doctype html>"),
            BodyClass::Text
        );
        assert_eq!(
            body_class(generic, b"just some utf-8 text"),
            BodyClass::Text
        );
        assert_eq!(body_class("", b"\x00\xff\xfe"), BodyClass::Binary);
    }
}
