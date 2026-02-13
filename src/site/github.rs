//! GitHub Issues and Pull Requests content extraction via GitHub API.
//!
//! Uses the public GitHub REST API to extract issue/PR content and comments.
//! No authentication required for public repositories.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::site::{SiteProvider, github::GitHubProvider};
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let client = AcceleratedClient::new()?;
//! let provider = GitHubProvider;
//!
//! let content = provider.extract(
//!     "https://github.com/rust-lang/rust/issues/12345",
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

/// GitHub Issues/PRs content provider using GitHub API.
pub struct GitHubProvider;

#[async_trait]
impl SiteProvider for GitHubProvider {
    fn name(&self) -> &'static str {
        "github"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        normalized.contains("github.com/")
            && (normalized.contains("/issues/") || normalized.contains("/pull/"))
    }

    async fn extract(&self, url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
        let (owner, repo, number) = parse_github_url(url)?;

        let api_url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}");
        tracing::debug!("Fetching from GitHub: {}", api_url);

        let response = client
            .inner()
            .get(&api_url)
            .header("User-Agent", "nab/0.3.0")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .context("Failed to fetch from GitHub API")?
            .text()
            .await
            .context("Failed to read GitHub response body")?;

        let issue: GitHubIssue =
            serde_json::from_str(&response).context("Failed to parse GitHub response")?;

        // Fetch comments
        let comments = fetch_comments(client, &issue.comments_url).await?;

        let markdown = format_github_markdown(&issue, &comments);

        let engagement = Engagement {
            likes: None,
            reposts: None,
            replies: Some(issue.comments),
            views: None,
        };

        let metadata = SiteMetadata {
            author: Some(issue.user.login.clone()),
            title: Some(issue.title.clone()),
            published: Some(issue.created_at.clone()),
            platform: "GitHub".to_string(),
            canonical_url: issue.html_url.clone(),
            media_urls: vec![],
            engagement: Some(engagement),
        };

        Ok(SiteContent { markdown, metadata })
    }
}

/// Parse GitHub URL to extract owner, repo, and issue/PR number.
fn parse_github_url(url: &str) -> Result<(String, String, String)> {
    let url = url.split('?').next().unwrap_or(url);
    let parts: Vec<&str> = url.split('/').collect();

    // Find "issues" or "pull" in the URL
    let issue_idx = parts
        .iter()
        .position(|&p| p == "issues" || p == "pull")
        .context("URL does not contain /issues/ or /pull/")?;

    let owner = parts
        .get(issue_idx - 2)
        .context("Could not extract owner from URL")?
        .to_string();

    let repo = parts
        .get(issue_idx - 1)
        .context("Could not extract repo from URL")?
        .to_string();

    let number = parts
        .get(issue_idx + 1)
        .context("Could not extract issue/PR number from URL")?
        .to_string();

    Ok((owner, repo, number))
}

/// Fetch comments for an issue/PR.
async fn fetch_comments(
    client: &AcceleratedClient,
    comments_url: &str,
) -> Result<Vec<GitHubComment>> {
    if comments_url.is_empty() {
        return Ok(vec![]);
    }

    let response = client
        .inner()
        .get(comments_url)
        .header("User-Agent", "nab/0.3.0")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("Failed to fetch comments")?
        .text()
        .await
        .context("Failed to read comments response")?;

    let comments: Vec<GitHubComment> =
        serde_json::from_str(&response).context("Failed to parse comments")?;

    Ok(comments)
}

/// Format GitHub issue/PR and comments as markdown.
fn format_github_markdown(issue: &GitHubIssue, comments: &[GitHubComment]) -> String {
    let mut md = String::new();

    // Title with state badge
    md.push_str("## ");
    md.push_str(&issue.title);
    md.push_str(&format!(" [{}]", issue.state.to_uppercase()));
    md.push_str("\n\n");

    // Metadata line
    md.push_str(&format!(
        "by @{} · {} comments",
        issue.user.login, issue.comments
    ));

    // Labels
    if !issue.labels.is_empty() {
        md.push_str(" · Labels: ");
        let label_names: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
        md.push_str(&label_names.join(", "));
    }

    md.push_str("\n\n");

    // Issue body (already markdown!)
    if let Some(body) = &issue.body {
        md.push_str(body);
        md.push_str("\n\n");
    }

    // Comments (up to 10)
    if !comments.is_empty() {
        md.push_str("### Comments\n\n");

        for (count, comment) in comments.iter().enumerate() {
            if count >= 10 {
                break;
            }

            md.push_str(&format!("**@{}**:\n\n{}\n\n---\n\n", comment.user.login, comment.body));
        }
    }

    md
}

// ============================================================================
// GitHub API Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct GitHubIssue {
    html_url: String,
    title: String,
    state: String,
    user: GitHubUser,
    body: Option<String>,
    comments: u64,
    comments_url: String,
    created_at: String,
    #[serde(default)]
    labels: Vec<GitHubLabel>,
}

#[derive(Debug, Deserialize)]
struct GitHubUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GitHubLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GitHubComment {
    user: GitHubUser,
    body: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_github_issues_urls() {
        let provider = GitHubProvider;
        assert!(provider.matches("https://github.com/rust-lang/rust/issues/12345"));
        assert!(provider.matches("https://GITHUB.COM/owner/repo/ISSUES/999"));
    }

    #[test]
    fn matches_github_pull_request_urls() {
        let provider = GitHubProvider;
        assert!(provider.matches("https://github.com/rust-lang/rust/pull/67890"));
        assert!(provider.matches("https://github.com/owner/repo/pull/1"));
    }

    #[test]
    fn does_not_match_non_issue_urls() {
        let provider = GitHubProvider;
        assert!(!provider.matches("https://github.com/rust-lang/rust"));
        assert!(!provider.matches("https://github.com/owner/repo/commits"));
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
    }

    #[test]
    fn parse_github_url_extracts_owner_repo_number() {
        let (owner, repo, number) =
            parse_github_url("https://github.com/rust-lang/rust/issues/12345").unwrap();
        assert_eq!(owner, "rust-lang");
        assert_eq!(repo, "rust");
        assert_eq!(number, "12345");
    }

    #[test]
    fn parse_github_url_handles_pull_requests() {
        let (owner, repo, number) =
            parse_github_url("https://github.com/owner/repo/pull/999").unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
        assert_eq!(number, "999");
    }

    #[test]
    fn parse_github_url_strips_query() {
        let (owner, repo, number) =
            parse_github_url("https://github.com/owner/repo/issues/123?ref=foo").unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
        assert_eq!(number, "123");
    }
}
