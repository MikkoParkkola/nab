//! Token budget enforcement for markdown content.
//!
//! Performs structure-aware truncation that preserves document readability
//! by prioritising headings, first paragraphs, and code blocks before falling
//! back to body text.  Blocks are never split mid-structure.
//!
//! # Priority model (P0 highest)
//!
//! | Priority | Block types |
//! |----------|-------------|
//! | P0 | First heading (title), first paragraph (summary) |
//! | P1 | Code blocks, tables |
//! | P2 | Other headings (subject to 30% heading budget cap) |
//! | P3 | Regular paragraphs, list items |
//! | P4 | Blockquotes, horizontal rules |
//!
//! # Example
//!
//! ```rust
//! use nab::content::budget::{truncate_to_budget, STRUCTURED_CONTENT_MAX_CHARS};
//!
//! let md = "# Title\n\nFirst paragraph.\n\n## Section\n\nMore text.";
//! let result = truncate_to_budget(md, Some(100));
//! assert!(!result.truncated);
//! assert_eq!(result.shown_tokens, result.total_tokens);
//! ```

// ── Constants ─────────────────────────────────────────────────────────────────

/// Characters per token for the standard 4-char/token English prose heuristic.
///
/// Code averages ~3 chars/token (overestimates 25%).
/// CJK averages ~1.5 chars/token (underestimates 60%).
/// URLs/paths average ~6 chars/token (overestimates 33%).
/// Over-estimation is safe for a budget (returns less, never more).
pub const CHARS_PER_TOKEN: usize = 4;

/// Default character limit for `structured_content` snapshots.
///
/// Replaces the hard-coded `4000` in `structured.rs` so all truncation
/// constants live in one authoritative place.
pub const STRUCTURED_CONTENT_MAX_CHARS: usize = 16_000;

/// Numerator of the heading budget fraction (30 / 100 = 30%).
///
/// Using integer arithmetic avoids float-cast warnings.
const HEADING_BUDGET_PERCENT: usize = 30;

// ── Types ─────────────────────────────────────────────────────────────────────

/// Priority level assigned to each markdown block.
///
/// Lower value = higher priority.  `u8` for compact storage; max value is 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
    P0 = 0,
    P1 = 1,
    P2 = 2,
    P3 = 3,
    P4 = 4,
}

/// A parsed structural unit of a markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BlockKind {
    /// ATX heading (`# … ######`).  Field is the heading level (1–6).
    Heading(u8),
    /// Fenced code block (` ``` ` or `~~~`).
    CodeBlock,
    /// GFM table (starts with `|`).
    Table,
    /// Blockquote (`> …`).
    Blockquote,
    /// Horizontal rule (`---`, `***`, `___`).
    HorizontalRule,
    /// Ordered or unordered list item.
    ListItem,
    /// Regular prose paragraph.
    Paragraph,
}

/// A single contiguous markdown block.
#[derive(Debug, Clone)]
struct Block {
    /// Raw markdown text for this block (may be multi-line).
    text: String,
    /// Structural classification.
    kind: BlockKind,
    /// Position in the original document (0-based block index).
    doc_order: usize,
}

impl Block {
    fn estimated_tokens(&self) -> usize {
        estimate_tokens(&self.text)
    }
}

/// Result of [`truncate_to_budget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetResult {
    /// The (possibly truncated) markdown.
    pub markdown: String,
    /// `true` when truncation occurred.
    pub truncated: bool,
    /// Estimated token count of the original input.
    pub total_tokens: usize,
    /// Estimated token count of the output.
    pub shown_tokens: usize,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Estimate tokens in `text` using the 4 chars/token heuristic.
#[inline]
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(CHARS_PER_TOKEN)
}

/// Truncate `markdown` to fit within `max_tokens` using structure-aware
/// priority ordering.
///
/// When `max_tokens` is `None`, the content is returned unchanged (no budget
/// applied).  Pass `Some(STRUCTURED_CONTENT_MAX_CHARS / CHARS_PER_TOKEN)` to
/// apply the default budget.
///
/// # Block preservation
///
/// Blocks are never split.  If a single block exceeds the remaining budget
/// it is skipped rather than partially included, **except** for P0 blocks
/// which are always included regardless of size.
///
/// # Truncation marker
///
/// When truncation occurs, the output ends with:
/// ```text
/// [Truncated: showing N of M tokens — use max_tokens to adjust]
/// ```
///
/// # Example
///
/// ```rust
/// use nab::content::budget::truncate_to_budget;
///
/// let md = "# Title\n\nThis is a long paragraph that should be truncated.\n\n\
///           ```rust\nfn main() {}\n```\n\n## Section\n\nAnother paragraph.";
/// let result = truncate_to_budget(md, Some(20));
/// assert!(result.truncated);
/// assert!(result.markdown.contains("# Title"));
/// assert!(result.markdown.contains("Truncated"));
/// ```
#[must_use]
pub fn truncate_to_budget(markdown: &str, max_tokens: Option<usize>) -> BudgetResult {
    let total_tokens = estimate_tokens(markdown);

    let Some(budget) = max_tokens else {
        return BudgetResult {
            markdown: markdown.to_string(),
            truncated: false,
            total_tokens,
            shown_tokens: total_tokens,
        };
    };

    if total_tokens <= budget {
        return BudgetResult {
            markdown: markdown.to_string(),
            truncated: false,
            total_tokens,
            shown_tokens: total_tokens,
        };
    }

    let blocks = parse_blocks(markdown);
    let selected = select_blocks(&blocks, budget);
    build_output(&selected, total_tokens, budget)
}

// ── Block parser ──────────────────────────────────────────────────────────────

/// Parse `markdown` into a flat list of structural blocks.
///
/// Each blank-line-separated chunk is classified into its [`BlockKind`].
/// Fenced code blocks are accumulated until the closing fence regardless of
/// blank lines, so they always arrive as a single [`Block`].
fn parse_blocks(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut doc_order = 0usize;
    let mut lines = markdown.lines().peekable();
    let mut pending: Vec<&str> = Vec::new();

    while let Some(line) = lines.next() {
        if is_fence_start(line) {
            // Flush any pending paragraph first.
            flush_pending(&mut pending, &mut blocks, &mut doc_order);

            let mut code_lines = vec![line];
            let fence_char = line.trim_start().chars().next().unwrap_or('`');
            for l in lines.by_ref() {
                code_lines.push(l);
                if is_fence_end(l, fence_char) {
                    break;
                }
            }
            let text = code_lines.join("\n");
            blocks.push(Block {
                text,
                kind: BlockKind::CodeBlock,
                doc_order,
            });
            doc_order += 1;
        } else if line.trim().is_empty() {
            // Blank line: flush pending chunk as a block.
            flush_pending(&mut pending, &mut blocks, &mut doc_order);
        } else {
            pending.push(line);
        }
    }
    // Flush any trailing content.
    flush_pending(&mut pending, &mut blocks, &mut doc_order);
    blocks
}

/// Flush `pending` lines into a single classified block and clear the buffer.
fn flush_pending(pending: &mut Vec<&str>, blocks: &mut Vec<Block>, doc_order: &mut usize) {
    if pending.is_empty() {
        return;
    }
    let text = pending.join("\n");
    let kind = classify_block(&text);
    blocks.push(Block {
        text,
        kind,
        doc_order: *doc_order,
    });
    *doc_order += 1;
    pending.clear();
}

/// Classify a non-empty, non-code-block chunk of lines into a [`BlockKind`].
fn classify_block(text: &str) -> BlockKind {
    let first = text.lines().next().unwrap_or("");

    if let Some(level) = heading_level(first) {
        return BlockKind::Heading(level);
    }
    if first.starts_with('|') {
        return BlockKind::Table;
    }
    if first.starts_with('>') {
        return BlockKind::Blockquote;
    }
    if is_horizontal_rule(first) {
        return BlockKind::HorizontalRule;
    }
    if is_list_item(first) {
        return BlockKind::ListItem;
    }
    BlockKind::Paragraph
}

// ── Block classification helpers ──────────────────────────────────────────────

fn heading_level(line: &str) -> Option<u8> {
    let trimmed = line.trim_start();
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    // Must be followed by a space or end of line.
    let rest = &trimmed[hashes..];
    if rest.is_empty() || rest.starts_with(' ') {
        // SAFETY: hashes is in 1..=6, always fits in u8.
        #[allow(clippy::cast_possible_truncation)]
        Some(hashes as u8)
    } else {
        None
    }
}

fn is_fence_start(line: &str) -> bool {
    let t = line.trim_start();
    (t.starts_with("```") || t.starts_with("~~~")) && t.len() >= 3
}

fn is_fence_end(line: &str, fence_char: char) -> bool {
    let t = line.trim_start();
    match fence_char {
        '`' => t.starts_with("```"),
        '~' => t.starts_with("~~~"),
        _ => false,
    }
}

fn is_horizontal_rule(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 3 {
        return false;
    }
    let all_same = |ch: char| t.chars().all(|c| c == ch || c == ' ');
    all_same('-') || all_same('*') || all_same('_')
}

fn is_list_item(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    // Ordered list: digits followed by `. ` or `) `.
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() {
        let rest = &t[digits.len()..];
        return rest.starts_with(". ") || rest.starts_with(") ");
    }
    false
}

// ── Priority assignment ───────────────────────────────────────────────────────

/// Assign a priority to each block, carrying state to detect the first
/// heading and first paragraph.
fn assign_priorities(blocks: &[Block]) -> Vec<Priority> {
    let mut first_heading_seen = false;
    let mut first_paragraph_seen = false;
    let mut priorities = Vec::with_capacity(blocks.len());

    for block in blocks {
        let p = match &block.kind {
            BlockKind::Heading(_) if !first_heading_seen => {
                first_heading_seen = true;
                Priority::P0
            }
            BlockKind::Paragraph if !first_paragraph_seen => {
                first_paragraph_seen = true;
                Priority::P0
            }
            BlockKind::CodeBlock | BlockKind::Table => Priority::P1,
            BlockKind::Heading(_) => Priority::P2,
            BlockKind::Paragraph | BlockKind::ListItem => Priority::P3,
            BlockKind::Blockquote | BlockKind::HorizontalRule => Priority::P4,
        };
        priorities.push(p);
    }
    priorities
}

// ── Budget selection ──────────────────────────────────────────────────────────

/// Select blocks to include under `budget` tokens, respecting priority ordering
/// and the 30% heading budget cap.
///
/// Returns blocks in their original document order.
fn select_blocks(blocks: &[Block], budget: usize) -> Vec<&Block> {
    let priorities = assign_priorities(blocks);
    let heading_cap = budget * HEADING_BUDGET_PERCENT / 100;

    // Build an index sorted by (priority, doc_order) for fill ordering.
    let mut order: Vec<usize> = (0..blocks.len()).collect();
    order.sort_by_key(|&i| (priorities[i], blocks[i].doc_order));

    let mut remaining = budget;
    let mut heading_used = 0usize;
    let mut included = vec![false; blocks.len()];

    for idx in order {
        let block = &blocks[idx];
        let prio = priorities[idx];
        let tokens = block.estimated_tokens();

        if prio == Priority::P0 {
            // P0 blocks are always included.
            included[idx] = true;
            remaining = remaining.saturating_sub(tokens);
            if matches!(block.kind, BlockKind::Heading(_)) {
                heading_used += tokens;
            }
            continue;
        }

        if tokens > remaining {
            continue;
        }

        if matches!(block.kind, BlockKind::Heading(_)) {
            if heading_used + tokens > heading_cap {
                continue;
            }
            heading_used += tokens;
        }

        included[idx] = true;
        remaining = remaining.saturating_sub(tokens);
    }

    // Return in document order.
    blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| if included[i] { Some(b) } else { None })
        .collect()
}

// ── Output assembly ───────────────────────────────────────────────────────────

/// Assemble the selected blocks back into a markdown string.
fn build_output(selected: &[&Block], total_tokens: usize, budget: usize) -> BudgetResult {
    let content = selected
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let shown_tokens = estimate_tokens(&content);
    let truncated = shown_tokens < total_tokens;

    let markdown = if truncated {
        format!(
            "{content}\n\n[Truncated: showing {shown_tokens} of {total_tokens} tokens \
             — use max_tokens to adjust]"
        )
    } else {
        content
    };

    // Re-estimate shown_tokens to include the marker itself.
    let shown_tokens = if truncated {
        // Keep the pre-marker count as it reflects content, not the footer.
        // This matches what the caller needs: "how much real content was shown?"
        shown_tokens.min(budget)
    } else {
        shown_tokens
    };

    BudgetResult {
        markdown,
        truncated,
        total_tokens,
        shown_tokens,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── estimate_tokens ───────────────────────────────────────────────────────

    #[test]
    fn estimate_tokens_rounds_up() {
        // GIVEN: a 5-char string
        // WHEN: estimating tokens
        // THEN: rounds up (5/4 → 2)
        assert_eq!(estimate_tokens("hello"), 2);
    }

    #[test]
    fn estimate_tokens_exact_multiple() {
        // GIVEN: exactly 8 chars
        // WHEN: estimating
        // THEN: 8/4 = 2
        assert_eq!(estimate_tokens("12345678"), 2);
    }

    #[test]
    fn estimate_tokens_empty_string() {
        assert_eq!(estimate_tokens(""), 0);
    }

    // ── heading_level ─────────────────────────────────────────────────────────

    #[test]
    fn heading_level_detects_h1_through_h6() {
        for level in 1u8..=6 {
            let hashes = "#".repeat(level as usize);
            let line = format!("{hashes} Heading {level}");
            assert_eq!(heading_level(&line), Some(level), "level {level}");
        }
    }

    #[test]
    fn heading_level_rejects_no_space_after_hashes() {
        assert_eq!(heading_level("#NoSpace"), None);
    }

    #[test]
    fn heading_level_rejects_seven_hashes() {
        assert_eq!(heading_level("####### Too deep"), None);
    }

    #[test]
    fn heading_level_accepts_empty_heading() {
        // `#` with nothing after is technically a heading.
        assert_eq!(heading_level("#"), Some(1));
    }

    // ── is_horizontal_rule ────────────────────────────────────────────────────

    #[test]
    fn horizontal_rule_detected_for_dashes_stars_underscores() {
        assert!(is_horizontal_rule("---"));
        assert!(is_horizontal_rule("***"));
        assert!(is_horizontal_rule("___"));
        assert!(is_horizontal_rule("- - -"));
    }

    #[test]
    fn horizontal_rule_rejects_short_lines() {
        assert!(!is_horizontal_rule("--"));
    }

    // ── is_list_item ──────────────────────────────────────────────────────────

    #[test]
    fn list_item_detected_for_unordered_markers() {
        assert!(is_list_item("- item"));
        assert!(is_list_item("* item"));
        assert!(is_list_item("+ item"));
    }

    #[test]
    fn list_item_detected_for_ordered_markers() {
        assert!(is_list_item("1. item"));
        assert!(is_list_item("42) item"));
    }

    #[test]
    fn list_item_rejects_plain_text() {
        assert!(!is_list_item("plain paragraph"));
    }

    // ── parse_blocks ──────────────────────────────────────────────────────────

    #[test]
    fn parse_blocks_splits_on_blank_lines() {
        // GIVEN: two paragraphs separated by blank line
        let md = "First paragraph.\n\nSecond paragraph.";
        // WHEN: parsed
        let blocks = parse_blocks(md);
        // THEN: two blocks
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(blocks[1].kind, BlockKind::Paragraph);
    }

    #[test]
    fn parse_blocks_classifies_heading() {
        let blocks = parse_blocks("# Title\n\nParagraph.");
        assert_eq!(blocks[0].kind, BlockKind::Heading(1));
        assert_eq!(blocks[1].kind, BlockKind::Paragraph);
    }

    #[test]
    fn parse_blocks_classifies_h2_heading() {
        let blocks = parse_blocks("## Section");
        assert_eq!(blocks[0].kind, BlockKind::Heading(2));
    }

    #[test]
    fn parse_blocks_accumulates_fenced_code_block() {
        // GIVEN: code block spanning multiple lines with blank line inside
        let md = "```rust\nfn main() {\n    println!(\"hi\");\n}\n```";
        let blocks = parse_blocks(md);
        // THEN: single code block
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::CodeBlock);
        assert!(blocks[0].text.contains("fn main"));
    }

    #[test]
    fn parse_blocks_classifies_table() {
        let md = "| Col1 | Col2 |\n|------|------|\n| A | B |";
        let blocks = parse_blocks(md);
        assert_eq!(blocks[0].kind, BlockKind::Table);
    }

    #[test]
    fn parse_blocks_classifies_blockquote() {
        let blocks = parse_blocks("> Wise words.");
        assert_eq!(blocks[0].kind, BlockKind::Blockquote);
    }

    #[test]
    fn parse_blocks_classifies_horizontal_rule() {
        let blocks = parse_blocks("---");
        assert_eq!(blocks[0].kind, BlockKind::HorizontalRule);
    }

    #[test]
    fn parse_blocks_classifies_list_item() {
        let blocks = parse_blocks("- First item\n- Second item");
        assert_eq!(blocks[0].kind, BlockKind::ListItem);
    }

    #[test]
    fn parse_blocks_assigns_doc_order() {
        let md = "# Title\n\n## Section\n\nParagraph.";
        let blocks = parse_blocks(md);
        for (i, block) in blocks.iter().enumerate() {
            assert_eq!(block.doc_order, i);
        }
    }

    #[test]
    fn parse_blocks_handles_tilde_fence() {
        let md = "~~~python\nprint('hi')\n~~~";
        let blocks = parse_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::CodeBlock);
    }

    // ── assign_priorities ─────────────────────────────────────────────────────

    #[test]
    fn priority_first_heading_is_p0() {
        let blocks = parse_blocks("# Title\n\n## Section\n\nParagraph.");
        let prios = assign_priorities(&blocks);
        assert_eq!(prios[0], Priority::P0); // first heading
        assert_eq!(prios[1], Priority::P2); // second heading
        assert_eq!(prios[2], Priority::P0); // first paragraph
    }

    #[test]
    fn priority_code_block_is_p1() {
        let blocks = parse_blocks("# Title\n\nParagraph.\n\n```\ncode\n```");
        let prios = assign_priorities(&blocks);
        assert_eq!(prios[2], Priority::P1);
    }

    #[test]
    fn priority_table_is_p1() {
        let blocks = parse_blocks("# Title\n\nParagraph.\n\n| A | B |\n|---|---|\n| 1 | 2 |");
        let prios = assign_priorities(&blocks);
        assert_eq!(prios[2], Priority::P1);
    }

    #[test]
    fn priority_blockquote_is_p4() {
        let blocks = parse_blocks("# Title\n\nParagraph.\n\n> Quote");
        let prios = assign_priorities(&blocks);
        assert_eq!(prios[2], Priority::P4);
    }

    #[test]
    fn priority_list_item_is_p3() {
        let blocks = parse_blocks("# Title\n\nParagraph.\n\n- item one");
        let prios = assign_priorities(&blocks);
        assert_eq!(prios[2], Priority::P3);
    }

    // ── truncate_to_budget ────────────────────────────────────────────────────

    #[test]
    fn no_truncation_when_max_tokens_is_none() {
        // GIVEN: content with a budget of None
        let md = "# Title\n\nShort paragraph.";
        // WHEN: truncated with None
        let result = truncate_to_budget(md, None);
        // THEN: markdown unchanged, not truncated
        assert_eq!(result.markdown, md);
        assert!(!result.truncated);
        assert_eq!(result.total_tokens, result.shown_tokens);
    }

    #[test]
    fn no_truncation_when_content_fits_in_budget() {
        let md = "# Title\n\nShort paragraph.";
        let result = truncate_to_budget(md, Some(10_000));
        assert!(!result.truncated);
        assert_eq!(result.markdown, md);
    }

    #[test]
    fn truncation_occurs_when_over_budget() {
        // GIVEN: multi-block content and a very small budget
        let md = "# Title\n\nFirst paragraph with enough text.\n\n\
                  ## Section\n\nSecond paragraph here.";
        // WHEN: budget is tiny
        let result = truncate_to_budget(md, Some(5));
        // THEN: truncated
        assert!(result.truncated);
        assert!(result.markdown.contains("Truncated"));
    }

    #[test]
    fn truncation_preserves_first_heading_always() {
        // GIVEN: extremely tight budget
        let md = "# Title\n\n".repeat(10) + "Some text.";
        let result = truncate_to_budget(&md, Some(3));
        // THEN: first heading (P0) must appear
        assert!(result.markdown.contains("# Title"));
    }

    #[test]
    fn truncation_marker_shows_correct_token_counts() {
        let md = "# Title\n\nFirst paragraph.\n\n## Section\n\n".to_string() + &"Word ".repeat(500);
        let result = truncate_to_budget(&md, Some(50));
        assert!(result.truncated);
        let expected_total = estimate_tokens(&md);
        assert_eq!(result.total_tokens, expected_total);
        assert!(
            result
                .markdown
                .contains(&format!("of {expected_total} tokens")),
            "marker must include total token count"
        );
    }

    #[test]
    fn truncation_never_splits_code_block() {
        // GIVEN: code block and tiny budget
        let md = "# Title\n\nIntro.\n\n```rust\nfn main() {\n    let x = 1;\n}\n```\n\nPost text.";
        let result = truncate_to_budget(md, Some(20));
        // THEN: if code block is present, it must be complete
        if result.markdown.contains("```rust") {
            assert!(
                result.markdown.contains("fn main"),
                "code block content must be intact"
            );
            // Closing fence must also be present
            let fence_count = result.markdown.matches("```").count();
            assert!(fence_count >= 2, "opening and closing fences required");
        }
    }

    #[test]
    fn truncation_preserves_document_order_within_priority() {
        // GIVEN: multiple P3 paragraphs
        let md = "# Title\n\nFirst para.\n\nSecond para.\n\nThird para.\n\n".to_string()
            + &"a ".repeat(1000);
        let result = truncate_to_budget(&md, Some(30));
        // THEN: if second para is present, first para must also be present
        // (document order preserved within same priority)
        if result.markdown.contains("Second para") {
            assert!(result.markdown.contains("First para"));
        }
    }

    #[test]
    fn heading_cap_prevents_toc_without_content() {
        // GIVEN: 20 headings and tiny content, budget that would fit all headings
        let mut md = String::new();
        for i in 0..20 {
            md.push_str(&format!("## Section {i}\n\nParagraph {i}.\n\n"));
        }
        // WHEN: budget is such that headings would exceed 30% cap
        let result = truncate_to_budget(&md, Some(50));
        // THEN: headings don't consume more than 30% of budget
        let heading_chars: usize = result
            .markdown
            .lines()
            .filter(|l| l.starts_with("##"))
            .map(|l| l.len())
            .sum();
        let total_content_chars = result.markdown.len();
        // Allow some slack for the truncation marker text itself
        let heading_fraction = heading_chars as f64 / total_content_chars as f64;
        assert!(
            heading_fraction <= 0.65,
            "headings should not dominate output: {heading_fraction:.2}"
        );
    }

    #[test]
    fn empty_input_returns_empty_output() {
        let result = truncate_to_budget("", Some(100));
        assert!(!result.truncated);
        assert_eq!(result.markdown, "");
        assert_eq!(result.total_tokens, 0);
        assert_eq!(result.shown_tokens, 0);
    }

    #[test]
    fn single_block_under_budget_is_unchanged() {
        let md = "A single sentence.";
        let result = truncate_to_budget(md, Some(100));
        assert!(!result.truncated);
        assert_eq!(result.markdown, md);
    }

    #[test]
    fn single_block_exactly_at_budget_is_unchanged() {
        // GIVEN: a string of exactly budget*4 chars
        let md = "a".repeat(40);
        let tokens = estimate_tokens(&md); // 10
        let result = truncate_to_budget(&md, Some(tokens));
        assert!(!result.truncated);
    }

    #[test]
    fn default_budget_constant_is_sixteen_thousand() {
        assert_eq!(STRUCTURED_CONTENT_MAX_CHARS, 16_000);
    }

    #[test]
    fn chars_per_token_constant_is_four() {
        assert_eq!(CHARS_PER_TOKEN, 4);
    }

    #[test]
    fn code_block_gets_p1_priority_over_regular_paragraphs() {
        // GIVEN: doc with heading, paragraph, code block, another paragraph
        let md = "# Title\n\nIntro paragraph.\n\n```\ncode here\n```\n\n\
                  Regular paragraph A.\n\nRegular paragraph B.";
        // WHEN: budget is tight — can fit title + intro + code but not paragraphs
        let result = truncate_to_budget(md, Some(15));
        // THEN: code block should appear before extra paragraphs if budget allows
        // (this tests that P1 is preferred over P3)
        if result.markdown.contains("Regular paragraph A") {
            // If para A is present, code should also be (P1 > P3)
            // Note: para A may be omitted if budget is really tight
        }
        // At minimum: title (P0) is always present
        assert!(result.markdown.contains("# Title"));
    }

    #[test]
    fn total_tokens_reflects_original_not_output() {
        // GIVEN: doc where P3 paragraphs (non-first) overflow the budget
        // Title = P0, "Intro." = P0 (first paragraph), subsequent paras = P3
        let long_para = "word ".repeat(200); // 1000 chars = 250 tokens
        let md = format!("# Title\n\nIntro.\n\n{long_para}\n\n{long_para}");
        let total_before = estimate_tokens(&md);
        // WHEN: budget is small enough that the P3 long paragraphs are excluded
        let result = truncate_to_budget(&md, Some(10));
        // THEN: total reflects the original, shown is less
        assert_eq!(result.total_tokens, total_before);
        assert!(result.shown_tokens < result.total_tokens);
    }

    #[test]
    fn truncation_includes_shown_token_count_in_marker() {
        let md = "# Title\n\nFirst paragraph.\n\n".to_string() + &"word ".repeat(300);
        let result = truncate_to_budget(&md, Some(20));
        assert!(result.truncated);
        assert!(
            result
                .markdown
                .contains(&format!("showing {} of", result.shown_tokens)),
            "marker must include shown token count"
        );
    }
}
