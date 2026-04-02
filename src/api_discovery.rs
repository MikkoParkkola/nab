//! Static API Endpoint Discovery
//!
//! Extracts API endpoints from JavaScript bundles without execution.
//! Uses regex patterns to find `fetch()`, axios, GraphQL, and other HTTP calls.
//!
//! ## Strategy
//!
//! 1. **Fast Path**: Extract and try endpoints directly (~50ms)
//! 2. **Fallback**: If no data, fall back to JavaScript execution (~200ms)
//!
//! ## Patterns Detected
//!
//! - `fetch("/api/users")` - Direct fetch calls
//! - `axios.get("/api/data")` - Axios HTTP client
//! - `$.ajax({url: "/api/..."})` - jQuery AJAX
//! - `new XMLHttpRequest()` with `.open("GET", "/api/...")`
//! - `baseURL: "https://api.example.com"` - API base configuration
//! - GraphQL endpoints: `/graphql`, `/__graphql`

use anyhow::Result;
use regex::Regex;
use std::collections::HashSet;

/// Discovered API endpoint
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApiEndpoint {
    /// URL path (may be relative like "/api/users" or absolute)
    pub url: String,
    /// HTTP method if detected (GET, POST, etc.)
    pub method: Option<String>,
    /// Source pattern that found this (for debugging)
    pub source: String,
}

/// API endpoint discovery engine
pub struct ApiDiscovery {
    /// Regex patterns for finding endpoints
    patterns: Vec<EndpointPattern>,
    /// Optional user-provided URL fragments to prioritize or discover.
    url_hints: Vec<String>,
}

struct EndpointPattern {
    name: String,
    regex: Regex,
    url_group: usize,            // Which capture group contains the URL
    method_group: Option<usize>, // Which capture group contains the method (if any)
}

impl ApiDiscovery {
    /// Create a new API discovery engine with built-in patterns
    pub fn new() -> Result<Self> {
        Self::with_url_hints(&[])
    }

    /// Create a discovery engine with additional user-provided URL hints.
    pub fn with_url_hints(url_hints: &[String]) -> Result<Self> {
        let mut patterns = default_patterns()?;

        let mut normalized_hints = Vec::new();
        for hint in url_hints {
            let trimmed = hint.trim();
            if trimmed.is_empty() {
                continue;
            }

            normalized_hints.push(trimmed.to_string());
            patterns.push(EndpointPattern {
                name: format!("custom_hint:{trimmed}"),
                regex: Regex::new(&format!(
                    r#"["'`]([^"'`]*{}[^"'`]*)["'`]"#,
                    regex::escape(trimmed)
                ))?,
                url_group: 1,
                method_group: None,
            });
        }

        Ok(Self {
            patterns,
            url_hints: normalized_hints,
        })
    }

    /// Discover API endpoints from JavaScript code
    #[must_use]
    pub fn discover(&self, js_code: &str) -> Vec<ApiEndpoint> {
        let mut endpoints = Vec::new();
        let mut seen = HashSet::new();

        for pattern in &self.patterns {
            for cap in pattern.regex.captures_iter(js_code) {
                if let Some(url_match) = cap.get(pattern.url_group) {
                    let url = url_match.as_str().to_string();

                    // Skip template literals with variables (can't resolve statically)
                    if url.contains("${") {
                        continue;
                    }

                    // Skip very short URLs (likely false positives)
                    if url.len() < 4 {
                        continue;
                    }

                    if pattern.name.starts_with("custom_hint:")
                        && !url.starts_with('/')
                        && !url.starts_with("http://")
                        && !url.starts_with("https://")
                    {
                        continue;
                    }

                    let method = pattern
                        .method_group
                        .and_then(|group| cap.get(group))
                        .map(|m| m.as_str().to_uppercase());

                    if seen.insert((url.clone(), method.clone())) {
                        endpoints.push(ApiEndpoint {
                            url,
                            method,
                            source: pattern.name.clone(),
                        });
                    }
                }
            }
        }

        endpoints.sort_by(|a, b| a.url.cmp(&b.url));
        endpoints
    }

    /// Discover endpoints from HTML (extracts inline scripts and external script URLs)
    #[must_use]
    pub fn discover_from_html(&self, html: &str) -> Vec<ApiEndpoint> {
        use scraper::{Html, Selector};

        let mut all_endpoints = Vec::new();
        let document = Html::parse_document(html);

        // Extract inline scripts
        if let Ok(script_selector) = Selector::parse("script") {
            for script in document.select(&script_selector) {
                // Skip external scripts (we can't fetch them... yet)
                if script.value().attr("src").is_some() {
                    continue;
                }

                let script_content = script.text().collect::<String>();
                let endpoints = self.discover(&script_content);
                all_endpoints.extend(endpoints);
            }
        }

        all_endpoints
    }

    /// Score an endpoint for likelihood of containing useful data
    /// Higher score = more likely to be useful
    #[must_use]
    pub fn score_endpoint(endpoint: &ApiEndpoint) -> i32 {
        let mut score = 0;

        // Prefer GET requests (more likely to return data)
        if let Some(ref method) = endpoint.method {
            if method == "GET" {
                score += 10;
            }
        } else {
            // No method specified, might be GET
            score += 5;
        }

        // Prefer /api/ paths
        if endpoint.url.contains("/api/") {
            score += 20;
        }

        // Prefer GraphQL
        if endpoint.url.contains("graphql") {
            score += 15;
        }

        // Prefer data-related keywords
        for keyword in &["data", "list", "get", "fetch", "load", "users", "items"] {
            if endpoint.url.to_lowercase().contains(keyword) {
                score += 5;
            }
        }

        // Penalize very long URLs (likely to be specific queries)
        if endpoint.url.len() > 100 {
            score -= 10;
        }

        // Penalize URLs with query params (might be incomplete)
        if endpoint.url.contains('?') && !endpoint.url.contains('=') {
            score -= 5;
        }

        if endpoint.source.starts_with("custom_hint:") {
            score += 25;
        }

        score
    }

    /// Return true when the endpoint matches a user-provided URL hint.
    #[must_use]
    pub fn matches_hint(&self, endpoint: &ApiEndpoint) -> bool {
        let endpoint_url = endpoint.url.to_lowercase();
        self.url_hints
            .iter()
            .any(|hint| endpoint_url.contains(&hint.to_lowercase()))
    }
}

fn default_patterns() -> Result<Vec<EndpointPattern>> {
    Ok(vec![
        // fetch() calls: fetch("/api/users"), fetch(`/api/${id}`)
        EndpointPattern {
            name: "fetch".to_string(),
            regex: Regex::new(r#"fetch\s*\(\s*["'`]([^"'`]+)["'`]"#)?,
            url_group: 1,
            method_group: None,
        },
        // fetch with method: fetch("/api/users", {method: "POST"})
        EndpointPattern {
            name: "fetch_with_method".to_string(),
            regex: Regex::new(
                r#"fetch\s*\(\s*["'`]([^"'`]+)["'`]\s*,\s*\{[^}]*method:\s*["'](\w+)["']"#,
            )?,
            url_group: 1,
            method_group: Some(2),
        },
        // axios: axios.get("/api/users"), axios.post("/api/users")
        EndpointPattern {
            name: "axios_method".to_string(),
            regex: Regex::new(r#"axios\.(\w+)\s*\(\s*["'`]([^"'`]+)["'`]"#)?,
            url_group: 2,
            method_group: Some(1),
        },
        // axios: axios({url: "/api/users", method: "GET"})
        EndpointPattern {
            name: "axios_config".to_string(),
            regex: Regex::new(
                r#"axios\s*\(\s*\{[^}]*url:\s*["'`]([^"'`]+)["'`][^}]*method:\s*["'](\w+)["']"#,
            )?,
            url_group: 1,
            method_group: Some(2),
        },
        // XMLHttpRequest: xhr.open("GET", "/api/users")
        EndpointPattern {
            name: "xhr_open".to_string(),
            regex: Regex::new(r#"\.open\s*\(\s*["'](\w+)["']\s*,\s*["'`]([^"'`]+)["'`]"#)?,
            url_group: 2,
            method_group: Some(1),
        },
        // jQuery AJAX: $.ajax({url: "/api/users", type: "GET"})
        EndpointPattern {
            name: "jquery_ajax".to_string(),
            regex: Regex::new(
                r#"\$\.ajax\s*\(\s*\{[^}]*url:\s*["'`]([^"'`]+)["'`][^}]*type:\s*["'](\w+)["']"#,
            )?,
            url_group: 1,
            method_group: Some(2),
        },
        // GraphQL: Common GraphQL endpoint patterns
        EndpointPattern {
            name: "graphql_endpoint".to_string(),
            regex: Regex::new(r#"["'`](/graphql|/__graphql|/api/graphql)["'`]"#)?,
            url_group: 1,
            method_group: None,
        },
        // Base URL configuration: baseURL: "https://api.example.com"
        EndpointPattern {
            name: "base_url".to_string(),
            regex: Regex::new(r#"baseURL:\s*["'`](https?://[^"'`]+)["'`]"#)?,
            url_group: 1,
            method_group: None,
        },
        // API endpoint in string: const API_URL = "/api/users"
        EndpointPattern {
            name: "api_constant".to_string(),
            regex: Regex::new(r#"(?:API_URL|ENDPOINT|API_ENDPOINT)\s*=\s*["'`]([^"'`]+)["'`]"#)?,
            url_group: 1,
            method_group: None,
        },
        // Google batchexecute: "/_/FlightSearch/data/batchexecute"
        EndpointPattern {
            name: "google_batchexecute".to_string(),
            regex: Regex::new(r#"["'`](/_/[A-Za-z]+/data/batchexecute)["'`]"#)?,
            url_group: 1,
            method_group: None,
        },
        // Google RPC-style: "https://www.google.com/_/..." or "/_/..."
        EndpointPattern {
            name: "google_rpc".to_string(),
            regex: Regex::new(r#"["'`]((?:https?://[^"'`]+)?/_/[A-Za-z]+[^"'`]*)["'`]"#)?,
            url_group: 1,
            method_group: None,
        },
        // Google Travel/Flights API patterns
        EndpointPattern {
            name: "google_travel".to_string(),
            regex: Regex::new(r#"["'`](/travel/[a-z]+/(?:search|offers|booking)[^"'`]*)["'`]"#)?,
            url_group: 1,
            method_group: None,
        },
        // gRPC-Web endpoints
        EndpointPattern {
            name: "grpc_web".to_string(),
            regex: Regex::new(r#"["'`]([^"'`]+\.grpc\.web[^"'`]*)["'`]"#)?,
            url_group: 1,
            method_group: None,
        },
        // Internal data endpoints: /data/, /api/v*, /_ah/
        EndpointPattern {
            name: "internal_data".to_string(),
            regex: Regex::new(r#"["'`](/(?:data|_ah|api/v\d+)/[^"'`]+)["'`]"#)?,
            url_group: 1,
            method_group: None,
        },
    ])
}

impl Default for ApiDiscovery {
    fn default() -> Self {
        Self::new().expect("Failed to create API discovery engine")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fetch_detection() {
        let discovery = ApiDiscovery::new().unwrap();
        let code = r#"
            fetch("/api/users");
            fetch('/api/posts');
            fetch(`/api/comments`);
        "#;

        let endpoints = discovery.discover(code);
        assert_eq!(endpoints.len(), 3);
        assert!(endpoints.iter().any(|e| e.url == "/api/users"));
        assert!(endpoints.iter().any(|e| e.url == "/api/posts"));
        assert!(endpoints.iter().any(|e| e.url == "/api/comments"));
    }

    #[test]
    fn test_axios_detection() {
        let discovery = ApiDiscovery::new().unwrap();
        let code = r#"
            axios.get("/api/users");
            axios.post("/api/users", data);
            axios({url: "/api/settings", method: "GET"});
        "#;

        let endpoints = discovery.discover(code);
        assert!(
            endpoints
                .iter()
                .any(|e| e.url == "/api/users" && e.method == Some("GET".to_string()))
        );
        assert!(
            endpoints
                .iter()
                .any(|e| e.url == "/api/users" && e.method == Some("POST".to_string()))
        );
        assert!(
            endpoints
                .iter()
                .any(|e| e.url == "/api/settings" && e.method == Some("GET".to_string()))
        );
    }

    #[test]
    fn test_skip_template_literals() {
        let discovery = ApiDiscovery::new().unwrap();
        let code = r#"
            fetch(`/api/users/${userId}`);  // Should be skipped
            fetch("/api/users");             // Should be found
        "#;

        let endpoints = discovery.discover(code);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].url, "/api/users");
    }

    #[test]
    fn test_endpoint_scoring() {
        let ep1 = ApiEndpoint {
            url: "/api/data".to_string(),
            method: Some("GET".to_string()),
            source: "fetch".to_string(),
        };

        let ep2 = ApiEndpoint {
            url: "/some/path".to_string(),
            method: Some("POST".to_string()),
            source: "axios".to_string(),
        };

        assert!(ApiDiscovery::score_endpoint(&ep1) > ApiDiscovery::score_endpoint(&ep2));
    }

    #[test]
    fn test_custom_url_hints_find_matching_strings() {
        let discovery = ApiDiscovery::with_url_hints(&["/custom/data".to_string()]).unwrap();
        let code = r#"
            const endpoint = "/custom/data/items";
            const ignored = "not-an-endpoint";
        "#;

        let endpoints = discovery.discover(code);
        assert!(endpoints.iter().any(|e| e.url == "/custom/data/items"));
        assert!(
            endpoints
                .iter()
                .any(|e| e.source == "custom_hint:/custom/data")
        );
    }

    #[test]
    fn test_custom_hints_do_not_duplicate_existing_discoveries() {
        let discovery = ApiDiscovery::with_url_hints(&["/api/users".to_string()]).unwrap();
        let code = r#"fetch("/api/users");"#;

        let endpoints = discovery.discover(code);
        let users_matches = endpoints.iter().filter(|e| e.url == "/api/users").count();

        assert_eq!(users_matches, 1);
    }
}
