// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! `LessWrong` / `GreaterWrong` extraction.
//!
//! Canonical `lesswrong.com` posts SSR the author bio into the DOM and stash
//! the article body in a `ForumMagnum` JSON `html` field. `GreaterWrong` is a
//! JavaScript viewer whose static HTML is chrome-only. Both resolve to the
//! markdown API at `/api/post/{slug}` when the path is a post.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use url::Url;

use super::{SiteContent, SiteMetadata, SiteProvider};
use crate::content::html::html_to_markdown_with_url;
use crate::http_client::AcceleratedClient;

const LESSWRONG_HOST: &str = "www.lesswrong.com";
const LESSWRONG_HOSTS: &[&str] = &["lesswrong.com", "www.lesswrong.com"];
const GREATERWRONG_HOSTS: &[&str] = &["greaterwrong.com", "www.greaterwrong.com"];

/// Provider for `LessWrong` mirror URLs.
pub struct LessWrongProvider;

#[async_trait]
impl SiteProvider for LessWrongProvider {
    fn name(&self) -> &'static str {
        "lesswrong"
    }

    fn matches(&self, url: &str) -> bool {
        is_lesswrong_family_url(url)
    }

    async fn extract(
        &self,
        url: &str,
        client: &AcceleratedClient,
        _cookies: Option<&str>,
        prefetched_html: Option<&[u8]>,
    ) -> Result<SiteContent> {
        let canonical_url = canonical_lesswrong_url(url)?;
        if let Some(bytes) = prefetched_html
            && let Ok(html) = std::str::from_utf8(bytes)
        {
            return Ok(site_content_from_html(html, &canonical_url));
        }

        if let Some(api_url) = lesswrong_markdown_api_url(&canonical_url)? {
            let markdown = client
                .inner()
                .get(&api_url)
                .send()
                .await
                .with_context(|| format!("failed to fetch LessWrong markdown URL '{api_url}'"))?
                .error_for_status()
                .with_context(|| format!("HTTP error for LessWrong markdown URL '{api_url}'"))?
                .text()
                .await
                .with_context(|| {
                    format!("failed to read LessWrong markdown response from '{api_url}'")
                })?;

            return Ok(site_content_from_markdown(&markdown, &canonical_url));
        }

        let html = client
            .fetch_text(&canonical_url)
            .await
            .with_context(|| format!("failed to fetch LessWrong URL '{canonical_url}'"))?;

        Ok(site_content_from_html(&html, &canonical_url))
    }
}

fn is_lesswrong_family_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    LESSWRONG_HOSTS.contains(&host.as_str()) || GREATERWRONG_HOSTS.contains(&host.as_str())
}

fn canonical_lesswrong_url(url: &str) -> Result<String> {
    let mut parsed = Url::parse(url).context("invalid LessWrong URL")?;
    let host = parsed
        .host_str()
        .map(str::to_ascii_lowercase)
        .context("LessWrong URL has no host")?;

    if GREATERWRONG_HOSTS.contains(&host.as_str()) || host == "lesswrong.com" {
        parsed
            .set_host(Some(LESSWRONG_HOST))
            .context("failed to rewrite LessWrong host")?;
    } else if host != LESSWRONG_HOST {
        bail!("URL is not a LessWrong family URL");
    }

    parsed.set_fragment(None);

    Ok(parsed.to_string())
}

fn lesswrong_markdown_api_url(canonical_url: &str) -> Result<Option<String>> {
    let parsed = Url::parse(canonical_url).context("invalid LessWrong canonical URL")?;
    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|segments| segments.filter(|segment| !segment.is_empty()).collect())
        .unwrap_or_default();

    let Some(slug) = segments
        .as_slice()
        .strip_prefix(&["posts"])
        .and_then(|rest| {
            if rest.len() >= 2 {
                rest.last().copied()
            } else {
                None
            }
        })
    else {
        return Ok(None);
    };

    Ok(Some(format!("https://{LESSWRONG_HOST}/api/post/{slug}")))
}

fn site_content_from_markdown(markdown: &str, canonical_url: &str) -> SiteContent {
    SiteContent {
        markdown: markdown.to_string(),
        metadata: SiteMetadata {
            author: None,
            title: markdown_title(markdown),
            published: None,
            platform: "lesswrong".to_string(),
            canonical_url: canonical_url.to_string(),
            media_urls: Vec::new(),
            engagement: None,
        },
    }
}

fn site_content_from_html(html: &str, canonical_url: &str) -> SiteContent {
    let from_dom = html_to_markdown_with_url(html, Some(canonical_url));
    let markdown = extract_forummagnum_embedded_html(html)
        .map(|embedded| {
            html_to_markdown_with_url(
                &format!("<article>{embedded}</article>"),
                Some(canonical_url),
            )
        })
        .filter(|from_json| from_json.len() > from_dom.len())
        .unwrap_or(from_dom);
    let title = html_heading_title(html).or_else(|| markdown_title(&markdown));

    SiteContent {
        markdown,
        metadata: SiteMetadata {
            author: None,
            title,
            published: None,
            platform: "lesswrong".to_string(),
            canonical_url: canonical_url.to_string(),
            media_urls: Vec::new(),
            engagement: None,
        },
    }
}

fn markdown_title(markdown: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        line.strip_prefix("# ")
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
    })
}

/// Pull the longest `ForumMagnum` JSON `"html":"..."` payload out of SSR HTML.
fn extract_forummagnum_embedded_html(html: &str) -> Option<String> {
    const MARKER: &str = "\"html\":\"";
    let mut search = html;
    let mut best: Option<String> = None;
    while let Some(idx) = search.find(MARKER) {
        let rest = &search[idx + MARKER.len()..];
        if let Some(value) = parse_json_string_body(rest)
            && value.contains('<')
            && value.len() > best.as_ref().map_or(0, String::len)
        {
            best = Some(value);
        }
        search = &search[idx + MARKER.len()..];
    }
    best
}

fn parse_json_string_body(rest: &str) -> Option<String> {
    let mut encoded = String::from('"');
    let mut chars = rest.chars();
    loop {
        match chars.next()? {
            '"' => {
                encoded.push('"');
                break;
            }
            '\\' => {
                encoded.push('\\');
                encoded.push(chars.next()?);
            }
            c => encoded.push(c),
        }
        if encoded.len() > 2_000_000 {
            return None;
        }
    }
    serde_json::from_str(&encoded).ok()
}

fn html_heading_title(html: &str) -> Option<String> {
    let document = scraper::Html::parse_document(html);
    let selector = scraper::Selector::parse("h1").ok()?;

    document.select(&selector).find_map(|element| {
        let text = element.text().collect::<Vec<_>>().join(" ");
        let title = text.trim();
        if title.is_empty() {
            None
        } else {
            Some(title.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_lesswrong_family_hosts() {
        let provider = LessWrongProvider;

        assert!(provider.matches("https://greaterwrong.com/posts/abc/title"));
        assert!(provider.matches("https://www.greaterwrong.com/posts/abc/title"));
        assert!(provider.matches("https://www.lesswrong.com/posts/abc/title"));
        assert!(provider.matches("https://lesswrong.com/posts/abc/title"));
        assert!(!provider.matches("https://example.com/posts/abc/title"));
    }

    #[test]
    fn canonicalizes_representative_greaterwrong_urls_to_lesswrong() {
        let cases = [
            (
                "https://www.greaterwrong.com/posts/fewDbvpKMZLgGuWT2/the-world-can-t-keep-up-with-ai-labs",
                "https://www.lesswrong.com/posts/fewDbvpKMZLgGuWT2/the-world-can-t-keep-up-with-ai-labs",
            ),
            (
                "https://greaterwrong.com/posts/abc123/a-post-title",
                "https://www.lesswrong.com/posts/abc123/a-post-title",
            ),
            (
                "http://greaterwrong.com/posts/abc123/a-post-title?compact=1",
                "http://www.lesswrong.com/posts/abc123/a-post-title?compact=1",
            ),
            (
                "https://www.greaterwrong.com/w/ai",
                "https://www.lesswrong.com/w/ai",
            ),
            (
                "https://www.greaterwrong.com/users/lee-aao#comments",
                "https://www.lesswrong.com/users/lee-aao",
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(canonical_lesswrong_url(input).unwrap(), expected);
        }
    }

    #[test]
    fn maps_representative_post_urls_to_lesswrong_markdown_api() {
        let cases = [
            (
                "https://www.lesswrong.com/posts/fewDbvpKMZLgGuWT2/the-world-can-t-keep-up-with-ai-labs",
                "https://www.lesswrong.com/api/post/the-world-can-t-keep-up-with-ai-labs",
            ),
            (
                "https://www.lesswrong.com/posts/abc123/a-post-title",
                "https://www.lesswrong.com/api/post/a-post-title",
            ),
            (
                "http://www.lesswrong.com/posts/abc123/a-post-title?compact=1",
                "https://www.lesswrong.com/api/post/a-post-title",
            ),
            (
                "https://www.lesswrong.com/posts/abc123/a-post-title/",
                "https://www.lesswrong.com/api/post/a-post-title",
            ),
            (
                "https://www.lesswrong.com/posts/abc123/title-with-dashes",
                "https://www.lesswrong.com/api/post/title-with-dashes",
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(
                lesswrong_markdown_api_url(input).unwrap().as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn leaves_non_post_urls_on_html_fallback() {
        assert_eq!(
            lesswrong_markdown_api_url("https://www.lesswrong.com/w/ai")
                .unwrap()
                .as_deref(),
            None
        );
    }

    #[test]
    fn wraps_markdown_api_response_as_site_content() {
        let markdown = r"# The World Can't Keep Up With AI Labs

*   By [Lee.aao](/users/lee-aao)

Late last year a new AI psychosis kicked off. This time it was coding agents.
";

        let content = site_content_from_markdown(
            markdown,
            "https://www.lesswrong.com/posts/fewDbvpKMZLgGuWT2/the-world-can-t-keep-up-with-ai-labs",
        );

        assert!(
            content
                .markdown
                .contains("Late last year a new AI psychosis kicked off")
        );
        assert_eq!(
            content.metadata.title.as_deref(),
            Some("The World Can't Keep Up With AI Labs")
        );
        assert_eq!(
            content.metadata.canonical_url,
            "https://www.lesswrong.com/posts/fewDbvpKMZLgGuWT2/the-world-can-t-keep-up-with-ai-labs"
        );
    }

    #[test]
    fn extracts_article_markdown_from_lesswrong_html() {
        let html = r#"
            <!doctype html>
            <html>
              <head><title>Viewer chrome</title></head>
              <body>
                <nav>Frontpage Tags Library</nav>
                <article>
                  <h1>The World Can't Keep Up With AI Labs</h1>
                  <p>Late last year a new AI psychosis kicked off. This time it was coding agents.</p>
                  <p>Governments and labs are moving at different speeds, which is the core article body.</p>
                </article>
                <section class="comments-node"><p>Comment text should not dominate extraction.</p></section>
              </body>
            </html>
        "#;

        let content = site_content_from_html(
            html,
            "https://www.lesswrong.com/posts/fewDbvpKMZLgGuWT2/the-world-can-t-keep-up-with-ai-labs",
        );

        assert!(
            content
                .markdown
                .contains("The World Can't Keep Up With AI Labs")
        );
        assert!(
            content
                .markdown
                .contains("Late last year a new AI psychosis kicked off")
        );
        assert_eq!(
            content.metadata.title.as_deref(),
            Some("The World Can't Keep Up With AI Labs")
        );
        assert_eq!(content.metadata.platform, "lesswrong");
    }

    #[test]
    fn extracts_article_from_forummagnum_embedded_html_json() {
        let body = "<p>The hidden structures of problems include coordination traps and value lock-in, a 31-item taxonomy that must survive extraction.</p>";
        let encoded = serde_json::to_string(body).unwrap();
        let html = format!(
            r#"
            <html>
              <body>
                <div class="author-bio">Denkenberger has worked on food security for years and this bio is in the DOM.</div>
                <article><p>.</p></article>
                <script type="application/json">{{"html":{encoded},"wordCount":4888}}</script>
              </body>
            </html>
            "#
        );

        let content = site_content_from_html(
            &html,
            "https://www.lesswrong.com/posts/Cisy9STMoFYwboTsy/the-hidden-structures-of-problems",
        );

        assert!(
            content.markdown.contains("31-item taxonomy"),
            "expected JSON body, got: {}",
            content.markdown
        );
        assert!(!content.markdown.contains("Denkenberger"));
    }

    #[test]
    fn canonicalizes_lesswrong_com_to_www() {
        assert_eq!(
            canonical_lesswrong_url(
                "https://lesswrong.com/posts/Cisy9STMoFYwboTsy/the-hidden-structures-of-problems#comments"
            )
            .unwrap(),
            "https://www.lesswrong.com/posts/Cisy9STMoFYwboTsy/the-hidden-structures-of-problems"
        );
    }
}
