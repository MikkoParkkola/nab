#![allow(clippy::cast_precision_loss)]
//! Markdown link extraction, resolution, and relevance scoring.
//!
//! Parses `[text](url)` patterns from markdown, resolves relative URLs against
//! a base, filters by same-site policy (eTLD+1), and optionally ranks by
//! relevance to a focus query using BM25-lite term overlap.
//!
//! # Same-site policy
//!
//! Uses the `addr` crate which embeds Mozilla's public suffix list.
//! This correctly handles multi-part TLDs (`co.uk`, `github.io`, `amazonaws.com`)
//! where naive two-segment comparison fails.
//!
//! # Example
//!
//! ```rust
//! use nab::content::link_extract::extract_links;
//!
//! let md = "See [auth docs](https://docs.example.com/auth) and [setup](/setup).";
//! let links = extract_links(md, "https://docs.example.com/", None);
//! assert!(!links.is_empty());
//! assert_eq!(links[0].url, "https://docs.example.com/auth");
//! ```

use url::Url;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum links inspected per document to avoid O(n²) on link-heavy pages.
const MAX_LINKS_SCANNED: usize = 200;

// ── Public types ──────────────────────────────────────────────────────────────

/// A resolved, filtered, and optionally scored link extracted from markdown.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredLink {
    /// Absolute URL, normalized (no fragment).
    pub url: String,
    /// Anchor text from `[text](url)`.
    pub anchor: String,
    /// Relevance score: BM25-lite when a focus query is provided,
    /// otherwise descending position score (1.0 / (1 + position)).
    pub score: f64,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Extract, resolve, deduplicate, filter, and score links from `markdown`.
///
/// Only HTTP/HTTPS links on the same registered domain (eTLD+1) as `base_url`
/// are returned.  Fragments (`#…`) and non-HTTP schemes are excluded.
/// At most [`MAX_LINKS_SCANNED`] links are inspected; from those the top
/// results are returned, sorted by score descending.
///
/// When `focus_query` is `Some`, links are scored by BM25-lite overlap between
/// the anchor text and the query.  When `None`, earlier links score higher
/// (position-based ranking suitable for index pages).
#[must_use]
pub fn extract_links(markdown: &str, base_url: &str, focus_query: Option<&str>) -> Vec<ScoredLink> {
    let Ok(base) = Url::parse(base_url) else {
        return vec![];
    };

    let base_domain = registered_domain(&base);

    let raw = collect_raw_links(markdown, &base);
    let deduped = deduplicate(raw);
    score_links(deduped, focus_query, &base_domain)
}

// ── Link collection ───────────────────────────────────────────────────────────

/// Scan `markdown` for `[text](url)` patterns and resolve each URL.
///
/// Stops after [`MAX_LINKS_SCANNED`] valid entries to bound work on
/// link-heavy pages.
fn collect_raw_links(markdown: &str, base: &Url) -> Vec<(String, String)> {
    let mut links: Vec<(String, String)> = Vec::new();
    let bytes = markdown.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len && links.len() < MAX_LINKS_SCANNED {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }

        let Some((anchor, raw_url, end)) = parse_inline_link(markdown, bytes, i, len) else {
            i += 1;
            continue;
        };

        if let Some(resolved) = resolve_and_filter(&raw_url, base) {
            links.push((anchor, resolved));
        }

        i = end;
    }

    links
}

/// Parse `[text](url)` at position `start`.  Returns `(anchor, url, end_pos)`.
fn parse_inline_link(
    markdown: &str,
    bytes: &[u8],
    start: usize,
    len: usize,
) -> Option<(String, String, usize)> {
    // Find closing `]`.
    let mut j = start + 1;
    while j < len && bytes[j] != b']' {
        j += 1;
    }
    if j >= len || j + 1 >= len || bytes[j + 1] != b'(' {
        return None;
    }

    let anchor = markdown[start + 1..j].to_owned();
    let open_paren = j + 1;

    // Find matching `)`, skipping nested parens (titles etc.).
    let mut depth = 0usize;
    let mut k = open_paren + 1;
    while k < len {
        match bytes[k] {
            b'(' => depth += 1,
            b')' if depth > 0 => depth -= 1,
            b')' => break,
            _ => {}
        }
        k += 1;
    }
    if k >= len {
        return None;
    }

    // Strip optional title: `url "Title"` → take until first whitespace.
    let raw_href = markdown[open_paren + 1..k].trim();
    let url_part = raw_href
        .split_ascii_whitespace()
        .next()
        .unwrap_or("")
        .to_owned();

    Some((anchor, url_part, k + 1))
}

/// Resolve a raw href against the base URL and validate it is HTTP/HTTPS.
///
/// Returns `None` for fragments, non-HTTP schemes, and malformed URLs.
fn resolve_and_filter(raw: &str, base: &Url) -> Option<String> {
    if raw.is_empty() || raw.starts_with('#') {
        return None;
    }

    let resolved = base.join(raw).ok()?;

    if resolved.scheme() != "http" && resolved.scheme() != "https" {
        return None;
    }

    // Normalize: drop fragment.
    let mut clean = resolved.clone();
    clean.set_fragment(None);
    Some(clean.into())
}

// ── Deduplication ────────────────────────────────────────────────────────────

/// Remove duplicate URLs, keeping the first occurrence's anchor text.
fn deduplicate(links: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    links
        .into_iter()
        .filter(|(_, url)| seen.insert(url.clone()))
        .collect()
}

// ── Domain helpers ────────────────────────────────────────────────────────────

/// Extract the registered domain (eTLD+1) from a URL using Mozilla's PSL.
///
/// Returns an empty string when the URL has no recognisable registered domain
/// (e.g., bare IP addresses), causing same-site filtering to exclude the link.
fn registered_domain(url: &Url) -> String {
    let host = url.host_str().unwrap_or("");
    // `addr::parse_domain_name` returns a `DomainName` that exposes
    // `root()` — the eTLD+1.  For IP addresses `parse_domain_name` fails.
    addr::parse_domain_name(host)
        .ok()
        .and_then(|d| d.root().map(str::to_owned))
        .unwrap_or_default()
}

/// Return `true` when `url` shares the same eTLD+1 as `base_domain`.
fn is_same_site(url: &str, base_domain: &str) -> bool {
    if base_domain.is_empty() {
        return false;
    }
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    registered_domain(&parsed) == base_domain
}

// ── Scoring ──────────────────────────────────────────────────────────────────

/// Score and sort links, applying same-site filtering.
fn score_links(
    links: Vec<(String, String)>,
    focus_query: Option<&str>,
    base_domain: &str,
) -> Vec<ScoredLink> {
    let query_terms: Vec<String> = focus_query
        .filter(|q| !q.trim().is_empty())
        .map(|q| tokenise(q.trim()))
        .unwrap_or_default();

    let total = links.len();
    #[allow(clippy::cast_precision_loss)]
    let mut scored: Vec<ScoredLink> = links
        .into_iter()
        .enumerate()
        .filter(|(_, (_, url))| is_same_site(url, base_domain))
        .map(|(pos, (anchor, url))| {
            let score = if query_terms.is_empty() {
                // Position-based: earlier links score higher.
                1.0 / (1.0 + pos as f64 / total.max(1) as f64)
            } else {
                term_overlap_score(&anchor, &query_terms)
            };
            ScoredLink { url, anchor, score }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
}

// ── BM25-lite term overlap ────────────────────────────────────────────────────

/// Simple term-overlap score: fraction of query terms present in `text`.
///
/// Returns a value in `[0.0, 1.0]`.  Used for anchor-text relevance when a
/// focus query is provided.  Full BM25 is overkill for short anchor strings.
fn term_overlap_score(text: &str, query_terms: &[String]) -> f64 {
    if query_terms.is_empty() {
        return 0.0;
    }
    let anchor_tokens = tokenise(text);
    #[allow(clippy::cast_precision_loss)]
    let matched = query_terms
        .iter()
        .filter(|t| anchor_tokens.iter().any(|a| a == *t))
        .count() as f64;
    #[allow(clippy::cast_precision_loss)]
    let denom = query_terms.len() as f64;
    matched / denom
}

/// Tokenise text into lowercase words (≥2 chars).
fn tokenise(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(str::to_lowercase)
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::*;

    // ── parse_inline_link ─────────────────────────────────────────────────────

    #[test]
    fn parse_inline_link_extracts_anchor_and_url() {
        let md = "[auth guide](https://example.com/auth)";
        let bytes = md.as_bytes();
        let result = parse_inline_link(md, bytes, 0, md.len());
        let (anchor, url, _) = result.unwrap();
        assert_eq!(anchor, "auth guide");
        assert_eq!(url, "https://example.com/auth");
    }

    #[test]
    fn parse_inline_link_strips_title_attribute() {
        let md = r#"[text](https://example.com "My Title")"#;
        let bytes = md.as_bytes();
        let result = parse_inline_link(md, bytes, 0, md.len());
        let (_, url, _) = result.unwrap();
        assert_eq!(url, "https://example.com");
    }

    #[test]
    fn parse_inline_link_returns_none_for_reference_style() {
        // `[text][ref]` has no `(` after `]`.
        let md = "[text][ref]";
        let bytes = md.as_bytes();
        assert!(parse_inline_link(md, bytes, 0, md.len()).is_none());
    }

    #[test]
    fn parse_inline_link_handles_empty_url() {
        let md = "[text]()";
        let bytes = md.as_bytes();
        let result = parse_inline_link(md, bytes, 0, md.len());
        // Parses but url_part is "".
        let (_, url, _) = result.unwrap();
        assert_eq!(url, "");
    }

    // ── resolve_and_filter ────────────────────────────────────────────────────

    #[test]
    fn resolve_and_filter_resolves_relative_path() {
        let base = Url::parse("https://docs.example.com/api/").unwrap();
        let result = resolve_and_filter("auth", &base);
        assert_eq!(result, Some("https://docs.example.com/api/auth".to_owned()));
    }

    #[test]
    fn resolve_and_filter_resolves_absolute_path() {
        let base = Url::parse("https://docs.example.com/api/v1").unwrap();
        let result = resolve_and_filter("/setup", &base);
        assert_eq!(result, Some("https://docs.example.com/setup".to_owned()));
    }

    #[test]
    fn resolve_and_filter_keeps_full_url_unchanged() {
        let base = Url::parse("https://example.com/").unwrap();
        let url = "https://example.com/page";
        let result = resolve_and_filter(url, &base);
        assert_eq!(result, Some(url.to_owned()));
    }

    #[test]
    fn resolve_and_filter_strips_fragment() {
        let base = Url::parse("https://example.com/").unwrap();
        let result = resolve_and_filter("https://example.com/page#section", &base);
        assert_eq!(result, Some("https://example.com/page".to_owned()));
    }

    #[test]
    fn resolve_and_filter_rejects_fragment_only() {
        let base = Url::parse("https://example.com/").unwrap();
        assert!(resolve_and_filter("#section", &base).is_none());
    }

    #[test]
    fn resolve_and_filter_rejects_mailto_scheme() {
        let base = Url::parse("https://example.com/").unwrap();
        assert!(resolve_and_filter("mailto:user@example.com", &base).is_none());
    }

    #[test]
    fn resolve_and_filter_rejects_javascript_scheme() {
        let base = Url::parse("https://example.com/").unwrap();
        assert!(resolve_and_filter("javascript:void(0)", &base).is_none());
    }

    #[test]
    fn resolve_and_filter_rejects_empty_href() {
        let base = Url::parse("https://example.com/").unwrap();
        assert!(resolve_and_filter("", &base).is_none());
    }

    // ── registered_domain ────────────────────────────────────────────────────

    #[test]
    fn registered_domain_simple_tld() {
        let url = Url::parse("https://www.example.com/path").unwrap();
        assert_eq!(registered_domain(&url), "example.com");
    }

    #[test]
    fn registered_domain_subdomain_stripped() {
        let url = Url::parse("https://docs.api.example.com/").unwrap();
        assert_eq!(registered_domain(&url), "example.com");
    }

    #[test]
    fn registered_domain_co_uk_handled_correctly() {
        let url = Url::parse("https://www.example.co.uk/").unwrap();
        // PSL must recognise `co.uk` as public suffix → registered domain = `example.co.uk`.
        assert_eq!(registered_domain(&url), "example.co.uk");
    }

    #[test]
    fn registered_domain_github_io_handled_correctly() {
        // `github.io` is a public suffix; `user.github.io` is the registered domain.
        let url = Url::parse("https://user.github.io/project").unwrap();
        assert_eq!(registered_domain(&url), "user.github.io");
    }

    #[test]
    fn registered_domain_ip_address_returns_empty() {
        let url = Url::parse("http://192.168.1.1/admin").unwrap();
        // IP addresses have no registered domain.
        assert_eq!(registered_domain(&url), "");
    }

    // ── is_same_site ─────────────────────────────────────────────────────────

    #[test]
    fn is_same_site_same_domain_true() {
        assert!(is_same_site("https://docs.example.com/", "example.com"));
    }

    #[test]
    fn is_same_site_different_domain_false() {
        assert!(!is_same_site("https://evil.com/", "example.com"));
    }

    #[test]
    fn is_same_site_cross_subdomain_same_registered_domain_true() {
        assert!(is_same_site("https://api.example.com/v2", "example.com"));
    }

    #[test]
    fn is_same_site_empty_base_domain_false() {
        assert!(!is_same_site("https://example.com/", ""));
    }

    // ── term_overlap_score ────────────────────────────────────────────────────

    #[test]
    fn term_overlap_score_full_match_returns_one() {
        let terms = vec!["auth".to_owned(), "guide".to_owned()];
        let score = term_overlap_score("auth guide", &terms);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn term_overlap_score_partial_match() {
        let terms = vec!["auth".to_owned(), "guide".to_owned(), "tokens".to_owned()];
        let score = term_overlap_score("auth docs", &terms);
        // 1 out of 3 terms matched.
        assert!((score - 1.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn term_overlap_score_no_match_returns_zero() {
        let terms = vec!["authentication".to_owned()];
        let score = term_overlap_score("unrelated text", &terms);
        assert!(score.abs() < f64::EPSILON);
    }

    #[test]
    fn term_overlap_score_empty_query_returns_zero() {
        let score = term_overlap_score("some text", &[]);
        assert!(score.abs() < f64::EPSILON);
    }

    // ── deduplicate ───────────────────────────────────────────────────────────

    #[test]
    fn deduplicate_removes_duplicate_urls() {
        let links = vec![
            ("link 1".to_owned(), "https://example.com/a".to_owned()),
            ("link 2".to_owned(), "https://example.com/b".to_owned()),
            (
                "link 1 again".to_owned(),
                "https://example.com/a".to_owned(),
            ),
        ];
        let deduped = deduplicate(links);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].1, "https://example.com/a");
    }

    #[test]
    fn deduplicate_preserves_first_anchor_on_collision() {
        let links = vec![
            ("first anchor".to_owned(), "https://example.com/".to_owned()),
            (
                "second anchor".to_owned(),
                "https://example.com/".to_owned(),
            ),
        ];
        let deduped = deduplicate(links);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].0, "first anchor");
    }

    // ── extract_links (integration) ───────────────────────────────────────────

    #[test]
    fn extract_links_resolves_relative_url() {
        let md = "Read the [setup guide](/setup) first.";
        let links = extract_links(md, "https://docs.example.com/", None);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://docs.example.com/setup");
        assert_eq!(links[0].anchor, "setup guide");
    }

    #[test]
    fn extract_links_excludes_different_domain() {
        let md = "See [external site](https://other.com/page).";
        let links = extract_links(md, "https://example.com/", None);
        assert!(links.is_empty(), "cross-domain link must be excluded");
    }

    #[test]
    fn extract_links_includes_same_subdomain_cross() {
        let md = "See [api docs](https://api.example.com/v1).";
        let links = extract_links(md, "https://docs.example.com/", None);
        assert_eq!(
            links.len(),
            1,
            "same eTLD+1, different subdomain must be included"
        );
        assert_eq!(links[0].url, "https://api.example.com/v1");
    }

    #[test]
    fn extract_links_excludes_fragment_only() {
        let md = "Jump to [section](#intro).";
        let links = extract_links(md, "https://example.com/page", None);
        assert!(links.is_empty());
    }

    #[test]
    fn extract_links_strips_fragment_from_full_url() {
        let md = "[link](https://example.com/page#section)";
        let links = extract_links(md, "https://example.com/", None);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com/page");
    }

    #[test]
    fn extract_links_deduplicates() {
        let md = "[a](https://example.com/page) and [b](https://example.com/page)";
        let links = extract_links(md, "https://example.com/", None);
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn extract_links_focus_query_raises_score_of_matching_anchor() {
        let md = "[Authentication Guide](https://example.com/auth) and \
                  [Installation](https://example.com/install)";
        let links = extract_links(md, "https://example.com/", Some("authentication guide"));
        assert_eq!(links.len(), 2);
        // Auth link should score higher (anchor matches query).
        assert_eq!(links[0].url, "https://example.com/auth");
        assert!(
            links[0].score > links[1].score,
            "auth link must score higher than install"
        );
    }

    #[test]
    fn extract_links_no_focus_orders_by_position() {
        let md = "[second](https://example.com/second) and \
                  [first](https://example.com/first)";
        let links = extract_links(md, "https://example.com/", None);
        // Without focus, first link in document should score higher.
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url, "https://example.com/second");
    }

    #[test]
    fn extract_links_empty_markdown_returns_empty() {
        assert!(extract_links("", "https://example.com/", None).is_empty());
    }

    #[test]
    fn extract_links_bad_base_url_returns_empty() {
        let md = "[link](https://example.com/)";
        assert!(extract_links(md, "not-a-url", None).is_empty());
    }

    #[test]
    fn extract_links_respects_200_link_cap() {
        // Build a markdown with 300 links.
        let mut md = String::new();
        for i in 0..300 {
            write!(md, "[link {i}](https://example.com/{i}) ").unwrap();
        }
        let links = extract_links(&md, "https://example.com/", None);
        assert!(
            links.len() <= MAX_LINKS_SCANNED,
            "must not process more than {MAX_LINKS_SCANNED} links"
        );
    }

    #[test]
    fn extract_links_excludes_mailto() {
        let md = "[Email us](mailto:support@example.com)";
        let links = extract_links(md, "https://example.com/", None);
        assert!(links.is_empty());
    }

    #[test]
    fn extract_links_multiple_links_all_same_site() {
        let md = "[auth](https://docs.example.com/auth) \
                  [api](/api) \
                  [other](https://evil.org/trap)";
        let links = extract_links(md, "https://docs.example.com/", None);
        assert_eq!(links.len(), 2);
        let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
        assert!(urls.contains(&"https://docs.example.com/auth"));
        assert!(urls.contains(&"https://docs.example.com/api"));
    }
}
