use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use scraper::{Html, Selector};

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
        println!("💾 Saved {} bytes to {}", body.len(), path.display());
        return Ok(());
    }

    // Extract links if requested
    if links {
        let extracted = extract_links(body);
        for (text, href) in &extracted {
            if text.is_empty() {
                println!("{href}");
            } else {
                println!("[{}]({href})", truncate_text(text, 50));
            }
        }
        println!("\n({} links)", extracted.len());
        return Ok(());
    }

    // Display with optional truncation (UTF-8 safe via floor_char_boundary)
    if max_body > 0 && body.len() > max_body {
        let at = body.floor_char_boundary(max_body);
        println!("\n{}", &body[..at]);
        println!("\n... [{} more bytes]", body.len() - at);
    } else {
        println!("\n{body}");
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
        println!("\nResponse Headers:");
        for (key, value) in response.headers() {
            println!("  {}: {}", key, value.to_str().unwrap_or("<binary>"));
        }
    }

    let body_text = response.text().await?;
    let router = nab::content::ContentRouter::new();
    let markdown = router.convert(body_text.as_bytes(), "text/html")?.markdown;
    output_body(&markdown, None, false, 0)?;

    Ok(())
}
