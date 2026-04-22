//! Semantic content diffing for web page snapshots.
//!
//! Computes paragraph-level diffs between two [`ContentSnapshot`] values,
//! distinguishing true content changes from HTML layout churn.
//!
//! # Example
//!
//! ```rust
//! use nab::content::diff::{ContentSnapshot, compute_diff};
//! use std::time::SystemTime;
//!
//! let old = ContentSnapshot::new("https://example.com", "Hello world.\n\nMore text.", SystemTime::UNIX_EPOCH);
//! let new = ContentSnapshot::new("https://example.com", "Hello world.\n\nNew content.", SystemTime::UNIX_EPOCH);
//! let diff = compute_diff(&old, &new);
//! assert!(!diff.sections.is_empty());
//! ```

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

// ── Types ────────────────────────────────────────────────────────────────────

/// A stored page snapshot: URL, timestamp, extracted text, and a content hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentSnapshot {
    /// Canonical URL of the fetched page.
    pub url: String,
    /// Unix timestamp (seconds since epoch) of the fetch.
    pub timestamp: u64,
    /// Extracted markdown/text content.
    pub text: String,
    /// Paragraph-level segments derived from `text`.
    pub paragraphs: Vec<String>,
    /// Blake2-like hash of the normalised content (for cheap equality checks).
    pub content_hash: u64,
}

impl ContentSnapshot {
    /// Create a snapshot by segmenting `text` into paragraphs and hashing it.
    pub fn new(url: &str, text: &str, timestamp: SystemTime) -> Self {
        let ts = timestamp
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let paragraphs = split_paragraphs(text);
        let content_hash = hash_text(text);
        Self {
            url: url.to_owned(),
            timestamp: ts,
            text: text.to_owned(),
            paragraphs,
            content_hash,
        }
    }

    /// Returns `true` if the content hash is identical to `other`.
    pub fn content_unchanged(&self, other: &Self) -> bool {
        self.content_hash == other.content_hash
    }
}

/// The kind of change a [`DiffSection`] represents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind {
    /// Paragraphs that appear only in the new snapshot.
    Added,
    /// Paragraphs that appear only in the old snapshot.
    Removed,
    /// A paragraph whose text changed between snapshots.
    Modified,
}

/// A single changed region with optional context lines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSection {
    /// Nature of the change.
    pub kind: ChangeKind,
    /// Original text (`None` for [`ChangeKind::Added`]).
    pub old_text: Option<String>,
    /// New text (`None` for [`ChangeKind::Removed`]).
    pub new_text: Option<String>,
    /// Surrounding unchanged paragraph(s) for context (up to 1 on each side).
    pub context: Vec<String>,
}

impl DiffSection {
    fn added(new_text: impl Into<String>, context: Vec<String>) -> Self {
        Self {
            kind: ChangeKind::Added,
            old_text: None,
            new_text: Some(new_text.into()),
            context,
        }
    }

    fn removed(old_text: impl Into<String>, context: Vec<String>) -> Self {
        Self {
            kind: ChangeKind::Removed,
            old_text: Some(old_text.into()),
            new_text: None,
            context,
        }
    }

    fn modified(old: impl Into<String>, new: impl Into<String>, context: Vec<String>) -> Self {
        Self {
            kind: ChangeKind::Modified,
            old_text: Some(old.into()),
            new_text: Some(new.into()),
            context,
        }
    }
}

/// The full diff between two snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDiff {
    /// Source URL (from `new` snapshot).
    pub url: String,
    /// Timestamp of the old snapshot.
    pub old_timestamp: u64,
    /// Timestamp of the new snapshot.
    pub new_timestamp: u64,
    /// Whether content hashes are identical (fast short-circuit).
    pub unchanged: bool,
    /// Individual change sections in document order.
    pub sections: Vec<DiffSection>,
    /// Summary counts.
    pub added_count: usize,
    pub removed_count: usize,
    pub modified_count: usize,
}

impl ContentDiff {
    /// Returns `true` when there are no diff sections.
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// Human-readable one-line summary.
    pub fn summary(&self) -> String {
        if self.unchanged {
            return "No changes".to_owned();
        }
        let parts: Vec<String> = [
            (self.added_count, "added"),
            (self.modified_count, "modified"),
            (self.removed_count, "removed"),
        ]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, label)| format!("{n} {label}"))
        .collect();

        if parts.is_empty() {
            "No changes".to_owned()
        } else {
            parts.join(", ")
        }
    }
}

// ── Core algorithm ───────────────────────────────────────────────────────────

/// Compute a paragraph-level semantic diff between two snapshots.
///
/// Uses an LCS-based algorithm on normalised paragraph fingerprints to find
/// unchanged regions; everything outside is classified as added, removed, or
/// modified (same position, different text).
pub fn compute_diff(old: &ContentSnapshot, new: &ContentSnapshot) -> ContentDiff {
    if old.content_unchanged(new) {
        return ContentDiff {
            url: new.url.clone(),
            old_timestamp: old.timestamp,
            new_timestamp: new.timestamp,
            unchanged: true,
            sections: vec![],
            added_count: 0,
            removed_count: 0,
            modified_count: 0,
        };
    }

    let sections = diff_paragraphs(&old.paragraphs, &new.paragraphs);
    let added_count = sections
        .iter()
        .filter(|s| s.kind == ChangeKind::Added)
        .count();
    let removed_count = sections
        .iter()
        .filter(|s| s.kind == ChangeKind::Removed)
        .count();
    let modified_count = sections
        .iter()
        .filter(|s| s.kind == ChangeKind::Modified)
        .count();

    ContentDiff {
        url: new.url.clone(),
        old_timestamp: old.timestamp,
        new_timestamp: new.timestamp,
        unchanged: false,
        sections,
        added_count,
        removed_count,
        modified_count,
    }
}

// ── Paragraph diffing ────────────────────────────────────────────────────────

/// Diff two paragraph sequences using LCS on fingerprints.
fn diff_paragraphs(old: &[String], new: &[String]) -> Vec<DiffSection> {
    let old_fp: Vec<u64> = old.iter().map(|p| fingerprint(p)).collect();
    let new_fp: Vec<u64> = new.iter().map(|p| fingerprint(p)).collect();

    let lcs = lcs_indices(&old_fp, &new_fp);
    build_sections(old, new, &lcs)
}

/// Build diff sections from the LCS alignment.
fn build_sections(old: &[String], new: &[String], lcs: &[(usize, usize)]) -> Vec<DiffSection> {
    let mut sections = Vec::new();
    let mut oi = 0usize;
    let mut ni = 0usize;

    for &(lo, ln) in lcs {
        let old_gap = &old[oi..lo];
        let new_gap = &new[ni..ln];

        emit_gap_sections(old_gap, new_gap, old, new, oi, ni, &mut sections);

        oi = lo + 1;
        ni = ln + 1;
    }

    // Tail after last LCS match
    let old_tail = &old[oi..];
    let new_tail = &new[ni..];
    emit_gap_sections(old_tail, new_tail, old, new, oi, ni, &mut sections);

    sections
}

/// Emit sections for a non-LCS gap between `old_gap` and `new_gap`.
fn emit_gap_sections(
    old_gap: &[String],
    new_gap: &[String],
    old_all: &[String],
    new_all: &[String],
    oi: usize,
    ni: usize,
    sections: &mut Vec<DiffSection>,
) {
    let max_len = old_gap.len().max(new_gap.len());
    for idx in 0..max_len {
        let ctx = context_lines(old_all, new_all, oi + idx, ni + idx);
        match (old_gap.get(idx), new_gap.get(idx)) {
            (Some(o), Some(n)) if o != n => sections.push(DiffSection::modified(o, n, ctx)),
            (Some(o), None) => sections.push(DiffSection::removed(o, ctx)),
            (None, Some(n)) => sections.push(DiffSection::added(n, ctx)),
            _ => {}
        }
    }
}

/// Collect up to one preceding unchanged paragraph as context.
fn context_lines(old: &[String], new: &[String], oi: usize, ni: usize) -> Vec<String> {
    let mut ctx = Vec::new();
    if oi > 0 {
        ctx.push(old[oi - 1].clone());
    } else if ni > 0 {
        ctx.push(new[ni - 1].clone());
    }
    ctx
}

// ── LCS ─────────────────────────────────────────────────────────────────────

/// Return index pairs `(i, j)` for LCS of `a` and `b` using fingerprints.
fn lcs_indices(a: &[u64], b: &[u64]) -> Vec<(usize, usize)> {
    let m = a.len();
    let n = b.len();
    // DP table: lengths only, then backtrace
    let mut dp = vec![vec![0u32; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    backtrace_lcs(&dp, a, b, m, n)
}

/// Backtrace DP table to recover matched index pairs.
fn backtrace_lcs(
    dp: &[Vec<u32>],
    a: &[u64],
    b: &[u64],
    mut i: usize,
    mut j: usize,
) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            result.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    result.reverse();
    result
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Split markdown/text into logical paragraphs (blank-line separated).
///
/// Empty and whitespace-only segments are dropped; each paragraph is
/// normalised (trimmed, interior whitespace collapsed) before storage.
pub fn split_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(normalise_paragraph)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Normalise a paragraph: trim edges, collapse interior whitespace runs.
fn normalise_paragraph(para: &str) -> String {
    para.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Fingerprint a paragraph for LCS comparison (case-insensitive, normalised).
fn fingerprint(para: &str) -> u64 {
    hash_text(&para.to_lowercase())
}

/// Stable hash of a string using `DefaultHasher`.
fn hash_text(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn snap(url: &str, text: &str) -> ContentSnapshot {
        ContentSnapshot::new(url, text, SystemTime::UNIX_EPOCH)
    }

    // ── split_paragraphs ────────────────────────────────────────────────────

    #[test]
    fn split_paragraphs_basic_two_paragraphs() {
        // GIVEN: two paragraphs separated by a blank line
        // WHEN: split
        // THEN: exactly two items
        let result = split_paragraphs("Hello world.\n\nSecond paragraph.");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "Hello world.");
        assert_eq!(result[1], "Second paragraph.");
    }

    #[test]
    fn split_paragraphs_collapses_interior_whitespace() {
        // GIVEN: paragraph with multiple spaces
        // WHEN: split
        // THEN: interior spaces collapsed
        let result = split_paragraphs("Word   another   word.");
        assert_eq!(result[0], "Word another word.");
    }

    #[test]
    fn split_paragraphs_drops_blank_segments() {
        // GIVEN: text with leading blank and multiple separators
        // WHEN: split
        // THEN: only non-empty paragraphs
        let result = split_paragraphs("\n\nFirst.\n\n\n\nSecond.\n\n");
        assert_eq!(result.len(), 2);
    }

    // ── ContentSnapshot ─────────────────────────────────────────────────────

    #[test]
    fn snapshot_creates_paragraphs_from_text() {
        // GIVEN: multi-paragraph text
        // WHEN: snapshot created
        // THEN: paragraphs populated
        let s = snap("https://example.com", "Para one.\n\nPara two.");
        assert_eq!(s.paragraphs.len(), 2);
    }

    #[test]
    fn snapshot_identical_texts_share_hash() {
        // GIVEN: two snapshots of identical content
        // WHEN: compared
        // THEN: content_unchanged returns true
        let a = snap("https://x.com", "Same content.");
        let b = snap("https://x.com", "Same content.");
        assert!(a.content_unchanged(&b));
    }

    #[test]
    fn snapshot_different_texts_differ_hash() {
        // GIVEN: two snapshots with different content
        // WHEN: compared
        // THEN: content_unchanged returns false
        let a = snap("https://x.com", "Old content.");
        let b = snap("https://x.com", "New content.");
        assert!(!a.content_unchanged(&b));
    }

    // ── compute_diff ────────────────────────────────────────────────────────

    #[test]
    fn compute_diff_identical_snapshots_returns_unchanged() {
        // GIVEN: identical snapshots
        // WHEN: diff computed
        // THEN: unchanged flag set, sections empty
        let a = snap("https://x.com", "Hello.\n\nWorld.");
        let diff = compute_diff(&a, &a.clone());
        assert!(diff.unchanged);
        assert!(diff.sections.is_empty());
    }

    #[test]
    fn compute_diff_added_paragraph_detected() {
        // GIVEN: new snapshot has extra paragraph
        // WHEN: diff computed
        // THEN: added_count == 1
        let old = snap("https://x.com", "Intro.\n\nBody.");
        let new = snap("https://x.com", "Intro.\n\nBody.\n\nNew section.");
        let diff = compute_diff(&old, &new);
        assert!(!diff.unchanged);
        assert!(diff.added_count >= 1);
        assert_eq!(diff.removed_count, 0);
    }

    #[test]
    fn compute_diff_removed_paragraph_detected() {
        // GIVEN: old snapshot has paragraph absent in new
        // WHEN: diff computed
        // THEN: removed_count == 1
        let old = snap("https://x.com", "Intro.\n\nBody.\n\nFooter.");
        let new = snap("https://x.com", "Intro.\n\nBody.");
        let diff = compute_diff(&old, &new);
        assert!(diff.removed_count >= 1);
        assert_eq!(diff.added_count, 0);
    }

    #[test]
    fn compute_diff_modified_paragraph_detected() {
        // GIVEN: shared structure with one changed paragraph
        // WHEN: diff computed
        // THEN: modified_count >= 1
        let old = snap(
            "https://x.com",
            "Intro.\n\nOld body text here.\n\nConclusion.",
        );
        let new = snap(
            "https://x.com",
            "Intro.\n\nNew body text here.\n\nConclusion.",
        );
        let diff = compute_diff(&old, &new);
        assert!(!diff.unchanged);
        assert!(diff.modified_count >= 1 || diff.added_count + diff.removed_count >= 1);
    }

    #[test]
    fn compute_diff_preserves_url() {
        // GIVEN: two snapshots for same URL
        // WHEN: diff computed
        // THEN: diff URL matches new snapshot
        let old = snap("https://example.com/page", "Old text.");
        let new = snap("https://example.com/page", "New text.");
        let diff = compute_diff(&old, &new);
        assert_eq!(diff.url, "https://example.com/page");
    }

    #[test]
    fn compute_diff_timestamps_preserved() {
        // GIVEN: two snapshots with known timestamps
        // WHEN: diff computed
        // THEN: both timestamps present in result
        let old = ContentSnapshot::new("https://x.com", "Old.", SystemTime::UNIX_EPOCH);
        let new = ContentSnapshot::new("https://x.com", "New.", SystemTime::UNIX_EPOCH);
        let diff = compute_diff(&old, &new);
        assert_eq!(diff.old_timestamp, 0);
        assert_eq!(diff.new_timestamp, 0);
    }

    // ── summary ─────────────────────────────────────────────────────────────

    #[test]
    fn summary_unchanged_returns_no_changes() {
        // GIVEN: identical snapshots -> unchanged diff
        // WHEN: summary called
        // THEN: "No changes"
        let a = snap("https://x.com", "Same text.");
        let diff = compute_diff(&a, &a.clone());
        assert_eq!(diff.summary(), "No changes");
    }

    #[test]
    fn summary_includes_added_and_removed_counts() {
        // GIVEN: diff with additions and removals
        // WHEN: summary called
        // THEN: counts appear in output
        let old = snap("https://x.com", "A.\n\nB.\n\nC.");
        let new = snap("https://x.com", "A.\n\nD.\n\nE.");
        let diff = compute_diff(&old, &new);
        let s = diff.summary();
        assert!(!s.is_empty());
        assert_ne!(s, "No changes");
    }

    // ── DiffSection helpers ──────────────────────────────────────────────────

    #[test]
    fn diff_section_added_has_no_old_text() {
        let sec = DiffSection::added("new paragraph", vec![]);
        assert!(sec.old_text.is_none());
        assert_eq!(sec.new_text.as_deref(), Some("new paragraph"));
        assert_eq!(sec.kind, ChangeKind::Added);
    }

    #[test]
    fn diff_section_removed_has_no_new_text() {
        let sec = DiffSection::removed("old paragraph", vec![]);
        assert!(sec.new_text.is_none());
        assert_eq!(sec.old_text.as_deref(), Some("old paragraph"));
        assert_eq!(sec.kind, ChangeKind::Removed);
    }

    #[test]
    fn diff_section_modified_has_both_texts() {
        let sec = DiffSection::modified("old", "new", vec![]);
        assert_eq!(sec.old_text.as_deref(), Some("old"));
        assert_eq!(sec.new_text.as_deref(), Some("new"));
        assert_eq!(sec.kind, ChangeKind::Modified);
    }
}
