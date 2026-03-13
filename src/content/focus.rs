#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
//! Query-focused extraction: BM25-lite scoring to keep only relevant sections.
//!
//! Splits markdown into heading-bounded sections, scores each against a natural-
//! language query, and returns the top-scoring sections with omitted-section
//! markers replacing the rest.
//!
//! # Example
//!
//! ```rust
//! use nab::content::focus::extract_focused;
//!
//! let md = "# Intro\n\nWelcome.\n\n## Auth\n\nBearer tokens.\n\n## Styling\n\nCSS.\n\n## Deploy\n\nDocker.\n\n## Logging\n\nJSON.\n\n## Metrics\n\nProm.";
//! let result = extract_focused(md, "authentication bearer tokens");
//! assert!(result.markdown.contains("## Auth"));
//! assert!(result.omitted_sections > 0);
//! ```

// ── Constants ─────────────────────────────────────────────────────────────────

/// BM25 term-frequency saturation constant.
const K1: f64 = 1.2;

/// BM25 length-normalisation constant.
const B: f64 = 0.75;

/// Maximum fraction of sections to keep.
const KEEP_FRACTION: f64 = 0.20;

/// Minimum sections always kept when filtering.
const MIN_KEEP: usize = 3;

/// Maximum sections kept regardless of corpus size.
const MAX_KEEP: usize = 10;

/// Maximum query length (chars) before truncation.
const MAX_QUERY_CHARS: usize = 200;

/// Diff markers that exempt a section from filtering.
const DIFF_MARKERS: &[&str] = &["[changed]", "[added]", "[removed]"];

// ── Public types ──────────────────────────────────────────────────────────────

/// Result of query-focused extraction.
#[derive(Debug, Clone)]
pub struct FocusResult {
    /// The filtered markdown with omitted-section markers.
    pub markdown: String,
    /// Number of sections dropped from the original document.
    pub omitted_sections: usize,
    /// Total section count before filtering.
    pub total_sections: usize,
}

// ── Internal types ────────────────────────────────────────────────────────────

/// Internal section representation.
///
/// `level` is retained for use by the upcoming `budget` module (T1.2), which
/// needs heading depth to apply the 30 % heading-budget cap.
#[derive(Debug)]
#[allow(dead_code)] // `level` consumed by budget module (T1.2)
struct Section {
    /// Raw text including heading line (if any).
    raw: String,
    /// Heading level (1–6), or 0 for the implicit intro before first heading.
    level: u8,
    /// Whether this section contains a diff marker and must be kept.
    has_diff_marker: bool,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Extract sections relevant to `query` from `markdown`.
///
/// When `query` is empty or the document has ≤3 sections the full document is
/// returned unchanged with `omitted_sections = 0`.
///
/// # Algorithm
///
/// 1. Split on heading boundaries (`#`…`######`).
/// 2. Score each section with BM25-lite against query terms.
/// 3. Always keep the first section and any section containing a diff marker.
/// 4. Keep the top 20 % of remaining sections by score (min 3, max 10 total).
/// 5. Replace consecutive dropped sections with a single omitted-section marker.
#[must_use]
pub fn extract_focused(markdown: &str, query: &str) -> FocusResult {
    let query = truncate_query(query);
    if query.is_empty() {
        return passthrough(markdown);
    }

    let sections = split_sections(markdown);
    let total = sections.len();

    if total <= MIN_KEEP {
        return FocusResult {
            markdown: markdown.to_owned(),
            omitted_sections: 0,
            total_sections: total,
        };
    }

    let keep_mask = build_keep_mask(&sections, query);
    assemble_output(sections, &keep_mask, total)
}

// ── Section splitting ─────────────────────────────────────────────────────────

/// Split `markdown` into sections at heading boundaries.
///
/// A section owns its heading line plus all non-heading lines until the next
/// heading of any level.  Content that appears before the first heading forms an
/// implicit section with `level = 0`.
fn split_sections(markdown: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current = String::new();
    let mut current_level: u8 = 0;

    for line in markdown.lines() {
        if let Some(level) = heading_level(line) {
            if !current.is_empty() {
                sections.push(build_section(current, current_level));
            }
            current = format!("{line}\n");
            current_level = level;
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }

    if !current.is_empty() {
        sections.push(build_section(current, current_level));
    }

    sections
}

/// Parse a heading prefix and return its level (1–6), or `None`.
fn heading_level(line: &str) -> Option<u8> {
    let stripped = line.trim_start_matches('#');
    let hashes = line.len() - stripped.len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    // A valid ATX heading requires a space after the hashes.
    if stripped.starts_with(' ') {
        Some(hashes as u8)
    } else {
        None
    }
}

fn build_section(raw: String, level: u8) -> Section {
    let has_diff_marker = contains_diff_marker(&raw);
    Section {
        raw,
        level,
        has_diff_marker,
    }
}

fn contains_diff_marker(text: &str) -> bool {
    DIFF_MARKERS.iter().any(|m| text.contains(m))
}

// ── BM25-lite scoring ─────────────────────────────────────────────────────────

/// Score all sections against query terms using BM25-lite.
///
/// URLs inside markdown links are stripped before tokenisation so that
/// link-heavy sections (navigation, "See also") don't get artificially high
/// scores from URL tokens that happen to share substrings with the query.
fn score_sections(sections: &[Section], query_terms: &[String]) -> Vec<f64> {
    let stripped: Vec<String> = sections.iter().map(|s| strip_urls(&s.raw)).collect();
    let tokenised: Vec<Vec<String>> = stripped.iter().map(|s| tokenise(s)).collect();

    let avg_len = average_len(&tokenised);
    let n = sections.len() as f64;

    let idf: Vec<f64> = query_terms
        .iter()
        .map(|term| compute_idf(term, &tokenised, n))
        .collect();

    tokenised
        .iter()
        .map(|tokens| bm25_score(tokens, query_terms, &idf, avg_len))
        .collect()
}

/// Strip markdown link URLs: `[text](url)` -> `text`, bare `https?://...` -> empty.
fn strip_urls(text: &str) -> String {
    // Two-pass replacement keeps the implementation O(n) with a single allocation.
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Inline link: [text](url) — keep only "text".
        if bytes[i] == b'['
            && let Some((close_bracket, _open_paren, close_paren)) = find_inline_link(bytes, i)
        {
            out.push_str(&text[i + 1..close_bracket]);
            i = close_paren + 1;
            continue;
        }
        // Bare URL scheme: skip until whitespace.
        if bytes[i..].starts_with(b"http://") || bytes[i..].starts_with(b"https://") {
            while i < len && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            continue;
        }
        out.push(text[i..].chars().next().unwrap_or_default());
        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }

    out
}

/// Find `[text](url)` components: returns `(close_bracket, open_paren, close_paren)`.
fn find_inline_link(bytes: &[u8], start: usize) -> Option<(usize, usize, usize)> {
    let len = bytes.len();
    // Find `](`
    let mut j = start + 1;
    while j < len && bytes[j] != b']' {
        j += 1;
    }
    if j >= len || j + 1 >= len || bytes[j + 1] != b'(' {
        return None;
    }
    let close_bracket = j;
    let open_paren = j + 1;
    // Find matching `)`
    let mut k = open_paren + 1;
    while k < len && bytes[k] != b')' {
        k += 1;
    }
    if k >= len {
        return None;
    }
    Some((close_bracket, open_paren, k))
}

/// Tokenise text into lowercase ASCII words (≥2 chars).
fn tokenise(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(str::to_lowercase)
        .collect()
}

fn average_len(tokenised: &[Vec<String>]) -> f64 {
    if tokenised.is_empty() {
        return 1.0;
    }
    let total: usize = tokenised.iter().map(Vec::len).sum();
    (total as f64 / tokenised.len() as f64).max(1.0)
}

/// IDF with Laplace smoothing: `ln((N + 1) / (df + 1))`.
fn compute_idf(term: &str, tokenised: &[Vec<String>], n: f64) -> f64 {
    let df = tokenised
        .iter()
        .filter(|tokens| tokens.iter().any(|t| t == term))
        .count() as f64;
    ((n + 1.0) / (df + 1.0)).ln().max(0.0)
}

/// BM25 score for a single section against all query terms.
fn bm25_score(tokens: &[String], query_terms: &[String], idf: &[f64], avg_len: f64) -> f64 {
    let doc_len = tokens.len() as f64;
    let norm = 1.0 - B + B * doc_len / avg_len;

    query_terms
        .iter()
        .enumerate()
        .map(|(i, term)| {
            let tf = tokens.iter().filter(|t| *t == term).count() as f64;
            let tf_norm = tf * (K1 + 1.0) / (tf + K1 * norm);
            idf[i] * tf_norm
        })
        .sum()
}

// ── Keep mask ─────────────────────────────────────────────────────────────────

/// Build a boolean mask of which sections to keep.
///
/// Always keeps index 0 (intro / page title) and any section with a diff marker.
/// Remaining quota is filled by top BM25-scored sections.
fn build_keep_mask(sections: &[Section], query: &str) -> Vec<bool> {
    let query_terms = tokenise(query);
    let scores = score_sections(sections, &query_terms);

    let n = sections.len();
    let quota = compute_quota(n);

    let mut keep = vec![false; n];

    // Index 0 is always kept (page title / intro).
    keep[0] = true;

    // Diff-marker sections are unconditionally kept.
    for (i, s) in sections.iter().enumerate().skip(1) {
        if s.has_diff_marker {
            keep[i] = true;
        }
    }

    let auto_kept = keep.iter().filter(|&&k| k).count();
    let from_ranked = quota.saturating_sub(auto_kept);

    if from_ranked == 0 {
        return keep;
    }

    // Rank non-auto-kept sections by score descending.
    let mut ranked: Vec<(f64, usize)> = scores
        .iter()
        .enumerate()
        .skip(1)
        .filter(|&(idx, _)| !keep[idx])
        .map(|(idx, &score)| (score, idx))
        .collect();

    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    for (_, idx) in ranked.iter().take(from_ranked) {
        keep[*idx] = true;
    }

    keep
}

/// Compute how many sections to keep from the corpus of `n`.
fn compute_quota(n: usize) -> usize {
    let by_fraction = ((n as f64 * KEEP_FRACTION).ceil() as usize).max(MIN_KEEP);
    by_fraction.min(MAX_KEEP)
}

// ── Output assembly ───────────────────────────────────────────────────────────

fn assemble_output(sections: Vec<Section>, keep: &[bool], total: usize) -> FocusResult {
    let mut out = String::new();
    let mut omitted = 0usize;
    let mut run = 0usize; // consecutive omitted sections

    let flush_run = |out: &mut String, run: usize| {
        if run > 0 {
            use std::fmt::Write;
            let _ = write!(
                out,
                "\n[{run} section{} omitted — use focus parameter to adjust]\n\n",
                if run == 1 { "" } else { "s" }
            );
        }
    };

    for (i, section) in sections.into_iter().enumerate() {
        if keep[i] {
            flush_run(&mut out, run);
            run = 0;
            out.push_str(&section.raw);
        } else {
            run += 1;
            omitted += 1;
        }
    }
    flush_run(&mut out, run);

    FocusResult {
        markdown: out,
        omitted_sections: omitted,
        total_sections: total,
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn truncate_query(query: &str) -> &str {
    let q = query.trim();
    if q.len() <= MAX_QUERY_CHARS {
        q
    } else {
        // Truncate at a char boundary.
        let mut end = MAX_QUERY_CHARS;
        while !q.is_char_boundary(end) {
            end -= 1;
        }
        &q[..end]
    }
}

fn passthrough(markdown: &str) -> FocusResult {
    FocusResult {
        markdown: markdown.to_owned(),
        omitted_sections: 0,
        total_sections: 0,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_doc(sections: &[(&str, &str)]) -> String {
        sections
            .iter()
            .map(|(heading, body)| {
                if heading.is_empty() {
                    format!("{body}\n\n")
                } else {
                    format!("{heading}\n\n{body}\n\n")
                }
            })
            .collect()
    }

    // ── heading_level ─────────────────────────────────────────────────────────

    #[test]
    fn heading_level_returns_none_for_plain_text() {
        assert_eq!(heading_level("hello world"), None);
    }

    #[test]
    fn heading_level_parses_h1_through_h6() {
        for (line, expected) in [
            ("# H1", 1u8),
            ("## H2", 2),
            ("### H3", 3),
            ("#### H4", 4),
            ("##### H5", 5),
            ("###### H6", 6),
        ] {
            assert_eq!(heading_level(line), Some(expected), "failed for: {line}");
        }
    }

    #[test]
    fn heading_level_requires_space_after_hashes() {
        // `##nospace` is not a valid ATX heading.
        assert_eq!(heading_level("##nospace"), None);
    }

    #[test]
    fn heading_level_returns_none_for_more_than_six_hashes() {
        assert_eq!(heading_level("####### Too deep"), None);
    }

    // ── split_sections ────────────────────────────────────────────────────────

    #[test]
    fn split_sections_produces_one_section_per_heading() {
        let md = "# Title\n\nIntro.\n\n## Auth\n\nBearer tokens.\n\n## Styling\n\nCSS.";
        let sections = split_sections(md);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].level, 1);
        assert_eq!(sections[1].level, 2);
        assert_eq!(sections[2].level, 2);
    }

    #[test]
    fn split_sections_captures_intro_before_first_heading() {
        let md = "Preamble text.\n\n## Section\n\nBody.";
        let sections = split_sections(md);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].level, 0);
        assert!(sections[0].raw.contains("Preamble"));
    }

    #[test]
    fn split_sections_handles_no_headings() {
        let md = "Just plain text.\n\nNo headings here.";
        let sections = split_sections(md);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].level, 0);
    }

    #[test]
    fn split_sections_handles_empty_input() {
        assert!(split_sections("").is_empty());
    }

    #[test]
    fn split_sections_handles_adjacent_headings() {
        let md = "## A\n\n## B\n\nBody B.\n";
        let sections = split_sections(md);
        assert_eq!(sections.len(), 2);
        assert!(sections[0].raw.trim_end().ends_with("## A"));
    }

    // ── strip_urls ────────────────────────────────────────────────────────────

    #[test]
    fn strip_urls_removes_inline_link_url_but_keeps_anchor_text() {
        let input = "See [authentication guide](https://example.com/auth) for details.";
        let out = strip_urls(input);
        assert!(out.contains("authentication guide"));
        assert!(!out.contains("https://"));
    }

    #[test]
    fn strip_urls_removes_bare_https_urls() {
        let input = "Visit https://example.com/some/path for more info.";
        let out = strip_urls(input);
        assert!(!out.contains("https://"));
        assert!(out.contains("Visit"));
        assert!(out.contains("for more info"));
    }

    #[test]
    fn strip_urls_leaves_normal_text_untouched() {
        let input = "Bearer tokens are used for authentication.";
        assert_eq!(strip_urls(input), input);
    }

    // ── tokenise ─────────────────────────────────────────────────────────────

    #[test]
    fn tokenise_lowercases_and_splits_on_non_alphanumeric() {
        let tokens = tokenise("Hello, World! 123");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"123".to_string()));
    }

    #[test]
    fn tokenise_filters_single_char_words() {
        let tokens = tokenise("a b c de");
        assert!(!tokens.contains(&"a".to_string()));
        assert!(tokens.contains(&"de".to_string()));
    }

    // ── BM25 scoring ──────────────────────────────────────────────────────────

    #[test]
    fn bm25_score_returns_zero_for_no_matching_terms() {
        let tokens: Vec<String> = vec!["hello".into(), "world".into()];
        let query_terms: Vec<String> = vec!["authentication".into()];
        let idf = vec![1.0_f64];
        let score = bm25_score(&tokens, &query_terms, &idf, 2.0);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn bm25_score_is_higher_for_more_matching_terms() {
        let tokens_rich: Vec<String> = vec![
            "authentication".into(),
            "bearer".into(),
            "token".into(),
            "auth".into(),
        ];
        let tokens_poor: Vec<String> = vec!["authentication".into(), "styling".into()];
        let query_terms: Vec<String> = vec!["authentication".into(), "bearer".into()];
        let idf = vec![1.0, 1.0];
        let avg = 3.0;
        let score_rich = bm25_score(&tokens_rich, &query_terms, &idf, avg);
        let score_poor = bm25_score(&tokens_poor, &query_terms, &idf, avg);
        assert!(
            score_rich > score_poor,
            "rich={score_rich}, poor={score_poor}"
        );
    }

    // ── compute_quota ─────────────────────────────────────────────────────────

    #[test]
    fn compute_quota_applies_min_of_3_for_small_corpus() {
        // 10 sections → 20% = 2 → clamped to MIN_KEEP=3
        assert_eq!(compute_quota(10), 3);
    }

    #[test]
    fn compute_quota_applies_max_of_10_for_large_corpus() {
        // 100 sections → 20% = 20 → clamped to MAX_KEEP=10
        assert_eq!(compute_quota(100), 10);
    }

    #[test]
    fn compute_quota_is_correct_in_mid_range() {
        // 25 sections → 20% = 5 → no clamping needed
        assert_eq!(compute_quota(25), 5);
    }

    // ── extract_focused ───────────────────────────────────────────────────────

    #[test]
    fn extract_focused_passthrough_on_empty_query() {
        let md = "# Title\n\n## Auth\n\nUse tokens.\n\n## Styling\n\nUse CSS.";
        let result = extract_focused(md, "");
        assert_eq!(result.markdown, md);
        assert_eq!(result.omitted_sections, 0);
    }

    #[test]
    fn extract_focused_passthrough_on_three_or_fewer_sections() {
        let md = "# Title\n\nIntro.\n\n## Auth\n\nBearers.\n\n## Other\n\nThing.";
        // Exactly 3 sections (title + auth + other) — should not filter.
        let sections = split_sections(md);
        assert_eq!(sections.len(), 3);
        let result = extract_focused(md, "authentication");
        assert_eq!(result.omitted_sections, 0);
    }

    #[test]
    fn extract_focused_keeps_relevant_section_and_drops_irrelevant() {
        let md = make_doc(&[
            ("# Guide", "Welcome to the guide."),
            (
                "## Authentication",
                "Bearer tokens are required for every API call.",
            ),
            ("## Installation", "Run npm install to set up the package."),
            ("## Usage", "Call the API endpoint with your token."),
            ("## Contributing", "Fork the repo and open a pull request."),
            ("## License", "MIT license applies."),
        ]);
        let result = extract_focused(&md, "authentication bearer tokens api");
        assert!(
            result.markdown.contains("## Authentication"),
            "relevant section missing"
        );
        assert!(result.omitted_sections > 0, "nothing was omitted");
        assert!(result.total_sections == 6);
    }

    #[test]
    fn extract_focused_always_keeps_first_section() {
        let md = make_doc(&[
            ("# Title", "Page title, not about auth."),
            ("## Authentication", "Bearer tokens."),
            ("## Styling", "CSS variables."),
            ("## Deployment", "Deploy to Kubernetes."),
            ("## Testing", "Run the test suite."),
        ]);
        let result = extract_focused(&md, "authentication bearer");
        assert!(
            result.markdown.contains("# Title"),
            "first section must always be kept"
        );
    }

    #[test]
    fn extract_focused_omitted_marker_contains_accurate_count() {
        let md = make_doc(&[
            ("# Docs", "Documentation home."),
            ("## Authentication", "Bearer tokens for auth."),
            ("## Pagination", "Use cursor-based pagination."),
            ("## Rate Limiting", "Max 100 req/s."),
            ("## Errors", "HTTP status codes."),
            ("## Webhooks", "POST events to your endpoint."),
        ]);
        let result = extract_focused(&md, "authentication bearer token");
        // The marker text should reflect the number of omitted sections.
        // Each marker looks like "[N section(s) omitted — ...]"; extract N.
        let total_in_markers: usize = result
            .markdown
            .lines()
            .filter(|l| l.contains("omitted"))
            .map(|l| {
                l.split(|c: char| !c.is_ascii_digit())
                    .find_map(|w| w.parse::<usize>().ok())
                    .unwrap_or(0)
            })
            .sum();
        assert_eq!(
            total_in_markers, result.omitted_sections,
            "marker counts don't sum to omitted_sections"
        );
    }

    #[test]
    fn extract_focused_diff_marker_sections_always_kept() {
        let md = make_doc(&[
            ("# Changelog", "Version history."),
            ("## v2.0", "[changed] Authentication flow redesigned."),
            ("## Styling", "CSS variables."),
            ("## Deployment", "Deploy to Kubernetes."),
            ("## Testing", "Run the test suite."),
            ("## Performance", "Benchmark results."),
        ]);
        // Query is unrelated to [changed] section content.
        let result = extract_focused(&md, "kubernetes deployment");
        assert!(
            result.markdown.contains("[changed]"),
            "diff-marker section must survive regardless of score"
        );
    }

    #[test]
    fn extract_focused_all_diff_markers_kept_regardless_of_quota() {
        // Build a doc where every non-intro section has a diff marker.
        let md = make_doc(&[
            ("# Changelog", "Overview."),
            ("## v1.0", "[added] Initial release."),
            ("## v1.1", "[changed] Auth flow."),
            ("## v1.2", "[removed] Deprecated endpoint."),
            ("## v1.3", "[added] New webhook support."),
        ]);
        let result = extract_focused(&md, "completely unrelated query xyz");
        // All 3 diff-marker sections must be present.
        assert!(result.markdown.contains("[added] Initial release"));
        assert!(result.markdown.contains("[changed] Auth flow"));
        assert!(result.markdown.contains("[removed] Deprecated endpoint"));
        assert!(result.markdown.contains("[added] New webhook support"));
    }

    #[test]
    fn extract_focused_no_headings_returns_full_content() {
        let md = "This is plain text.\n\nNo headings here.\n\nJust paragraphs.";
        let result = extract_focused(md, "authentication");
        assert_eq!(result.markdown, md);
        assert_eq!(result.omitted_sections, 0);
    }

    #[test]
    fn extract_focused_query_truncated_at_200_chars() {
        let long_query = "a".repeat(300);
        let md = make_doc(&[
            ("# Title", "Intro."),
            ("## Auth", "Bearer tokens."),
            ("## Install", "npm install."),
            ("## Usage", "Call the API."),
            ("## Deploy", "Push to production."),
        ]);
        // Must not panic; results are valid.
        let result = extract_focused(&md, &long_query);
        assert!(result.total_sections == 5 || result.total_sections == 0);
    }

    #[test]
    fn extract_focused_url_stripped_before_scoring() {
        // A section packed with irrelevant URLs should not outscore a section
        // with actual query term matches.
        let md = make_doc(&[
            ("# Guide", "Introduction."),
            (
                "## See Also",
                "- [link one](https://authentication.example.com/bearer/token/one)\n\
                 - [link two](https://authentication.example.com/bearer/token/two)\n\
                 - [link three](https://authentication.example.com/bearer/token/three)",
            ),
            (
                "## Authentication",
                "Bearer tokens are used for every authenticated request.",
            ),
            ("## Styling", "CSS classes and variables."),
            ("## Performance", "Benchmark numbers here."),
        ]);
        let result = extract_focused(&md, "authentication bearer token");
        // The real Auth section must be kept.
        assert!(
            result.markdown.contains("## Authentication"),
            "Auth section must be kept"
        );
    }

    #[test]
    fn extract_focused_single_section_returns_unchanged() {
        let md = "## Auth\n\nBearer token explanation.";
        let result = extract_focused(md, "authentication");
        assert_eq!(result.markdown, md);
        assert_eq!(result.omitted_sections, 0);
    }

    #[test]
    fn extract_focused_omitted_sections_plus_kept_equals_total() {
        let md = make_doc(&[
            ("# Docs", "Intro."),
            ("## Auth", "Bearer tokens."),
            ("## Pagination", "Cursors."),
            ("## Rate Limiting", "Limits."),
            ("## Errors", "Status codes."),
            ("## Webhooks", "Events."),
            ("## Changelog", "History."),
            ("## Contributing", "Fork the repo."),
        ]);
        let result = extract_focused(&md, "authentication bearer");
        let kept = result.total_sections - result.omitted_sections;
        assert!(
            kept >= MIN_KEEP,
            "kept={kept} must be >= MIN_KEEP={MIN_KEEP}"
        );
        assert!(
            kept <= MAX_KEEP,
            "kept={kept} must be <= MAX_KEEP={MAX_KEEP}"
        );
        assert_eq!(
            kept + result.omitted_sections,
            result.total_sections,
            "kept + omitted != total"
        );
    }
}
