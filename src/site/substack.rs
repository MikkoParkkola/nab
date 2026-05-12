// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Substack article extraction.
//!
//! Substack posts ship authored content in a stable `.available-content
//! .body.markup` subtree. The generic HTML path can extract it, but a provider
//! makes Substack posts first-class and keeps post chrome out of the returned
//! markdown before budget/focus post-processing runs.

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::header::{COOKIE, HeaderMap, HeaderValue};
use url::Url;

use super::{SiteContent, SiteMetadata, SiteProvider};
use crate::content::readability;
use crate::http_client::AcceleratedClient;
use crate::{SafeFetchConfig, SafeRequestOptions};

/// Provider for Substack post URLs.
pub struct SubstackProvider;

#[async_trait]
impl SiteProvider for SubstackProvider {
    fn name(&self) -> &'static str {
        "substack"
    }

    fn matches(&self, url: &str) -> bool {
        is_substack_post_url(url)
    }

    async fn extract(
        &self,
        url: &str,
        client: &AcceleratedClient,
        cookies: Option<&str>,
        prefetched_html: Option<&[u8]>,
    ) -> Result<SiteContent> {
        let html = match prefetched_html {
            Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            None => fetch_html(client, url, cookies).await?,
        };

        site_content_from_html(&html, url)
    }
}

fn is_substack_post_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    let is_substack_host =
        host == "substack.com" || host == "www.substack.com" || host.ends_with(".substack.com");
    if !is_substack_host {
        return false;
    }

    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|segments| segments.filter(|segment| !segment.is_empty()).collect())
        .unwrap_or_default();
    segments.first().is_some_and(|segment| *segment == "p")
        || segments.windows(2).any(|window| window[1] == "p")
}

async fn fetch_html(
    client: &AcceleratedClient,
    url: &str,
    cookies: Option<&str>,
) -> Result<String> {
    let mut headers = HeaderMap::new();
    if let Some(cookie_header) = cookies.filter(|value| !value.trim().is_empty()) {
        headers.insert(
            COOKIE,
            HeaderValue::from_str(cookie_header).context("invalid cookie header for Substack")?,
        );
    }

    let response = client
        .request_safe(
            url,
            SafeRequestOptions {
                headers,
                config: SafeFetchConfig::default(),
                ..SafeRequestOptions::default()
            },
        )
        .await
        .with_context(|| format!("failed to fetch Substack post '{url}'"))?;

    Ok(String::from_utf8_lossy(&response.body).into_owned())
}

fn site_content_from_html(html: &str, url: &str) -> Result<SiteContent> {
    let article = readability::extract_article(html, url)
        .with_context(|| format!("Substack article body not found for {url}"))?;
    let markdown = readability::article_to_markdown(&article);
    let canonical_url = canonicalize_url(url);

    Ok(SiteContent {
        markdown,
        metadata: SiteMetadata {
            author: None,
            title: Some(article.title),
            published: None,
            platform: "Substack".to_string(),
            canonical_url,
            media_urls: Vec::new(),
            engagement: None,
        },
    })
}

fn canonicalize_url(url: &str) -> String {
    Url::parse(url).map_or_else(
        |_| url.to_string(),
        |mut parsed| {
            parsed.set_fragment(None);
            parsed.to_string()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    fn substack_fixture(title: &str, body: &[&str], chrome_repeat: usize) -> String {
        let chrome = "Subscribe Archive Comments Share Recommended ".repeat(chrome_repeat);
        let mut body_html = String::new();
        for paragraph in body {
            let _ = write!(body_html, "<p>{paragraph}</p>");
        }
        format!(
            r#"
            <html>
              <head><title>Chrome title</title></head>
              <body>
                <header>{chrome}</header>
                <article class="typography newsletter-post post">
                  <div class="post-header">
                    <h1 class="post-title published">{title}</h1>
                    <div class="post-ufi">451 likes 90 comments Share</div>
                  </div>
                  <div class="available-content">
                    <div dir="auto" class="body markup">
                      {body_html}
                      <div class="subscription-widget-wrap">
                        <p>Subscribe now for more posts.</p>
                      </div>
                    </div>
                  </div>
                </article>
                <section class="comments">{chrome}</section>
                <footer>{chrome}</footer>
              </body>
            </html>
            "#
        )
    }

    fn body_ratio(markdown: &str, body: &[&str]) -> f64 {
        let body_chars: usize = body.iter().map(|part| part.len()).sum();
        #[allow(clippy::cast_precision_loss)]
        {
            body_chars as f64 / markdown.len().max(1) as f64
        }
    }

    #[test]
    fn matches_substack_post_urls_only() {
        let provider = SubstackProvider;

        assert!(provider.matches("https://freddiedeboer.substack.com/p/we-are-still-living"));
        assert!(provider.matches("https://www.substack.com/@writer/p/post-title"));
        assert!(!provider.matches("https://freddiedeboer.substack.com/archive"));
        assert!(!provider.matches("https://example.com/p/not-substack"));
    }

    #[test]
    fn extracts_substack_article_with_body_ratio_above_eighty_percent() {
        let cases = [
            (
                "https://freddiedeboer.substack.com/p/we-are-still-living-in-the-long-boring",
                "We Are Still Living in the Long Boring",
                vec![
                    "This short post opens with a compact argument that still needs to survive extraction intact.",
                    "The point is not the surrounding navigation; the authored paragraph is the payload that agents should receive.",
                    "Even a brief note needs enough original prose to keep the body ratio check meaningful.",
                ],
                120,
            ),
            (
                "https://examplewriter.substack.com/p/a-medium-post",
                "A Medium Post",
                vec![
                    "The first medium-length paragraph lays out context, stakes, and the claim being evaluated.",
                    "The second paragraph adds counterarguments and concrete examples for the reader to inspect.",
                    "The final paragraph closes the loop without needing comments, subscribe widgets, or footer links.",
                ],
                160,
            ),
            (
                "https://policy.substack.com/p/a-long-policy-note",
                "A Long Policy Note",
                vec![
                    "Long-form Substack posts often include enough interface chrome to overwhelm extraction if selectors are too broad.",
                    "The article body should remain the dominant returned markdown even when recommendations and comments are present.",
                    "This paragraph represents the middle of the piece and should not be discarded by the provider.",
                    "The conclusion remains part of the authored body and gives downstream summarizers the final claim.",
                ],
                220,
            ),
            (
                "https://research.substack.com/p/another-long-post",
                "Another Long Post",
                vec![
                    "A representative research essay starts by framing the evidence and defining the central question.",
                    "It then walks through the supporting observations, preserving named concepts and local context.",
                    "A later section may include caveats, but it is still article text and must remain in markdown.",
                    "The extraction should exclude buttons, signup forms, related posts, and comments from the output.",
                ],
                240,
            ),
            (
                "https://letters.substack.com/p/a-brief-letter",
                "A Brief Letter",
                vec![
                    "Brief posts are the hard case because a small amount of chrome can dominate the output quickly.",
                    "The provider keeps only the authored body so the useful text stays above the ratio invariant.",
                    "A final sentence gives the fixture realistic length without adding any interface chrome.",
                ],
                100,
            ),
        ];

        for (url, title, body, chrome_repeat) in cases {
            let html = substack_fixture(title, &body, chrome_repeat);
            let content = site_content_from_html(&html, url).unwrap();

            assert!(content.markdown.contains(title));
            for paragraph in &body {
                assert!(content.markdown.contains(paragraph));
            }
            assert!(!content.markdown.contains("Subscribe now"));
            assert!(!content.markdown.contains("451 likes"));
            assert!(
                body_ratio(&content.markdown, &body) >= 0.80,
                "body ratio too low for {url}: {:.2}\n{}",
                body_ratio(&content.markdown, &body),
                content.markdown
            );
        }
    }
}
