//! Extraction quality scoring for LLM confidence calibration.
//!
//! Produces a composite `confidence` score (0.0–1.0) from four orthogonal signals
//! measured against the raw HTML source and the extracted markdown output.
//! The score is a weighted average:
//!
//! | Signal              | Weight | What it measures                                      |
//! |---------------------|--------|-------------------------------------------------------|
//! | `content_density`   | 0.35   | Meaningful text vs. raw HTML size                     |
//! | `structure`         | 0.25   | Headings, lists, code blocks in the markdown output   |
//! | `completeness`      | 0.25   | Main content captured (not just nav/footer fragments) |
//! | `encoding_quality`  | 0.15   | Clean Unicode — no mojibake or replacement characters |
//!
//! # Example
//!
//! ```rust
//! use nab::content::quality::score_extraction;
//!
//! let html  = b"<html><body><article><h1>Title</h1><p>Body text.</p></article></body></html>";
//! let md    = "# Title\n\nBody text.";
//! let score = score_extraction(html, md);
//!
//! assert!(score.confidence > 0.5, "article extraction should be high quality");
//! assert!(score.content_density  > 0.0);
//! assert!(score.structure        > 0.0);
//! assert!(score.completeness     > 0.0);
//! assert!(score.encoding_quality > 0.0);
//! ```

use serde::{Deserialize, Serialize};

// ── Signal weights ────────────────────────────────────────────────────────────

const W_CONTENT_DENSITY: f64 = 0.35;
const W_STRUCTURE: f64 = 0.25;
const W_COMPLETENESS: f64 = 0.25;
const W_ENCODING: f64 = 0.15;

// ── Scoring constants ─────────────────────────────────────────────────────────

/// HTML below this size is too small to score reliably; treat as maximum quality.
const MIN_HTML_BYTES: usize = 500;

/// Markdown-to-HTML-byte ratio at which `content_density` saturates at 1.0.
///
/// Typical well-extracted article pages achieve 0.10–0.25 (10–25% of the raw
/// HTML turns into useful text). We saturate at 0.30 so pages with very small
/// HTML that yield dense markdown don't get penalised.
const DENSITY_SATURATION: f64 = 0.30;

/// Markdown character count at which `completeness` saturates at 1.0.
const COMPLETENESS_SATURATION_CHARS: usize = 500;

/// One mojibake/replacement sequence out of this many characters reduces the
/// score by one full unit, capped at score 0.0.
const ENCODING_BAD_PER_CHARS: f64 = 200.0;

// ── Public types ──────────────────────────────────────────────────────────────

/// Per-signal and composite extraction quality scores, all in [0.0, 1.0].
///
/// Serialises to / deserialises from JSON with snake_case field names so it
/// slots cleanly into nab's JSON output envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityScore {
    /// Weighted composite confidence signal (0.0 = worst, 1.0 = best).
    pub confidence: f64,
    /// Ratio of meaningful text to raw HTML size.
    pub content_density: f64,
    /// Presence of structural markdown elements (headings, lists, code).
    pub structure: f64,
    /// Whether main article content was captured rather than nav/footer noise.
    pub completeness: f64,
    /// Absence of mojibake / replacement characters in the output.
    pub encoding_quality: f64,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Compute a [`QualityScore`] from raw HTML bytes and extracted markdown.
///
/// Both arguments must be the *final* values after all extraction passes:
/// `bytes` is the original HTTP response body; `markdown` is the output
/// produced by `ContentRouter`.
///
/// The function is `O(n)` in the size of both inputs, with no allocations
/// beyond a few small counters.
#[must_use]
pub fn score_extraction(html_bytes: &[u8], markdown: &str) -> QualityScore {
    // Trivial case: nothing to score.
    if html_bytes.len() < MIN_HTML_BYTES && markdown.is_empty() {
        return QualityScore::perfect();
    }

    let content_density = score_content_density(html_bytes, markdown);
    let structure = score_structure(markdown);
    let completeness = score_completeness(markdown);
    let encoding_quality = score_encoding(markdown);

    let confidence = weighted_average(content_density, structure, completeness, encoding_quality);

    QualityScore {
        confidence,
        content_density,
        structure,
        completeness,
        encoding_quality,
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

impl QualityScore {
    /// All signals at 1.0 — used for degenerate inputs too small to assess.
    fn perfect() -> Self {
        Self {
            confidence: 1.0,
            content_density: 1.0,
            structure: 1.0,
            completeness: 1.0,
            encoding_quality: 1.0,
        }
    }
}

/// Weighted average of the four sub-scores.
#[inline]
fn weighted_average(density: f64, structure: f64, completeness: f64, encoding: f64) -> f64 {
    let raw = density * W_CONTENT_DENSITY
        + structure * W_STRUCTURE
        + completeness * W_COMPLETENESS
        + encoding * W_ENCODING;
    clamp01(raw)
}

/// Signal 1: content density — how much of the HTML became useful text.
///
/// Score = min(markdown_chars / (html_bytes * SATURATION), 1.0).
/// HTML-only pages (no markdown) that are very large → score 0.
/// Tiny HTML → perfect (see caller guard above).
fn score_content_density(html_bytes: &[u8], markdown: &str) -> f64 {
    let html_len = html_bytes.len();
    if html_len < MIN_HTML_BYTES {
        return 1.0;
    }
    let md_chars = markdown.chars().count();
    #[allow(clippy::cast_precision_loss)]
    let ratio = md_chars as f64 / html_len as f64;
    clamp01(ratio / DENSITY_SATURATION)
}

/// Signal 2: structural richness of the markdown output.
///
/// Each structural element type that is present contributes one point (max 4).
/// Score is normalised to [0, 1].
///
/// | Pattern                   | Points |
/// |---------------------------|--------|
/// | At least one `#` heading  | 1      |
/// | At least one list item    | 1      |
/// | At least one code block   | 1      |
/// | More than 3 paragraphs    | 1      |
fn score_structure(markdown: &str) -> f64 {
    const MAX_POINTS: f64 = 4.0;
    let mut points: f64 = 0.0;

    let has_heading = markdown.lines().any(|l| l.starts_with('#'));
    let has_list = markdown.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") || starts_with_ordered_list(t)
    });
    let has_code = markdown.contains("```") || markdown.contains("    "); // fenced or indented
    let paragraph_count = count_paragraphs(markdown);

    if has_heading {
        points += 1.0;
    }
    if has_list {
        points += 1.0;
    }
    if has_code {
        points += 1.0;
    }
    if paragraph_count > 3 {
        points += 1.0;
    }

    clamp01(points / MAX_POINTS)
}

/// Signal 3: completeness — whether substantial content was captured.
///
/// A very short output on a large page suggests nav/footer only was extracted.
/// Score ramps linearly from 0 at 0 chars to 1 at `COMPLETENESS_SATURATION_CHARS`.
fn score_completeness(markdown: &str) -> f64 {
    let md_chars = markdown.chars().count();
    #[allow(clippy::cast_precision_loss)]
    let ratio = md_chars as f64 / COMPLETENESS_SATURATION_CHARS as f64;
    clamp01(ratio)
}

/// Signal 4: encoding quality — penalise mojibake and replacement characters.
///
/// Counts:
/// - `\u{FFFD}` Unicode replacement character (sign of lossy decoding)
/// - Common Latin-1 mojibake digraphs (`Ã©`, `â€`, `Ã¤`, `Ã¶`, `Ã¼`, `â€™`)
///
/// One bad occurrence per `ENCODING_BAD_PER_CHARS` characters costs one unit.
fn score_encoding(markdown: &str) -> f64 {
    if markdown.is_empty() {
        return 1.0;
    }

    let replacement_count = markdown.chars().filter(|&c| c == '\u{FFFD}').count();
    let mojibake_count = count_mojibake(markdown);
    let total_bad = replacement_count + mojibake_count;

    if total_bad == 0 {
        return 1.0;
    }

    #[allow(clippy::cast_precision_loss)]
    let penalty = total_bad as f64 / ENCODING_BAD_PER_CHARS;
    clamp01(1.0 - penalty)
}

// ── Low-level helpers ─────────────────────────────────────────────────────────

/// Count paragraphs as double-newline-separated non-empty blocks of text.
fn count_paragraphs(markdown: &str) -> usize {
    markdown
        .split("\n\n")
        .filter(|block| {
            let t = block.trim();
            !t.is_empty() && !t.starts_with('#') && !t.starts_with("```")
        })
        .count()
}

/// Return `true` if `text` starts with an ordered list item like `1. `.
fn starts_with_ordered_list(text: &str) -> bool {
    let mut chars = text.chars();
    let first = chars.next();
    first.map_or(false, |c| c.is_ascii_digit())
        && chars.next() == Some('.')
        && chars.next() == Some(' ')
}

/// Count occurrences of well-known Latin-1 mojibake sequences in UTF-8 text.
///
/// These arise when a Windows-1252 / Latin-1 encoded page is decoded as UTF-8.
/// Detecting even a handful provides a strong signal.
fn count_mojibake(text: &str) -> usize {
    const MOJIBAKE_PATTERNS: &[&str] = &[
        "Ã©",  // é  (U+00E9)
        "â€",  // start of smart-quote mojibake
        "Ã¤",  // ä
        "Ã¶",  // ö
        "Ã¼",  // ü
        "â€™", // '  (right single quotation mark)
        "â€œ", // "  (left double quotation mark)
        "â€",  // "  (right double quotation mark)
        "Ã ",  // À
        "Ã®",  // î
    ];

    MOJIBAKE_PATTERNS
        .iter()
        .map(|pat| text.matches(pat).count())
        .sum()
}

/// Clamp a value to [0.0, 1.0] without NaN propagation.
#[inline]
fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── score_content_density ─────────────────────────────────────────────────

    #[test]
    fn content_density_tiny_html_scores_perfect() {
        // GIVEN: HTML under MIN_HTML_BYTES threshold
        // WHEN: scoring content density
        // THEN: returns 1.0 (can't meaningfully score tiny pages)
        assert_eq!(score_content_density(b"<p>Hi</p>", "Hi"), 1.0);
    }

    #[test]
    fn content_density_zero_markdown_on_large_html_scores_zero() {
        // GIVEN: large HTML with no extracted markdown
        // WHEN: scoring content density
        // THEN: score is 0.0 (nothing extracted)
        let html = b"<!DOCTYPE html><html><body>".iter()
            .chain(b"x".repeat(2000).iter())
            .chain(b"</body></html>".iter())
            .copied()
            .collect::<Vec<u8>>();
        assert_eq!(score_content_density(&html, ""), 0.0);
    }

    #[test]
    fn content_density_saturates_at_one_for_dense_output() {
        // GIVEN: moderate HTML with proportionally large markdown (high density)
        // WHEN: scoring content density
        // THEN: score is capped at 1.0
        let html = vec![b'x'; 1000];
        let md = "a".repeat(400); // 40% ratio > 30% saturation
        assert_eq!(score_content_density(&html, &md), 1.0);
    }

    #[test]
    fn content_density_mid_range_proportional() {
        // GIVEN: 5000-byte HTML, 150 markdown chars (ratio = 3%, saturation = 30%)
        // WHEN: scoring
        // THEN: score ≈ 0.10 (3/30)
        let html = vec![b'x'; 5000];
        let md = "a".repeat(150);
        let score = score_content_density(&html, &md);
        assert!(
            (score - 0.10).abs() < 0.01,
            "expected ~0.10, got {score:.4}"
        );
    }

    // ── score_structure ───────────────────────────────────────────────────────

    #[test]
    fn structure_empty_markdown_scores_zero() {
        // GIVEN: empty markdown
        // WHEN: scoring structure
        // THEN: 0.0 (no structural elements)
        assert_eq!(score_structure(""), 0.0);
    }

    #[test]
    fn structure_all_elements_present_scores_one() {
        // GIVEN: markdown with heading, list, code block, and >3 paragraphs
        // WHEN: scoring structure
        // THEN: 1.0 (all four points)
        let md = "# Title\n\n- item\n\n```\ncode\n```\n\npara1\n\npara2\n\npara3\n\npara4";
        assert_eq!(score_structure(md), 1.0);
    }

    #[test]
    fn structure_heading_only_scores_quarter() {
        // GIVEN: markdown with only a heading (1/4 points)
        // WHEN: scoring structure
        // THEN: 0.25
        let md = "# Just a heading";
        assert_eq!(score_structure(md), 0.25);
    }

    #[test]
    fn structure_ordered_list_detected() {
        // GIVEN: markdown with an ordered list item
        // WHEN: scoring structure
        // THEN: list point is awarded
        let md = "1. First item\n2. Second item";
        let score = score_structure(md);
        assert!(score > 0.0, "ordered list should contribute to structure score");
    }

    #[test]
    fn structure_indented_code_detected() {
        // GIVEN: markdown with indented code block (4-space indent)
        // WHEN: scoring structure
        // THEN: code point is awarded
        let md = "Some intro:\n\n    let x = 1;\n    let y = 2;";
        let score = score_structure(md);
        assert!(score > 0.0, "indented code should contribute to structure score");
    }

    // ── score_completeness ────────────────────────────────────────────────────

    #[test]
    fn completeness_empty_markdown_scores_zero() {
        // GIVEN: empty output
        // WHEN: scoring completeness
        // THEN: 0.0
        assert_eq!(score_completeness(""), 0.0);
    }

    #[test]
    fn completeness_saturates_at_saturation_chars() {
        // GIVEN: markdown at exactly the saturation threshold
        // WHEN: scoring
        // THEN: 1.0
        let md = "a".repeat(COMPLETENESS_SATURATION_CHARS);
        assert_eq!(score_completeness(&md), 1.0);
    }

    #[test]
    fn completeness_half_saturation_scores_half() {
        // GIVEN: markdown at half the saturation threshold
        // WHEN: scoring
        // THEN: ~0.5
        let md = "a".repeat(COMPLETENESS_SATURATION_CHARS / 2);
        let score = score_completeness(&md);
        assert!(
            (score - 0.5).abs() < 0.01,
            "expected 0.5, got {score:.4}"
        );
    }

    // ── score_encoding ────────────────────────────────────────────────────────

    #[test]
    fn encoding_clean_utf8_scores_one() {
        // GIVEN: clean UTF-8 text with no replacement chars or mojibake
        // WHEN: scoring encoding
        // THEN: 1.0
        assert_eq!(score_encoding("Hello, world! 你好 мир"), 1.0);
    }

    #[test]
    fn encoding_replacement_char_reduces_score() {
        // GIVEN: text containing a Unicode replacement character
        // WHEN: scoring encoding
        // THEN: score < 1.0
        let text = "Hello \u{FFFD} world";
        assert!(
            score_encoding(text) < 1.0,
            "replacement char should reduce encoding score"
        );
    }

    #[test]
    fn encoding_mojibake_reduces_score() {
        // GIVEN: text with a Latin-1 mojibake sequence
        // WHEN: scoring encoding
        // THEN: score < 1.0
        let text = "caf\u{00C3}\u{00A9} au lait"; // "café" mis-decoded
        assert!(
            score_encoding(text) < 1.0,
            "mojibake should reduce encoding score"
        );
    }

    #[test]
    fn encoding_empty_scores_one() {
        // GIVEN: empty string
        // WHEN: scoring encoding
        // THEN: 1.0 (nothing bad to detect)
        assert_eq!(score_encoding(""), 1.0);
    }

    // ── score_extraction (composite) ──────────────────────────────────────────

    #[test]
    fn score_extraction_well_structured_article_scores_high() {
        // GIVEN: realistic article HTML with clean markdown output
        // WHEN: computing composite score
        // THEN: confidence > 0.6
        let html = b"<!DOCTYPE html><html><body><article><h1>Title</h1>\
                     <p>Paragraph one of the article with useful content.</p>\
                     <p>Paragraph two continues the discussion.</p>\
                     <ul><li>Point one</li><li>Point two</li></ul>\
                     </article></body></html>";
        let html_padded = html.iter().chain(b" ".repeat(200).iter()).copied().collect::<Vec<u8>>();
        let md = "# Title\n\nParagraph one of the article with useful content.\n\n\
                  Paragraph two continues the discussion.\n\n- Point one\n- Point two";
        let score = score_extraction(&html_padded, md);
        assert!(
            score.confidence > 0.6,
            "well-structured article should score > 0.6, got {:.3}",
            score.confidence
        );
    }

    #[test]
    fn score_extraction_empty_html_empty_markdown_scores_perfect() {
        // GIVEN: both inputs empty / trivially small
        // WHEN: computing score
        // THEN: 1.0 across all signals (no evidence of failure)
        let score = score_extraction(b"", "");
        assert_eq!(score.confidence, 1.0);
    }

    #[test]
    fn score_extraction_large_html_no_markdown_scores_low() {
        // GIVEN: 10 KB HTML producing empty markdown (JS-rendered page)
        // WHEN: computing score
        // THEN: confidence is low (content_density and completeness are both ~0)
        let html = vec![b'x'; 10_000];
        let score = score_extraction(&html, "");
        assert!(
            score.confidence < 0.4,
            "empty extraction from large HTML should score < 0.4, got {:.3}",
            score.confidence
        );
    }

    #[test]
    fn score_extraction_encoding_issues_penalise_confidence() {
        // GIVEN: HTML with mojibake in the extracted markdown
        // WHEN: computing score
        // THEN: encoding_quality < 1.0 and it drags confidence down
        let html = vec![b'x'; 600];
        let md = "caf\u{00C3}\u{00A9} article ".repeat(50); // dense mojibake
        let score = score_extraction(&html, &md);
        assert!(
            score.encoding_quality < 1.0,
            "mojibake should reduce encoding_quality, got {:.3}",
            score.encoding_quality
        );
    }

    #[test]
    fn score_extraction_composite_signals_sum_to_confidence() {
        // GIVEN: arbitrary inputs
        // WHEN: computing score
        // THEN: confidence matches the weighted formula to float precision
        let html = vec![b'x'; 2000];
        let md = "# Head\n\n- item\n\nsome text\n\nmore text";
        let score = score_extraction(&html, md);

        let expected = (score.content_density * W_CONTENT_DENSITY
            + score.structure * W_STRUCTURE
            + score.completeness * W_COMPLETENESS
            + score.encoding_quality * W_ENCODING)
            .clamp(0.0, 1.0);

        assert!(
            (score.confidence - expected).abs() < 1e-9,
            "confidence {:.6} ≠ weighted sum {:.6}",
            score.confidence,
            expected
        );
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    #[test]
    fn count_paragraphs_counts_non_heading_blocks() {
        // GIVEN: markdown with headings and prose paragraphs
        // WHEN: counting paragraphs
        // THEN: only prose blocks are counted
        let md = "# Heading\n\nFirst para.\n\nSecond para.\n\nThird para.";
        assert_eq!(count_paragraphs(md), 3);
    }

    #[test]
    fn starts_with_ordered_list_true_for_digit_dot_space() {
        assert!(starts_with_ordered_list("1. item"));
        assert!(starts_with_ordered_list("42. item"));
    }

    #[test]
    fn starts_with_ordered_list_false_for_non_matching() {
        assert!(!starts_with_ordered_list("- item"));
        assert!(!starts_with_ordered_list("1.item"));
        assert!(!starts_with_ordered_list("text"));
        assert!(!starts_with_ordered_list(""));
    }

    #[test]
    fn clamp01_clamps_negative_and_above_one() {
        assert_eq!(clamp01(-1.0), 0.0);
        assert_eq!(clamp01(2.0), 1.0);
        assert_eq!(clamp01(0.5), 0.5);
    }
}
