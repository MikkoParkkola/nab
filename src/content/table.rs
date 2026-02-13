//! Table detection from positioned text lines.
//!
//! Detects tables in PDF content by analyzing character X positions for
//! column alignment. The algorithm:
//!
//! 1. Find column boundaries (large horizontal gaps) in each line
//! 2. Group consecutive lines with aligned boundaries
//! 3. Runs of 3+ aligned lines are classified as tables
//! 4. Extract cell text by splitting at boundary positions
//!
//! Complexity: O(L * C) where L = lines, C = max columns per line.

use super::pdf::TextLine;

/// A detected table region in the document.
#[derive(Debug, Clone)]
pub struct Table {
    /// Page index (0-based).
    pub page: usize,
    /// Bounding box (in PDF points).
    pub x_min: f32,
    pub x_max: f32,
    pub y_min: f32,
    pub y_max: f32,
    /// Cell contents: `rows[row_idx][col_idx]`.
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// Render this table as a GitHub-flavored markdown table.
    pub fn to_markdown(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }

        let col_count = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        if col_count == 0 {
            return String::new();
        }

        let mut md = String::new();

        // Header row
        md.push('|');
        let header = &self.rows[0];
        for col in 0..col_count {
            let cell = header.get(col).map(String::as_str).unwrap_or("");
            md.push_str(&format!(" {cell} |"));
        }
        md.push('\n');

        // Separator row
        md.push('|');
        for _ in 0..col_count {
            md.push_str(" --- |");
        }
        md.push('\n');

        // Data rows
        for row in self.rows.iter().skip(1) {
            md.push('|');
            for col in 0..col_count {
                let cell = row.get(col).map(String::as_str).unwrap_or("");
                md.push_str(&format!(" {cell} |"));
            }
            md.push('\n');
        }

        md
    }
}

/// Minimum number of consecutive aligned rows to consider a table.
const MIN_TABLE_ROWS: usize = 3;

/// Tolerance (in PDF points) for column boundary alignment.
const BOUNDARY_TOLERANCE: f32 = 5.0;

/// Detect tables from reconstructed text lines.
///
/// Groups lines by page, finds column boundaries in each line, then
/// identifies runs of lines with aligned column boundaries. Runs of
/// [`MIN_TABLE_ROWS`]+ lines are classified as tables.
pub fn detect_tables(lines: &[TextLine]) -> Vec<Table> {
    let mut tables = Vec::new();

    // Group lines by page
    let mut page_groups: std::collections::BTreeMap<usize, Vec<&TextLine>> =
        std::collections::BTreeMap::new();
    for line in lines {
        page_groups.entry(line.page).or_default().push(line);
    }

    for (page, page_lines) in &page_groups {
        let line_boundaries: Vec<Vec<f32>> = page_lines
            .iter()
            .map(|line| find_column_boundaries(line))
            .collect();

        // Find runs of aligned boundaries
        let mut run_start = 0;
        while run_start < page_lines.len() {
            let mut run_end = run_start + 1;

            while run_end < page_lines.len()
                && boundaries_align(
                    &line_boundaries[run_start],
                    &line_boundaries[run_end],
                    BOUNDARY_TOLERANCE,
                )
            {
                run_end += 1;
            }

            let run_len = run_end - run_start;
            if run_len >= MIN_TABLE_ROWS && !line_boundaries[run_start].is_empty() {
                let boundaries = &line_boundaries[run_start];
                let rows: Vec<Vec<String>> = page_lines[run_start..run_end]
                    .iter()
                    .map(|line| split_at_boundaries(line, boundaries))
                    .collect();

                let table_lines = &page_lines[run_start..run_end];
                tables.push(Table {
                    page: *page,
                    x_min: table_lines
                        .iter()
                        .map(|l| l.x)
                        .fold(f32::INFINITY, f32::min),
                    x_max: table_lines
                        .iter()
                        .map(|l| {
                            l.chars
                                .last()
                                .map(|c| c.x + c.width)
                                .unwrap_or(l.x)
                        })
                        .fold(f32::NEG_INFINITY, f32::max),
                    y_min: table_lines
                        .iter()
                        .map(|l| l.y)
                        .fold(f32::INFINITY, f32::min),
                    y_max: table_lines
                        .iter()
                        .map(|l| l.y)
                        .fold(f32::NEG_INFINITY, f32::max),
                    rows,
                });
            }

            run_start = run_end;
        }
    }

    tables
}

/// Find X positions where column gaps occur in a text line.
///
/// A column gap is defined as a horizontal space greater than 2x the
/// average character width in that line.
fn find_column_boundaries(line: &TextLine) -> Vec<f32> {
    if line.chars.len() < 2 {
        return Vec::new();
    }

    let avg_width: f32 =
        line.chars.iter().map(|c| c.width).sum::<f32>() / line.chars.len() as f32;
    let gap_threshold = avg_width * 2.0;

    let mut boundaries = Vec::new();
    for i in 1..line.chars.len() {
        let gap = line.chars[i].x - (line.chars[i - 1].x + line.chars[i - 1].width);
        if gap > gap_threshold {
            // Boundary at the midpoint of the gap
            boundaries.push(line.chars[i - 1].x + line.chars[i - 1].width + gap / 2.0);
        }
    }
    boundaries
}

/// Check if two sets of column boundaries are aligned within tolerance.
fn boundaries_align(a: &[f32], b: &[f32], tolerance: f32) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(ax, bx)| (ax - bx).abs() < tolerance)
}

/// Split a line's text at column boundaries, producing cell strings.
fn split_at_boundaries(line: &TextLine, boundaries: &[f32]) -> Vec<String> {
    let mut cells = vec![String::new(); boundaries.len() + 1];

    for ch in &line.chars {
        let col = boundaries
            .iter()
            .position(|&b| ch.x < b)
            .unwrap_or(boundaries.len());
        cells[col].push(ch.ch);
    }

    cells.iter().map(|s| s.trim().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::pdf::PdfChar;

    fn make_char(ch: char, x: f32, y: f32, width: f32, page: usize) -> PdfChar {
        PdfChar {
            ch,
            x,
            y,
            width,
            height: 12.0,
            page,
        }
    }

    fn make_line(text: &str, x_start: f32, y: f32, char_width: f32, page: usize) -> TextLine {
        let chars: Vec<PdfChar> = text
            .chars()
            .enumerate()
            .map(|(i, ch)| make_char(ch, x_start + i as f32 * char_width, y, char_width, page))
            .collect();
        TextLine {
            text: text.to_string(),
            x: x_start,
            y,
            chars,
            page,
        }
    }

    fn make_table_line(
        cells: &[&str],
        y: f32,
        page: usize,
        col_width: f32,
        gap: f32,
    ) -> TextLine {
        let char_w = 6.0;
        let mut chars = Vec::new();
        let mut full_text = String::new();
        let mut x = 10.0;

        for (col_idx, cell) in cells.iter().enumerate() {
            if col_idx > 0 {
                x += gap; // inter-column gap
            }
            for ch in cell.chars() {
                chars.push(make_char(ch, x, y, char_w, page));
                full_text.push(ch);
                x += char_w;
            }
            // Pad to column width
            let used = cell.len() as f32 * char_w;
            if used < col_width {
                x += col_width - used;
            }
        }

        TextLine {
            text: full_text,
            x: chars.first().map(|c| c.x).unwrap_or(10.0),
            y,
            chars,
            page,
        }
    }

    #[test]
    fn table_to_markdown_empty() {
        let table = Table {
            page: 0,
            x_min: 0.0,
            x_max: 100.0,
            y_min: 0.0,
            y_max: 100.0,
            rows: vec![],
        };
        assert_eq!(table.to_markdown(), "");
    }

    #[test]
    fn table_to_markdown_simple() {
        let table = Table {
            page: 0,
            x_min: 0.0,
            x_max: 200.0,
            y_min: 0.0,
            y_max: 100.0,
            rows: vec![
                vec!["Name".into(), "Age".into()],
                vec!["Alice".into(), "30".into()],
                vec!["Bob".into(), "25".into()],
            ],
        };
        let md = table.to_markdown();
        assert!(md.contains("| Name | Age |"));
        assert!(md.contains("| --- | --- |"));
        assert!(md.contains("| Alice | 30 |"));
        assert!(md.contains("| Bob | 25 |"));
    }

    #[test]
    fn table_to_markdown_ragged_rows() {
        let table = Table {
            page: 0,
            x_min: 0.0,
            x_max: 200.0,
            y_min: 0.0,
            y_max: 100.0,
            rows: vec![
                vec!["A".into(), "B".into(), "C".into()],
                vec!["1".into(), "2".into()], // missing last column
            ],
        };
        let md = table.to_markdown();
        assert!(md.contains("| A | B | C |"));
        assert!(md.contains("| 1 | 2 |  |")); // empty cell for missing column
    }

    #[test]
    fn detect_tables_finds_aligned_columns() {
        // Create 4 lines with aligned column gaps
        let gap = 50.0; // large gap between columns
        let lines: Vec<TextLine> = vec![
            make_table_line(&["Name", "Age", "City"], 100.0, 0, 40.0, gap),
            make_table_line(&["Alice", "30", "NYC"], 88.0, 0, 40.0, gap),
            make_table_line(&["Bob", "25", "LA"], 76.0, 0, 40.0, gap),
            make_table_line(&["Carol", "35", "SF"], 64.0, 0, 40.0, gap),
        ];

        let tables = detect_tables(&lines);
        assert!(!tables.is_empty(), "Should detect at least one table");
        assert_eq!(tables[0].rows.len(), 4);
    }

    #[test]
    fn detect_tables_ignores_plain_text() {
        // Paragraphs without columnar alignment should not be detected as tables
        let lines: Vec<TextLine> = vec![
            make_line("This is a paragraph of regular text.", 10.0, 100.0, 6.0, 0),
            make_line("Another line of plain text content.", 10.0, 88.0, 6.0, 0),
            make_line("And one more line for good measure.", 10.0, 76.0, 6.0, 0),
        ];

        let tables = detect_tables(&lines);
        assert!(tables.is_empty(), "Plain text should not be detected as table");
    }

    #[test]
    fn boundaries_align_same() {
        assert!(boundaries_align(&[10.0, 50.0], &[10.0, 50.0], 5.0));
    }

    #[test]
    fn boundaries_align_within_tolerance() {
        assert!(boundaries_align(&[10.0, 50.0], &[12.0, 48.0], 5.0));
    }

    #[test]
    fn boundaries_do_not_align_different_count() {
        assert!(!boundaries_align(&[10.0], &[10.0, 50.0], 5.0));
    }

    #[test]
    fn boundaries_do_not_align_out_of_tolerance() {
        assert!(!boundaries_align(&[10.0, 50.0], &[20.0, 50.0], 5.0));
    }
}
