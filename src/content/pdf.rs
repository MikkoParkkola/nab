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
use super::{ContentHandler, ConversionResult};

/// A positioned character extracted from a PDF page.
#[derive(Debug, Clone)]
pub struct PdfChar {
    pub ch: char,
    /// Left edge in PDF points (1pt = 1/72 inch).
    pub x: f32,
    /// Baseline Y position (bottom-up coordinate system).
    pub y: f32,
    pub width: f32,
    /// Font size approximation (character height).
    pub height: f32,
    /// Page index (0-based).
    pub page: usize,
}

/// A reconstructed text line from grouped characters.
#[derive(Debug, Clone)]
pub struct TextLine {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub chars: Vec<PdfChar>,
    pub page: usize,
}

/// Converts PDF responses to markdown with table detection.
pub struct PdfHandler;

impl PdfHandler {
    pub fn new() -> Self {
        Self
    }

    /// Extract all characters with their bounding rectangles from the document.
    #[allow(deprecated)] // PdfRect field access deprecated in 0.8.28, removed in 0.9.0
    fn extract_chars(bytes: &[u8]) -> Result<(Vec<PdfChar>, usize)> {
        let pdfium = Pdfium::default();
        let doc = pdfium
            .load_pdf_from_byte_slice(bytes, None)
            .context("Failed to parse PDF")?;
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
    fn build_line(chars: &[PdfChar]) -> TextLine {
        let mut text = String::new();
        let avg_char_width =
            chars.iter().map(|c| c.width).sum::<f32>() / chars.len() as f32;
        let space_threshold = avg_char_width * 0.3;

        for (i, ch) in chars.iter().enumerate() {
            if i > 0 {
                let gap = ch.x - (chars[i - 1].x + chars[i - 1].width);
                if gap > space_threshold {
                    text.push(' ');
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

        let (chars, page_count) = Self::extract_chars(bytes)?;

        // Handle scanned PDFs (images without text layer)
        if chars.is_empty() && page_count > 0 {
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

        let lines = Self::reconstruct_lines(&chars);
        let tables = detect_tables(&lines);
        let markdown = Self::render_markdown(&lines, &tables);

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
