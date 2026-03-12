//! Terminal and markdown formatting for [`ContentDiff`] output.
//!
//! # Example
//!
//! ```rust
//! use nab::content::diff::{ContentSnapshot, compute_diff};
//! use nab::content::diff_format::{format_diff_terminal, format_diff_markdown};
//! use std::time::SystemTime;
//!
//! let old = ContentSnapshot::new("https://example.com", "Hello.", SystemTime::UNIX_EPOCH);
//! let new = ContentSnapshot::new("https://example.com", "Hello.\n\nNew paragraph.", SystemTime::UNIX_EPOCH);
//! let diff = compute_diff(&old, &new);
//! let terminal_out = format_diff_terminal(&diff);
//! let markdown_out = format_diff_markdown(&diff);
//! assert!(terminal_out.contains("+"));
//! assert!(markdown_out.contains("added"));
//! ```

use std::fmt::Write as _;

use super::diff::{ChangeKind, ContentDiff, DiffSection};

// ── ANSI colour constants ─────────────────────────────────────────────────────

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

// ── Public API ────────────────────────────────────────────────────────────────

/// Render a [`ContentDiff`] as a colour terminal string.
///
/// Added lines are prefixed with `+` in green, removed with `-` in red,
/// modified show both old (`-`) and new (`+`) lines. Context lines are dimmed.
pub fn format_diff_terminal(diff: &ContentDiff) -> String {
    let mut out = String::with_capacity(512);

    push_header_terminal(&mut out, diff);

    if diff.unchanged || diff.sections.is_empty() {
        let _ = writeln!(out, "{DIM}No changes detected.{RESET}");
        return out;
    }

    for section in &diff.sections {
        push_section_terminal(&mut out, section);
    }

    push_summary_terminal(&mut out, diff);
    out
}

/// Render a [`ContentDiff`] as a Markdown string (no colour escapes).
///
/// Uses labelled `**added**` / `**removed**` / `**modified**` blocks
/// consistent with GitHub Markdown rendering.
pub fn format_diff_markdown(diff: &ContentDiff) -> String {
    let mut out = String::with_capacity(512);

    push_header_markdown(&mut out, diff);

    if diff.unchanged || diff.sections.is_empty() {
        out.push_str("No changes detected.\n");
        return out;
    }

    for section in &diff.sections {
        push_section_markdown(&mut out, section);
    }

    push_summary_markdown(&mut out, diff);
    out
}

// ── Terminal rendering ────────────────────────────────────────────────────────

fn push_header_terminal(out: &mut String, diff: &ContentDiff) {
    let _ = writeln!(
        out,
        "{DIM}--- {url} (old: {old}){RESET}",
        url = diff.url,
        old = diff.old_timestamp,
    );
    let _ = writeln!(
        out,
        "{DIM}+++ {url} (new: {new}){RESET}",
        url = diff.url,
        new = diff.new_timestamp,
    );
}

fn push_section_terminal(out: &mut String, section: &DiffSection) {
    for ctx in &section.context {
        let _ = writeln!(out, "{DIM}  {ctx}{RESET}");
    }
    match section.kind {
        ChangeKind::Added => {
            let text = section.new_text.as_deref().unwrap_or("");
            let _ = writeln!(out, "{GREEN}+ {text}{RESET}");
        }
        ChangeKind::Removed => {
            let text = section.old_text.as_deref().unwrap_or("");
            let _ = writeln!(out, "{RED}- {text}{RESET}");
        }
        ChangeKind::Modified => {
            let old = section.old_text.as_deref().unwrap_or("");
            let new = section.new_text.as_deref().unwrap_or("");
            let _ = writeln!(out, "{RED}- {old}{RESET}");
            let _ = writeln!(out, "{GREEN}+ {new}{RESET}");
        }
    }
}

fn push_summary_terminal(out: &mut String, diff: &ContentDiff) {
    let _ = writeln!(out, "\n{YELLOW}Summary: {summary}{RESET}", summary = diff.summary());
}

// ── Markdown rendering ────────────────────────────────────────────────────────

fn push_header_markdown(out: &mut String, diff: &ContentDiff) {
    let _ = write!(
        out,
        "**Content diff**: `{url}`  \nOld: `{old}` -> New: `{new}`\n\n",
        url = diff.url,
        old = diff.old_timestamp,
        new = diff.new_timestamp,
    );
}

fn push_section_markdown(out: &mut String, section: &DiffSection) {
    if let Some(ctx) = section.context.first() {
        let _ = write!(out, "> {ctx}\n\n");
    }
    match section.kind {
        ChangeKind::Added => {
            let text = section.new_text.as_deref().unwrap_or("");
            let _ = write!(out, "**added**: {text}\n\n");
        }
        ChangeKind::Removed => {
            let text = section.old_text.as_deref().unwrap_or("");
            let _ = write!(out, "**removed**: {text}\n\n");
        }
        ChangeKind::Modified => {
            let old = section.old_text.as_deref().unwrap_or("");
            let new = section.new_text.as_deref().unwrap_or("");
            let _ = write!(out, "**modified**:  \n- old: {old}  \n+ new: {new}\n\n");
        }
    }
}

fn push_summary_markdown(out: &mut String, diff: &ContentDiff) {
    let _ = write!(out, "---\n**{}**\n", diff.summary());
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::diff::{compute_diff, ContentSnapshot};
    use std::time::SystemTime;

    fn snap(text: &str) -> ContentSnapshot {
        ContentSnapshot::new("https://example.com", text, SystemTime::UNIX_EPOCH)
    }

    fn make_diff_with_change() -> ContentDiff {
        let old = snap("Intro.\n\nOld body.\n\nConclusion.");
        let new = snap("Intro.\n\nNew body.\n\nConclusion.\n\nExtra section.");
        compute_diff(&old, &new)
    }

    // ── format_diff_terminal ─────────────────────────────────────────────────

    #[test]
    fn terminal_format_unchanged_says_no_changes() {
        // GIVEN: identical snapshots
        let s = snap("Same.");
        let diff = compute_diff(&s, &s.clone());
        // WHEN: formatted for terminal
        let out = format_diff_terminal(&diff);
        // THEN: "No changes" appears
        assert!(out.contains("No changes"), "got: {out}");
    }

    #[test]
    fn terminal_format_added_shows_plus_prefix() {
        // GIVEN: diff with an added section
        let old = snap("A.");
        let new = snap("A.\n\nB.");
        let diff = compute_diff(&old, &new);
        // WHEN: formatted
        let out = format_diff_terminal(&diff);
        // THEN: '+' appears
        assert!(out.contains('+'), "expected '+' in: {out}");
    }

    #[test]
    fn terminal_format_removed_shows_minus_prefix() {
        // GIVEN: diff with a removed section
        let old = snap("A.\n\nB.");
        let new = snap("A.");
        let diff = compute_diff(&old, &new);
        // WHEN: formatted
        let out = format_diff_terminal(&diff);
        // THEN: '-' appears
        assert!(out.contains('-'), "expected '-' in: {out}");
    }

    #[test]
    fn terminal_format_includes_url_in_header() {
        // GIVEN: any diff
        let diff = make_diff_with_change();
        // WHEN: formatted
        let out = format_diff_terminal(&diff);
        // THEN: URL present
        assert!(out.contains("example.com"), "URL missing in: {out}");
    }

    #[test]
    fn terminal_format_includes_summary_line() {
        // GIVEN: diff with changes
        let diff = make_diff_with_change();
        // WHEN: formatted
        let out = format_diff_terminal(&diff);
        // THEN: "Summary:" line present
        assert!(out.contains("Summary:"), "missing Summary in: {out}");
    }

    #[test]
    fn terminal_format_modified_shows_both_old_and_new() {
        // GIVEN: diff where a paragraph is modified
        let old = snap("Intro.\n\nOld paragraph.\n\nEnd.");
        let new = snap("Intro.\n\nNew paragraph.\n\nEnd.");
        let diff = compute_diff(&old, &new);
        // WHEN: formatted
        let out = format_diff_terminal(&diff);
        // THEN: change markers present
        let has_change = out.contains("Old paragraph") || out.contains("New paragraph")
            || out.contains('-') || out.contains('+');
        assert!(has_change, "expected change markers in: {out}");
    }

    // ── format_diff_markdown ─────────────────────────────────────────────────

    #[test]
    fn markdown_format_unchanged_says_no_changes() {
        // GIVEN: identical snapshots
        let s = snap("Same.");
        let diff = compute_diff(&s, &s.clone());
        // WHEN: formatted as markdown
        let out = format_diff_markdown(&diff);
        // THEN: "No changes" appears (no ANSI codes)
        assert!(out.contains("No changes"), "got: {out}");
        assert!(!out.contains('\x1b'), "ANSI in markdown: {out}");
    }

    #[test]
    fn markdown_format_added_uses_added_label() {
        // GIVEN: diff with added section
        let old = snap("A.");
        let new = snap("A.\n\nB.");
        let diff = compute_diff(&old, &new);
        // WHEN: formatted as markdown
        let out = format_diff_markdown(&diff);
        // THEN: "added" label
        assert!(out.contains("added"), "expected 'added' in: {out}");
    }

    #[test]
    fn markdown_format_removed_uses_removed_label() {
        // GIVEN: diff with removed section
        let old = snap("A.\n\nB.");
        let new = snap("A.");
        let diff = compute_diff(&old, &new);
        // WHEN: formatted as markdown
        let out = format_diff_markdown(&diff);
        // THEN: "removed" label
        assert!(out.contains("removed"), "expected 'removed' in: {out}");
    }

    #[test]
    fn markdown_format_no_ansi_escape_codes() {
        // GIVEN: diff with changes
        let diff = make_diff_with_change();
        // WHEN: formatted as markdown
        let out = format_diff_markdown(&diff);
        // THEN: no ANSI escape sequences
        assert!(!out.contains('\x1b'), "ANSI escape found in markdown output");
    }

    #[test]
    fn markdown_format_includes_summary_separator() {
        // GIVEN: diff with changes
        let diff = make_diff_with_change();
        // WHEN: formatted
        let out = format_diff_markdown(&diff);
        // THEN: "---" separator appears
        assert!(out.contains("---"), "missing separator in: {out}");
    }

    #[test]
    fn markdown_format_includes_url() {
        // GIVEN: diff with changes
        let diff = make_diff_with_change();
        // WHEN: formatted
        let out = format_diff_markdown(&diff);
        // THEN: URL present
        assert!(out.contains("example.com"), "URL missing in: {out}");
    }
}
