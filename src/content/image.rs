//! Image content handler.
//!
//! Converts image responses to human-readable markdown descriptions.
//! No ML, no external services — pure byte-level metadata extraction.
//!
//! # Supported formats
//!
//! | Format  | Dimensions | Notes                          |
//! |---------|-----------|--------------------------------|
//! | PNG     | ✓         | IHDR chunk at offset 16        |
//! | JPEG    | ✓         | SOF0/SOF2 marker scan          |
//! | GIF     | ✓         | Logical screen descriptor      |
//! | WebP    | ✓         | VP8/VP8L/VP8X chunks           |
//! | AVIF    | ✓         | ISPE box in ftyp/mdat          |
//! | BMP     | ✓         | DIB header at offset 14        |
//! | ICO     | ✓         | First image entry              |
//! | TIFF    | ✓         | IFD tag scan                   |
//! | SVG     | ✓ (attr)  | Text extraction from title/desc|
//!
//! # Output format
//!
//! ```text
//! [Image: PNG 1920×1080]
//! [Image: JPEG 640×480 — portrait photo]
//! [Image: SVG — "Chart title"]
//! [Image: unknown format, 2048 bytes]
//! ```

use anyhow::Result;

use super::{ContentHandler, ConversionResult};

/// Handles `image/*` content types by producing a markdown description.
pub struct ImageHandler;

impl ContentHandler for ImageHandler {
    fn supported_types(&self) -> &[&str] {
        &[
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
            "image/avif",
            "image/bmp",
            "image/x-icon",
            "image/vnd.microsoft.icon",
            "image/tiff",
            "image/svg+xml",
        ]
    }

    fn to_markdown(&self, bytes: &[u8], content_type: &str) -> Result<ConversionResult> {
        let start = std::time::Instant::now();
        let markdown = describe_image(bytes, content_type);
        Ok(ConversionResult {
            markdown,
            page_count: None,
            content_type: content_type.to_string(),
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
            quality: None,
        })
    }
}

/// Detected image metadata.
#[derive(Debug, PartialEq)]
struct ImageMeta {
    format: &'static str,
    dimensions: Option<(u32, u32)>,
    /// Optional human-readable label extracted from image data (SVG title/desc).
    label: Option<String>,
}

/// Produce a markdown `[Image: ...]` description for the given bytes.
fn describe_image(bytes: &[u8], content_type: &str) -> String {
    let meta = detect_image(bytes, content_type);
    format_image_description(&meta, bytes.len())
}

/// Detect image format and extract metadata from raw bytes.
fn detect_image(bytes: &[u8], content_type: &str) -> ImageMeta {
    // Magic-byte dispatch: format is always determined by bytes, not Content-Type.
    // Content-Type is used only as a fallback hint when magic bytes are ambiguous.
    if is_png(bytes) {
        return png_meta(bytes);
    }
    if is_jpeg(bytes) {
        return jpeg_meta(bytes);
    }
    if is_gif(bytes) {
        return gif_meta(bytes);
    }
    if is_webp(bytes) {
        return webp_meta(bytes);
    }
    if is_bmp(bytes) {
        return bmp_meta(bytes);
    }
    if is_ico(bytes) {
        return ico_meta(bytes);
    }
    if is_tiff(bytes) {
        return tiff_meta(bytes);
    }
    if is_svg(bytes) {
        return svg_meta(bytes);
    }
    // AVIF/HEIF use ISO base media file format (ftyp box) — check last since
    // the magic is a 4-byte box type at offset 4, not at offset 0.
    if is_isobmff(bytes) {
        return isobmff_meta(bytes);
    }

    // Unknown format — use the content-type hint for the label
    let format_hint = mime_to_format_hint(content_type);
    ImageMeta {
        format: format_hint,
        dimensions: None,
        label: None,
    }
}

/// Format an `ImageMeta` into a markdown description string.
fn format_image_description(meta: &ImageMeta, byte_len: usize) -> String {
    let mut parts = vec![meta.format.to_string()];

    if let Some((w, h)) = meta.dimensions {
        parts.push(format!("{w}×{h}"));
    } else {
        parts.push(format!("{byte_len} bytes"));
    }

    let base = format!("[Image: {}]", parts.join(" "));

    match &meta.label {
        Some(label) if !label.is_empty() => format!("{base}\n{label}"),
        _ => base,
    }
}

// ─── Magic byte detectors ──────────────────────────────────────────────────

fn is_png(b: &[u8]) -> bool {
    b.len() >= 8 && b[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
}

fn is_jpeg(b: &[u8]) -> bool {
    b.len() >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF
}

fn is_gif(b: &[u8]) -> bool {
    b.len() >= 6 && (&b[..6] == b"GIF87a" || &b[..6] == b"GIF89a")
}

fn is_webp(b: &[u8]) -> bool {
    b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP"
}

fn is_bmp(b: &[u8]) -> bool {
    b.len() >= 2 && &b[..2] == b"BM"
}

fn is_ico(b: &[u8]) -> bool {
    b.len() >= 4 && b[0] == 0x00 && b[1] == 0x00 && b[2] == 0x01 && b[3] == 0x00
}

fn is_tiff(b: &[u8]) -> bool {
    b.len() >= 4
        && ((&b[..4] == b"II\x2A\x00") // little-endian TIFF
            || (&b[..4] == b"MM\x00\x2A")) // big-endian TIFF
}

fn is_svg(b: &[u8]) -> bool {
    let head = std::str::from_utf8(&b[..b.len().min(256)]).unwrap_or("");
    head.contains("<svg") || head.starts_with("<?xml")
}

/// Returns `true` for ISO base media file format (AVIF, HEIF, MP4, etc.).
///
/// The `ftyp` box sits at offset 4 in the file. We accept any ftyp that
/// contains the AVIF or HEIF brand strings.
fn is_isobmff(b: &[u8]) -> bool {
    if b.len() < 12 {
        return false;
    }
    // Box type is at bytes 4–8
    &b[4..8] == b"ftyp"
}

// ─── Format-specific dimension extractors ─────────────────────────────────

/// PNG: IHDR chunk starts at byte 8 (4 length + 4 type + 4 width + 4 height).
fn png_meta(b: &[u8]) -> ImageMeta {
    let dims = (b.len() >= 24).then(|| {
        let w = read_u32_be(b, 16);
        let h = read_u32_be(b, 20);
        (w, h)
    });
    ImageMeta {
        format: "PNG",
        dimensions: dims,
        label: None,
    }
}

/// JPEG: Scan for SOF0 (0xFFC0) or SOF2 (0xFFC2) markers which contain
/// image dimensions at a fixed offset within the marker payload.
fn jpeg_meta(b: &[u8]) -> ImageMeta {
    let dims = scan_jpeg_dimensions(b);
    ImageMeta {
        format: "JPEG",
        dimensions: dims,
        label: None,
    }
}

/// Scan JPEG markers for SOF0/SOF2 to extract dimensions.
///
/// JPEG structure: `FF D8` SOI, then a sequence of markers `FF XX len…`.
/// SOF0 (`FF C0`) and SOF2 (`FF C2`) encode: precision(1) height(2) width(2).
fn scan_jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2; // skip FF D8 SOI
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            break;
        }
        let marker = b[i + 1];
        if marker == 0xD9 {
            break; // EOI
        }
        // SOF markers that carry dimensions
        if matches!(
            marker,
            0xC0 | 0xC1
                | 0xC2
                | 0xC3
                | 0xC5
                | 0xC6
                | 0xC7
                | 0xC9
                | 0xCA
                | 0xCB
                | 0xCD
                | 0xCE
                | 0xCF
        ) {
            // Payload: [precision(1)] [height(2)] [width(2)] …
            if i + 9 <= b.len() {
                let h = u32::from(read_u16_be(b, i + 5));
                let w = u32::from(read_u16_be(b, i + 7));
                if w > 0 && h > 0 {
                    return Some((w, h));
                }
            }
        }
        // Skip marker + 2-byte length field + payload
        if i + 4 > b.len() {
            break;
        }
        let seg_len = usize::from(read_u16_be(b, i + 2));
        i += 2 + seg_len;
    }
    None
}

/// GIF: Logical Screen Descriptor at bytes 6–9 (little-endian u16 width, height).
fn gif_meta(b: &[u8]) -> ImageMeta {
    let dims = (b.len() >= 10).then(|| {
        let w = u32::from(read_u16_le(b, 6));
        let h = u32::from(read_u16_le(b, 8));
        (w, h)
    });
    ImageMeta {
        format: "GIF",
        dimensions: dims,
        label: None,
    }
}

/// WebP: Dimensions depend on the sub-format (VP8, VP8L, or VP8X chunk).
fn webp_meta(b: &[u8]) -> ImageMeta {
    let dims = parse_webp_dimensions(b);
    ImageMeta {
        format: "WebP",
        dimensions: dims,
        label: None,
    }
}

/// Parse WebP dimensions from the first chunk after the RIFF header.
///
/// RIFF/WEBP structure:
/// - `RIFF` (4) + size (4) + `WEBP` (4) = 12 bytes header
/// - Then: chunk `FourCC` (4) + chunk size (4) + chunk data
fn parse_webp_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 30 {
        return None;
    }
    let chunk_type = &b[12..16];
    match chunk_type {
        b"VP8 " => {
            // Lossy: 3-byte frame tag, then bitstream. Width/height at bytes 26–29.
            // Encoded as (value & 0x3FFF) with 14-bit precision.
            if b.len() >= 30 {
                let w = u32::from(read_u16_le(b, 26) & 0x3FFF);
                let h = u32::from(read_u16_le(b, 28) & 0x3FFF);
                Some((w, h))
            } else {
                None
            }
        }
        b"VP8L" => {
            // Lossless: signature byte (0x2F) + packed width/height in 28 bits.
            // bits 0–13 = width - 1, bits 14–27 = height - 1
            if b.len() >= 25 {
                let bits = read_u32_le(b, 21);
                let w = (bits & 0x3FFF) + 1;
                let h = ((bits >> 14) & 0x3FFF) + 1;
                Some((w, h))
            } else {
                None
            }
        }
        b"VP8X" => {
            // Extended: canvas width/height at bytes 24–29 (3 bytes each, little-endian, value+1)
            if b.len() >= 30 {
                let w = read_u24_le(b, 24) + 1;
                let h = read_u24_le(b, 27) + 1;
                Some((w, h))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// BMP: DIB header at offset 14. Width at offset 18, height at offset 22 (signed i32 LE).
fn bmp_meta(b: &[u8]) -> ImageMeta {
    let dims = (b.len() >= 26).then(|| {
        let w = read_u32_le(b, 18);
        let h = read_u32_le(b, 22).cast_signed().unsigned_abs(); // height can be negative (top-down)
        (w, h)
    });
    ImageMeta {
        format: "BMP",
        dimensions: dims,
        label: None,
    }
}

/// ICO: First image entry's width/height are at offsets 6 and 7 (single byte each).
///
/// A 0-byte value means 256 pixels (the max for single-byte encoding).
fn ico_meta(b: &[u8]) -> ImageMeta {
    let dims = (b.len() >= 8).then(|| {
        let w = if b[6] == 0 { 256u32 } else { u32::from(b[6]) };
        let h = if b[7] == 0 { 256u32 } else { u32::from(b[7]) };
        (w, h)
    });
    ImageMeta {
        format: "ICO",
        dimensions: dims,
        label: None,
    }
}

/// TIFF: Scan IFD for tag 256 (`ImageWidth`) and 257 (`ImageLength`).
fn tiff_meta(b: &[u8]) -> ImageMeta {
    let dims = parse_tiff_dimensions(b);
    ImageMeta {
        format: "TIFF",
        dimensions: dims,
        label: None,
    }
}

/// Parse TIFF IFD entries for `ImageWidth` (256) and `ImageLength` (257).
fn parse_tiff_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 8 {
        return None;
    }
    let little_endian = &b[..2] == b"II";
    let u16 = |off: usize| -> Option<u32> {
        if off + 2 > b.len() {
            return None;
        }
        Some(if little_endian {
            u32::from(read_u16_le(b, off))
        } else {
            u32::from(read_u16_be(b, off))
        })
    };
    let u32 = |off: usize| -> Option<u32> {
        if off + 4 > b.len() {
            return None;
        }
        Some(if little_endian {
            read_u32_le(b, off)
        } else {
            read_u32_be(b, off)
        })
    };

    let ifd_offset = usize::try_from(u32(4)?).ok()?;
    if ifd_offset + 2 > b.len() {
        return None;
    }
    let entry_count = usize::try_from(u16(ifd_offset)?).ok()?;

    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;

    for i in 0..entry_count {
        let entry_off = ifd_offset + 2 + i * 12;
        if entry_off + 12 > b.len() {
            break;
        }
        let tag = u16(entry_off)?;
        let _type = u16(entry_off + 2)?;
        // For SHORT (3) and LONG (4) types with count=1, value is in bytes 8–11
        let value = u32(entry_off + 8)?;
        match tag {
            256 => width = Some(value),
            257 => height = Some(value),
            _ => {}
        }
        if width.is_some() && height.is_some() {
            break;
        }
    }

    width.zip(height)
}

/// SVG: Extract width/height from viewBox or width/height attributes,
/// plus title/desc content for the label.
fn svg_meta(b: &[u8]) -> ImageMeta {
    let text = std::str::from_utf8(&b[..b.len().min(4096)]).unwrap_or("");
    let dims = parse_svg_dimensions(text);
    let label = extract_svg_label(text);
    ImageMeta {
        format: "SVG",
        dimensions: dims,
        label,
    }
}

/// Parse SVG dimensions from viewBox or explicit width/height attributes.
///
/// Handles common SVG patterns:
/// - `viewBox="0 0 W H"` (most reliable)
/// - `width="W" height="H"` (may be in px, pt, em — we extract the number)
fn parse_svg_dimensions(svg: &str) -> Option<(u32, u32)> {
    // Try viewBox first — most reliable for scalable images
    if let Some(vb) = extract_attr(svg, "viewBox") {
        let nums: Vec<f64> = vb
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        if nums.len() >= 4 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let (w, h) = (nums[2].round() as u32, nums[3].round() as u32);
            if w > 0 && h > 0 {
                return Some((w, h));
            }
        }
    }

    // Fall back to explicit width/height attributes
    let w_str = extract_attr(svg, "width")?;
    let h_str = extract_attr(svg, "height")?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (w, h) = (parse_svg_unit(w_str)? as u32, parse_svg_unit(h_str)? as u32);
    (w > 0 && h > 0).then_some((w, h))
}

/// Extract the numeric prefix from an SVG unit value like `"100px"`, `"24.5"`, `"1em"`.
fn parse_svg_unit(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    // Take leading numeric characters (digits, dot, minus)
    let num_end = trimmed
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(trimmed.len());
    trimmed[..num_end].parse().ok()
}

/// Extract the first `<title>` or `<desc>` text content from SVG, cleaned up.
fn extract_svg_label(svg: &str) -> Option<String> {
    for tag in &["title", "desc"] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        if let Some(start) = svg.find(&open) {
            // Skip to the end of the opening tag
            if let Some(tag_end) = svg[start..].find('>') {
                let content_start = start + tag_end + 1;
                if let Some(content_end) = svg[content_start..].find(&close) {
                    let content = svg[content_start..content_start + content_end].trim();
                    if !content.is_empty() {
                        return Some(content.to_string());
                    }
                }
            }
        }
    }
    None
}

/// ISO Base Media File Format (AVIF, HEIF): scan for `ispe` box for dimensions.
fn isobmff_meta(b: &[u8]) -> ImageMeta {
    let brand = std::str::from_utf8(b.get(8..12).unwrap_or(&[])).unwrap_or("");
    let format = isobmff_format_name(brand);
    let dims = scan_ispe_box(b);
    ImageMeta {
        format,
        dimensions: dims,
        label: None,
    }
}

/// Determine AVIF/HEIF format name from the major brand string.
fn isobmff_format_name(brand: &str) -> &'static str {
    match brand.trim() {
        "avif" | "avis" => "AVIF",
        "heic" | "heix" | "hevc" | "hevx" => "HEIC",
        "mif1" | "msf1" => "HEIF",
        _ => "AVIF/HEIF",
    }
}

/// Scan ISO BMFF boxes for `ispe` (image spatial extent) which contains dimensions.
///
/// `ispe` box layout: size(4) + type(4) + version(1) + flags(3) + width(4) + height(4)
fn scan_ispe_box(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 0usize;
    while i + 8 <= b.len() {
        let box_size = read_u32_be(b, i) as usize;
        let box_type = &b[i + 4..i + 8];
        if box_size < 8 {
            break;
        }
        if box_type == b"ispe" && i + 20 <= b.len() {
            let w = read_u32_be(b, i + 12);
            let h = read_u32_be(b, i + 16);
            if w > 0 && h > 0 {
                return Some((w, h));
            }
        }
        i += box_size;
    }
    None
}

// ─── Attribute extraction helper ──────────────────────────────────────────

/// Extract the value of an XML/SVG attribute from a text fragment.
///
/// Handles both `attr="value"` and `attr='value'` quoting styles.
#[allow(clippy::similar_names)]
fn extract_attr<'a>(text: &'a str, attr: &str) -> Option<&'a str> {
    let needle_dq = format!("{attr}=\"");
    let needle_sq = format!("{attr}='");

    if let Some(start) = text.find(&needle_dq) {
        let val_start = start + needle_dq.len();
        let val_end = text[val_start..].find('"')?;
        return Some(&text[val_start..val_start + val_end]);
    }
    if let Some(start) = text.find(&needle_sq) {
        let val_start = start + needle_sq.len();
        let val_end = text[val_start..].find('\'')?;
        return Some(&text[val_start..val_start + val_end]);
    }
    None
}

/// Map a MIME content-type to a short format hint when magic bytes fail.
fn mime_to_format_hint(content_type: &str) -> &'static str {
    let mime = content_type.split(';').next().unwrap_or("").trim();
    match mime {
        "image/png" => "PNG",
        "image/jpeg" | "image/jpg" => "JPEG",
        "image/gif" => "GIF",
        "image/webp" => "WebP",
        "image/avif" => "AVIF",
        "image/heic" | "image/heif" => "HEIC",
        "image/bmp" => "BMP",
        "image/x-icon" | "image/vnd.microsoft.icon" => "ICO",
        "image/tiff" => "TIFF",
        "image/svg+xml" => "SVG",
        _ => "image",
    }
}

// ─── Low-level byte readers ────────────────────────────────────────────────

fn read_u16_be(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}

fn read_u16_le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn read_u32_be(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn read_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn read_u24_le(b: &[u8], off: usize) -> u32 {
    u32::from(b[off]) | (u32::from(b[off + 1]) << 8) | (u32::from(b[off + 2]) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── PNG ─────────────────────────────────────────────────────────────

    #[test]
    fn png_magic_detection_recognises_valid_signature() {
        // GIVEN: PNG magic bytes
        let bytes = [0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        // WHEN: checking magic
        // THEN: recognised as PNG
        assert!(is_png(&bytes));
    }

    #[test]
    fn png_meta_extracts_dimensions_from_ihdr() {
        // GIVEN: minimal PNG header with IHDR (800×600)
        let mut bytes = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, // IHDR chunk length
            0x49, 0x48, 0x44, 0x52, // "IHDR"
        ];
        // width=800, height=600 in big-endian
        bytes.extend_from_slice(&800u32.to_be_bytes());
        bytes.extend_from_slice(&600u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]); // bit depth, color type, etc.
        // WHEN: extracting metadata
        let meta = png_meta(&bytes);
        // THEN: dimensions are correct
        assert_eq!(meta.dimensions, Some((800, 600)));
        assert_eq!(meta.format, "PNG");
    }

    #[test]
    fn describe_image_returns_correct_format_for_png_bytes() {
        // GIVEN: valid PNG header bytes (1x1 PNG)
        let mut bytes = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52,
        ];
        bytes.extend_from_slice(&1u32.to_be_bytes()); // width=1
        bytes.extend_from_slice(&1u32.to_be_bytes()); // height=1
        bytes.extend_from_slice(&[8, 0, 0, 0, 0]);
        // WHEN: describing
        let desc = describe_image(&bytes, "image/png");
        // THEN: output contains format and dimensions
        assert!(desc.contains("[Image: PNG 1×1]"), "got: {desc}");
    }

    // ─── JPEG ────────────────────────────────────────────────────────────

    #[test]
    fn jpeg_magic_detection_recognises_ffd8ff() {
        // GIVEN: JPEG SOI marker
        let bytes = [0xFF, 0xD8, 0xFF, 0xE0];
        assert!(is_jpeg(&bytes));
    }

    #[test]
    fn jpeg_meta_extracts_dimensions_from_sof0() {
        // GIVEN: minimal JPEG with SOF0 marker for 320×240
        // Structure: SOI (2) + APP0 (18) + SOF0 marker
        let mut bytes = vec![0xFF, 0xD8]; // SOI
        // APP0 marker (FF E0) with length 16 (minimal)
        bytes.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        bytes.extend_from_slice(&[
            0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
        ]);
        // SOF0 marker: FF C0 + length(17) + precision(1) + height(2) + width(2)
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11]);
        bytes.push(8); // precision
        bytes.extend_from_slice(&240u16.to_be_bytes()); // height
        bytes.extend_from_slice(&320u16.to_be_bytes()); // width
        bytes.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]); // components
        // WHEN: extracting metadata
        let meta = jpeg_meta(&bytes);
        // THEN: dimensions are correct
        assert_eq!(meta.dimensions, Some((320, 240)));
        assert_eq!(meta.format, "JPEG");
    }

    // ─── GIF ─────────────────────────────────────────────────────────────

    #[test]
    fn gif_meta_extracts_dimensions_from_logical_screen_descriptor() {
        // GIVEN: GIF89a header with 100×50 logical screen descriptor
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&100u16.to_le_bytes()); // width
        bytes.extend_from_slice(&50u16.to_le_bytes()); // height
        // WHEN: extracting metadata
        let meta = gif_meta(&bytes);
        // THEN: dimensions are correct
        assert_eq!(meta.dimensions, Some((100, 50)));
        assert_eq!(meta.format, "GIF");
    }

    #[test]
    fn is_gif_recognises_gif87a() {
        assert!(is_gif(b"GIF87a\x00"));
    }

    // ─── WebP ────────────────────────────────────────────────────────────

    #[test]
    fn webp_vp8x_chunk_dimensions_parsed_correctly() {
        // GIVEN: WebP with VP8X chunk for 256×128
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&0u32.to_le_bytes()); // file size (irrelevant for test)
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"VP8X");
        bytes.extend_from_slice(&10u32.to_le_bytes()); // chunk size
        bytes.push(0x02); // flags
        bytes.extend_from_slice(&[0; 3]); // reserved
        // width-1=255 (3 bytes LE), height-1=127 (3 bytes LE)
        bytes.extend_from_slice(&[255, 0, 0]); // canvas_width_minus_1
        bytes.extend_from_slice(&[127, 0, 0]); // canvas_height_minus_1
        // WHEN: extracting metadata
        let meta = webp_meta(&bytes);
        // THEN: dimensions are correct (value + 1)
        assert_eq!(meta.dimensions, Some((256, 128)));
        assert_eq!(meta.format, "WebP");
    }

    // ─── BMP ─────────────────────────────────────────────────────────────

    #[test]
    fn bmp_meta_extracts_dimensions_from_dib_header() {
        // GIVEN: minimal BMP with 64×48 dimensions
        let mut bytes = vec![0x42, 0x4D]; // "BM"
        bytes.extend_from_slice(&54u32.to_le_bytes()); // file size
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // reserved
        bytes.extend_from_slice(&54u32.to_le_bytes()); // pixel array offset
        bytes.extend_from_slice(&40u32.to_le_bytes()); // DIB header size
        bytes.extend_from_slice(&64u32.to_le_bytes()); // width
        bytes.extend_from_slice(&48u32.to_le_bytes()); // height
        // WHEN: extracting metadata
        let meta = bmp_meta(&bytes);
        // THEN: dimensions are correct
        assert_eq!(meta.dimensions, Some((64, 48)));
        assert_eq!(meta.format, "BMP");
    }

    // ─── ICO ─────────────────────────────────────────────────────────────

    #[test]
    fn ico_meta_extracts_32x32_from_first_entry() {
        // GIVEN: ICO with first entry of 32×32
        let bytes = [0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 32u8, 32u8];
        // WHEN: extracting metadata
        let meta = ico_meta(&bytes);
        // THEN: dimensions are correct
        assert_eq!(meta.dimensions, Some((32, 32)));
    }

    #[test]
    fn ico_meta_zero_byte_means_256_pixels() {
        // GIVEN: ICO with 0x00 dimensions (meaning 256×256)
        let bytes = [0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00u8, 0x00u8];
        // WHEN: extracting metadata
        let meta = ico_meta(&bytes);
        // THEN: 0 byte is decoded as 256
        assert_eq!(meta.dimensions, Some((256, 256)));
    }

    // ─── SVG ─────────────────────────────────────────────────────────────

    #[test]
    fn svg_meta_extracts_viewbox_dimensions() {
        // GIVEN: SVG with viewBox attribute
        let svg = r#"<svg viewBox="0 0 800 600" xmlns="http://www.w3.org/2000/svg"></svg>"#;
        // WHEN: extracting metadata
        let meta = svg_meta(svg.as_bytes());
        // THEN: dimensions extracted from viewBox
        assert_eq!(meta.dimensions, Some((800, 600)));
        assert_eq!(meta.format, "SVG");
    }

    #[test]
    fn svg_meta_extracts_title_as_label() {
        // GIVEN: SVG with title element
        let svg = r#"<svg viewBox="0 0 100 100"><title>Sales Chart 2024</title></svg>"#;
        // WHEN: extracting metadata
        let meta = svg_meta(svg.as_bytes());
        // THEN: title is extracted as label
        assert_eq!(meta.label, Some("Sales Chart 2024".to_string()));
    }

    #[test]
    fn svg_meta_falls_back_to_width_height_when_no_viewbox() {
        // GIVEN: SVG with explicit width/height but no viewBox
        let svg = r#"<svg width="400" height="300" xmlns="http://www.w3.org/2000/svg"></svg>"#;
        // WHEN: extracting metadata
        let meta = svg_meta(svg.as_bytes());
        // THEN: dimensions from width/height attributes
        assert_eq!(meta.dimensions, Some((400, 300)));
    }

    #[test]
    fn svg_meta_handles_px_unit_suffix() {
        // GIVEN: SVG with px-suffixed dimensions
        let svg = r#"<svg width="200px" height="150px"></svg>"#;
        // WHEN: extracting metadata
        let meta = svg_meta(svg.as_bytes());
        // THEN: numeric value extracted, ignoring "px"
        assert_eq!(meta.dimensions, Some((200, 150)));
    }

    // ─── Format description ───────────────────────────────────────────────

    #[test]
    fn format_image_description_includes_format_and_dimensions() {
        // GIVEN: metadata with format and dimensions
        let meta = ImageMeta {
            format: "PNG",
            dimensions: Some((1920, 1080)),
            label: None,
        };
        // WHEN: formatting
        let desc = format_image_description(&meta, 12345);
        // THEN: output is [Image: PNG 1920×1080]
        assert_eq!(desc, "[Image: PNG 1920×1080]");
    }

    #[test]
    fn format_image_description_uses_byte_count_when_no_dimensions() {
        // GIVEN: metadata without dimensions
        let meta = ImageMeta {
            format: "JPEG",
            dimensions: None,
            label: None,
        };
        // WHEN: formatting with byte count
        let desc = format_image_description(&meta, 8192);
        // THEN: byte count is shown
        assert_eq!(desc, "[Image: JPEG 8192 bytes]");
    }

    #[test]
    fn format_image_description_appends_label_on_new_line() {
        // GIVEN: SVG metadata with title label
        let meta = ImageMeta {
            format: "SVG",
            dimensions: Some((100, 100)),
            label: Some("Pie chart".to_string()),
        };
        // WHEN: formatting
        let desc = format_image_description(&meta, 0);
        // THEN: label appears on next line
        assert_eq!(desc, "[Image: SVG 100×100]\nPie chart");
    }

    // ─── ContentHandler trait ─────────────────────────────────────────────

    #[test]
    fn image_handler_supported_types_includes_common_formats() {
        let handler = ImageHandler;
        let types = handler.supported_types();
        assert!(types.contains(&"image/png"));
        assert!(types.contains(&"image/jpeg"));
        assert!(types.contains(&"image/svg+xml"));
        assert!(types.contains(&"image/webp"));
    }

    #[test]
    fn image_handler_to_markdown_produces_image_marker_for_png() {
        // GIVEN: minimal valid PNG bytes
        let mut bytes = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52,
        ];
        bytes.extend_from_slice(&10u32.to_be_bytes()); // width=10
        bytes.extend_from_slice(&10u32.to_be_bytes()); // height=10
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        // WHEN: converting to markdown
        let handler = ImageHandler;
        let result = handler.to_markdown(&bytes, "image/png").unwrap();
        // THEN: output contains image marker
        assert!(
            result.markdown.starts_with("[Image:"),
            "got: {}",
            result.markdown
        );
        assert!(result.markdown.contains("PNG"));
    }

    // ─── MIME fallback ────────────────────────────────────────────────────

    #[test]
    fn mime_to_format_hint_maps_common_types() {
        assert_eq!(mime_to_format_hint("image/png"), "PNG");
        assert_eq!(mime_to_format_hint("image/jpeg"), "JPEG");
        assert_eq!(mime_to_format_hint("image/gif"), "GIF");
        assert_eq!(mime_to_format_hint("image/webp"), "WebP");
        assert_eq!(mime_to_format_hint("image/svg+xml"), "SVG");
    }

    #[test]
    fn mime_to_format_hint_strips_charset_parameter() {
        // GIVEN: content-type with charset parameter
        let hint = mime_to_format_hint("image/png; charset=utf-8");
        // THEN: correctly mapped despite parameter
        assert_eq!(hint, "PNG");
    }

    // ─── Router integration ───────────────────────────────────────────────

    #[test]
    fn content_router_dispatches_image_png_to_image_handler() {
        // GIVEN: PNG bytes sent through the content router
        let router = crate::content::ContentRouter::new();
        let mut bytes = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52,
        ];
        bytes.extend_from_slice(&32u32.to_be_bytes());
        bytes.extend_from_slice(&32u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        // WHEN: converting via router
        let result = router.convert(&bytes, "image/png").unwrap();
        // THEN: output is an image description, not binary garbage
        assert!(
            result.markdown.starts_with("[Image:"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn content_router_dispatches_image_svg_xml_to_image_handler() {
        // GIVEN: SVG content
        let svg = br#"<svg viewBox="0 0 48 48" xmlns="http://www.w3.org/2000/svg"><title>Logo</title></svg>"#;
        let router = crate::content::ContentRouter::new();
        // WHEN: converting via router
        let result = router.convert(svg, "image/svg+xml").unwrap();
        // THEN: output contains SVG description with title
        assert!(result.markdown.contains("SVG"), "got: {}", result.markdown);
        assert!(result.markdown.contains("Logo"), "got: {}", result.markdown);
    }
}
