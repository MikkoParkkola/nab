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

use std::io::Read as _;

use anyhow::Result;
use flate2::read::{DeflateDecoder, ZlibDecoder};

use super::{ContentHandler, ConversionResult};

/// Maximum bytes to scan for text extraction (2 MB).
///
/// Protects against extremely large PDFs consuming excessive memory.
const MAX_SCAN_BYTES: usize = 2 * 1024 * 1024;

/// Maximum total inflated bytes to retain from `FlateDecode` streams.
///
/// Bounds memory + protects against zip-bomb content streams. Decompression
/// stops appending once this ceiling is reached.
const MAX_DECOMPRESSED_BYTES: usize = 8 * 1024 * 1024;

/// Maximum output characters to prevent flooding LLM context windows.
const MAX_OUTPUT_CHARS: usize = 50_000;

/// Classification of *why* a PDF has no extractable text layer in the light path.
///
/// Born-digital PDFs (with embedded fonts) must never be reported as "scanned":
/// they have a text layer the lightweight extractor simply could not decode
/// (e.g. custom CMap/ToUnicode font encoding). That is a distinct failure from a
/// genuinely image-only scan, which needs OCR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PdfKind {
    /// Embedded fonts present → there is a text layer (born-digital).
    HasTextLayer,
    /// Only image `XObjects`, no fonts → genuine scan, needs OCR.
    ImageOnly,
    /// No clear font or image markers were found.
    Indeterminate,
}

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
        let scan = &bytes[..bytes.len().min(MAX_SCAN_BYTES)];

        // Collect stream contents (inflating FlateDecode, skipping images).
        // Born-digital PDFs keep their text inside compressed content streams,
        // which a raw byte scan cannot see.
        let decompressed = collect_stream_contents(scan).unwrap_or_default();

        // Scan decompressed streams first; fall back to raw bytes for
        // old-style uncompressed PDFs.
        let text = extract_pdf_text(&decompressed).or_else(|| extract_pdf_text(scan));

        // Classify on raw bytes plus decompressed content, so fonts hidden
        // inside compressed object streams still count as a text layer.
        let kind = classify_pdf(scan, &decompressed);

        let markdown = build_markdown(text, page_count, kind);

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
                match bytes[i + 1] {
                    b'n' => {
                        result.push('\n');
                        i += 2;
                    }
                    b'r' => {
                        result.push('\r');
                        i += 2;
                    }
                    b't' => {
                        result.push('\t');
                        i += 2;
                    }
                    b'b' => {
                        result.push('\u{8}');
                        i += 2;
                    }
                    b'f' => {
                        result.push('\u{c}');
                        i += 2;
                    }
                    // Octal escape `\ddd` (1-3 octal digits) per PDF spec —
                    // e.g. \050 → '(', \051 → ')'. Without this the digits leak
                    // into the text as literal mojibake (#195).
                    d @ b'0'..=b'7' => {
                        let mut val = (d - b'0') as u32;
                        let mut consumed = 2;
                        for k in 0..2 {
                            match bytes.get(i + 2 + k) {
                                Some(&c @ b'0'..=b'7') => {
                                    val = val * 8 + (c - b'0') as u32;
                                    consumed += 1;
                                }
                                _ => break,
                            }
                        }
                        let byte = (val & 0xFF) as u8;
                        if byte.is_ascii_graphic() || byte == b' ' {
                            result.push(char::from(byte));
                        }
                        i += consumed;
                    }
                    // `\<newline>` is a line continuation: emit nothing.
                    b'\n' => i += 2,
                    b'\r' => {
                        i += 2;
                        if bytes.get(i) == Some(&b'\n') {
                            i += 1;
                        }
                    }
                    c => {
                        // `\(`, `\)`, `\\`, and any other escaped char → literal.
                        result.push(char::from(c));
                        i += 2;
                    }
                }
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

/// Collect decoded `stream … endstream` payloads from a PDF for text scanning.
///
/// `FlateDecode` (zlib) streams are inflated; uncompressed content streams are
/// kept verbatim; image streams (`/DCTDecode`, `/CCITTFaxDecode`, `/JPXDecode`,
/// `/Subtype /Image`) are skipped — they carry no text. Returns `None` when no
/// streams are found so the caller can fall back to a raw byte scan.
///
/// ponytail: heuristic stream walker, not a full PDF parser. It does not apply
/// PNG/TIFF predictors (used by some object/xref streams) or LZW; such streams
/// simply yield no text rather than wrong text. Upgrade path: `--features pdf`
/// (pdfium) for full-fidelity decoding.
fn collect_stream_contents(bytes: &[u8]) -> Option<Vec<u8>> {
    const KW: &[u8] = b"stream";
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    let mut found_any = false;
    // Start of the current object's dictionary lookback — never reaches back
    // past the previous stream, so one object's filters can't bleed into the next.
    let mut prev_end = 0usize;

    while i + KW.len() <= bytes.len() {
        if &bytes[i..i + KW.len()] != KW {
            i += 1;
            continue;
        }
        // Reject the "stream" inside "endstream": the preceding byte is a letter.
        if i > 0 && bytes[i - 1].is_ascii_alphabetic() {
            i += KW.len();
            continue;
        }
        found_any = true;

        // The stream dictionary precedes the keyword; inspect a bounded window
        // clamped to this object (after the previous stream's end).
        let dict_start = i.saturating_sub(512).max(prev_end);
        let dict = &bytes[dict_start..i];

        // Data begins after the EOL that follows the `stream` keyword
        // (CRLF or LF per the PDF spec).
        let mut data_start = i + KW.len();
        if bytes.get(data_start) == Some(&b'\r') {
            data_start += 1;
        }
        if bytes.get(data_start) == Some(&b'\n') {
            data_start += 1;
        }

        let Some(rel_end) = find_subsequence(&bytes[data_start..], b"endstream") else {
            break;
        };
        let data = &bytes[data_start..data_start + rel_end];
        i = data_start + rel_end + b"endstream".len();
        prev_end = i;

        if dict_is_image(dict) {
            continue;
        }

        if window_contains(dict, b"/FlateDecode") {
            if let Some(inflated) = inflate_flate(data) {
                append_capped(&mut out, &inflated);
            }
        } else if !window_contains(dict, b"Decode") {
            // Uncompressed content stream — keep verbatim.
            append_capped(&mut out, data);
        }

        if out.len() >= MAX_DECOMPRESSED_BYTES {
            break;
        }
    }

    if found_any { Some(out) } else { None }
}

/// Append `src` to `dst` without exceeding [`MAX_DECOMPRESSED_BYTES`].
fn append_capped(dst: &mut Vec<u8>, src: &[u8]) {
    let room = MAX_DECOMPRESSED_BYTES.saturating_sub(dst.len());
    if room == 0 {
        return;
    }
    dst.extend_from_slice(&src[..src.len().min(room)]);
}

/// Returns `true` if a stream dictionary window denotes image data (no text).
fn dict_is_image(dict: &[u8]) -> bool {
    window_contains(dict, b"/DCTDecode")
        || window_contains(dict, b"/CCITTFaxDecode")
        || window_contains(dict, b"/JPXDecode")
        || window_contains(dict, b"/JBIG2Decode")
        || window_contains(dict, b"/Image")
}

/// Inflate a zlib (`FlateDecode`) stream, capped at [`MAX_DECOMPRESSED_BYTES`].
///
/// Tries zlib (with header) first, then raw DEFLATE for producers that omit it.
/// Partial output is accepted: a truncated/corrupt tail still yields the text
/// decoded so far.
fn inflate_flate(data: &[u8]) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut z = ZlibDecoder::new(data).take(MAX_DECOMPRESSED_BYTES as u64);
    if z.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
        return Some(buf);
    }
    buf.clear();
    let mut d = DeflateDecoder::new(data).take(MAX_DECOMPRESSED_BYTES as u64);
    let _ = d.read_to_end(&mut buf);
    if buf.is_empty() { None } else { Some(buf) }
}

/// Classify why a PDF has (or lacks) a text layer, for honest messaging (#195).
///
/// An embedded font anywhere (raw bytes or decompressed streams) means the
/// document has a real text layer — it must never be labelled "scanned".
fn classify_pdf(raw: &[u8], decompressed: &[u8]) -> PdfKind {
    let has_font = window_contains(raw, b"/Font")
        || window_contains(raw, b"/FontDescriptor")
        || window_contains(decompressed, b"/Font")
        || window_contains(decompressed, b"/FontDescriptor");
    if has_font {
        return PdfKind::HasTextLayer;
    }

    let has_image = dict_is_image(raw) || dict_is_image(decompressed);
    if has_image {
        return PdfKind::ImageOnly;
    }

    PdfKind::Indeterminate
}

/// Returns `true` if `haystack` contains `needle`.
fn window_contains(haystack: &[u8], needle: &[u8]) -> bool {
    find_subsequence(haystack, needle).is_some()
}

/// First index of `needle` in `haystack`, or `None`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Build the final markdown output from extracted text and page count.
fn build_markdown(text: Option<String>, page_count: usize, kind: PdfKind) -> String {
    let pages_label = if page_count == 1 {
        "1 page".to_string()
    } else {
        format!("{page_count} pages")
    };

    match text {
        Some(extracted) if !extracted.trim().is_empty() => {
            format!(
                "[PDF: {pages_label}, text extracted — for full fidelity rebuild with `--features pdf`]\n\n{}",
                extracted.trim()
            )
        }
        // No text extracted. Distinguish a born-digital PDF the light extractor
        // could not decode from a genuine image-only scan (#195) — never report
        // a PDF with an embedded font layer as "scanned".
        _ => match kind {
            PdfKind::HasTextLayer => format!(
                "[PDF: {pages_label}, text layer present but not decoded by the built-in extractor \
                 (likely custom font encoding). Rebuild with `--features pdf` for pdfium extraction.]"
            ),
            PdfKind::ImageOnly => format!(
                "[PDF: {pages_label}, scanned (image-only) — no text layer detected. Use OCR to extract text.]"
            ),
            PdfKind::Indeterminate => format!(
                "[PDF: {pages_label}, no extractable text. If this is a scan, use OCR; \
                 otherwise rebuild with `--features pdf` for pdfium extraction.]"
            ),
        },
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
    fn parse_literal_string_decodes_octal_escapes() {
        // \050 → '(', \051 → ')' per PDF spec (#195 mojibake fix)
        let (s, _) = parse_literal_string(b"(RL\\050x\\051)").unwrap();
        assert_eq!(s, "RL(x)");
    }

    #[test]
    fn parse_literal_string_octal_line_continuation_is_dropped() {
        // A backslash before a newline is a line continuation, not a char.
        let (s, _) = parse_literal_string(b"(ab\\\ncd)").unwrap();
        assert_eq!(s, "abcd");
    }

    #[test]
    fn parse_literal_string_handles_nested_parens() {
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
        let md = build_markdown(Some("Sample content".to_string()), 2, PdfKind::HasTextLayer);
        // THEN: contains PDF header and the text
        assert!(md.contains("[PDF: 2 pages"), "got: {md}");
        assert!(md.contains("Sample content"), "got: {md}");
    }

    #[test]
    fn build_markdown_image_only_reports_scanned() {
        // GIVEN: no text, classified as image-only
        let md = build_markdown(None, 5, PdfKind::ImageOnly);
        // THEN: reports as scanned PDF, suggests OCR
        assert!(md.contains("[PDF:"), "got: {md}");
        assert!(md.contains("scanned"), "got: {md}");
        assert!(md.contains("OCR"), "got: {md}");
        assert!(md.contains("5 pages"), "got: {md}");
    }

    #[test]
    fn build_markdown_born_digital_never_reports_scanned() {
        // GIVEN: no text extracted but an embedded font layer is present (#195)
        let md = build_markdown(None, 28, PdfKind::HasTextLayer);
        // THEN: must NOT mislabel a born-digital PDF as scanned
        assert!(
            !md.contains("scanned"),
            "born-digital must not say scanned: {md}"
        );
        assert!(md.contains("text layer present"), "got: {md}");
        assert!(md.contains("--features pdf"), "got: {md}");
    }

    #[test]
    fn build_markdown_messages_are_distinct_per_kind() {
        // AC #195: image-only message must differ from the feature-not-compiled case.
        let image = build_markdown(None, 3, PdfKind::ImageOnly);
        let born = build_markdown(None, 3, PdfKind::HasTextLayer);
        assert_ne!(image, born, "scanned vs born-digital messages must differ");
    }

    #[test]
    fn build_markdown_single_page_uses_singular_form() {
        // GIVEN: single-page PDF with no text
        let md = build_markdown(None, 1, PdfKind::ImageOnly);
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
    fn pdf_light_handler_reports_scanned_for_image_only_pdf() {
        // GIVEN: an image-only PDF (image XObject, DCTDecode, no fonts)
        let pdf = b"%PDF-1.4\n/Type /Page\n<< /Type /XObject /Subtype /Image /Filter /DCTDecode >>\nstream\n\xff\xd8\xff\xe0junk\nendstream\nxref\n%%EOF";
        let handler = PdfLightHandler;
        // WHEN: converting to markdown
        let result = handler.to_markdown(pdf, "application/pdf").unwrap();
        // THEN: reports scanned PDF (genuine image, needs OCR)
        assert!(
            result.markdown.contains("scanned"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn pdf_light_handler_extracts_text_from_flate_compressed_stream() {
        // GIVEN: a born-digital PDF whose text lives in a FlateDecode stream (#195).
        // This is the regression case: raw byte scanning sees nothing; only
        // inflating the stream recovers the text.
        let content = b"BT\n(Hello from a compressed stream) Tj\nET";
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write as _;
        enc.write_all(content).unwrap();
        let compressed = enc.finish().unwrap();

        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n/Type /Page\n/Font /F1\n");
        pdf.extend_from_slice(b"<< /Filter /FlateDecode /Length ");
        pdf.extend_from_slice(compressed.len().to_string().as_bytes());
        pdf.extend_from_slice(b" >>\nstream\n");
        pdf.extend_from_slice(&compressed);
        pdf.extend_from_slice(b"\nendstream\n%%EOF");

        let handler = PdfLightHandler;
        // WHEN: converting to markdown
        let result = handler.to_markdown(&pdf, "application/pdf").unwrap();
        // THEN: the compressed text is extracted and it is NOT called scanned
        assert!(
            result.markdown.contains("Hello from a compressed stream"),
            "got: {}",
            result.markdown
        );
        assert!(
            !result.markdown.contains("scanned"),
            "born-digital compressed PDF must not be scanned: {}",
            result.markdown
        );
    }

    #[test]
    fn collect_stream_contents_inflates_flate_and_skips_images() {
        let text = b"BT (visible) Tj ET";
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write as _;
        enc.write_all(text).unwrap();
        let compressed = enc.finish().unwrap();

        let mut pdf: Vec<u8> = Vec::new();
        // image stream — must be skipped
        pdf.extend_from_slice(
            b"<< /Subtype /Image /Filter /DCTDecode >>\nstream\nRAWIMAGE\nendstream\n",
        );
        // text stream — must be inflated
        pdf.extend_from_slice(b"<< /Filter /FlateDecode >>\nstream\n");
        pdf.extend_from_slice(&compressed);
        pdf.extend_from_slice(b"\nendstream\n");

        let out = collect_stream_contents(&pdf).expect("streams present");
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("visible"), "inflated text missing: {s}");
        assert!(
            !s.contains("RAWIMAGE"),
            "image stream should be skipped: {s}"
        );
    }

    #[test]
    fn classify_pdf_detects_born_digital_via_font() {
        let pdf = b"%PDF-1.5\n<< /Type /Font /Subtype /Type1 /FontDescriptor 1 0 R >>";
        assert_eq!(classify_pdf(pdf, b""), PdfKind::HasTextLayer);
    }

    #[test]
    fn classify_pdf_detects_image_only() {
        let pdf = b"%PDF-1.5\n<< /Subtype /Image /Filter /DCTDecode >>";
        assert_eq!(classify_pdf(pdf, b""), PdfKind::ImageOnly);
    }

    #[test]
    fn inflate_flate_handles_corrupt_tail_gracefully() {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write as _;
        enc.write_all(b"good text here").unwrap();
        let mut compressed = enc.finish().unwrap();
        compressed.truncate(compressed.len().saturating_sub(2)); // corrupt the tail
        // Should not panic; returns whatever decoded (possibly None on tiny input).
        let _ = inflate_flate(&compressed);
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
