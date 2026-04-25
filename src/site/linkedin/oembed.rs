// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! `LinkedIn` oEmbed fallback extraction.
//!
//! Provides limited data (title, author, thumbnail) for public posts, pulse
//! articles, and feed updates when authenticated extraction is unavailable.

use std::fmt::Write as _;

use anyhow::{Context, Result};

use super::helpers::strip_html;
use super::types::LinkedInOEmbed;
use crate::http_client::AcceleratedClient;
use crate::site::{SiteContent, SiteMetadata};

/// Fetch content via the `LinkedIn` oEmbed API endpoint.
pub(super) async fn fetch_oembed(url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
    let oembed_url = format!(
        "https://www.linkedin.com/oembed?url={}&format=json",
        urlencoding::encode(url)
    );
    let response = client
        .fetch_text(&oembed_url)
        .await
        .context("Failed to fetch from LinkedIn oEmbed API")?;

    let oembed: LinkedInOEmbed =
        serde_json::from_str(&response).context("Failed to parse LinkedIn oEmbed response")?;

    let markdown = format_oembed_markdown(&oembed, url);

    let metadata = SiteMetadata {
        author: oembed.author_name.clone(),
        title: oembed.title.clone(),
        published: None,
        platform: "LinkedIn".to_string(),
        canonical_url: oembed.author_url.clone().unwrap_or_else(|| url.to_string()),
        media_urls: oembed
            .thumbnail_url
            .as_ref()
            .map(|t| vec![t.clone()])
            .unwrap_or_default(),
        engagement: None,
    };

    Ok(SiteContent { markdown, metadata })
}

/// Format `LinkedIn` oEmbed data as markdown.
pub(super) fn format_oembed_markdown(oembed: &LinkedInOEmbed, url: &str) -> String {
    let mut md = String::new();

    if let Some(title) = &oembed.title {
        let _ = writeln!(md, "## {title}\n");
    }

    if let Some(author) = &oembed.author_name {
        let _ = writeln!(md, "by {author}\n");
    }

    if let Some(thumb) = &oembed.thumbnail_url {
        let _ = writeln!(md, "![LinkedIn post]({thumb})\n");
    }

    if let Some(html) = &oembed.html {
        let text = strip_html(html);
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let _ = writeln!(md, "{trimmed}\n");
        }
    }

    let _ = writeln!(md, "[View on LinkedIn]({url})");

    md
}
