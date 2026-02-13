//! Shared types for content handlers.
//!
//! Contains character and line types used across PDF extraction
//! and table detection. Extracted to avoid tight coupling between
//! `pdf.rs` and `table.rs`.

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
