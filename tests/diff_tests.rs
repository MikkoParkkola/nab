//! Integration tests for content diffing (Issue #21).
//!
//! Covers `ContentSnapshot`, `compute_diff`, `SnapshotStore`, and
//! `format_diff_terminal` / `format_diff_markdown` end-to-end.

use std::time::{Duration, SystemTime};

use nab::content::diff::{ChangeKind, ContentSnapshot, compute_diff, split_paragraphs};
use nab::content::diff_format::{format_diff_markdown, format_diff_terminal};
use nab::content::snapshot_store::SnapshotStore;
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn snap(url: &str, text: &str) -> ContentSnapshot {
    ContentSnapshot::new(url, text, SystemTime::UNIX_EPOCH)
}

fn snap_at(url: &str, text: &str, secs: u64) -> ContentSnapshot {
    ContentSnapshot::new(
        url,
        text,
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
    )
}

fn tmp_store() -> (TempDir, SnapshotStore) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = SnapshotStore::with_root(dir.path());
    (dir, store)
}

// ── 1. split_paragraphs ───────────────────────────────────────────────────────

#[test]
fn split_blank_string_yields_empty_vec() {
    // GIVEN: empty input
    // WHEN: split
    // THEN: empty result
    assert!(split_paragraphs("").is_empty());
}

#[test]
fn split_single_paragraph_yields_one_item() {
    // GIVEN: single block with no blank lines
    // WHEN: split
    // THEN: exactly one item
    let result = split_paragraphs("Hello world.");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], "Hello world.");
}

#[test]
fn split_two_paragraphs_separated_by_double_newline() {
    // GIVEN: two paragraphs
    // WHEN: split
    // THEN: two items
    let result = split_paragraphs("First paragraph.\n\nSecond paragraph.");
    assert_eq!(result.len(), 2);
}

#[test]
fn split_strips_surrounding_whitespace() {
    // GIVEN: paragraph with leading/trailing spaces
    // WHEN: split
    // THEN: trimmed
    let result = split_paragraphs("  Hello  ");
    assert_eq!(result[0], "Hello");
}

// ── 2. ContentSnapshot creation ──────────────────────────────────────────────

#[test]
fn snapshot_stores_url() {
    let s = snap("https://test.com", "content");
    assert_eq!(s.url, "https://test.com");
}

#[test]
fn snapshot_stores_text() {
    let s = snap("https://test.com", "Hello.\n\nWorld.");
    assert_eq!(s.text, "Hello.\n\nWorld.");
}

#[test]
fn snapshot_generates_paragraphs() {
    let s = snap("https://test.com", "Hello.\n\nWorld.");
    assert_eq!(s.paragraphs.len(), 2);
}

#[test]
fn snapshot_hash_same_for_identical_content() {
    // GIVEN: two snapshots with same text
    let a = snap("https://x.com", "Same content here.");
    let b = snap("https://x.com", "Same content here.");
    // THEN: hashes identical → content_unchanged
    assert!(a.content_unchanged(&b));
}

#[test]
fn snapshot_hash_differs_for_different_content() {
    // GIVEN: two snapshots with different text
    let a = snap("https://x.com", "Text A.");
    let b = snap("https://x.com", "Text B.");
    // THEN: hashes differ
    assert!(!a.content_unchanged(&b));
}

// ── 3. compute_diff — unchanged ───────────────────────────────────────────────

#[test]
fn diff_identical_content_is_empty() {
    // GIVEN: same text
    let a = snap("https://x.com", "No change here.");
    let diff = compute_diff(&a, &a.clone());
    // THEN: unchanged, empty sections
    assert!(diff.unchanged);
    assert!(diff.is_empty());
}

#[test]
fn diff_identical_summary_is_no_changes() {
    let a = snap("https://x.com", "Static page.");
    let diff = compute_diff(&a, &a.clone());
    assert_eq!(diff.summary(), "No changes");
}

// ── 4. compute_diff — additions ───────────────────────────────────────────────

#[test]
fn diff_new_paragraph_at_end_counted_as_added() {
    // GIVEN: new snapshot appends a paragraph
    let old = snap("https://x.com", "Intro.");
    let new = snap("https://x.com", "Intro.\n\nNew section.");
    let diff = compute_diff(&old, &new);
    assert!(diff.added_count >= 1, "added_count={}", diff.added_count);
    assert_eq!(diff.removed_count, 0);
}

#[test]
fn diff_new_paragraph_in_middle_counted_as_added() {
    // GIVEN: new snapshot inserts a paragraph between two unchanged ones
    let old = snap("https://x.com", "A.\n\nC.");
    let new = snap("https://x.com", "A.\n\nB.\n\nC.");
    let diff = compute_diff(&old, &new);
    // A and C are common, B is new
    assert!(diff.added_count >= 1);
}

// ── 5. compute_diff — removals ────────────────────────────────────────────────

#[test]
fn diff_removed_paragraph_at_end_counted() {
    // GIVEN: paragraph dropped
    let old = snap("https://x.com", "Intro.\n\nFooter.");
    let new = snap("https://x.com", "Intro.");
    let diff = compute_diff(&old, &new);
    assert!(
        diff.removed_count >= 1,
        "removed_count={}",
        diff.removed_count
    );
    assert_eq!(diff.added_count, 0);
}

#[test]
fn diff_completely_replaced_content_has_both_added_and_removed() {
    // GIVEN: completely different text (no common paragraphs)
    let old = snap("https://x.com", "Alpha text.\n\nBeta text.");
    let new = snap("https://x.com", "Gamma text.\n\nDelta text.");
    let diff = compute_diff(&old, &new);
    // Should have changes (modified or add+remove)
    assert!(
        diff.added_count + diff.removed_count + diff.modified_count > 0,
        "expected changes, got: {diff:?}"
    );
}

// ── 6. compute_diff — metadata ────────────────────────────────────────────────

#[test]
fn diff_url_matches_new_snapshot() {
    let old = snap("https://example.com/page", "Old.");
    let new = snap("https://example.com/page", "New.");
    let diff = compute_diff(&old, &new);
    assert_eq!(diff.url, "https://example.com/page");
}

#[test]
fn diff_timestamps_reflect_snapshots() {
    let old = snap_at("https://x.com", "Old.", 1000);
    let new = snap_at("https://x.com", "New.", 2000);
    let diff = compute_diff(&old, &new);
    assert_eq!(diff.old_timestamp, 1000);
    assert_eq!(diff.new_timestamp, 2000);
}

// ── 7. DiffSection structure ──────────────────────────────────────────────────

#[test]
fn diff_added_section_has_new_text_only() {
    let old = snap("https://x.com", "A.");
    let new = snap("https://x.com", "A.\n\nB.");
    let diff = compute_diff(&old, &new);
    let added: Vec<_> = diff
        .sections
        .iter()
        .filter(|s| s.kind == ChangeKind::Added)
        .collect();
    assert!(!added.is_empty());
    assert!(added[0].new_text.is_some());
    assert!(added[0].old_text.is_none());
}

#[test]
fn diff_removed_section_has_old_text_only() {
    let old = snap("https://x.com", "A.\n\nB.");
    let new = snap("https://x.com", "A.");
    let diff = compute_diff(&old, &new);
    let removed: Vec<_> = diff
        .sections
        .iter()
        .filter(|s| s.kind == ChangeKind::Removed)
        .collect();
    assert!(!removed.is_empty());
    assert!(removed[0].old_text.is_some());
    assert!(removed[0].new_text.is_none());
}

// ── 8. SnapshotStore ──────────────────────────────────────────────────────────

#[test]
fn store_save_and_load_latest_roundtrip() {
    // GIVEN: a store and snapshot
    let (_dir, store) = tmp_store();
    let snap_val = snap_at("https://example.com", "Hello snapshot.", 500);
    // WHEN: saved and loaded
    store
        .save_snapshot("https://example.com", &snap_val)
        .unwrap();
    let loaded = store.load_latest_snapshot("https://example.com").unwrap();
    // THEN: content matches
    assert_eq!(loaded.text, snap_val.text);
}

#[test]
fn store_load_latest_is_newest_when_multiple() {
    // GIVEN: two snapshots at different times
    let (_dir, store) = tmp_store();
    for ts in [100u64, 999] {
        let s = snap_at("https://example.com", &format!("ts={ts}"), ts);
        store.save_snapshot("https://example.com", &s).unwrap();
    }
    // WHEN: load latest
    let latest = store.load_latest_snapshot("https://example.com").unwrap();
    // THEN: newest
    assert!(latest.text.contains("999"), "got: {}", latest.text);
}

#[test]
fn store_unknown_url_returns_none() {
    // GIVEN: fresh store
    let (_dir, store) = tmp_store();
    // WHEN: load for URL with no snapshot
    assert!(store.load_latest_snapshot("https://never.com").is_none());
}

#[test]
fn store_prunes_beyond_max() {
    // GIVEN: max=2, 4 saves
    let (_dir, store) = tmp_store();
    let store = store.with_max_per_url(2);
    for ts in 1u64..=4 {
        let s = snap_at("https://prune.com", &format!("t{ts}"), ts);
        store.save_snapshot("https://prune.com", &s).unwrap();
    }
    // THEN: at most 2 remain
    let metas = store.list_snapshots("https://prune.com");
    assert!(metas.len() <= 2, "expected <=2, got {}", metas.len());
}

// ── 9. format_diff_terminal ───────────────────────────────────────────────────

#[test]
fn terminal_no_changes_says_no_changes() {
    let s = snap("https://x.com", "Same.");
    let diff = compute_diff(&s, &s.clone());
    let out = format_diff_terminal(&diff);
    assert!(out.contains("No changes"));
}

#[test]
fn terminal_added_contains_plus_sign() {
    let old = snap("https://x.com", "A.");
    let new = snap("https://x.com", "A.\n\nB.");
    let diff = compute_diff(&old, &new);
    let out = format_diff_terminal(&diff);
    assert!(out.contains('+'), "expected '+' in terminal output");
}

#[test]
fn terminal_removed_contains_minus_sign() {
    let old = snap("https://x.com", "A.\n\nB.");
    let new = snap("https://x.com", "A.");
    let diff = compute_diff(&old, &new);
    let out = format_diff_terminal(&diff);
    assert!(out.contains('-'), "expected '-' in terminal output");
}

// ── 10. format_diff_markdown ──────────────────────────────────────────────────

#[test]
fn markdown_no_changes_says_no_changes() {
    let s = snap("https://x.com", "Same.");
    let diff = compute_diff(&s, &s.clone());
    let out = format_diff_markdown(&diff);
    assert!(out.contains("No changes"));
}

#[test]
fn markdown_no_ansi_escape_codes() {
    let old = snap("https://x.com", "Old.");
    let new = snap("https://x.com", "New.");
    let diff = compute_diff(&old, &new);
    let out = format_diff_markdown(&diff);
    assert!(!out.contains('\x1b'), "ANSI found in markdown output");
}

#[test]
fn markdown_added_label_present() {
    let old = snap("https://x.com", "A.");
    let new = snap("https://x.com", "A.\n\nB.");
    let diff = compute_diff(&old, &new);
    let out = format_diff_markdown(&diff);
    assert!(out.contains("added"), "expected 'added' label");
}

#[test]
fn markdown_removed_label_present() {
    let old = snap("https://x.com", "A.\n\nB.");
    let new = snap("https://x.com", "A.");
    let diff = compute_diff(&old, &new);
    let out = format_diff_markdown(&diff);
    assert!(out.contains("removed"), "expected 'removed' label");
}

#[test]
fn markdown_includes_url_in_header() {
    let old = snap("https://target.com/path", "Old text.");
    let new = snap("https://target.com/path", "New text.");
    let diff = compute_diff(&old, &new);
    let out = format_diff_markdown(&diff);
    assert!(
        out.contains("target.com"),
        "URL missing from markdown header"
    );
}
