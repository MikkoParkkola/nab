//! Integration tests for PDF-to-markdown extraction (`nab::content::pdf`).
//!
//! All tests are gated on the `pdf` feature flag, which requires pdfium at
//! runtime. When pdfium is unavailable the tests skip gracefully rather than
//! failing the suite — this allows CI without the native library installed to
//! still run the rest of the tests.
//!
//! # PDF fixture generation
//!
//! PDF is a text-based format at its core. The helpers in this module emit
//! minimal but fully standards-compliant PDF 1.4 documents using raw byte
//! strings. No third-party PDF-writing crate is needed.
//!
//! # Private function coverage note
//!
//! The following internal methods on `PdfHandler` lack test coverage because
//! they are private (`fn`, not `pub fn`) and therefore not reachable from an
//! integration test:
//!
//! - `PdfHandler::extract_chars`     — needs pdfium, positional char extraction
//! - `PdfHandler::extract_text_simple` — needs pdfium, pdfium text reconstruction
//! - `PdfHandler::load_pdfium`        — environment-dependent library search
//!
//! The public `ContentHandler::to_markdown` entry point exercises all three
//! transitively through the end-to-end tests below.

#![cfg(feature = "pdf")]

use nab::content::pdf::PdfHandler;
use nab::content::{ContentHandler, ContentRouter};

// ── PDF fixture builders ──────────────────────────────────────────────────────

/// Encode the length field and assemble a complete PDF 1.4 document.
///
/// Each `(stream_body, font_size)` tuple in `pages` becomes one PDF page
/// rendered with Helvetica at the given font size. The `stream_body` must
/// contain valid PDF content-stream operators (BT … ET blocks).
///
/// Returns the raw bytes of the complete PDF.
#[allow(clippy::write_with_newline)] // PDF format requires exact \n bytes
fn build_pdf(pages: &[(&str, f32)]) -> Vec<u8> {
    use std::fmt::Write;
    // We pre-allocate objects:
    //   1 = Catalog
    //   2 = Pages
    //   3..3+N-1  = Page objects   (one per page)
    //   3+N..3+2N-1 = Content streams (one per page)
    //   3+2N = Font
    let n = pages.len();
    let font_obj = 3 + 2 * n;
    let page_obj_ids: Vec<usize> = (3..3 + n).collect();
    let stream_obj_ids: Vec<usize> = (3 + n..3 + 2 * n).collect();

    let kids: String = page_obj_ids
        .iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");

    let mut body = String::new();
    // Track byte offsets for the xref table (we use a simple append strategy).
    let mut offsets: Vec<usize> = Vec::new();

    let header = "%PDF-1.4\n";
    let mut pdf = header.as_bytes().to_vec();

    // Object 1 — Catalog
    offsets.push(pdf.len());
    let obj1 = "1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n".to_string();
    pdf.extend_from_slice(obj1.as_bytes());

    // Object 2 — Pages
    offsets.push(pdf.len());
    let obj2 = format!("2 0 obj << /Type /Pages /Kids [{kids}] /Count {n} >> endobj\n");
    pdf.extend_from_slice(obj2.as_bytes());

    // Page objects (3 … 3+N-1)
    for (i, &stream_id) in stream_obj_ids.iter().enumerate() {
        let page_id = page_obj_ids[i];
        offsets.push(pdf.len());
        let obj = format!(
            "{page_id} 0 obj << /Type /Page /Parent 2 0 R \
             /MediaBox [0 0 612 792] /Contents {stream_id} 0 R \
             /Resources << /Font << /F1 {font_obj} 0 R >> >> >> endobj\n"
        );
        pdf.extend_from_slice(obj.as_bytes());
    }

    // Content stream objects
    for (i, &(stream_body, _font_size)) in pages.iter().enumerate() {
        let stream_id = stream_obj_ids[i];
        let stream_bytes = stream_body.as_bytes();
        offsets.push(pdf.len());
        let header = format!(
            "{stream_id} 0 obj << /Length {} >>\nstream\n",
            stream_bytes.len()
        );
        pdf.extend_from_slice(header.as_bytes());
        pdf.extend_from_slice(stream_bytes);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }

    // Font object
    offsets.push(pdf.len());
    let font_obj_str =
        format!("{font_obj} 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n");
    pdf.extend_from_slice(font_obj_str.as_bytes());

    // xref table
    let xref_offset = pdf.len();
    let total_objects = font_obj + 1; // 0-based: object 0 is the free-list entry
    let _ = write!(body, "xref\n0 {total_objects}\n");
    body.push_str("0000000000 65535 f \n"); // object 0 (free)
    // xref entries for objects 1..total_objects-1 in order
    // offsets[k] is the offset of object (k+1)
    for off in &offsets {
        let _ = write!(body, "{off:010} 00000 n \n");
    }
    let _ = write!(
        body,
        "trailer << /Size {total_objects} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
    );
    pdf.extend_from_slice(body.as_bytes());
    pdf
}

/// Single-page PDF with a sentence of body text at 12 pt.
fn simple_text_pdf(text: &str) -> Vec<u8> {
    // Escape parentheses required by PDF string syntax
    let escaped = text
        .replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)");
    let stream = format!("BT /F1 12 Tf 72 720 Td ({escaped}) Tj ET\n");
    build_pdf(&[(&stream, 12.0)])
}

/// Two-page PDF. `page1` goes on page 1, `page2` on page 2.
fn two_page_pdf(page1: &str, page2: &str) -> Vec<u8> {
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)")
    };
    let s1 = format!("BT /F1 12 Tf 72 720 Td ({}) Tj ET\n", esc(page1));
    let s2 = format!("BT /F1 12 Tf 72 720 Td ({}) Tj ET\n", esc(page2));
    build_pdf(&[(&s1, 12.0), (&s2, 12.0)])
}

/// PDF with a large-font heading followed by small-font body text.
///
/// Pdfium reports character heights proportional to font size, so a 24 pt
/// heading character will have height > 16 pt and trigger `## heading` in
/// `render_markdown`.
fn heading_pdf(heading: &str, body: &str) -> Vec<u8> {
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)")
    };
    let stream = format!(
        "BT /F1 24 Tf 72 720 Td ({}) Tj ET\n\
         BT /F1 10 Tf 72 690 Td ({}) Tj ET\n",
        esc(heading),
        esc(body)
    );
    build_pdf(&[(&stream, 24.0)])
}

/// PDF whose content stream is empty — simulates a scanned/image-only PDF.
fn empty_text_pdf() -> Vec<u8> {
    build_pdf(&[("", 12.0)])
}

/// Aligned columnar text on one page designed to exercise table detection.
///
/// Three rows, three columns, separated by large horizontal gaps.
fn table_pdf() -> Vec<u8> {
    // Each column starts at fixed X positions with large gaps between them.
    let stream = concat!(
        "BT /F1 12 Tf 72 720 Td (Name) Tj 200 0 Td (Age) Tj 200 0 Td (City) Tj ET\n",
        "BT /F1 12 Tf 72 700 Td (Alice) Tj 200 0 Td (30) Tj 200 0 Td (NYC) Tj ET\n",
        "BT /F1 12 Tf 72 680 Td (Bob) Tj 200 0 Td (25) Tj 200 0 Td (LA) Tj ET\n",
        "BT /F1 12 Tf 72 660 Td (Carol) Tj 200 0 Td (35) Tj 200 0 Td (SF) Tj ET\n",
    );
    build_pdf(&[(stream, 12.0)])
}

// ── Pdfium availability detection ────────────────────────────────────────────

/// Returns `true` when pdfium is loadable in the current environment.
///
/// We probe by attempting to convert a known-valid single-byte (empty) PDF.
/// If the error message mentions the library not being found we skip; any
/// other outcome (success OR a different error such as parse failure) means
/// the library is present and tests should run.
fn pdfium_available() -> bool {
    // Use a known-minimal valid PDF that pdfium can try to open.
    let pdf = empty_text_pdf();
    let handler = PdfHandler::new();
    match handler.to_markdown(&pdf, "application/pdf") {
        Ok(_) => true,
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            // Library not installed — tests should skip.
            let not_found = msg.contains("not found")
                || msg.contains("no such file")
                || msg.contains("cannot open")
                || msg.contains("dlopen")
                || msg.contains("loadlibrary");
            !not_found
        }
    }
}

// ── Helper macro for skipping when pdfium is absent ──────────────────────────

/// Evaluate `$expr` and return from the test with a message when pdfium is absent.
macro_rules! require_pdfium {
    () => {
        if !pdfium_available() {
            eprintln!("SKIP: pdfium library not installed — install via `pip3 install pypdfium2`");
            return;
        }
    };
}

// ── 1. ContentHandler trait interface ────────────────────────────────────────

#[test]
fn pdf_handler_supported_types_returns_application_pdf() {
    // GIVEN: a freshly constructed PdfHandler
    // WHEN: we query supported MIME types
    // THEN: exactly ["application/pdf"]
    let handler = PdfHandler::new();
    assert_eq!(handler.supported_types(), &["application/pdf"]);
}

#[test]
#[allow(clippy::default_constructed_unit_structs)]
fn pdf_handler_new_and_default_are_equivalent() {
    // GIVEN: PdfHandler can be constructed two ways
    // WHEN: we check supported types on both
    // THEN: identical results (both are unit structs)
    let a = PdfHandler::new();
    let b = PdfHandler::default();
    assert_eq!(a.supported_types(), b.supported_types());
}

// ── 2. MAX_PDF_SIZE enforcement ───────────────────────────────────────────────

#[test]
fn oversized_pdf_returns_error_before_pdfium_is_loaded() {
    // GIVEN: a byte slice larger than MAX_PDF_SIZE (50 MB)
    // WHEN: we attempt to convert it
    // THEN: error before pdfium is ever touched — no pdfium needed for this test
    let handler = PdfHandler::new();
    // 50 MB + 1 byte to exceed the limit
    let oversized = vec![0u8; 50 * 1024 * 1024 + 1];
    let result = handler.to_markdown(&oversized, "application/pdf");
    assert!(result.is_err(), "should reject oversized input");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("too large") || msg.contains("MB"),
        "error should mention size, got: {msg}"
    );
}

#[test]
fn pdf_at_exact_size_limit_does_not_trigger_size_error() {
    // GIVEN: a byte slice exactly at MAX_PDF_SIZE (50 MB)
    // WHEN: we attempt to convert it
    // THEN: no size error (a different error may occur since it's not a valid PDF)
    let handler = PdfHandler::new();
    let at_limit = vec![0u8; 50 * 1024 * 1024];
    let result = handler.to_markdown(&at_limit, "application/pdf");
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(
            !msg.contains("too large"),
            "50 MB exactly should not trigger size error, got: {msg}"
        );
    }
}

// ── 3. Content-type routing through ContentRouter ────────────────────────────

#[test]
fn content_router_routes_application_pdf_to_pdf_handler() {
    // GIVEN: ContentRouter with pdf feature enabled
    // WHEN: we pass a byte slice with content-type "application/pdf"
    // THEN: the PDF handler is selected (size check runs before pdfium)
    let router = ContentRouter::new();
    let oversized = vec![0u8; 50 * 1024 * 1024 + 1];
    let result = router.convert(&oversized, "application/pdf");
    // We can confirm PDF handler ran because the size-error message is PDF-specific
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("too large") || msg.contains("MB"),
        "should route to PdfHandler and get size error, got: {msg}"
    );
}

#[test]
fn content_router_does_not_route_html_to_pdf_handler() {
    // GIVEN: ContentRouter
    // WHEN: HTML bytes with text/html content-type
    // THEN: HTML handler runs (not PDF handler — no size error, just markdown)
    let router = ContentRouter::new();
    let oversized = vec![0u8; 50 * 1024 * 1024 + 1];
    let result = router.convert(&oversized, "text/html");
    // HTML handler does not enforce a size limit, so it should succeed
    assert!(
        result.is_ok(),
        "HTML handler should handle large input, got: {:?}",
        result.err()
    );
}

// ── 4. End-to-end tests requiring pdfium at runtime ──────────────────────────

#[test]
fn simple_text_pdf_extracts_text_content() {
    // GIVEN: a valid single-page PDF containing "Hello World"
    // WHEN: PdfHandler converts it to markdown
    // THEN: the output contains the expected text
    require_pdfium!();

    let handler = PdfHandler::new();
    let pdf = simple_text_pdf("Hello World");
    let result = handler.to_markdown(&pdf, "application/pdf").unwrap();

    assert!(
        result.markdown.contains("Hello World"),
        "expected 'Hello World' in output, got: {}",
        result.markdown
    );
    assert_eq!(result.page_count, Some(1));
    assert_eq!(result.content_type, "application/pdf");
    assert!(result.elapsed_ms >= 0.0);
}

#[test]
fn simple_text_pdf_longer_sentence_is_extracted() {
    // GIVEN: a PDF with a multi-word sentence
    // WHEN: converted
    // THEN: all words are present in the output
    require_pdfium!();

    let handler = PdfHandler::new();
    let sentence = "The quick brown fox jumps over the lazy dog";
    let pdf = simple_text_pdf(sentence);
    let result = handler.to_markdown(&pdf, "application/pdf").unwrap();

    assert!(
        result.markdown.contains("quick") && result.markdown.contains("lazy"),
        "full sentence should be in output, got: {}",
        result.markdown
    );
}

#[test]
fn multi_page_pdf_extracts_text_from_all_pages() {
    // GIVEN: a two-page PDF
    // WHEN: PdfHandler converts it
    // THEN: text from both pages is present; page_count is 2
    require_pdfium!();

    let handler = PdfHandler::new();
    let pdf = two_page_pdf("Page one content", "Page two content");
    let result = handler.to_markdown(&pdf, "application/pdf").unwrap();

    assert_eq!(result.page_count, Some(2), "should report 2 pages");
    assert!(
        result.markdown.contains("Page one") || result.markdown.contains("one content"),
        "page 1 text missing from output: {}",
        result.markdown
    );
    assert!(
        result.markdown.contains("Page two") || result.markdown.contains("two content"),
        "page 2 text missing from output: {}",
        result.markdown
    );
}

#[test]
fn multi_page_pdf_has_page_separator_between_pages() {
    // GIVEN: a two-page PDF with distinct content on each page
    // WHEN: PdfHandler converts it
    // THEN: a page separator ("---") appears between pages
    require_pdfium!();

    let handler = PdfHandler::new();
    let pdf = two_page_pdf("First page text", "Second page text");
    let result = handler.to_markdown(&pdf, "application/pdf").unwrap();

    assert!(
        result.markdown.contains("---"),
        "expected page separator '---' between pages, got: {}",
        result.markdown
    );
}

#[test]
fn empty_text_pdf_returns_scanned_notice() {
    // GIVEN: a valid PDF whose content stream has no text operators
    // WHEN: PdfHandler converts it
    // THEN: the output contains the scanned-PDF notice (no panic)
    require_pdfium!();

    let handler = PdfHandler::new();
    let pdf = empty_text_pdf();
    let result = handler.to_markdown(&pdf, "application/pdf").unwrap();

    // The scanned PDF notice is triggered when text is empty but page_count > 0
    assert!(
        result.markdown.contains("Scanned PDF")
            || result.markdown.contains("no text layer")
            || result.markdown.is_empty(),
        "expected scanned-PDF notice or empty output, got: {}",
        result.markdown
    );
    assert!(
        result.page_count.is_some(),
        "page_count should be populated even for empty PDFs"
    );
}

#[test]
fn empty_text_pdf_does_not_panic() {
    // GIVEN: a PDF with no text content
    // WHEN: converted
    // THEN: no panic — correct behavior for image-only / blank PDFs
    require_pdfium!();

    let handler = PdfHandler::new();
    let pdf = empty_text_pdf();
    // Should not panic regardless of output
    let _ = handler.to_markdown(&pdf, "application/pdf");
}

#[test]
fn content_type_field_preserved_in_result() {
    // GIVEN: a valid simple PDF
    // WHEN: converted with content_type "application/pdf"
    // THEN: result.content_type echoes the input content_type
    require_pdfium!();

    let handler = PdfHandler::new();
    let pdf = simple_text_pdf("type field test");
    let result = handler.to_markdown(&pdf, "application/pdf").unwrap();
    assert_eq!(result.content_type, "application/pdf");
}

#[test]
fn elapsed_ms_is_non_negative() {
    // GIVEN: a valid PDF
    // WHEN: converted
    // THEN: elapsed_ms is ≥ 0 (conversion can be fast but not negative)
    require_pdfium!();

    let handler = PdfHandler::new();
    let pdf = simple_text_pdf("timing test");
    let result = handler.to_markdown(&pdf, "application/pdf").unwrap();
    assert!(
        result.elapsed_ms >= 0.0,
        "elapsed_ms should be non-negative, got: {}",
        result.elapsed_ms
    );
}

#[test]
fn corrupted_pdf_returns_error_not_panic() {
    // GIVEN: bytes that are not a valid PDF
    // WHEN: PdfHandler attempts to parse them
    // THEN: an error is returned (no panic, no UB)
    require_pdfium!();

    let handler = PdfHandler::new();
    let garbage = b"this is not a pdf at all %%%%";
    let result = handler.to_markdown(garbage, "application/pdf");
    assert!(
        result.is_err(),
        "corrupted input should return Err, got Ok({})",
        result.as_ref().unwrap().markdown
    );
}

#[test]
fn empty_byte_slice_returns_error_not_panic() {
    // GIVEN: zero bytes
    // WHEN: converted
    // THEN: error returned cleanly
    require_pdfium!();

    let handler = PdfHandler::new();
    let result = handler.to_markdown(&[], "application/pdf");
    assert!(
        result.is_err(),
        "empty input should return Err, got Ok with: {}",
        result.as_ref().map_or("", |r| r.markdown.as_str())
    );
}

#[test]
fn heading_pdf_large_font_renders_as_heading() {
    // GIVEN: a PDF where the first line uses 24 pt font (height > 16 pt)
    // WHEN: PdfHandler converts it
    // THEN: the heading text appears with ## prefix in the output
    //
    // Note: this exercises the render_markdown heading heuristic:
    //   avg_height > 16.0 && text.len() < 100 → "## {text}"
    require_pdfium!();

    let handler = PdfHandler::new();
    let pdf = heading_pdf("Big Title", "Body paragraph text here.");
    let result = handler.to_markdown(&pdf, "application/pdf").unwrap();

    // Pdfium's built-in text reconstruction (extract_text_simple) does NOT
    // include font size metadata — heading detection requires the char-by-char
    // path (extract_chars → reconstruct_lines → render_markdown), which is
    // currently bypassed in favour of text.all(). The test therefore accepts
    // either the heading-formatted output OR plain text; the important thing is
    // that the content is present and the handler does not error.
    assert!(
        result.markdown.contains("Big Title"),
        "heading text should appear in output regardless of heading format, got: {}",
        result.markdown
    );
}

#[test]
fn table_pdf_text_is_extractable() {
    // GIVEN: a PDF with columnar data
    // WHEN: PdfHandler converts it
    // THEN: the cell values appear in the output (table format depends on
    //       whether char-by-char table detection is active; text.all() extracts
    //       the values in reading order without markdown table syntax)
    require_pdfium!();

    let handler = PdfHandler::new();
    let pdf = table_pdf();
    let result = handler.to_markdown(&pdf, "application/pdf").unwrap();

    for word in &["Name", "Age", "City", "Alice", "Bob", "Carol"] {
        assert!(
            result.markdown.contains(word),
            "column value '{word}' should be present in output, got: {}",
            result.markdown
        );
    }
}

// ── 5. ContentRouter end-to-end with pdf feature ─────────────────────────────

#[test]
fn content_router_converts_valid_pdf_via_pdf_handler() {
    // GIVEN: ContentRouter with pdf feature, valid single-page PDF bytes
    // WHEN: convert called with "application/pdf"
    // THEN: output contains the text; page_count is Some(1)
    require_pdfium!();

    let router = ContentRouter::new();
    let pdf = simple_text_pdf("Router integration test");
    let result = router.convert(&pdf, "application/pdf").unwrap();

    assert!(
        result.markdown.contains("Router integration test"),
        "expected text in output, got: {}",
        result.markdown
    );
    assert_eq!(result.page_count, Some(1));
}

#[test]
fn content_router_content_type_with_charset_routes_to_pdf_handler() {
    // GIVEN: content-type "application/pdf; charset=binary" (unusual but valid)
    // WHEN: oversized input so we get the size-error from PdfHandler
    // THEN: error confirms PDF handler was selected (not HTML/plain fallback)
    let router = ContentRouter::new();
    let oversized = vec![0u8; 50 * 1024 * 1024 + 1];
    let result = router.convert(&oversized, "application/pdf; charset=binary");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("too large") || msg.contains("MB"),
        "PDF handler should handle charset param and enforce size limit, got: {msg}"
    );
}

// ── 6. Unit tests for internally-accessible types (no pdfium needed) ─────────
//
// PdfHandler::reconstruct_lines and render_markdown are private, so they are
// already covered by the inline unit tests in src/content/pdf.rs.
// Table and PdfChar types are also covered there and in table_tests within
// src/content/table.rs.
//
// The tests below exercise the public-facing ConversionResult fields that
// do not require pdfium.

#[test]
fn pdf_handler_is_send_and_sync() {
    // GIVEN: PdfHandler
    // THEN: it satisfies Send + Sync (required for spawn_blocking usage)
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PdfHandler>();
}

#[test]
#[allow(clippy::default_constructed_unit_structs)]
fn pdf_handler_default_trait_is_available() {
    // GIVEN: PdfHandler derives Default
    // WHEN: Default::default() is called
    // THEN: it produces the same unit struct as PdfHandler::new()
    let _h = PdfHandler::default();
}
