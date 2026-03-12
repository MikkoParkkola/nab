//! OOXML (Office Open XML) parsing for Google Workspace exports.
//!
//! Handles `.docx`, `.xlsx`, and `.pptx` comment/suggestion extraction,
//! as well as XLSX workbook/sheet/shared-string parsing and CSV conversion.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Cursor, Read};

use anyhow::{Context, Result};

// ─── OOXML Namespace URIs ──────────────────────────────────────────────────────

/// `WordprocessingML` namespace (used in `.docx` files).
pub(super) const W_NS: &str =
    "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

// ─── ZIP helpers ──────────────────────────────────────────────────────────────

/// Open a ZIP archive from raw bytes.
pub(super) fn open_zip(bytes: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>> {
    zip::ZipArchive::new(Cursor::new(bytes)).context("Failed to open ZIP/OOXML archive")
}

/// Read a named entry from a ZIP archive as UTF-8.
pub(super) fn read_zip_entry(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<String> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("ZIP entry '{name}' not found"))?;
    let mut buf = String::new();
    entry
        .read_to_string(&mut buf)
        .with_context(|| format!("Failed to read ZIP entry '{name}'"))?;
    Ok(buf)
}

// ─── CSV helpers ──────────────────────────────────────────────────────────────

/// Convert CSV text to a GFM markdown table.
///
/// The first row is treated as headers.
pub(super) fn csv_to_markdown(csv: &str) -> String {
    let rows: Vec<Vec<String>> = csv.lines().map(split_csv_line).collect();
    if rows.is_empty() {
        return String::new();
    }
    let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if col_count == 0 {
        return String::new();
    }

    let mut md = String::new();
    render_table_row(&rows[0], col_count, &mut md);
    md.push('|');
    for _ in 0..col_count {
        md.push_str(" --- |");
    }
    md.push('\n');
    for row in rows.iter().skip(1) {
        render_table_row(row, col_count, &mut md);
    }
    md
}

/// Split a single CSV line respecting double-quoted fields.
pub(super) fn split_csv_line(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    current.push('"');
                } else {
                    in_quotes = false;
                }
            }
            '"' => in_quotes = true,
            ',' if !in_quotes => {
                cells.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    cells.push(current.trim().to_string());
    cells
}

// ─── XLSX types ───────────────────────────────────────────────────────────────

/// A parsed sheet entry from `xl/workbook.xml`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct XlsxSheet {
    /// Sheet name as declared in the workbook.
    pub(crate) name: String,
    /// 1-based sheet index matching `xl/worksheets/sheetN.xml`.
    pub(crate) index: usize,
}

// ─── XLSX workbook parsing ────────────────────────────────────────────────────

/// Parse `xl/workbook.xml` and return sheets in workbook order.
pub(crate) fn parse_xlsx_workbook(xml: &str) -> Vec<XlsxSheet> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };
    doc.descendants()
        .filter(|n| n.has_tag_name("sheet"))
        .enumerate()
        .map(|(i, node)| {
            let name = node
                .attribute("name")
                .filter(|s| !s.is_empty())
                .map_or_else(|| format!("Sheet {}", i + 1), str::to_owned);
            XlsxSheet { name, index: i + 1 }
        })
        .collect()
}

/// Parse shared strings from `xl/sharedStrings.xml`.
pub(crate) fn parse_shared_strings(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };
    doc.descendants()
        .filter(|n| n.has_tag_name("si"))
        .map(|si| {
            si.descendants()
                .filter(|n| n.has_tag_name("t"))
                .filter_map(|n| n.text())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

/// Parse `xl/worksheets/sheetN.xml` into a 2-D grid of cell values.
pub(crate) fn parse_xlsx_sheet_xml(xml: &str, shared_strings: &[String]) -> Vec<Vec<String>> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    let mut row_map: std::collections::BTreeMap<
        usize,
        std::collections::BTreeMap<usize, String>,
    > = std::collections::BTreeMap::new();

    for row in doc.descendants().filter(|n| n.has_tag_name("row")) {
        let row_idx = row
            .attribute("r")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
            .saturating_sub(1);

        for cell in row.children().filter(|n| n.has_tag_name("c")) {
            let cell_ref = cell.attribute("r").unwrap_or("");
            let col_idx = col_letter_to_index(cell_ref);
            let cell_type = cell.attribute("t").unwrap_or("");
            let value = resolve_cell_value(cell, cell_type, shared_strings);
            if !value.is_empty() {
                row_map.entry(row_idx).or_default().insert(col_idx, value);
            }
        }
    }

    if row_map.is_empty() {
        return vec![];
    }

    let max_row = *row_map.keys().max().unwrap_or(&0);
    let max_col = row_map
        .values()
        .flat_map(|cols| cols.keys())
        .max()
        .copied()
        .unwrap_or(0);

    (0..=max_row)
        .map(|r| {
            (0..=max_col)
                .map(|c| {
                    row_map
                        .get(&r)
                        .and_then(|cols| cols.get(&c))
                        .cloned()
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect()
}

/// Resolve the display value for a worksheet cell.
fn resolve_cell_value(
    cell: roxmltree::Node<'_, '_>,
    cell_type: &str,
    shared_strings: &[String],
) -> String {
    let raw = cell
        .children()
        .find(|n| n.has_tag_name("v"))
        .and_then(|v| v.text())
        .unwrap_or("");

    match cell_type {
        "s" => raw
            .parse::<usize>()
            .ok()
            .and_then(|i| shared_strings.get(i))
            .cloned()
            .unwrap_or_else(|| raw.to_owned()),
        "b" => {
            if raw == "1" {
                "TRUE".to_owned()
            } else {
                "FALSE".to_owned()
            }
        }
        "inlineStr" => cell
            .descendants()
            .filter(|n| n.has_tag_name("t"))
            .filter_map(|n| n.text())
            .collect::<Vec<_>>()
            .join(""),
        _ => raw.to_owned(),
    }
}

/// Convert a cell reference like `"C5"` or `"AA12"` to a 0-based column index.
fn col_letter_to_index(cell_ref: &str) -> usize {
    cell_ref
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .fold(0usize, |acc, b| {
            acc * 26 + (b.to_ascii_uppercase() - b'A') as usize + 1
        })
        .saturating_sub(1)
}

/// Convert a 2-D grid to a GFM markdown table. First row is the header.
pub(crate) fn grid_to_markdown(grid: &[Vec<String>]) -> String {
    if grid.is_empty() {
        return String::new();
    }
    let col_count = grid.iter().map(Vec::len).max().unwrap_or(0);
    if col_count == 0 {
        return String::new();
    }

    let mut md = String::new();
    render_table_row(&grid[0], col_count, &mut md);
    md.push('|');
    for _ in 0..col_count {
        md.push_str(" --- |");
    }
    md.push('\n');
    for row in grid.iter().skip(1) {
        render_table_row(row, col_count, &mut md);
    }
    md
}

/// Write a single pipe-table row to `out`.
fn render_table_row(cells: &[String], col_count: usize, out: &mut String) {
    out.push('|');
    for i in 0..col_count {
        let cell = cells.get(i).map_or("", String::as_str);
        out.push(' ');
        out.push_str(&cell.replace('|', "\\|"));
        out.push_str(" |");
    }
    out.push('\n');
}

/// Parse all sheets from an xlsx byte slice into combined markdown.
pub(crate) fn xlsx_to_all_sheets_markdown(bytes: &[u8]) -> Result<String> {
    let mut archive = open_zip(bytes)?;

    let workbook_xml = read_zip_entry(&mut archive, "xl/workbook.xml")?;
    let sheets = parse_xlsx_workbook(&workbook_xml);

    let shared_strings = read_zip_entry(&mut archive, "xl/sharedStrings.xml")
        .ok()
        .as_deref()
        .map(parse_shared_strings)
        .unwrap_or_default();

    let multi = sheets.len() > 1;
    let mut combined = String::new();

    for sheet in &sheets {
        let sheet_path = format!("xl/worksheets/sheet{}.xml", sheet.index);
        let sheet_xml = match read_zip_entry(&mut archive, &sheet_path) {
            Ok(xml) => xml,
            Err(e) => {
                tracing::warn!("Skipping sheet '{}': {e}", sheet.name);
                continue;
            }
        };

        let grid = parse_xlsx_sheet_xml(&sheet_xml, &shared_strings);
        if grid.is_empty() {
            continue;
        }

        if multi {
            let _ = write!(combined, "## Sheet: {}\n\n", sheet.name);
        }
        combined.push_str(&grid_to_markdown(&grid));
        if multi {
            combined.push_str("\n\n");
        }
    }

    Ok(combined)
}

// ─── .xlsx comment parsing ────────────────────────────────────────────────────

/// Append parsed xlsx comments to `markdown` from already-downloaded bytes.
pub(super) fn append_xlsx_comments_from_bytes(xlsx_bytes: &[u8], markdown: &mut String) {
    match parse_xlsx_comments(xlsx_bytes) {
        Ok(comments) if !comments.is_empty() => {
            markdown.push_str("\n\n---\n\n## Comments\n\n");
            for comment in &comments {
                markdown.push_str(comment);
                markdown.push('\n');
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("Failed to parse .xlsx comments: {e}"),
    }
}

/// Extract comments from a `.xlsx` file.
pub(crate) fn parse_xlsx_comments(bytes: &[u8]) -> Result<Vec<String>> {
    let mut archive = open_zip(bytes)?;
    let names: Vec<String> = archive.file_names().map(String::from).collect();
    let mut results = Vec::new();

    let threaded: Vec<String> = names
        .iter()
        .filter(|n| n.starts_with("xl/threadedComments/"))
        .cloned()
        .collect();

    for name in &threaded {
        let xml = read_zip_entry(&mut archive, name)?;
        results.extend(parse_xlsx_threaded_xml(&xml));
    }

    if results.is_empty() {
        let legacy: Vec<String> = names
            .iter()
            .filter(|n| {
                n.starts_with("xl/comments")
                    && std::path::Path::new(n.as_str())
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
            })
            .cloned()
            .collect();

        for name in &legacy {
            let xml = read_zip_entry(&mut archive, name)?;
            results.extend(parse_xlsx_legacy_xml(&xml));
        }
    }

    Ok(results)
}

/// Parse `xl/threadedComments/threadedComment*.xml`.
pub(super) fn parse_xlsx_threaded_xml(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    doc.descendants()
        .filter(|n| n.has_tag_name("threadedComment"))
        .filter_map(|comment| {
            let text = comment
                .descendants()
                .find(|n| n.has_tag_name("text"))
                .and_then(|n| n.text())
                .unwrap_or("");
            if text.is_empty() {
                return None;
            }
            let ref_cell = comment.attribute("ref").unwrap_or("");
            let author_id = comment.attribute("personId").unwrap_or("");
            Some(format!("💬 [{ref_cell}] (author={author_id}): \"{text}\""))
        })
        .collect()
}

/// Parse legacy `xl/comments*.xml` format.
pub(super) fn parse_xlsx_legacy_xml(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    doc.descendants()
        .filter(|n| n.has_tag_name("comment"))
        .filter_map(|comment| {
            let ref_cell = comment.attribute("ref").unwrap_or("");
            let author = comment.attribute("authorId").unwrap_or("0");
            let text: String = comment
                .descendants()
                .filter(|n| n.has_tag_name("t"))
                .filter_map(|n| n.text())
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                return None;
            }
            Some(format!("💬 [{ref_cell}] (author={author}): \"{text}\""))
        })
        .collect()
}

// ─── .pptx comment parsing ───────────────────────────────────────────────────

/// Extract comments from a `.pptx` file.
pub(crate) fn parse_pptx_comments(bytes: &[u8]) -> Result<Vec<String>> {
    let mut archive = open_zip(bytes)?;
    let names: Vec<String> = archive.file_names().map(String::from).collect();

    let comment_files: Vec<String> = names
        .iter()
        .filter(|n| {
            n.starts_with("ppt/comments/")
                && std::path::Path::new(n.as_str())
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
        })
        .cloned()
        .collect();

    let mut results = Vec::new();
    for name in &comment_files {
        let xml = read_zip_entry(&mut archive, name)?;
        results.extend(parse_pptx_comment_xml(&xml));
    }

    Ok(results)
}

/// Parse a `ppt/comments/comment*.xml` file.
pub(super) fn parse_pptx_comment_xml(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    doc.descendants()
        .filter(|n| n.has_tag_name("cm") || n.has_tag_name("comment"))
        .filter_map(|comment| {
            let author = comment.attribute("authorId").unwrap_or("Unknown");
            let created = comment.attribute("created").unwrap_or("");
            let date_short = created.get(..10).unwrap_or(created);
            let text: String = comment
                .descendants()
                .filter(|n| n.has_tag_name("t"))
                .filter_map(|n| n.text())
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string();
            if text.is_empty() {
                return None;
            }
            Some(format!("💬 **{author}** ({date_short}): \"{text}\""))
        })
        .collect()
}

// ─── .docx comment & suggestion parsing ──────────────────────────────────────

/// Extract comments and suggested edits from a `.docx` file.
pub(crate) fn parse_docx_comments(bytes: &[u8]) -> Result<Vec<String>> {
    let mut archive = open_zip(bytes)?;
    let mut results = Vec::new();

    let anchors = if let Ok(xml) = read_zip_entry(&mut archive, "word/document.xml") {
        let suggestions = parse_docx_suggestions(&xml);
        results.extend(suggestions);
        parse_comment_anchors(&xml)
    } else {
        HashMap::new()
    };

    if let Ok(xml) = read_zip_entry(&mut archive, "word/comments.xml") {
        results.extend(parse_docx_comment_xml(&xml, &anchors));
    }

    Ok(results)
}

/// Build a map of `comment_id → anchored_text` from `word/document.xml`.
pub(crate) fn parse_comment_anchors(xml: &str) -> HashMap<String, String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return HashMap::new();
    };

    let nodes: Vec<roxmltree::Node<'_, '_>> = doc.descendants().collect();
    let mut range_starts: HashMap<String, usize> = HashMap::new();
    let mut anchors: HashMap<String, String> = HashMap::new();

    for (idx, node) in nodes.iter().enumerate() {
        if node.has_tag_name("commentRangeStart") {
            if let Some(cid) = comment_id_attr(node) {
                range_starts.insert(cid, idx);
            }
        } else if node.has_tag_name("commentRangeEnd")
            && let Some(cid) = comment_id_attr(node)
            && let Some(&start_idx) = range_starts.get(&cid)
        {
            let snippet = collect_text_in_range(&nodes, start_idx, idx);
            if !snippet.is_empty() {
                anchors.insert(cid, snippet);
            }
        }
    }

    anchors
}

/// Extract the comment `w:id` attribute value from a node.
fn comment_id_attr(node: &roxmltree::Node<'_, '_>) -> Option<String> {
    node.attribute((W_NS, "id"))
        .or_else(|| node.attribute("id"))
        .map(String::from)
}

/// Collect all `w:t` text within `nodes[start_idx..end_idx]`.
fn collect_text_in_range(
    nodes: &[roxmltree::Node<'_, '_>],
    start_idx: usize,
    end_idx: usize,
) -> String {
    nodes[start_idx..end_idx.min(nodes.len())]
        .iter()
        .filter(|n| n.has_tag_name("t"))
        .filter_map(roxmltree::Node::text)
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string()
}

/// Parse `word/comments.xml` and return formatted comment strings.
pub(super) fn parse_docx_comment_xml(
    xml: &str,
    anchors: &HashMap<String, String>,
) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    doc.descendants()
        .filter(|n| n.has_tag_name("comment"))
        .filter_map(|comment| {
            let author = comment
                .attribute((W_NS, "author"))
                .or_else(|| comment.attribute("author"))
                .unwrap_or("Unknown");
            let date = comment
                .attribute((W_NS, "date"))
                .or_else(|| comment.attribute("date"))
                .unwrap_or("");
            let date_short = date.get(..10).unwrap_or(date);
            let text = collect_text_nodes(&comment);
            if text.is_empty() {
                return None;
            }
            let id = comment
                .attribute((W_NS, "id"))
                .or_else(|| comment.attribute("id"))
                .unwrap_or("");
            let anchor_suffix = anchors
                .get(id)
                .map(|a| format!(" → on: \"{a}\""))
                .unwrap_or_default();
            Some(format!(
                "💬 **{author}** ({date_short}): \"{text}\"{anchor_suffix}"
            ))
        })
        .collect()
}

/// Collect all `w:t` text node contents under a node, joined with spaces.
pub(super) fn collect_text_nodes(node: &roxmltree::Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.has_tag_name("t"))
        .filter_map(|n| n.text())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Parse `word/document.xml` for `<w:ins>` and `<w:del>` tracked changes.
pub(super) fn parse_docx_suggestions(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return vec![];
    };

    let mut results = Vec::new();

    for node in doc.descendants() {
        if node.has_tag_name("ins") {
            let author = node
                .attribute((W_NS, "author"))
                .or_else(|| node.attribute("author"))
                .unwrap_or("Unknown");
            let inserted = collect_text_nodes(&node);
            if !inserted.is_empty() {
                results.push(format!(
                    "✏️ suggestion by **{author}**: insert \"{inserted}\""
                ));
            }
        } else if node.has_tag_name("del") {
            let author = node
                .attribute((W_NS, "author"))
                .or_else(|| node.attribute("author"))
                .unwrap_or("Unknown");
            let deleted = collect_del_text(&node);
            if !deleted.is_empty() {
                results.push(format!(
                    "✏️ suggestion by **{author}**: delete \"{deleted}\""
                ));
            }
        }
    }

    results
}

/// Collect `w:delText` nodes (deleted text in tracked changes).
fn collect_del_text(node: &roxmltree::Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.has_tag_name("delText"))
        .filter_map(|n| n.text())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    // ── CSV → Markdown ────────────────────────────────────────────────────────

    #[test]
    fn csv_to_markdown_produces_gfm_pipe_table() {
        let csv = "Name,Age,City\nAlice,30,Helsinki\nBob,25,Tampere";
        let md = csv_to_markdown(csv);
        assert!(md.contains("| Name | Age | City |"), "header row missing");
        assert!(md.contains("| --- | --- | --- |"), "separator missing");
        assert!(md.contains("| Alice | 30 | Helsinki |"), "data row missing");
        assert!(md.contains("| Bob | 25 | Tampere |"), "data row missing");
    }

    #[test]
    fn csv_to_markdown_handles_quoted_fields_with_commas() {
        let csv = "Name,Notes\nAlice,\"Hello, world\"";
        let md = csv_to_markdown(csv);
        assert!(
            md.contains("Hello, world"),
            "quoted comma field should be preserved"
        );
    }

    #[test]
    fn csv_to_markdown_escapes_pipe_characters() {
        let csv = "Col1,Col2\nA|B,C";
        let md = csv_to_markdown(csv);
        assert!(md.contains("A\\|B"), "pipe in cell must be escaped");
    }

    #[test]
    fn csv_to_markdown_empty_input_returns_empty() {
        assert_eq!(csv_to_markdown(""), "");
    }

    // ── XLSX workbook parsing ──────────────────────────────────────────────────

    #[test]
    fn parse_xlsx_workbook_extracts_sheet_names_in_order() {
        let xml = r#"<?xml version="1.0"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheets>
    <sheet name="Budget" sheetId="1" r:id="rId1"
           xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"/>
    <sheet name="Summary" sheetId="2" r:id="rId2"
           xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"/>
  </sheets>
</workbook>"#;
        let sheets = parse_xlsx_workbook(xml);
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].name, "Budget");
        assert_eq!(sheets[0].index, 1);
        assert_eq!(sheets[1].name, "Summary");
        assert_eq!(sheets[1].index, 2);
    }

    #[test]
    fn parse_xlsx_workbook_generates_fallback_name_for_empty_name_attr() {
        let xml = r#"<?xml version="1.0"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheets>
    <sheet name="" sheetId="1" r:id="rId1"
           xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"/>
  </sheets>
</workbook>"#;
        let sheets = parse_xlsx_workbook(xml);
        assert_eq!(sheets.len(), 1);
        assert!(
            sheets[0].name.starts_with("Sheet"),
            "expected fallback name, got '{}'",
            sheets[0].name
        );
    }

    #[test]
    fn parse_xlsx_workbook_returns_empty_for_invalid_xml() {
        assert!(parse_xlsx_workbook("<this is not valid xml<<<").is_empty());
    }

    // ── XLSX shared strings ────────────────────────────────────────────────────

    #[test]
    fn parse_shared_strings_extracts_ordered_strings() {
        let xml = r#"<?xml version="1.0"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="2">
  <si><t>Hello</t></si>
  <si><t>World</t></si>
</sst>"#;
        let strings = parse_shared_strings(xml);
        assert_eq!(strings, vec!["Hello", "World"]);
    }

    #[test]
    fn parse_shared_strings_concatenates_rich_text_runs() {
        let xml = r#"<?xml version="1.0"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <si>
    <r><rPr/><t>foo</t></r>
    <r><rPr/><t>bar</t></r>
  </si>
</sst>"#;
        let strings = parse_shared_strings(xml);
        assert_eq!(strings, vec!["foobar"]);
    }

    #[test]
    fn parse_shared_strings_returns_empty_for_invalid_xml() {
        assert!(parse_shared_strings("not xml").is_empty());
    }

    // ── XLSX sheet cell parsing ────────────────────────────────────────────────

    #[test]
    fn parse_xlsx_sheet_xml_reads_inline_numbers() {
        let xml = r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1"><v>10</v></c>
      <c r="B1"><v>20</v></c>
    </row>
    <row r="2">
      <c r="A2"><v>30</v></c>
      <c r="B2"><v>40</v></c>
    </row>
  </sheetData>
</worksheet>"#;
        let grid = parse_xlsx_sheet_xml(xml, &[]);
        assert_eq!(grid.len(), 2);
        assert_eq!(grid[0], vec!["10", "20"]);
        assert_eq!(grid[1], vec!["30", "40"]);
    }

    #[test]
    fn parse_xlsx_sheet_xml_resolves_shared_strings() {
        let xml = r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>0</v></c>
      <c r="B1" t="s"><v>1</v></c>
    </row>
  </sheetData>
</worksheet>"#;
        let shared = vec!["Name".to_string(), "Value".to_string()];
        let grid = parse_xlsx_sheet_xml(xml, &shared);
        assert_eq!(grid.len(), 1);
        assert_eq!(grid[0], vec!["Name", "Value"]);
    }

    #[test]
    fn parse_xlsx_sheet_xml_handles_boolean_cells() {
        let xml = r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="b"><v>1</v></c>
      <c r="B1" t="b"><v>0</v></c>
    </row>
  </sheetData>
</worksheet>"#;
        let grid = parse_xlsx_sheet_xml(xml, &[]);
        assert_eq!(grid[0], vec!["TRUE", "FALSE"]);
    }

    #[test]
    fn parse_xlsx_sheet_xml_returns_empty_for_invalid_xml() {
        assert!(parse_xlsx_sheet_xml("not xml", &[]).is_empty());
    }

    #[test]
    fn parse_xlsx_sheet_xml_skips_cells_with_empty_values() {
        let xml = r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1"><v>hello</v></c>
      <c r="C1"><v>world</v></c>
    </row>
  </sheetData>
</worksheet>"#;
        let grid = parse_xlsx_sheet_xml(xml, &[]);
        assert_eq!(grid.len(), 1);
        assert_eq!(grid[0].len(), 3);
        assert_eq!(grid[0][0], "hello");
        assert_eq!(grid[0][1], "");
        assert_eq!(grid[0][2], "world");
    }

    // ── col_letter_to_index ───────────────────────────────────────────────────

    #[test]
    fn col_letter_to_index_converts_single_letters() {
        assert_eq!(col_letter_to_index("A1"), 0);
        assert_eq!(col_letter_to_index("B5"), 1);
        assert_eq!(col_letter_to_index("Z10"), 25);
    }

    #[test]
    fn col_letter_to_index_converts_double_letters() {
        assert_eq!(col_letter_to_index("AA1"), 26);
        assert_eq!(col_letter_to_index("AB1"), 27);
        assert_eq!(col_letter_to_index("AZ1"), 51);
        assert_eq!(col_letter_to_index("BA1"), 52);
    }

    #[test]
    fn col_letter_to_index_returns_zero_for_empty_ref() {
        assert_eq!(col_letter_to_index(""), 0);
        assert_eq!(col_letter_to_index("123"), 0);
    }

    // ── grid_to_markdown ──────────────────────────────────────────────────────

    #[test]
    fn grid_to_markdown_produces_gfm_pipe_table() {
        let grid = vec![
            vec!["Name".to_string(), "Age".to_string()],
            vec!["Alice".to_string(), "30".to_string()],
        ];
        let md = grid_to_markdown(&grid);
        assert!(md.contains("| Name | Age |"), "header row missing: {md}");
        assert!(md.contains("| --- | --- |"), "separator missing: {md}");
        assert!(md.contains("| Alice | 30 |"), "data row missing: {md}");
    }

    #[test]
    fn grid_to_markdown_escapes_pipe_characters_in_cells() {
        let grid = vec![vec!["Col".to_string()], vec!["A|B".to_string()]];
        let md = grid_to_markdown(&grid);
        assert!(md.contains("A\\|B"), "pipe must be escaped: {md}");
    }

    #[test]
    fn grid_to_markdown_returns_empty_for_empty_grid() {
        assert_eq!(grid_to_markdown(&[]), "");
    }

    // ── xlsx_to_all_sheets_markdown ───────────────────────────────────────────

    #[test]
    fn xlsx_to_all_sheets_markdown_single_sheet_no_header() {
        let xlsx_bytes = create_minimal_xlsx(&[(
            "Budget",
            r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1"><c r="A1"><v>Revenue</v></c><c r="B1"><v>100</v></c></row>
    <row r="2"><c r="A2"><v>Cost</v></c><c r="B2"><v>60</v></c></row>
  </sheetData>
</worksheet>"#,
        )]);
        let md = xlsx_to_all_sheets_markdown(&xlsx_bytes).unwrap();
        assert!(!md.contains("## Sheet:"), "single sheet must not have section header");
        assert!(md.contains("Revenue"), "cell data must appear: {md}");
    }

    #[test]
    fn xlsx_to_all_sheets_markdown_multi_sheet_adds_section_headers() {
        let xlsx_bytes = create_minimal_xlsx(&[
            (
                "Alpha",
                r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>
</worksheet>"#,
            ),
            (
                "Beta",
                r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData><row r="1"><c r="A1"><v>2</v></c></row></sheetData>
</worksheet>"#,
            ),
        ]);
        let md = xlsx_to_all_sheets_markdown(&xlsx_bytes).unwrap();
        assert!(md.contains("## Sheet: Alpha"), "first sheet header missing: {md}");
        assert!(md.contains("## Sheet: Beta"), "second sheet header missing: {md}");
    }

    #[test]
    fn xlsx_to_all_sheets_markdown_returns_error_for_non_zip() {
        assert!(xlsx_to_all_sheets_markdown(b"not a zip file").is_err());
    }

    // ── Comment anchors ───────────────────────────────────────────────────────

    #[test]
    fn parse_comment_anchors_extracts_text_between_range_markers() {
        let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:commentRangeStart w:id="1"/>
      <w:r><w:t>important text</w:t></w:r>
      <w:commentRangeEnd w:id="1"/>
      <w:r><w:t>other text</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;
        let anchors = parse_comment_anchors(xml);
        assert_eq!(anchors.get("1").map(String::as_str), Some("important text"));
    }

    #[test]
    fn parse_comment_anchors_handles_multiple_comments() {
        let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:commentRangeStart w:id="1"/>
      <w:r><w:t>first anchor</w:t></w:r>
      <w:commentRangeEnd w:id="1"/>
      <w:commentRangeStart w:id="2"/>
      <w:r><w:t>second anchor</w:t></w:r>
      <w:commentRangeEnd w:id="2"/>
    </w:p>
  </w:body>
</w:document>"#;
        let anchors = parse_comment_anchors(xml);
        assert_eq!(anchors.get("1").map(String::as_str), Some("first anchor"));
        assert_eq!(anchors.get("2").map(String::as_str), Some("second anchor"));
    }

    #[test]
    fn parse_comment_anchors_returns_empty_for_no_markers() {
        let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p><w:r><w:t>no comments here</w:t></w:r></w:p></w:body>
</w:document>"#;
        assert!(parse_comment_anchors(xml).is_empty());
    }

    #[test]
    fn parse_docx_comment_xml_includes_anchor_when_available() {
        let xml = r#"<?xml version="1.0"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:comment w:author="Alice" w:date="2025-03-01T10:00:00Z" w:id="1">
    <w:p><w:r><w:t>Please clarify this</w:t></w:r></w:p>
  </w:comment>
</w:comments>"#;
        let mut anchors = HashMap::new();
        anchors.insert("1".to_string(), "important phrase".to_string());
        let comments = parse_docx_comment_xml(xml, &anchors);
        assert_eq!(comments.len(), 1);
        assert!(comments[0].contains("Alice"));
        assert!(comments[0].contains("Please clarify this"));
        assert!(comments[0].contains("→ on: \"important phrase\""));
    }

    #[test]
    fn parse_docx_comment_xml_omits_anchor_suffix_when_no_anchor() {
        let xml = r#"<?xml version="1.0"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:comment w:author="Bob" w:date="2025-04-01T00:00:00Z" w:id="99">
    <w:p><w:r><w:t>Good point</w:t></w:r></w:p>
  </w:comment>
</w:comments>"#;
        let comments = parse_docx_comment_xml(xml, &HashMap::new());
        assert_eq!(comments.len(), 1);
        assert!(!comments[0].contains("→ on:"));
    }

    #[test]
    fn parse_docx_comments_returns_empty_for_no_comments_entry() {
        let bytes = create_minimal_zip(&[("word/document.xml", "<document/>")]);
        let result = parse_docx_comments(&bytes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_docx_comment_xml_extracts_author_and_text() {
        let xml = r#"<?xml version="1.0"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:comment w:author="Alice" w:date="2025-01-15T10:00:00Z" w:id="1">
    <w:p><w:r><w:t>This needs revision</w:t></w:r></w:p>
  </w:comment>
</w:comments>"#;
        let comments = parse_docx_comment_xml(xml, &HashMap::new());
        assert_eq!(comments.len(), 1);
        assert!(comments[0].contains("Alice"));
        assert!(comments[0].contains("2025-01-15"));
        assert!(comments[0].contains("This needs revision"));
    }

    #[test]
    fn parse_docx_suggestions_extracts_insertions() {
        let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:ins w:author="Bob" w:date="2025-01-16T09:00:00Z" w:id="2">
      <w:r><w:t>new text</w:t></w:r>
    </w:ins>
  </w:body>
</w:document>"#;
        let suggestions = parse_docx_suggestions(xml);
        assert_eq!(suggestions.len(), 1);
        assert!(suggestions[0].contains("Bob"));
        assert!(suggestions[0].contains("insert"));
        assert!(suggestions[0].contains("new text"));
    }

    #[test]
    fn parse_docx_suggestions_extracts_deletions() {
        let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:del w:author="Carol" w:date="2025-01-17T08:00:00Z" w:id="3">
      <w:r><w:delText>old text</w:delText></w:r>
    </w:del>
  </w:body>
</w:document>"#;
        let suggestions = parse_docx_suggestions(xml);
        assert_eq!(suggestions.len(), 1);
        assert!(suggestions[0].contains("Carol"));
        assert!(suggestions[0].contains("delete"));
        assert!(suggestions[0].contains("old text"));
    }

    #[test]
    fn parse_xlsx_comments_returns_empty_for_minimal_xlsx() {
        let bytes = create_minimal_zip(&[("[Content_Types].xml", "<Types/>")]);
        let result = parse_xlsx_comments(&bytes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_xlsx_legacy_xml_extracts_comments() {
        let xml = r#"<?xml version="1.0"?>
<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <authors><author>Alice</author></authors>
  <commentList>
    <comment ref="B5" authorId="0">
      <text><r><t>Check this value</t></r></text>
    </comment>
  </commentList>
</comments>"#;
        let comments = parse_xlsx_legacy_xml(xml);
        assert_eq!(comments.len(), 1);
        assert!(comments[0].contains("B5"));
        assert!(comments[0].contains("Check this value"));
    }

    #[test]
    fn parse_pptx_comments_returns_empty_for_minimal_pptx() {
        let bytes = create_minimal_zip(&[("[Content_Types].xml", "<Types/>")]);
        let result = parse_pptx_comments(&bytes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_pptx_comment_xml_extracts_comments() {
        let xml = r#"<?xml version="1.0"?>
<cmLst xmlns="http://schemas.openxmlformats.org/presentationml/2006/main">
  <cm authorId="0" created="2025-01-20T12:00:00Z">
    <pos x="1524000" y="1524000"/>
    <text><r><t>Nice slide!</t></r></text>
  </cm>
</cmLst>"#;
        let comments = parse_pptx_comment_xml(xml);
        assert_eq!(comments.len(), 1);
        assert!(comments[0].contains("Nice slide!"));
    }

    // ── split_csv_line ────────────────────────────────────────────────────────

    #[test]
    fn split_csv_line_handles_simple_fields() {
        assert_eq!(split_csv_line("a,b,c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn split_csv_line_handles_quoted_fields() {
        let cells = split_csv_line("\"hello, world\",b");
        assert_eq!(cells, vec!["hello, world", "b"]);
    }

    #[test]
    fn split_csv_line_handles_escaped_quotes() {
        let cells = split_csv_line("\"say \"\"hi\"\"\",b");
        assert_eq!(cells, vec!["say \"hi\"", "b"]);
    }

    // ── Helper ────────────────────────────────────────────────────────────────

    pub(super) fn create_minimal_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let buf = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, content) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    pub(super) fn create_minimal_xlsx(sheets: &[(&str, &str)]) -> Vec<u8> {
        let mut workbook = String::from(
            r#"<?xml version="1.0"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>"#,
        );
        for (i, (name, _)) in sheets.iter().enumerate() {
            let _ = write!(
                workbook,
                "\n    <sheet name=\"{name}\" sheetId=\"{id}\" r:id=\"rId{id}\"/>",
                id = i + 1
            );
        }
        workbook.push_str("\n  </sheets>\n</workbook>");

        let buf = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file("xl/workbook.xml", options).unwrap();
        zip.write_all(workbook.as_bytes()).unwrap();

        for (i, (_, sheet_xml)) in sheets.iter().enumerate() {
            let path = format!("xl/worksheets/sheet{}.xml", i + 1);
            zip.start_file(path, options).unwrap();
            zip.write_all(sheet_xml.as_bytes()).unwrap();
        }

        zip.finish().unwrap().into_inner()
    }
}
