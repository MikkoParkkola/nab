use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};
use scraper::{Html, Selector};

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
    let selector = Selector::parse("a[href]").unwrap();

    let mut links = Vec::new();
    let mut seen = HashSet::new();

    for element in document.select(&selector) {
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
