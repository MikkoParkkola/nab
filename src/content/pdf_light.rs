//! Lightweight PDF text extraction — no external library required.
//!
//! This handler is active when the `pdf` feature flag is **not** enabled.
//! It uses a pure-Rust heuristic approach to extract readable text from PDFs:
//!
//! 1. Verify the `%PDF-` magic header
//! 2. Scan the raw byte stream for text between `BT`/`ET` operators
//! 3. Extract strings from `Tj`, `TJ`, `'`, and `"` PDF text operators
//! 4. Decode basic hex strings and escaped parentheses
//! 5. If no text operators are found, report as a scanned (image-only) PDF
//!
//! # Limitations
//!
//! - Does not handle compressed object streams (`/FlateDecode`, `/LZWDecode`, etc.)
//! - Character encoding: PDFs may use custom font encoding (`WinAnsi`, `MacRoman`, custom `ToUnicode` `CMaps`).
//!   This extractor outputs raw bytes within the printable ASCII range and replaces
//!   non-ASCII bytes with `?`.
//! - For full Unicode extraction with proper encoding, build with `--features pdf`
//!   to use the pdfium-backed handler.
//!
//! # Output format
//!
//! ```text
//! [PDF: 12 pages, text extracted]
//! # Extracted text follows...
//! ```
//! or
//! ```text
//! [PDF: 3 pages, scanned — no text layer. Use OCR or rebuild with --features pdf]
//! ```

use anyhow::Result;

use super::{ContentHandler, ConversionResult};

/// Maximum bytes to scan for text extraction (2 MB).
///
/// Protects against extremely large PDFs consuming excessive memory.
const MAX_SCAN_BYTES: usize = 2 * 1024 * 1024;

/// Maximum output characters to prevent flooding LLM context windows.
const MAX_OUTPUT_CHARS: usize = 50_000;

/// Lightweight PDF handler (no pdfium dependency).
pub struct PdfLightHandler;

impl ContentHandler for PdfLightHandler {
    fn supported_types(&self) -> &[&str] {
        &["application/pdf"]
    }

    fn to_markdown(&self, bytes: &[u8], content_type: &str) -> Result<ConversionResult> {
        let start = std::time::Instant::now();

        if !is_pdf(bytes) {
            anyhow::bail!("Not a PDF: missing %PDF- header");
        }

        let page_count = count_pages(bytes);
        let text = extract_pdf_text(&bytes[..bytes.len().min(MAX_SCAN_BYTES)]);

        let markdown = build_markdown(text, page_count);

        Ok(ConversionResult {
            markdown,
            page_count: Some(page_count),
            content_type: content_type.to_string(),
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
            quality: None,
        })
    }
}

/// Returns `true` if bytes start with the `%PDF-` magic header.
fn is_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

/// Count pages by scanning for `/Type /Page` entries in the PDF structure.
///
/// This is an approximation — compressed cross-reference streams (PDF 1.5+)
/// may cause undercounting. In practice it is accurate for most PDFs.
fn count_pages(bytes: &[u8]) -> usize {
    let count = count_occurrences(bytes, b"/Type /Page");
    // Also handle alternative spacing
    let count2 = count_occurrences(bytes, b"/Type/Page");
    count.max(count2).max(1)
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if haystack[i..i + needle.len()] == *needle {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

/// Extract text from PDF byte stream using operator-based scanning.
///
/// Looks for `BT`/`ET` (Begin Text / End Text) blocks and extracts
/// strings from `Tj`, `TJ`, `'`, `"` operators within them.
fn extract_pdf_text(bytes: &[u8]) -> Option<String> {
    let mut output = String::with_capacity(4096);
    let mut in_bt_block = false;
    let mut pending_strings: Vec<String> = Vec::new();
    let mut i = 0;

    while i < bytes.len() && output.len() < MAX_OUTPUT_CHARS {
        // Scan for BT marker (Begin Text)
        if !in_bt_block {
            if bytes[i..].starts_with(b"BT") && is_pdf_token_boundary(bytes, i, 2) {
                in_bt_block = true;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        // Within BT block: scan for ET or string/operator tokens
        if bytes[i..].starts_with(b"ET") && is_pdf_token_boundary(bytes, i, 2) {
            // Flush pending strings
            flush_strings(&mut pending_strings, &mut output);
            in_bt_block = false;
            i += 2;
            continue;
        }

        // Parse a PDF literal string: (...)
        if bytes[i] == b'('
            && let Some((s, consumed)) = parse_literal_string(&bytes[i..])
        {
            pending_strings.push(s);
            i += consumed;
            continue;
        }

        // Parse a PDF hex string: <...>
        if bytes[i] == b'<'
            && bytes.get(i + 1).is_some_and(|&b| b != b'<')
            && let Some((s, consumed)) = parse_hex_string(&bytes[i..])
        {
            pending_strings.push(s);
            i += consumed;
            continue;
        }

        // Array operator TJ: [ (str1) spacing (str2) ... ] TJ
        if bytes[i] == b'['
            && let Some((strings, consumed)) = parse_array_strings(&bytes[i..])
        {
            pending_strings.extend(strings);
            i += consumed;
            continue;
        }

        // Operator after string(s): Tj, TJ, ', "
        if matches!(bytes[i], b'T' | b'\'' | b'"') {
            let op_end = scan_operator_end(bytes, i);
            let op = &bytes[i..op_end];
            match op {
                b"Tj" | b"TJ" | b"'" | b"\"" => {
                    flush_strings(&mut pending_strings, &mut output);
                }
                _ => {}
            }
            i = op_end;
            continue;
        }

        // Td / TD / T* — new line operators: add a newline
        if bytes[i] == b'T' && i + 1 < bytes.len() && matches!(bytes[i + 1], b'd' | b'D' | b'*') {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            i += 2;
            continue;
        }

        i += 1;
    }

    if output.trim().is_empty() {
        None
    } else {
        Some(output)
    }
}

/// Flush pending string segments as a single line.
fn flush_strings(pending: &mut Vec<String>, output: &mut String) {
    if pending.is_empty() {
        return;
    }
    let line: String = pending.drain(..).collect();
    let trimmed = line.trim();
    if !trimmed.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push(' ');
        }
        output.push_str(trimmed);
    }
}

/// Returns `true` if position `i + len` is a PDF token boundary
/// (whitespace, end of stream, or a PDF delimiter character).
fn is_pdf_token_boundary(bytes: &[u8], i: usize, len: usize) -> bool {
    // Also require that the character BEFORE i is a boundary (not inside a longer token)
    let before_ok = i == 0 || is_pdf_delimiter_or_ws(bytes[i - 1]);
    let after_ok = i + len >= bytes.len() || is_pdf_delimiter_or_ws(bytes[i + len]);
    before_ok && after_ok
}

fn is_pdf_delimiter_or_ws(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'/' | b'<' | b'>'
    )
}

/// Parse a PDF literal string `(text)`, handling escaped characters and nested parens.
///
/// Returns `(decoded_string, bytes_consumed)` or `None` if malformed.
fn parse_literal_string(bytes: &[u8]) -> Option<(String, usize)> {
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut result = String::new();
    let mut i = 1;
    let mut depth = 1usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                let escaped = match bytes[i + 1] {
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    c => char::from(c),
                };
                result.push(escaped);
                i += 2;
            }
            b'(' => {
                depth += 1;
                result.push('(');
                i += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((sanitize_pdf_string(&result), i + 1));
                }
                result.push(')');
                i += 1;
            }
            b => {
                // Keep printable ASCII, replace non-ASCII with '?'
                if b.is_ascii_graphic() || b == b' ' {
                    result.push(char::from(b));
                }
                i += 1;
            }
        }
    }
    None // unclosed string
}

/// Parse a PDF hex string `<4865 6C6C 6F>`, returning `(decoded_utf8, bytes_consumed)`.
fn parse_hex_string(bytes: &[u8]) -> Option<(String, usize)> {
    if bytes.first() != Some(&b'<') {
        return None;
    }
    let end = bytes[1..].iter().position(|&b| b == b'>')?;
    let hex_slice = &bytes[1..=end];
    let decoded = decode_hex_string(hex_slice);
    Some((decoded, end + 2))
}

/// Decode a PDF hex string (ASCII hex pairs, spaces ignored).
fn decode_hex_string(hex: &[u8]) -> String {
    let digits: Vec<u8> = hex
        .iter()
        .filter(|&&b| !b.is_ascii_whitespace())
        .copied()
        .collect();
    let mut result = String::new();
    let mut j = 0;
    while j < digits.len() {
        let hi = hex_digit(digits[j]);
        let lo = if j + 1 < digits.len() {
            hex_digit(digits[j + 1])
        } else {
            Some(0)
        };
        match (hi, lo) {
            (Some(h), Some(l)) => {
                let byte: u8 = (h << 4) | l;
                if byte.is_ascii_graphic() || byte == b' ' {
                    result.push(char::from(byte));
                }
                j += 2;
            }
            _ => {
                j += 1;
            }
        }
    }
    result
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse a PDF array `[ (str1) adj (str2) ... ]` used by the TJ operator.
///
/// Numbers (spacing adjustments) are ignored; only string elements are collected.
fn parse_array_strings(bytes: &[u8]) -> Option<(Vec<String>, usize)> {
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let mut strings = Vec::new();
    let mut i = 1;

    while i < bytes.len() {
        match bytes[i] {
            b']' => return Some((strings, i + 1)),
            b'(' => {
                if let Some((s, consumed)) = parse_literal_string(&bytes[i..]) {
                    strings.push(s);
                    i += consumed;
                } else {
                    i += 1;
                }
            }
            b'<' if bytes.get(i + 1).is_some_and(|&b| b != b'<') => {
                if let Some((s, consumed)) = parse_hex_string(&bytes[i..]) {
                    strings.push(s);
                    i += consumed;
                } else {
                    i += 1;
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    None // unclosed array
}

/// Find the end of a PDF operator token starting at `i`.
fn scan_operator_end(bytes: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < bytes.len() && !is_pdf_delimiter_or_ws(bytes[j]) {
        j += 1;
    }
    j
}

/// Replace control characters and excessive whitespace in extracted PDF strings.
fn sanitize_pdf_string(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect()
}

/// Build the final markdown output from extracted text and page count.
fn build_markdown(text: Option<String>, page_count: usize) -> String {
    match text {
        Some(extracted) if !extracted.trim().is_empty() => {
            let pages_label = if page_count == 1 {
                "1 page".to_string()
            } else {
                format!("{page_count} pages")
            };
            format!(
                "[PDF: {pages_label}, text extracted — for full fidelity rebuild with `--features pdf`]\n\n{}",
                extracted.trim()
            )
        }
        _ => {
            let pages_label = if page_count == 1 {
                "1 page".to_string()
            } else {
                format!("{page_count} pages")
            };
            format!(
                "[PDF: {pages_label}, scanned — no text layer detected. \
                 Use OCR or rebuild with `--features pdf` for pdfium extraction.]"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── PDF detection ────────────────────────────────────────────────────

    #[test]
    fn is_pdf_returns_true_for_valid_header() {
        assert!(is_pdf(b"%PDF-1.4\n"));
    }

    #[test]
    fn is_pdf_returns_false_for_non_pdf_bytes() {
        assert!(!is_pdf(b"<!DOCTYPE html>"));
    }

    #[test]
    fn is_pdf_returns_false_for_empty_bytes() {
        assert!(!is_pdf(b""));
    }

    // ─── Page counting ────────────────────────────────────────────────────

    #[test]
    fn count_pages_returns_1_for_single_page_pdf_fragment() {
        // GIVEN: PDF fragment with one /Type /Page entry
        let bytes = b"%PDF-1.4\n/Type /Page\n";
        // WHEN: counting pages
        let count = count_pages(bytes);
        // THEN: one page detected
        assert_eq!(count, 1);
    }

    #[test]
    fn count_pages_returns_3_for_three_page_entries() {
        // GIVEN: PDF fragment with three /Type /Page entries
        let bytes = b"%PDF-1.4\n/Type /Page\n/Type /Page\n/Type /Page\n";
        // WHEN: counting pages
        let count = count_pages(bytes);
        // THEN: three pages detected
        assert_eq!(count, 3);
    }

    #[test]
    fn count_pages_handles_compact_nospace_variant() {
        // GIVEN: PDF fragment with compact /Type/Page
        let bytes = b"%PDF-1.4\n/Type/Page\n/Type/Page\n";
        // WHEN: counting pages
        let count = count_pages(bytes);
        // THEN: two pages detected
        assert_eq!(count, 2);
    }

    // ─── Literal string parsing ───────────────────────────────────────────

    #[test]
    fn parse_literal_string_decodes_simple_string() {
        // GIVEN: simple literal string
        let (s, consumed) = parse_literal_string(b"(Hello, World!)").unwrap();
        // THEN: content decoded correctly
        assert_eq!(s, "Hello, World!");
        assert_eq!(consumed, 15);
    }

    #[test]
    fn parse_literal_string_handles_escaped_parens() {
        // GIVEN: literal string with escaped parentheses
        let (s, consumed) = parse_literal_string(b"(foo\\(bar\\)baz)").unwrap();
        // THEN: escaped parens included
        assert_eq!(s, "foo(bar)baz");
        assert_eq!(consumed, 15);
    }

    #[test]
    fn parse_literal_string_handles_nested_parens() {
        // GIVEN: literal string with unescaped nested parens
        let (s, _) = parse_literal_string(b"(outer (inner) end)").unwrap();
        // THEN: nested parens preserved
        assert_eq!(s, "outer (inner) end");
    }

    #[test]
    fn parse_literal_string_returns_none_for_unclosed() {
        // GIVEN: unclosed literal string
        let result = parse_literal_string(b"(unclosed");
        // THEN: returns None
        assert!(result.is_none());
    }

    // ─── Hex string parsing ───────────────────────────────────────────────

    #[test]
    fn parse_hex_string_decodes_ascii_hex_pairs() {
        // GIVEN: hex-encoded "Hi"
        let (s, consumed) = parse_hex_string(b"<4869>").unwrap();
        // THEN: decoded to "Hi"
        assert_eq!(s, "Hi");
        assert_eq!(consumed, 6);
    }

    #[test]
    fn parse_hex_string_ignores_spaces_in_hex_content() {
        // GIVEN: hex string with spaces
        let (s, _) = parse_hex_string(b"<48 65 6C 6C 6F>").unwrap();
        // THEN: "Hello" decoded
        assert_eq!(s, "Hello");
    }

    #[test]
    fn parse_hex_string_returns_none_for_unclosed() {
        // GIVEN: unclosed hex string
        let result = parse_hex_string(b"<4869");
        // THEN: None
        assert!(result.is_none());
    }

    // ─── Full text extraction ─────────────────────────────────────────────

    #[test]
    fn extract_pdf_text_finds_text_in_bt_et_block() {
        // GIVEN: minimal PDF BT/ET block with Tj operator
        let pdf = b"%PDF-1.4\nBT\n(Hello PDF) Tj\nET\n";
        // WHEN: extracting text
        let text = extract_pdf_text(pdf);
        // THEN: text content extracted
        assert!(text.is_some());
        let t = text.unwrap();
        assert!(t.contains("Hello PDF"), "got: {t}");
    }

    #[test]
    fn extract_pdf_text_returns_none_for_no_bt_blocks() {
        // GIVEN: PDF with no BT/ET blocks (image-only PDF)
        let pdf = b"%PDF-1.4\nxref\n0 1\n0000000000 65535 f \n";
        // WHEN: extracting text
        let text = extract_pdf_text(pdf);
        // THEN: None (no text layer)
        assert!(text.is_none());
    }

    #[test]
    fn extract_pdf_text_handles_multiple_bt_et_blocks() {
        // GIVEN: PDF with two separate BT/ET blocks
        let pdf = b"%PDF-1.4\nBT\n(First line) Tj\nET\nBT\n(Second line) Tj\nET\n";
        // WHEN: extracting text
        let text = extract_pdf_text(pdf);
        // THEN: both blocks extracted
        let t = text.expect("expected text");
        assert!(t.contains("First line"), "got: {t}");
        assert!(t.contains("Second line"), "got: {t}");
    }

    // ─── Markdown output ──────────────────────────────────────────────────

    #[test]
    fn build_markdown_with_text_includes_extraction_note() {
        // GIVEN: extracted text and page count
        let md = build_markdown(Some("Sample content".to_string()), 2);
        // THEN: contains PDF header and the text
        assert!(md.contains("[PDF: 2 pages"), "got: {md}");
        assert!(md.contains("Sample content"), "got: {md}");
    }

    #[test]
    fn build_markdown_with_no_text_reports_scanned_pdf() {
        // GIVEN: no extracted text
        let md = build_markdown(None, 5);
        // THEN: reports as scanned PDF
        assert!(md.contains("[PDF:"), "got: {md}");
        assert!(md.contains("scanned"), "got: {md}");
        assert!(md.contains("5 pages"), "got: {md}");
    }

    #[test]
    fn build_markdown_single_page_uses_singular_form() {
        // GIVEN: single-page PDF with no text
        let md = build_markdown(None, 1);
        // THEN: "1 page" (not "1 pages")
        assert!(md.contains("1 page"), "got: {md}");
        assert!(!md.contains("1 pages"), "got: {md}");
    }

    // ─── ContentHandler trait ─────────────────────────────────────────────

    #[test]
    fn pdf_light_handler_returns_error_for_non_pdf_bytes() {
        // GIVEN: HTML bytes passed as PDF
        let handler = PdfLightHandler;
        let result = handler.to_markdown(b"<html>not a pdf</html>", "application/pdf");
        // THEN: error returned
        assert!(result.is_err());
    }

    #[test]
    fn pdf_light_handler_extracts_text_from_simple_pdf() {
        // GIVEN: minimal well-formed PDF fragment with BT/ET block
        let pdf = b"%PDF-1.4\n/Type /Page\nBT\n(Test document) Tj\nET\n%%EOF";
        let handler = PdfLightHandler;
        // WHEN: converting to markdown
        let result = handler.to_markdown(pdf, "application/pdf").unwrap();
        // THEN: contains extracted text
        assert!(
            result.markdown.contains("Test document"),
            "got: {}",
            result.markdown
        );
        assert_eq!(result.page_count, Some(1));
    }

    #[test]
    fn pdf_light_handler_reports_scanned_for_no_text_layer() {
        // GIVEN: PDF with no BT/ET blocks (binary/scanned)
        let pdf = b"%PDF-1.4\n/Type /Page\n/Type /Page\nxref\n%%EOF";
        let handler = PdfLightHandler;
        // WHEN: converting to markdown
        let result = handler.to_markdown(pdf, "application/pdf").unwrap();
        // THEN: reports scanned PDF
        assert!(
            result.markdown.contains("scanned"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn pdf_light_supported_types_is_application_pdf() {
        let handler = PdfLightHandler;
        assert_eq!(handler.supported_types(), &["application/pdf"]);
    }

    // ─── count_occurrences helper ────────────────────────────────────────

    #[test]
    fn count_occurrences_finds_no_match_in_empty_haystack() {
        assert_eq!(count_occurrences(b"", b"needle"), 0);
    }

    #[test]
    fn count_occurrences_finds_multiple_non_overlapping() {
        let hay = b"abcabcabc";
        assert_eq!(count_occurrences(hay, b"abc"), 3);
    }
}
