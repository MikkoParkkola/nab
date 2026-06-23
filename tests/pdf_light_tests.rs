//! Integration tests for the **default-build** light PDF path
//! (`nab::content::pdf_light`), exercised through the public
//! `ContentHandler` / `ContentRouter` API.
//!
//! These tests are the regression guard for issue #195: born-digital PDFs whose
//! text lives in `FlateDecode` (zlib-compressed) content streams were falsely
//! reported as "scanned" because the light extractor only scanned raw bytes.
//!
//! Gated on `not(feature = "pdf")` — when the pdfium feature is enabled a
//! different handler is wired in (`tests/pdf_tests.rs` covers that path), so
//! these assertions about the light handler's messages would not apply.

#![cfg(not(feature = "pdf"))]

use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;

use nab::content::pdf_light::PdfLightHandler;
use nab::content::{ContentHandler, ContentRouter};

// ── Fixture builders ────────────────────────────────────────────────────────

/// Zlib-compress a PDF content-stream body (what `FlateDecode` stores).
fn flate(body: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(body).unwrap();
    enc.finish().unwrap()
}

/// Build a born-digital PDF whose only text lives in a `FlateDecode` stream.
/// Declares a `/Font` so it classifies as having a text layer.
fn born_digital_flate_pdf(content_stream: &[u8]) -> Vec<u8> {
    let compressed = flate(content_stream);
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.5\n");
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Page >>\nendobj\n");
    pdf.extend_from_slice(
        b"2 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );
    pdf.extend_from_slice(b"3 0 obj\n<< /Filter /FlateDecode /Length ");
    pdf.extend_from_slice(compressed.len().to_string().as_bytes());
    pdf.extend_from_slice(b" >>\nstream\n");
    pdf.extend_from_slice(&compressed);
    pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF");
    pdf
}

/// Build a genuinely image-only PDF: an image `XObject`, no fonts.
fn image_only_pdf() -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Page >>\nendobj\n");
    pdf.extend_from_slice(
        b"2 0 obj\n<< /Type /XObject /Subtype /Image /Width 100 /Height 100 /Filter /DCTDecode /Length 8 >>\nstream\n",
    );
    pdf.extend_from_slice(b"\xff\xd8\xff\xe0junk");
    pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF");
    pdf
}

// ── #195 regression: compressed born-digital extracts text ──────────────────

#[test]
fn flate_born_digital_extracts_text_via_handler() {
    let pdf = born_digital_flate_pdf(b"BT (Compressed body text) Tj ET");
    let md = PdfLightHandler
        .to_markdown(&pdf, "application/pdf")
        .unwrap()
        .markdown;
    assert!(md.contains("Compressed body text"), "got: {md}");
    assert!(
        !md.contains("scanned"),
        "must not mislabel born-digital: {md}"
    );
    assert!(md.contains("text extracted"), "got: {md}");
}

#[test]
fn flate_born_digital_routes_through_content_router() {
    // The default ContentRouter must dispatch application/pdf to the light handler
    // and extract compressed text end-to-end.
    let pdf = born_digital_flate_pdf(b"BT (Routed through ContentRouter) Tj ET");
    let md = ContentRouter::new()
        .convert(&pdf, "application/pdf")
        .unwrap()
        .markdown;
    assert!(md.contains("Routed through ContentRouter"), "got: {md}");
    assert!(!md.contains("scanned"), "got: {md}");
}

// ── Three distinct verdicts (AC #195) ───────────────────────────────────────

#[test]
fn image_only_pdf_reports_scanned_with_ocr_hint() {
    let md = PdfLightHandler
        .to_markdown(&image_only_pdf(), "application/pdf")
        .unwrap()
        .markdown;
    assert!(md.contains("scanned"), "image-only should be scanned: {md}");
    assert!(md.contains("OCR"), "should hint OCR: {md}");
}

#[test]
fn born_digital_without_decodable_text_is_not_scanned() {
    // Font present (text layer) but the stream carries no BT/ET we can decode:
    // must route to the "text layer present" message, never "scanned".
    let pdf = born_digital_flate_pdf(b"q 1 0 0 1 0 0 cm Q"); // graphics ops, no text
    let md = PdfLightHandler
        .to_markdown(&pdf, "application/pdf")
        .unwrap()
        .markdown;
    assert!(
        !md.contains("scanned"),
        "born-digital must not be scanned: {md}"
    );
    assert!(md.contains("text layer present"), "got: {md}");
}

#[test]
fn scanned_and_feature_messages_differ() {
    let image = PdfLightHandler
        .to_markdown(&image_only_pdf(), "application/pdf")
        .unwrap()
        .markdown;
    let born = PdfLightHandler
        .to_markdown(&born_digital_flate_pdf(b"q Q"), "application/pdf")
        .unwrap()
        .markdown;
    assert_ne!(
        image, born,
        "the two failure messages must be distinguishable"
    );
}

// ── Octal escape decoding through the full path (#195 mojibake) ──────────────

#[test]
fn octal_escapes_decode_in_compressed_text() {
    // \050 → '(', \051 → ')' — without decoding these leak as digits.
    let pdf = born_digital_flate_pdf(b"BT (RL\\050x\\051) Tj ET");
    let md = PdfLightHandler
        .to_markdown(&pdf, "application/pdf")
        .unwrap()
        .markdown;
    assert!(md.contains("RL(x)"), "octal escapes should decode: {md}");
    assert!(!md.contains("050"), "raw octal digits leaked: {md}");
}

// ── No regression: uncompressed PDFs still work ─────────────────────────────

#[test]
fn uncompressed_simple_pdf_still_extracts() {
    let pdf = b"%PDF-1.4\n/Type /Page\n/Font /F1\nBT (Plain uncompressed text) Tj ET\n%%EOF";
    let md = PdfLightHandler
        .to_markdown(pdf, "application/pdf")
        .unwrap()
        .markdown;
    assert!(md.contains("Plain uncompressed text"), "got: {md}");
}

#[test]
fn non_pdf_bytes_error() {
    assert!(
        PdfLightHandler
            .to_markdown(b"<html>nope</html>", "application/pdf")
            .is_err()
    );
}
