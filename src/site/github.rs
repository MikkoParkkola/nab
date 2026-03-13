//! GitHub Issues, Pull Requests, and repository README extraction via GitHub API.
//!
//! Uses the public GitHub REST API to extract issue/PR content and comments,
//! and repository README for repo root pages.
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
//!     &client,
//!     None,
//!     None
//! ).await?;
//!
//! println!("{}", content.markdown);
//! # Ok(())
//! # }
//! ```

use std::fmt::Write as _;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::{Engagement, SiteContent, SiteMetadata, SiteProvider};
use crate::http_client::AcceleratedClient;

/// Deep path segments that indicate this is NOT a repo root URL.
///
/// Repo root: `github.com/{owner}/{repo}` or `github.com/{owner}/{repo}/tree/{branch}`.
/// Any other segment (issues, pull, blob, commits, etc.) is excluded from repo-root matching.
const DEEP_PATH_SEGMENTS: &[&str] = &[
    "issues",
    "pull",
    "blob",
    "commits",
    "commit",
    "releases",
    "actions",
    "discussions",
    "security",
    "insights",
    "settings",
    "packages",
    "wiki",
    "compare",
];

/// GitHub Issues/PRs and repository README content provider using GitHub API.
pub struct GitHubProvider;

#[async_trait]
impl SiteProvider for GitHubProvider {
    fn name(&self) -> &'static str {
        "github"
    }

    fn matches(&self, url: &str) -> bool {
        let normalized = url.to_lowercase();
        let normalized = normalized.split('?').next().unwrap_or(&normalized);

        if !normalized.contains("github.com/") {
            return false;
        }

        // Issues and PRs always match.
        if normalized.contains("/issues/") || normalized.contains("/pull/") {
            return true;
        }

        // Repo root: `github.com/{owner}/{repo}` with at most a `/tree/` suffix.
        is_repo_root_url(normalized)
    }

    async fn extract(
        &self,
        url: &str,
        client: &AcceleratedClient,
        _cookies: Option<&str>,
        _prefetched_html: Option<&[u8]>,
    ) -> Result<SiteContent> {
        let normalized = url.to_lowercase();
        let stripped = normalized.split('?').next().unwrap_or(&normalized);

        if stripped.contains("/issues/") || stripped.contains("/pull/") {
            extract_issue_or_pr(url, client).await
        } else {
            extract_repo_readme(url, client).await
        }
    }
}

// ============================================================================
// URL helpers
// ============================================================================

/// Return `true` for `github.com/{owner}/{repo}` and
/// `github.com/{owner}/{repo}/tree/{branch}` URLs.
///
/// Requires exactly two path segments after `github.com/` (owner + repo),
/// optionally followed by `/tree/` and a branch name.
fn is_repo_root_url(normalized: &str) -> bool {
    // Strip scheme prefix to get to the path.
    let after_host = normalized
        .split_once("github.com/")
        .map_or("", |(_, rest)| rest);

    let segments: Vec<&str> = after_host.split('/').filter(|s| !s.is_empty()).collect();

    match segments.len() {
        // `github.com/{owner}/{repo}`
        2 => true,
        // `github.com/{owner}/{repo}/tree/{branch}` — any length >=3 starting with /tree/
        n if n >= 3 => {
            let third = segments[2];
            third == "tree" && !DEEP_PATH_SEGMENTS.contains(&segments[2])
        }
        _ => false,
    }
}

/// Parse `github.com/{owner}/{repo}` from a URL, stripping query/fragment.
fn parse_repo_url(url: &str) -> Result<(String, String)> {
    let url = url.split('?').next().unwrap_or(url);
    let parts: Vec<&str> = url.split('/').collect();

    let gh_idx = parts
        .iter()
        .position(|p| p.to_lowercase().contains("github.com"))
        .context("URL does not contain github.com")?;

    let owner = parts
        .get(gh_idx + 1)
        .filter(|s| !s.is_empty())
        .context("Could not extract owner from URL")?
        .to_string();

    let repo = parts
        .get(gh_idx + 2)
        .filter(|s| !s.is_empty())
        .context("Could not extract repo from URL")?
        .to_string();

    Ok((owner, repo))
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

// ============================================================================
// Extraction helpers
// ============================================================================

/// Extract issue or PR content via GitHub API.
async fn extract_issue_or_pr(url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
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

/// Fetch the raw README for a repository and return it as `SiteContent`.
async fn extract_repo_readme(url: &str, client: &AcceleratedClient) -> Result<SiteContent> {
    let (owner, repo) = parse_repo_url(url)?;
    let readme = fetch_readme(client, &owner, &repo).await?;

    let canonical_url = format!("https://github.com/{owner}/{repo}");

    let metadata = SiteMetadata {
        author: None,
        title: Some(format!("{owner}/{repo}")),
        published: None,
        platform: "GitHub".to_string(),
        canonical_url,
        media_urls: vec![],
        engagement: None,
    };

    Ok(SiteContent {
        markdown: readme,
        metadata,
    })
}

/// Fetch raw README content from the GitHub API.
///
/// Uses `Accept: application/vnd.github.raw+json` to receive the raw file content
/// directly without base64 encoding.
async fn fetch_readme(client: &AcceleratedClient, owner: &str, repo: &str) -> Result<String> {
    let api_url = format!("https://api.github.com/repos/{owner}/{repo}/readme");
    tracing::debug!("Fetching README from GitHub: {}", api_url);

    let response = client
        .inner()
        .get(&api_url)
        .header("User-Agent", "nab/0.3.0")
        .header("Accept", "application/vnd.github.raw+json")
        .send()
        .await
        .context("Failed to fetch README from GitHub API")?
        .text()
        .await
        .context("Failed to read README response body")?;

    Ok(response)
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
    let _ = write!(md, " [{}]", issue.state.to_uppercase());
    md.push_str("\n\n");

    // Metadata line
    let _ = write!(md, "by @{} · {} comments", issue.user.login, issue.comments);

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

            let _ = write!(
                md,
                "**@{}**:\n\n{}\n\n---\n\n",
                comment.user.login, comment.body
            );
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

    // ---- matches() tests -------------------------------------------------------

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
    fn matches_github_repo_root_urls() {
        let provider = GitHubProvider;
        // Plain repo root
        assert!(provider.matches("https://github.com/rust-lang/rust"));
        assert!(provider.matches("https://github.com/owner/repo"));
        // Repo root with trailing slash
        assert!(provider.matches("https://github.com/owner/repo/"));
        // Branch browsing via /tree/
        assert!(provider.matches("https://github.com/owner/repo/tree/main"));
        assert!(provider.matches("https://github.com/owner/repo/tree/feature/my-branch"));
    }

    #[test]
    fn does_not_match_non_repo_github_urls() {
        let provider = GitHubProvider;
        // Deep paths that are NOT repo root or issue/PR
        assert!(!provider.matches("https://github.com/owner/repo/commits"));
        assert!(!provider.matches("https://github.com/owner/repo/blob/main/src/lib.rs"));
        assert!(!provider.matches("https://github.com/owner/repo/releases"));
        assert!(!provider.matches("https://github.com/owner/repo/actions"));
        assert!(!provider.matches("https://github.com/owner/repo/wiki"));
        // Non-GitHub site
        assert!(!provider.matches("https://youtube.com/watch?v=abc"));
    }

    #[test]
    fn does_not_match_github_user_profile_urls() {
        let provider = GitHubProvider;
        // User profile: only one segment after github.com
        assert!(!provider.matches("https://github.com/rust-lang"));
        assert!(!provider.matches("https://github.com/"));
    }

    // ---- is_repo_root_url() tests ----------------------------------------------

    #[test]
    fn is_repo_root_url_recognises_owner_repo() {
        assert!(is_repo_root_url("github.com/owner/repo"));
        assert!(is_repo_root_url("github.com/owner/repo/"));
    }

    #[test]
    fn is_repo_root_url_accepts_tree_path() {
        assert!(is_repo_root_url("github.com/owner/repo/tree/main"));
        assert!(is_repo_root_url("github.com/owner/repo/tree/v1.0.0"));
    }

    #[test]
    fn is_repo_root_url_rejects_deep_segments() {
        assert!(!is_repo_root_url("github.com/owner/repo/issues/1"));
        assert!(!is_repo_root_url(
            "github.com/owner/repo/blob/main/Cargo.toml"
        ));
        assert!(!is_repo_root_url("github.com/owner"));
    }

    // ---- parse helpers ---------------------------------------------------------

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

    #[test]
    fn parse_repo_url_extracts_owner_and_repo() {
        let (owner, repo) = parse_repo_url("https://github.com/rust-lang/rust").unwrap();
        assert_eq!(owner, "rust-lang");
        assert_eq!(repo, "rust");
    }

    #[test]
    fn parse_repo_url_strips_query_and_tree_suffix() {
        let (owner, repo) =
            parse_repo_url("https://github.com/owner/my-repo/tree/main?tab=readme").unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "my-repo");
    }
}
