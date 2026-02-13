//! Stack Overflow content extraction via Stack Exchange API v2.3.
//!
//! Uses the public SE API for question and answer data. The API returns
//! gzip-compressed responses by default, which `reqwest` handles transparently.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, stackoverflow::StackOverflowProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = StackOverflowProvider;
//!
//! let content = provider.extract(
//!     "https://stackoverflow.com/questions/26946646/how-do-i-do-x-in-rust",
//!     &client
//! ).await?;
//!
//! println!("{}", content.markdown);
//! # Ok(())
//! # }
//! ```

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::{Engagement, SiteContent, SiteMetadata, SiteProvider};
use crate::http_client::AcceleratedClient;

/// Stack Overflow content provider using Stack Exchange API v2.3.
pub struct StackOverflowProvider;

#[async_trait]
impl SiteProvider for StackOverflowProvider {
    fn name(&self) -> &'static str {
        "stackoverflow"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        normalized.contains("stackoverflow.com/questions/")
            && normalized
                .split("stackoverflow.com/questions/")
                .nth(1)
                .is_some_and(|rest| {
                    rest.split('/')
                        .next()
                        .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
                })
    }

    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
        let question_id = parse_stackoverflow_url(url)?;

        // Fetch question with body and answers
        let api_url = format!(
            "https://api.stackexchange.com/2.3/questions/{}?site=stackoverflow&filter=withbody&order=desc&sort=votes",
            question_id
        );
        tracing::debug!("Fetching from Stack Exchange: {}", api_url);

        // SE API returns gzip-compressed responses; reqwest handles decompression
        let response = client
            .inner()
            .get(&api_url)
            .header("User-Agent", "nab/0.3.0")
            .send()
            .await
            .context("Failed to fetch from Stack Exchange API")?
            .text()
            .await
            .context("Failed to read Stack Exchange response body")?;

        let api_response: SEResponse =
            serde_json::from_str(&response).context("Failed to parse Stack Exchange response")?;

        let question = api_response
            .items
            .first()
            .context("No question found in response")?;

        // Fetch top answers
        let answers = fetch_answers(client, &question_id).await.unwrap_or_default();

        let markdown = format_stackoverflow_markdown(question, &answers);

        let engagement = Engagement {
            likes: Some(question.score),
            reposts: None,
            replies: Some(question.answer_count),
            views: Some(question.view_count),
        };

        let metadata = SiteMetadata {
            author: Some(question.owner.display_name.clone()),
            title: Some(question.title.clone()),
            published: Some(format_timestamp(question.creation_date)),
            platform: "Stack Overflow".to_string(),
            canonical_url: question.link.clone(),
            media_urls: vec![],
            engagement: Some(engagement),
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// Parse Stack Overflow URL to extract the question ID.
fn parse_stackoverflow_url(url: &str) -> Result<String> {
    let url = url.split('?').next().unwrap_or(url);
    let url = url.split('#').next().unwrap_or(url);

    // Pattern: stackoverflow.com/questions/{id}/...
    let after_questions = url
        .split("stackoverflow.com/questions/")
        .nth(1)
        .context("URL does not contain /questions/")?;

    let id = after_questions
        .split('/')
        .next()
        .context("Could not extract question ID")?
        .to_string();

    if id.is_empty() {
        anyhow::bail!("Empty question ID in URL");
    }

    Ok(id)
}

/// Fetch top answers for a question.
async fn fetch_answers(
    client: &AcceleratedClient,
    question_id: &str,
) -> Result<Vec<SEAnswer>> {
    let api_url = format!(
        "https://api.stackexchange.com/2.3/questions/{}/answers?site=stackoverflow&filter=withbody&order=desc&sort=votes&pagesize=3",
        question_id
    );

    let response = client
        .inner()
        .get(&api_url)
        .header("User-Agent", "nab/0.3.0")
        .send()
        .await
        .context("Failed to fetch answers")?
        .text()
        .await
        .context("Failed to read answers response")?;

    let api_response: SEAnswerResponse =
        serde_json::from_str(&response).context("Failed to parse answers response")?;

    Ok(api_response.items)
}

/// Strip HTML tags for plain text display.
fn strip_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Format Stack Overflow question and answers as markdown.
fn format_stackoverflow_markdown(question: &SEQuestion, answers: &[SEAnswer]) -> String {
    let mut md = String::new();

    // Title
    md.push_str("## ");
    md.push_str(&html_decode(&question.title));
    md.push_str("\n\n");

    // Metadata line
    md.push_str(&format!(
        "Asked by {} · {} votes · {} answers · {} views\n\n",
        question.owner.display_name,
        format_number(question.score),
        question.answer_count,
        format_number(question.view_count)
    ));

    // Tags
    if !question.tags.is_empty() {
        md.push_str("Tags: ");
        md.push_str(&question.tags.join(", "));
        md.push_str("\n\n");
    }

    // Question body
    if let Some(body) = &question.body {
        md.push_str("### Question\n\n");
        md.push_str(&strip_html(body));
        md.push_str("\n\n");
    }

    // Answers
    if !answers.is_empty() {
        md.push_str("### Top Answers\n\n");

        for answer in answers {
            let accepted = if answer.is_accepted { " [ACCEPTED]" } else { "" };
            md.push_str(&format!(
                "**{}** ({} votes){}\n\n",
                answer.owner.display_name,
                format_number(answer.score),
                accepted,
            ));

            if let Some(body) = &answer.body {
                md.push_str(&strip_html(body));
                md.push_str("\n\n");
            }

            md.push_str("---\n\n");
        }
    }

    // Link to original
    md.push_str("[View on Stack Overflow](");
    md.push_str(&question.link);
    md.push_str(")\n");

    md
}

/// Decode HTML entities in titles.
fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// Format Unix timestamp as ISO 8601 date string.
fn format_timestamp(timestamp: u64) -> String {
    let secs = i64::try_from(timestamp).unwrap_or(0);
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

/// Format large numbers with K/M suffixes.
fn format_number(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ============================================================================
// Stack Exchange API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct SEResponse {
    items: Vec<SEQuestion>,
}

#[derive(Debug, Deserialize)]
struct SEQuestion {
    title: String,
    body: Option<String>,
    score: u64,
    answer_count: u64,
    view_count: u64,
    link: String,
    creation_date: u64,
    #[serde(default)]
    tags: Vec<String>,
    owner: SEOwner,
}

#[derive(Debug, Deserialize)]
struct SEAnswerResponse {
    items: Vec<SEAnswer>,
}

#[derive(Debug, Deserialize)]
struct SEAnswer {
    body: Option<String>,
    score: u64,
    is_accepted: bool,
    owner: SEOwner,
}

#[derive(Debug, Deserialize)]
struct SEOwner {
    display_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_stackoverflow_question_urls() {
        let provider = StackOverflowProvider;
        assert!(provider.matches("https://stackoverflow.com/questions/26946646/how-do-i-do-x"));
        assert!(provider.matches("https://STACKOVERFLOW.COM/QUESTIONS/12345/some-title"));
    }

    #[test]
    fn matches_stackoverflow_urls_with_query_params() {
        let provider = StackOverflowProvider;
        assert!(provider.matches(
            "https://stackoverflow.com/questions/26946646/title?noredirect=1"
        ));
    }

    #[test]
    fn does_not_match_non_question_urls() {
        let provider = StackOverflowProvider;
        assert!(!provider.matches("https://stackoverflow.com/"));
        assert!(!provider.matches("https://stackoverflow.com/questions"));
        assert!(!provider.matches("https://stackoverflow.com/questions/"));
        assert!(!provider.matches("https://stackoverflow.com/tags/rust"));
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
    }

    #[test]
    fn does_not_match_question_listing_urls() {
        let provider = StackOverflowProvider;
        assert!(!provider.matches("https://stackoverflow.com/questions/tagged/rust"));
    }

    #[test]
    fn parse_stackoverflow_url_extracts_id() {
        let id = parse_stackoverflow_url(
            "https://stackoverflow.com/questions/26946646/how-do-i-do-x",
        )
        .unwrap();
        assert_eq!(id, "26946646");
    }

    #[test]
    fn parse_stackoverflow_url_strips_query_and_fragment() {
        let id = parse_stackoverflow_url(
            "https://stackoverflow.com/questions/12345/title?noredirect=1#answer-67890",
        )
        .unwrap();
        assert_eq!(id, "12345");
    }

    #[test]
    fn strip_html_removes_tags() {
        assert_eq!(strip_html("<p>Hello <b>world</b></p>"), "Hello world");
    }

    #[test]
    fn strip_html_decodes_entities() {
        assert_eq!(strip_html("&amp; &lt; &gt; &quot;"), "& < > \"");
    }

    #[test]
    fn strip_html_handles_code_blocks() {
        let html = "<pre><code>fn main() { println!(&quot;hello&quot;); }</code></pre>";
        let text = strip_html(html);
        assert!(text.contains("fn main()"));
        assert!(text.contains("\"hello\""));
    }

    #[test]
    fn html_decode_handles_common_entities() {
        assert_eq!(html_decode("How to use &amp; in Rust?"), "How to use & in Rust?");
        assert_eq!(html_decode("Vec&lt;T&gt;"), "Vec<T>");
    }

    #[test]
    fn format_number_uses_k_suffix() {
        assert_eq!(format_number(1_500), "1.5K");
        assert_eq!(format_number(8_800), "8.8K");
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn format_number_uses_m_suffix() {
        assert_eq!(format_number(1_000_000), "1.0M");
        assert_eq!(format_number(3_800_000), "3.8M");
    }

    #[test]
    fn format_stackoverflow_markdown_includes_question_and_answers() {
        let question = SEQuestion {
            title: "How to use Vec&lt;T&gt; in Rust?".to_string(),
            body: Some("<p>I want to create a vector.</p>".to_string()),
            score: 42,
            answer_count: 3,
            view_count: 15_000,
            link: "https://stackoverflow.com/questions/12345".to_string(),
            creation_date: 1_700_000_000,
            tags: vec!["rust".to_string(), "vector".to_string()],
            owner: SEOwner {
                display_name: "rustacean".to_string(),
            },
        };

        let answers = vec![SEAnswer {
            body: Some("<p>Use <code>Vec::new()</code>.</p>".to_string()),
            score: 100,
            is_accepted: true,
            owner: SEOwner {
                display_name: "expert".to_string(),
            },
        }];

        let md = format_stackoverflow_markdown(&question, &answers);

        assert!(md.contains("## How to use Vec<T> in Rust?"));
        assert!(md.contains("Asked by rustacean"));
        assert!(md.contains("42 votes"));
        assert!(md.contains("15.0K views"));
        assert!(md.contains("Tags: rust, vector"));
        assert!(md.contains("I want to create a vector."));
        assert!(md.contains("**expert** (100 votes) [ACCEPTED]"));
        assert!(md.contains("Vec::new()"));
        assert!(md.contains("[View on Stack Overflow]"));
    }

    #[test]
    fn format_stackoverflow_markdown_handles_no_answers() {
        let question = SEQuestion {
            title: "Unanswered question".to_string(),
            body: Some("<p>Help please.</p>".to_string()),
            score: 1,
            answer_count: 0,
            view_count: 50,
            link: "https://stackoverflow.com/questions/99999".to_string(),
            creation_date: 1_700_000_000,
            tags: vec![],
            owner: SEOwner {
                display_name: "newbie".to_string(),
            },
        };

        let md = format_stackoverflow_markdown(&question, &[]);

        assert!(md.contains("## Unanswered question"));
        assert!(!md.contains("### Top Answers"));
    }

    #[test]
    fn format_timestamp_produces_iso_date() {
        // 2023-11-14 roughly
        let result = format_timestamp(1_700_000_000);
        assert!(result.starts_with("2023-11-14"));
    }
}
