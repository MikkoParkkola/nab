//! Prefetch & Early Hints (103)
//!
//! Features:
//! - Preconnect: DNS + TCP + TLS handshake upfront
//! - Early Hints (103): Preload resources before response
//! - Link prefetching from HTML
//! - Connection warming for known hosts

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::http_client::AcceleratedClient;

/// Prefetch manager for connection warming
pub struct PrefetchManager {
    /// Warmed connections (hosts that have been preconnected)
    warmed: Arc<RwLock<HashSet<String>>>,
    /// HTTP client for warming
    client: AcceleratedClient,
}

impl PrefetchManager {
    /// Create new prefetch manager
    pub fn new() -> Result<Self> {
        Ok(Self {
            warmed: Arc::new(RwLock::new(HashSet::new())),
            client: AcceleratedClient::new()?,
        })
    }

    /// Preconnect to a host (DNS + TCP + TLS)
    ///
    /// This warms the connection so subsequent requests are faster.
    /// The connection pool in reqwest will keep it alive.
    pub async fn preconnect(&self, host: &str) -> Result<Duration> {
        let start = Instant::now();

        // Check if already warmed
        {
            let warmed = self.warmed.read().await;
            if warmed.contains(host) {
                debug!("Host already warmed: {}", host);
                return Ok(Duration::ZERO);
            }
        }

        info!("Preconnecting to {}", host);

        // Make a HEAD request to warm the connection
        // This performs DNS resolution, TCP handshake, and TLS handshake
        let url = if host.starts_with("http") {
            host.to_string()
        } else {
            format!("https://{host}")
        };

        // Use a lightweight request that most servers will handle quickly
        let response = self
            .client
            .inner()
            .head(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await?;

        let elapsed = start.elapsed();

        // Mark as warmed
        {
            let mut warmed = self.warmed.write().await;
            warmed.insert(host.to_string());
        }

        info!(
            "Preconnected to {} in {:?} (status: {})",
            host,
            elapsed,
            response.status()
        );

        Ok(elapsed)
    }

    /// Preconnect to multiple hosts in parallel
    pub async fn preconnect_many(&self, hosts: &[&str]) -> Vec<(String, Result<Duration>)> {
        let futures: Vec<_> = hosts
            .iter()
            .map(|host| {
                let host = (*host).to_string();
                let manager = self;
                async move { (host.clone(), manager.preconnect(&host).await) }
            })
            .collect();

        futures::future::join_all(futures).await
    }

    /// Check if a host is warmed
    pub async fn is_warmed(&self, host: &str) -> bool {
        self.warmed.read().await.contains(host)
    }

    /// Clear all warmed connections
    pub async fn clear(&self) {
        self.warmed.write().await.clear();
    }
}

impl Default for PrefetchManager {
    fn default() -> Self {
        Self::new().expect("Failed to create prefetch manager")
    }
}

/// Early Hints (103) response parser
///
/// Early Hints allow servers to send headers before the final response,
/// enabling preloading of resources.
#[derive(Debug, Clone)]
pub struct EarlyHints {
    /// Link headers with preload hints
    pub links: Vec<EarlyHintLink>,
}

/// A single Early Hint link
#[derive(Debug, Clone)]
pub struct EarlyHintLink {
    /// URL to preload
    pub url: String,
    /// Relationship (preload, preconnect, dns-prefetch, etc.)
    pub rel: String,
    /// Resource type (script, style, image, font, etc.)
    pub as_type: Option<String>,
    /// Crossorigin attribute
    pub crossorigin: Option<String>,
}

impl EarlyHints {
    /// Parse Early Hints from Link headers
    ///
    /// Format: `<url>; rel=preload; as=script`
    #[must_use] 
    pub fn parse(link_headers: &[&str]) -> Self {
        let mut links = Vec::new();

        for header in link_headers {
            if let Some(link) = Self::parse_link(header) {
                links.push(link);
            }
        }

        Self { links }
    }

    fn parse_link(header: &str) -> Option<EarlyHintLink> {
        // Parse: <url>; rel=preload; as=script; crossorigin
        let parts: Vec<&str> = header.split(';').map(str::trim).collect();

        if parts.is_empty() {
            return None;
        }

        // Extract URL
        let url = parts[0].trim_start_matches('<').trim_end_matches('>');
        if url.is_empty() {
            return None;
        }

        let mut rel = String::new();
        let mut as_type = None;
        let mut crossorigin = None;

        for part in parts.iter().skip(1) {
            let kv: Vec<&str> = part.splitn(2, '=').collect();
            if kv.is_empty() {
                continue;
            }

            let key = kv[0].trim().to_lowercase();
            let value = kv.get(1).map(|v| v.trim().trim_matches('"').to_string());

            match key.as_str() {
                "rel" => rel = value.unwrap_or_default(),
                "as" => as_type = value,
                "crossorigin" => crossorigin = value.or(Some("anonymous".to_string())),
                _ => {}
            }
        }

        if rel.is_empty() {
            return None;
        }

        Some(EarlyHintLink {
            url: url.to_string(),
            rel,
            as_type,
            crossorigin,
        })
    }

    /// Get all preload hints
    #[must_use] 
    pub fn preloads(&self) -> Vec<&EarlyHintLink> {
        self.links.iter().filter(|l| l.rel == "preload").collect()
    }

    /// Get all preconnect hints
    #[must_use] 
    pub fn preconnects(&self) -> Vec<&EarlyHintLink> {
        self.links
            .iter()
            .filter(|l| l.rel == "preconnect")
            .collect()
    }

    /// Get all dns-prefetch hints
    #[must_use] 
    pub fn dns_prefetches(&self) -> Vec<&EarlyHintLink> {
        self.links
            .iter()
            .filter(|l| l.rel == "dns-prefetch")
            .collect()
    }
}

/// Extract link hints from HTML
///
/// Parses `<link rel="preconnect">`, `<link rel="dns-prefetch">`, etc.
#[must_use] 
pub fn extract_link_hints(html: &str) -> Vec<EarlyHintLink> {
    let mut links = Vec::new();

    // Simple regex-free parsing for link tags
    let html_lower = html.to_lowercase();
    let mut pos = 0;

    while let Some(start) = html_lower[pos..].find("<link") {
        let abs_start = pos + start;
        if let Some(end) = html_lower[abs_start..].find('>') {
            let tag = &html[abs_start..=(abs_start + end)];

            // Extract href
            let href = extract_attr(tag, "href");
            let rel = extract_attr(tag, "rel");
            let as_type = extract_attr(tag, "as");
            let crossorigin = extract_attr(tag, "crossorigin");

            if let (Some(url), Some(rel)) = (href, rel) {
                if rel.contains("preconnect")
                    || rel.contains("dns-prefetch")
                    || rel.contains("preload")
                    || rel.contains("prefetch")
                {
                    links.push(EarlyHintLink {
                        url,
                        rel,
                        as_type,
                        crossorigin,
                    });
                }
            }

            pos = abs_start + end + 1;
        } else {
            break;
        }
    }

    links
}

fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let pattern = format!("{attr}=");
    if let Some(start) = tag.to_lowercase().find(&pattern) {
        let after_eq = &tag[start + pattern.len()..];
        let quote = after_eq.chars().next()?;
        if quote == '"' || quote == '\'' {
            let content = &after_eq[1..];
            if let Some(end) = content.find(quote) {
                return Some(content[..end].to_string());
            }
        } else {
            // Unquoted value (ends at space or >)
            let end = after_eq
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(after_eq.len());
            return Some(after_eq[..end].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_link_header() {
        let headers = vec![
            "</style.css>; rel=preload; as=style",
            "</script.js>; rel=preload; as=script; crossorigin",
            "<https://cdn.example.com>; rel=preconnect",
        ];

        let hints = EarlyHints::parse(&headers);
        assert_eq!(hints.links.len(), 3);
        assert_eq!(hints.preloads().len(), 2);
        assert_eq!(hints.preconnects().len(), 1);
    }

    #[test]
    fn test_extract_link_hints() {
        let html = r#"
            <head>
                <link rel="preconnect" href="https://fonts.googleapis.com">
                <link rel="dns-prefetch" href="//cdn.example.com">
                <link rel="preload" href="/main.js" as="script">
                <link rel="stylesheet" href="/style.css">
            </head>
        "#;

        let hints = extract_link_hints(html);
        assert_eq!(hints.len(), 3); // preconnect, dns-prefetch, preload (not stylesheet)
    }

    #[tokio::test]
    async fn test_preconnect() {
        let manager = PrefetchManager::new().unwrap();

        // Preconnect to a fast host
        let result = manager.preconnect("example.com").await;
        assert!(result.is_ok());
        assert!(manager.is_warmed("example.com").await);

        // Second preconnect should be instant (already warmed)
        let result2 = manager.preconnect("example.com").await;
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), Duration::ZERO);
    }
}
