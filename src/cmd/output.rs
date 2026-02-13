use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use scraper::{Html, Selector};

use crate::OutputFormat;

pub fn output_body(
    body: &str,
    output_file: Option<PathBuf>,
    _markdown: bool,
    links: bool,
    max_body: usize,
    _auto_spa: bool,
) -> Result<()> {
    // Save to file if requested (always full, no truncation)
    if let Some(path) = output_file {
        let mut file = File::create(&path)?;
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

    // Body is already converted (via ContentRouter) when markdown mode is active
    let output = body;

    // Display with optional limit
    let limit = if max_body == 0 {
        output.len()
    } else {
        max_body
    };
    if output.len() > limit {
        println!("\n{}", &output[..limit]);
        println!("\n... [{} more bytes]", output.len() - limit);
    } else {
        println!("\n{output}");
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
    if text.len() <= max {
        text.to_string()
    } else {
        format!("{}...", &text[..max - 3])
    }
}

/// Output response helper
#[allow(clippy::too_many_arguments)]
pub async fn output_response(
    response: reqwest::Response,
    show_headers: bool,
    show_body: bool,
    _format: OutputFormat,
    output_file: Option<PathBuf>,
    raw_html: bool,
    links: bool,
    max_body: usize,
) -> Result<()> {
    // Show headers if requested
    if show_headers {
        println!("\nResponse Headers:");
        for (key, value) in response.headers() {
            println!("  {}: {}", key, value.to_str().unwrap_or("<binary>"));
        }
    }

    // Get response body
    let body_text = response.text().await?;

    // Show body if requested
    if show_body {
        let markdown = if raw_html {
            body_text.clone()
        } else {
            let router = nab::content::ContentRouter::new();
            router.convert(body_text.as_bytes(), "text/html")?.markdown
        };

        output_body(&markdown, output_file, !raw_html, links, max_body, false)?;
    }

    Ok(())
}
