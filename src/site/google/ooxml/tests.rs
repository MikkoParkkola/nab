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

// ── Helpers ───────────────────────────────────────────────────────────────

pub(super) fn create_minimal_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Cursor;
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
    use std::io::Cursor;
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
