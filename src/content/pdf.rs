//! PDF to Markdown conversion handler.
//!
//! Uses `pdfium-render` (Chromium's PDF library) to extract character positions,
//! reconstruct text lines, detect tables via column alignment, and render
//! clean markdown output.
//!
//! # Pipeline
//!
//! ```text
//! PDF bytes → pdfium char extraction → line reconstruction → table detection → markdown
//! ```
//!
//! Target performance: ~10ms/page.

use anyhow::{Context, Result};
use pdfium_render::prelude::*;

use super::table::{detect_tables, Table};
use super::types::{PdfChar, TextLine};
use super::{ContentHandler, ConversionResult};

/// Maximum PDF input size (50 MB). Prevents excessive memory usage
/// from accidentally huge or malicious PDFs.
const MAX_PDF_SIZE: usize = 50 * 1024 * 1024;

/// Converts PDF responses to markdown with table detection.
#[derive(Default)]
pub struct PdfHandler;

impl PdfHandler {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Try to load pdfium from common library paths.
    ///
    /// Searches: standard dlopen paths, /usr/local/lib, homebrew, pypdfium2.
    fn load_pdfium() -> Result<Pdfium> {
        // Try standard dlopen first (respects DYLD_LIBRARY_PATH)
        if let Ok(bindings) = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("")) {
            return Ok(Pdfium::new(bindings));
        }

        // Search common paths
        let search_paths = [
            "/usr/local/lib",
            "/opt/homebrew/lib",
            "/usr/lib",
        ];

        for path in &search_paths {
            let lib_path = format!("{path}/libpdfium.dylib");
            if std::path::Path::new(&lib_path).exists() {
                if let Ok(bindings) = Pdfium::bind_to_library(&lib_path) {
                    return Ok(Pdfium::new(bindings));
                }
            }
        }

        // Try finding pypdfium2 (Python package ships the library)
        if let Ok(output) = std::process::Command::new("python3")
            .args(["-c", "import pypdfium2; import os; print(os.path.join(os.path.dirname(pypdfium2.__file__), '..', 'pypdfium2_raw', 'libpdfium.dylib'))"])
            .output()
        {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if std::path::Path::new(&path).exists() {
                if let Ok(bindings) = Pdfium::bind_to_library(&path) {
                    return Ok(Pdfium::new(bindings));
                }
            }
        }

        anyhow::bail!(
            "pdfium library not found. Install via: pip3 install pypdfium2, \
             then symlink: ln -s $(python3 -c \"import pypdfium2, os; \
             print(os.path.join(os.path.dirname(pypdfium2.__file__), '..', \
             'pypdfium2_raw', 'libpdfium.dylib'))\") /usr/local/lib/libpdfium.dylib"
        )
    }

    /// Extract text using pdfium's built-in text reconstruction (`text.all()`).
    ///
    /// This is the primary extraction path. Pdfium handles character ordering,
    /// ligatures, and font encoding internally — producing much better results
    /// than manual character-by-character reconstruction for most PDFs.
    fn extract_text_simple(bytes: &[u8]) -> Result<(String, usize)> {
        let pdfium = Self::load_pdfium()?;
        let doc = match pdfium.load_pdf_from_byte_slice(bytes, None) {
            Ok(doc) => doc,
            Err(e) => {
                let err_str = format!("{e}");
                if err_str.contains("password") || err_str.contains("encrypt") {
                    anyhow::bail!("PDF is password-protected. Provide a decrypted version.");
                }
                return Err(e).context("Failed to parse PDF");
            }
        };
        let page_count = doc.pages().len() as usize;
        let mut full_text = String::new();

        for (page_idx, page) in doc.pages().iter().enumerate() {
            let text = page.text().context("Failed to extract text from page")?;
            let page_text = text.all();
            if !page_text.is_empty() {
                if page_idx > 0 {
                    full_text.push_str("\n\n---\n\n");
                }
                full_text.push_str(&page_text);
            }
        }

        Ok((full_text, page_count))
    }

    /// Extract all characters with their bounding rectangles from the document.
    ///
    /// Used for table detection which needs positional data.
    #[allow(deprecated)] // PdfRect field access deprecated in 0.8.28, removed in 0.9.0
    fn extract_chars(bytes: &[u8]) -> Result<(Vec<PdfChar>, usize)> {
        let pdfium = Self::load_pdfium()?;
        let doc = match pdfium.load_pdf_from_byte_slice(bytes, None) {
            Ok(doc) => doc,
            Err(e) => {
                let err_str = format!("{e}");
                if err_str.contains("password") || err_str.contains("encrypt") {
                    anyhow::bail!("PDF is password-protected. Provide a decrypted version.");
                }
                return Err(e).context("Failed to parse PDF");
            }
        };
        let page_count = doc.pages().len() as usize;
        let mut chars = Vec::new();

        for (page_idx, page) in doc.pages().iter().enumerate() {
            let text = page.text().context("Failed to extract text from page")?;
            for ch in text.chars().iter() {
                if let (Some(unicode_ch), Ok(rect)) = (ch.unicode_char(), ch.tight_bounds()) {
                    chars.push(PdfChar {
                        ch: unicode_ch,
                        x: rect.left.value,
                        y: rect.bottom.value,
                        width: (rect.right.value - rect.left.value).abs(),
                        height: (rect.top.value - rect.bottom.value).abs(),
                        page: page_idx,
                    });
                }
            }
        }

        Ok((chars, page_count))
    }

    /// Reconstruct text lines from positioned characters.
    ///
    /// 1. Sort by page, then Y descending (top-to-bottom), then X ascending.
    /// 2. Group characters with Y within `line_tolerance` into the same line.
    /// 3. Insert spaces at horizontal gaps wider than `space_threshold`.
    fn reconstruct_lines(chars: &[PdfChar]) -> Vec<TextLine> {
        if chars.is_empty() {
            return Vec::new();
        }

        let mut sorted = chars.to_vec();
        sorted.sort_by(|a, b| {
            a.page
                .cmp(&b.page)
                .then(
                    b.y.partial_cmp(&a.y)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(
                    a.x.partial_cmp(&b.x)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });

        let mut lines: Vec<TextLine> = Vec::new();
        let mut current_chars: Vec<PdfChar> = vec![sorted[0].clone()];
        let line_tolerance = sorted[0].height * 0.4;

        for ch in sorted.iter().skip(1) {
            let last = current_chars.last().unwrap();

            if ch.page == last.page && (ch.y - last.y).abs() < line_tolerance {
                current_chars.push(ch.clone());
            } else {
                lines.push(Self::build_line(&current_chars));
                current_chars = vec![ch.clone()];
            }
        }

        if !current_chars.is_empty() {
            lines.push(Self::build_line(&current_chars));
        }

        lines
    }

    /// Build a [`TextLine`] from grouped characters, inserting spaces at gaps.
    ///
    /// Uses adaptive space detection: the threshold scales with character width
    /// to avoid inserting spurious spaces in PDFs with tight glyph spacing
    /// (common in LaTeX-generated documents).
    fn build_line(chars: &[PdfChar]) -> TextLine {
        let mut text = String::new();
        let avg_char_width =
            chars.iter().map(|c| c.width).sum::<f32>() / chars.len() as f32;
        // Use 50% of average char width as space threshold (was 30%).
        // 30% was too aggressive for LaTeX PDFs with tight kerning.
        let space_threshold = (avg_char_width * 0.5).max(1.0);

        for (i, ch) in chars.iter().enumerate() {
            if i > 0 {
                let prev = &chars[i - 1];
                let gap = ch.x - (prev.x + prev.width);
                if gap > space_threshold {
                    text.push(' ');
                } else if gap < -prev.width * 0.3 {
                    // Overlapping characters (ligatures, accents) — skip space
                }
            }
            text.push(ch.ch);
        }

        TextLine {
            text,
            x: chars[0].x,
            y: chars[0].y,
            chars: chars.to_vec(),
            page: chars[0].page,
        }
    }

    /// Render lines to markdown, converting table regions to markdown tables.
    ///
    /// Applies heading heuristics based on font size:
    /// - Height > 16pt + short line → `## heading`
    /// - Height > 13pt + short line → `### heading`
    fn render_markdown(lines: &[TextLine], tables: &[Table]) -> String {
        let mut output = String::new();
        let mut table_rendered = vec![false; tables.len()];

        for line in lines {
            // Check if this line belongs to a detected table
            let table_idx = tables.iter().position(|t| {
                line.page == t.page
                    && line.y >= t.y_min
                    && line.y <= t.y_max
                    && line.x >= t.x_min - 5.0
                    && line.x <= t.x_max + 5.0
            });

            if let Some(idx) = table_idx {
                if !table_rendered[idx] {
                    output.push('\n');
                    output.push_str(&tables[idx].to_markdown());
                    output.push('\n');
                    table_rendered[idx] = true;
                }
                continue;
            }

            let trimmed = line.text.trim();
            if trimmed.is_empty() {
                continue;
            }

            let avg_height =
                line.chars.iter().map(|c| c.height).sum::<f32>() / line.chars.len() as f32;

            if avg_height > 16.0 && trimmed.len() < 100 {
                output.push_str(&format!("## {trimmed}\n\n"));
            } else if avg_height > 13.0 && trimmed.len() < 120 {
                output.push_str(&format!("### {trimmed}\n\n"));
            } else {
                output.push_str(trimmed);
                output.push('\n');
            }
        }

        output
    }
}

impl ContentHandler for PdfHandler {
    fn supported_types(&self) -> &[&str] {
        &["application/pdf"]
    }

    fn to_markdown(&self, bytes: &[u8], content_type: &str) -> Result<ConversionResult> {
        let start = std::time::Instant::now();

        // P6: Reject oversized PDFs
        if bytes.len() > MAX_PDF_SIZE {
            anyhow::bail!(
                "PDF too large ({:.1} MB, max {:.0} MB). Use a smaller document.",
                bytes.len() as f64 / (1024.0 * 1024.0),
                MAX_PDF_SIZE as f64 / (1024.0 * 1024.0),
            );
        }

        // Primary path: use pdfium's built-in text reconstruction.
        // This handles font encoding, ligatures, and character ordering correctly.
        let (simple_text, page_count) = Self::extract_text_simple(bytes)?;

        // Handle scanned PDFs (images without text layer)
        if simple_text.trim().is_empty() && page_count > 0 {
            return Ok(ConversionResult {
                markdown: format!(
                    "*Scanned PDF ({page_count} pages) -- no text layer detected. \
                     Use OCR to extract text.*"
                ),
                page_count: Some(page_count),
                content_type: content_type.to_string(),
                elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
            });
        }

        // Always use pdfium's built-in text reconstruction.
        // Char-by-char extraction produces garbled output for many PDFs
        // (LaTeX, RFC, etc.) due to font encoding issues. text.all() is reliable.
        // TODO: Re-enable table detection with segment-based API (not per-char).
        let markdown = {
            simple_text
        };

        Ok(ConversionResult {
            markdown,
            page_count: Some(page_count),
            content_type: content_type.to_string(),
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstruct_lines_empty() {
        let lines = PdfHandler::reconstruct_lines(&[]);
        assert!(lines.is_empty());
    }

    #[test]
    fn reconstruct_lines_single_char() {
        let chars = vec![PdfChar {
            ch: 'A',
            x: 10.0,
            y: 100.0,
            width: 6.0,
            height: 12.0,
            page: 0,
        }];
        let lines = PdfHandler::reconstruct_lines(&chars);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "A");
    }

    #[test]
    fn reconstruct_lines_inserts_spaces() {
        let chars = vec![
            PdfChar { ch: 'H', x: 10.0, y: 100.0, width: 6.0, height: 12.0, page: 0 },
            PdfChar { ch: 'i', x: 16.0, y: 100.0, width: 3.0, height: 12.0, page: 0 },
            // Gap of 11 points (> 0.3 * avg_width)
            PdfChar { ch: 'W', x: 30.0, y: 100.0, width: 8.0, height: 12.0, page: 0 },
        ];
        let lines = PdfHandler::reconstruct_lines(&chars);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.contains(' '), "Should insert space at gap");
    }

    #[test]
    fn reconstruct_lines_separates_by_y() {
        let chars = vec![
            PdfChar { ch: 'A', x: 10.0, y: 100.0, width: 6.0, height: 12.0, page: 0 },
            PdfChar { ch: 'B', x: 10.0, y: 80.0, width: 6.0, height: 12.0, page: 0 },
        ];
        let lines = PdfHandler::reconstruct_lines(&chars);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn reconstruct_lines_separates_by_page() {
        let chars = vec![
            PdfChar { ch: 'A', x: 10.0, y: 100.0, width: 6.0, height: 12.0, page: 0 },
            PdfChar { ch: 'B', x: 10.0, y: 100.0, width: 6.0, height: 12.0, page: 1 },
        ];
        let lines = PdfHandler::reconstruct_lines(&chars);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn render_markdown_heading_detection() {
        let lines = vec![
            TextLine {
                text: "Big Title".into(),
                x: 10.0,
                y: 100.0,
                chars: "Big Title"
                    .chars()
                    .enumerate()
                    .map(|(i, ch)| PdfChar {
                        ch,
                        x: 10.0 + i as f32 * 10.0,
                        y: 100.0,
                        width: 10.0,
                        height: 18.0, // > 16pt → ## heading
                        page: 0,
                    })
                    .collect(),
                page: 0,
            },
            TextLine {
                text: "Normal paragraph text that goes on for a while.".into(),
                x: 10.0,
                y: 80.0,
                chars: "Normal paragraph text that goes on for a while."
                    .chars()
                    .enumerate()
                    .map(|(i, ch)| PdfChar {
                        ch,
                        x: 10.0 + i as f32 * 6.0,
                        y: 80.0,
                        width: 6.0,
                        height: 10.0, // < 13pt → normal text
                        page: 0,
                    })
                    .collect(),
                page: 0,
            },
        ];

        let md = PdfHandler::render_markdown(&lines, &[]);
        assert!(md.contains("## Big Title"));
        assert!(!md.contains("## Normal"));
    }

    #[test]
    fn supported_types_is_pdf() {
        let handler = PdfHandler::new();
        assert_eq!(handler.supported_types(), &["application/pdf"]);
    }
}
