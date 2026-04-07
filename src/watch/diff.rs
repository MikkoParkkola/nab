//! Diff strategies for comparing watch snapshot bodies.
//!
//! All strategies operate on UTF-8 text (HTML is decoded before hashing).
//! The primary concern for watch polling is content-addressed hashing —
//! the actual diff text shown to users is kept simple (line count changes).

use sha2::{Digest, Sha256};

use super::types::DiffKind;

// ─── Hash ─────────────────────────────────────────────────────────────────────

/// Compute the SHA-256 hex digest of `body`.
pub fn sha256_hex(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    hex::encode(hasher.finalize())
}

// ─── Content extraction ───────────────────────────────────────────────────────

/// Extract the text to be hashed from `html`, applying `selector` if given.
///
/// For `DiffKind::Dom` / `DiffKind::Semantic` with a selector, only the
/// matched subtree is extracted. Falls back to full-page text on any error.
pub fn extract_content(html: &str, selector: Option<&str>, kind: &DiffKind) -> String {
    match (selector, kind) {
        (Some(sel), DiffKind::Dom | DiffKind::Text | DiffKind::Semantic) => {
            extract_selector(html, sel).unwrap_or_else(|| html.to_owned())
        }
        _ => html.to_owned(),
    }
}

/// Extract the inner text of the first element matching `selector`.
fn extract_selector(html: &str, selector: &str) -> Option<String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let sel = Selector::parse(selector).ok()?;
    doc.select(&sel)
        .next()
        .map(|el| el.html())
}

// ─── Diff summary ─────────────────────────────────────────────────────────────

/// Produce a human-readable one-line summary of the change.
///
/// Currently reports the number of lines added/removed relative to the old body.
/// Future work: full semantic or DOM diffing.
pub fn diff_summary(old: &str, new: &str) -> String {
    let old_lines = old.lines().count();
    let new_lines = new.lines().count();
    match new_lines.cmp(&old_lines) {
        std::cmp::Ordering::Greater => {
            format!("content grew by {} lines", new_lines - old_lines)
        }
        std::cmp::Ordering::Less => {
            format!("content shrank by {} lines", old_lines - new_lines)
        }
        std::cmp::Ordering::Equal => "content changed (same line count)".into(),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_diff_detects_change() {
        // GIVEN: two different bodies
        let a = b"hello world";
        let b = b"hello rust";
        // WHEN: hashed
        let ha = sha256_hex(a);
        let hb = sha256_hex(b);
        // THEN: hashes differ
        assert_ne!(ha, hb);
    }

    #[test]
    fn identical_bodies_produce_same_hash() {
        // GIVEN: identical bytes
        let body = b"same content";
        assert_eq!(sha256_hex(body), sha256_hex(body));
    }

    #[test]
    fn selector_diff_isolates_subtree() {
        // GIVEN: HTML with a specific section
        let html = r#"<html><body><div id="target"><p>price: $99</p></div><footer>noise</footer></body></html>"#;
        // WHEN: extract with selector
        let extracted = extract_content(html, Some("#target"), &DiffKind::Dom);
        // THEN: contains target, does not contain noise
        assert!(extracted.contains("price"), "got: {extracted}");
        assert!(!extracted.contains("noise"), "got: {extracted}");
    }

    #[test]
    fn selector_fallback_on_miss() {
        // GIVEN: HTML without the selector
        let html = "<html><body><p>no match</p></body></html>";
        // WHEN: selector doesn't match
        let extracted = extract_content(html, Some("#nonexistent"), &DiffKind::Dom);
        // THEN: falls back to full page
        assert!(extracted.contains("no match"), "got: {extracted}");
    }

    #[test]
    fn diff_summary_growing() {
        let old = "a\nb\nc";
        let new = "a\nb\nc\nd\ne";
        let s = diff_summary(old, new);
        assert!(s.contains("grew"), "got: {s}");
    }

    #[test]
    fn diff_summary_shrinking() {
        let old = "a\nb\nc\nd";
        let new = "a\nb";
        let s = diff_summary(old, new);
        assert!(s.contains("shrank"), "got: {s}");
    }
}
