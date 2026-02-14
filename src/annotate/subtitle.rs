//! Subtitle generation from transcripts
//!
//! Supports SRT and ASS formats with styling options.

#![allow(dead_code)] // VTT format support reserved for future

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Write as FmtWrite;
use std::path::Path;
use tokio::fs;
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Subtitle format type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubtitleFormat {
    /// `SubRip` format (.srt) - simple, widely compatible
    #[default]
    Srt,
    /// Advanced `SubStation` Alpha (.ass) - rich styling support
    Ass,
    /// `WebVTT` format (.vtt) - web standard
    Vtt,
}

impl SubtitleFormat {
    /// Get file extension for this format
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Ass => "ass",
            Self::Vtt => "vtt",
        }
    }
}

/// A single subtitle entry with timing and text
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleEntry {
    /// Start time in milliseconds
    pub start_ms: u64,
    /// End time in milliseconds
    pub end_ms: u64,
    /// Subtitle text (may contain newlines)
    pub text: String,
    /// Optional speaker label
    pub speaker: Option<String>,
    /// Optional style name (for ASS format)
    pub style: Option<String>,
}

impl SubtitleEntry {
    /// Create a new subtitle entry
    #[must_use]
    pub fn new(start_ms: u64, end_ms: u64, text: impl Into<String>) -> Self {
        Self {
            start_ms,
            end_ms,
            text: text.into(),
            speaker: None,
            style: None,
        }
    }

    /// Set speaker label
    #[must_use]
    pub fn with_speaker(mut self, speaker: impl Into<String>) -> Self {
        self.speaker = Some(speaker.into());
        self
    }

    /// Set style name
    #[must_use]
    pub fn with_style(mut self, style: impl Into<String>) -> Self {
        self.style = Some(style.into());
        self
    }

    /// Format time as SRT timestamp (HH:MM:SS,mmm)
    fn format_srt_time(ms: u64) -> String {
        let hours = ms / 3_600_000;
        let minutes = (ms % 3_600_000) / 60_000;
        let seconds = (ms % 60_000) / 1000;
        let millis = ms % 1000;
        format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
    }

    /// Format time as ASS timestamp (H:MM:SS.cc)
    fn format_ass_time(ms: u64) -> String {
        let hours = ms / 3_600_000;
        let minutes = (ms % 3_600_000) / 60_000;
        let seconds = (ms % 60_000) / 1000;
        let centis = (ms % 1000) / 10;
        format!("{hours}:{minutes:02}:{seconds:02}.{centis:02}")
    }

    /// Format time as VTT timestamp (HH:MM:SS.mmm)
    fn format_vtt_time(ms: u64) -> String {
        let hours = ms / 3_600_000;
        let minutes = (ms % 3_600_000) / 60_000;
        let seconds = (ms % 60_000) / 1000;
        let millis = ms % 1000;
        format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
    }
}

/// Style configuration for ASS subtitles
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleStyle {
    /// Style name
    pub name: String,
    /// Font name
    pub font_name: String,
    /// Font size
    pub font_size: u32,
    /// Primary color (AABBGGRR format for ASS)
    pub primary_color: String,
    /// Outline color
    pub outline_color: String,
    /// Background/shadow color
    pub back_color: String,
    /// Bold (0 or 1)
    pub bold: bool,
    /// Italic (0 or 1)
    pub italic: bool,
    /// Outline width
    pub outline: f32,
    /// Shadow depth
    pub shadow: f32,
    /// Alignment (numpad style: 1-9)
    pub alignment: u8,
    /// Margin from left edge
    pub margin_l: u32,
    /// Margin from right edge
    pub margin_r: u32,
    /// Margin from vertical edge
    pub margin_v: u32,
}

impl Default for SubtitleStyle {
    fn default() -> Self {
        Self {
            name: "Default".to_string(),
            font_name: "Arial".to_string(),
            font_size: 48,
            primary_color: "&H00FFFFFF".to_string(), // White
            outline_color: "&H00000000".to_string(), // Black
            back_color: "&H80000000".to_string(),    // Semi-transparent black
            bold: false,
            italic: false,
            outline: 2.0,
            shadow: 1.0,
            alignment: 2, // Bottom center
            margin_l: 20,
            margin_r: 20,
            margin_v: 20,
        }
    }
}

impl SubtitleStyle {
    /// Create a style for speaker labels (top-left positioning)
    #[must_use]
    pub fn speaker_label() -> Self {
        Self {
            name: "Speaker".to_string(),
            font_name: "Arial".to_string(),
            font_size: 32,
            primary_color: "&H0000FFFF".to_string(), // Yellow
            outline_color: "&H00000000".to_string(),
            back_color: "&H80000000".to_string(),
            bold: true,
            italic: false,
            outline: 2.0,
            shadow: 1.0,
            alignment: 7, // Top left
            margin_l: 20,
            margin_r: 20,
            margin_v: 20,
        }
    }

    /// Create a style for analysis overlay (top-right positioning)
    #[must_use]
    pub fn analysis_overlay() -> Self {
        Self {
            name: "Analysis".to_string(),
            font_name: "Consolas".to_string(),
            font_size: 24,
            primary_color: "&H0080FF80".to_string(), // Light green
            outline_color: "&H00000000".to_string(),
            back_color: "&HC0000000".to_string(), // More opaque background
            bold: false,
            italic: false,
            outline: 1.0,
            shadow: 0.0,
            alignment: 9, // Top right
            margin_l: 20,
            margin_r: 20,
            margin_v: 20,
        }
    }

    /// Format as ASS style line
    fn to_ass_line(&self) -> String {
        format!(
            "Style: {},{},{},{},{},{},{},0,0,{},{},{},{},{},{},{},{},0",
            self.name,
            self.font_name,
            self.font_size,
            self.primary_color,
            "&H000000FF", // Secondary color (karaoke)
            self.outline_color,
            self.back_color,
            if self.bold { -1 } else { 0 },
            if self.italic { -1 } else { 0 },
            self.outline,
            self.shadow,
            self.alignment,
            self.margin_l,
            self.margin_r,
            self.margin_v
        )
    }
}

/// Trait for subtitle generators
pub trait SubtitleGenerator: Send + Sync {
    /// Get the format this generator produces
    fn format(&self) -> SubtitleFormat;

    /// Generate subtitle content from entries
    fn generate(&self, entries: &[SubtitleEntry]) -> Result<String>;

    /// Write subtitles to a file
    fn write_to_file<'a>(
        &'a self,
        entries: &'a [SubtitleEntry],
        path: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;

    /// Write subtitles to an async writer
    fn write_to<'a, W: AsyncWrite + Unpin + Send + 'a>(
        &'a self,
        entries: &'a [SubtitleEntry],
        writer: &'a mut W,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;
}

/// SRT subtitle generator
#[derive(Debug, Clone, Default)]
pub struct SrtGenerator {
    /// Include speaker labels in subtitle text
    pub include_speaker: bool,
}

impl SrtGenerator {
    /// Create a new SRT generator
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable speaker label inclusion
    #[must_use]
    pub fn with_speaker_labels(mut self) -> Self {
        self.include_speaker = true;
        self
    }
}

impl SubtitleGenerator for SrtGenerator {
    fn format(&self) -> SubtitleFormat {
        SubtitleFormat::Srt
    }

    fn generate(&self, entries: &[SubtitleEntry]) -> Result<String> {
        let mut output = String::new();

        for (i, entry) in entries.iter().enumerate() {
            // Sequence number (1-indexed)
            writeln!(output, "{}", i + 1)?;

            // Timestamps
            writeln!(
                output,
                "{} --> {}",
                SubtitleEntry::format_srt_time(entry.start_ms),
                SubtitleEntry::format_srt_time(entry.end_ms)
            )?;

            // Text (with optional speaker prefix)
            if self.include_speaker {
                if let Some(ref speaker) = entry.speaker {
                    writeln!(output, "[{speaker}] {}", entry.text)?;
                } else {
                    writeln!(output, "{}", entry.text)?;
                }
            } else {
                writeln!(output, "{}", entry.text)?;
            }

            // Blank line separator
            writeln!(output)?;
        }

        Ok(output)
    }

    fn write_to_file<'a>(
        &'a self,
        entries: &'a [SubtitleEntry],
        path: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let content = self.generate(entries)?;
            fs::write(path, content).await?;
            Ok(())
        })
    }

    fn write_to<'a, W: AsyncWrite + Unpin + Send + 'a>(
        &'a self,
        entries: &'a [SubtitleEntry],
        writer: &'a mut W,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let content = self.generate(entries)?;
            writer.write_all(content.as_bytes()).await?;
            Ok(())
        })
    }
}

/// ASS subtitle generator with rich styling support
#[derive(Debug, Clone)]
pub struct AssGenerator {
    /// Video resolution (width)
    pub play_res_x: u32,
    /// Video resolution (height)
    pub play_res_y: u32,
    /// Styles to include in the file
    pub styles: Vec<SubtitleStyle>,
    /// Script title
    pub title: String,
}

impl Default for AssGenerator {
    fn default() -> Self {
        Self {
            play_res_x: 1920,
            play_res_y: 1080,
            styles: vec![
                SubtitleStyle::default(),
                SubtitleStyle::speaker_label(),
                SubtitleStyle::analysis_overlay(),
            ],
            title: "nab annotation".to_string(),
        }
    }
}

impl AssGenerator {
    /// Create a new ASS generator
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set video resolution
    #[must_use]
    pub fn with_resolution(mut self, width: u32, height: u32) -> Self {
        self.play_res_x = width;
        self.play_res_y = height;
        self
    }

    /// Add a custom style
    #[must_use]
    pub fn with_style(mut self, style: SubtitleStyle) -> Self {
        self.styles.push(style);
        self
    }

    /// Set script title
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Generate ASS header
    fn generate_header(&self) -> String {
        let mut header = String::new();

        // Script Info section
        // Writing to String never fails, so unwrap is safe
        writeln!(header, "[Script Info]").expect("Writing to String should not fail");
        writeln!(header, "Title: {}", self.title).expect("Writing to String should not fail");
        writeln!(header, "ScriptType: v4.00+").expect("Writing to String should not fail");
        writeln!(header, "PlayResX: {}", self.play_res_x)
            .expect("Writing to String should not fail");
        writeln!(header, "PlayResY: {}", self.play_res_y)
            .expect("Writing to String should not fail");
        writeln!(header, "ScaledBorderAndShadow: yes")
            .expect("Writing to String should not fail");
        writeln!(header, "YCbCr Matrix: TV.709").expect("Writing to String should not fail");
        writeln!(header).expect("Writing to String should not fail");

        // Styles section
        writeln!(header, "[V4+ Styles]").expect("Writing to String should not fail");
        writeln!(
            header,
            "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, \
             OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, \
             ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, \
             MarginL, MarginR, MarginV, Encoding"
        )
        .expect("Writing to String should not fail");

        for style in &self.styles {
            writeln!(header, "{}", style.to_ass_line()).expect("Writing to String should not fail");
        }
        writeln!(header).expect("Writing to String should not fail");

        // Events section header
        writeln!(header, "[Events]").expect("Writing to String should not fail");
        writeln!(
            header,
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text"
        )
        .expect("Writing to String should not fail");

        header
    }
}

impl SubtitleGenerator for AssGenerator {
    fn format(&self) -> SubtitleFormat {
        SubtitleFormat::Ass
    }

    fn generate(&self, entries: &[SubtitleEntry]) -> Result<String> {
        let mut output = self.generate_header();

        for entry in entries {
            let style = entry.style.as_deref().unwrap_or("Default");
            let speaker = entry.speaker.as_deref().unwrap_or("");

            // Escape special characters for ASS
            let text = entry
                .text
                .replace('\\', "\\\\")
                .replace('{', "\\{")
                .replace('}', "\\}")
                .replace('\n', "\\N");

            writeln!(
                output,
                "Dialogue: 0,{},{},{},{},0,0,0,,{}",
                SubtitleEntry::format_ass_time(entry.start_ms),
                SubtitleEntry::format_ass_time(entry.end_ms),
                style,
                speaker,
                text
            )?;
        }

        Ok(output)
    }

    fn write_to_file<'a>(
        &'a self,
        entries: &'a [SubtitleEntry],
        path: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let content = self.generate(entries)?;
            fs::write(path, content).await?;
            Ok(())
        })
    }

    fn write_to<'a, W: AsyncWrite + Unpin + Send + 'a>(
        &'a self,
        entries: &'a [SubtitleEntry],
        writer: &'a mut W,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let content = self.generate(entries)?;
            writer.write_all(content.as_bytes()).await?;
            Ok(())
        })
    }
}

/// Parse SRT file content into subtitle entries
pub fn parse_srt(content: &str) -> Result<Vec<SubtitleEntry>> {
    let mut entries = Vec::new();
    let mut lines = content.lines().peekable();

    while lines.peek().is_some() {
        // Skip empty lines
        while lines.peek().is_some_and(|l| l.trim().is_empty()) {
            lines.next();
        }

        // Sequence number (skip)
        let seq_line = match lines.next() {
            Some(l) => l,
            None => break,
        };

        // Verify it's a number
        if seq_line.trim().parse::<u32>().is_err() {
            continue;
        }

        // Timestamp line
        let time_line = match lines.next() {
            Some(l) => l,
            None => break,
        };

        let (start_ms, end_ms) = parse_srt_timestamp_line(time_line)?;

        // Text lines (until blank line)
        let mut text_lines = Vec::new();
        while lines.peek().is_some_and(|l| !l.trim().is_empty()) {
            if let Some(line) = lines.next() {
                text_lines.push(line);
            }
        }

        let text = text_lines.join("\n");

        entries.push(SubtitleEntry::new(start_ms, end_ms, text));
    }

    Ok(entries)
}

/// Parse SRT timestamp line "HH:MM:SS,mmm --> HH:MM:SS,mmm"
fn parse_srt_timestamp_line(line: &str) -> Result<(u64, u64)> {
    let parts: Vec<&str> = line.split("-->").collect();
    if parts.len() != 2 {
        return Err(anyhow!("Invalid timestamp line: {line}"));
    }

    let start = parse_srt_timestamp(parts[0].trim())?;
    let end = parse_srt_timestamp(parts[1].trim())?;

    Ok((start, end))
}

/// Parse SRT timestamp "HH:MM:SS,mmm" to milliseconds
fn parse_srt_timestamp(ts: &str) -> Result<u64> {
    let parts: Vec<&str> = ts.split(&[',', ':'][..]).collect();
    if parts.len() != 4 {
        return Err(anyhow!("Invalid timestamp: {ts}"));
    }

    let hours: u64 = parts[0].parse()?;
    let minutes: u64 = parts[1].parse()?;
    let seconds: u64 = parts[2].parse()?;
    let millis: u64 = parts[3].parse()?;

    Ok(hours * 3_600_000 + minutes * 60_000 + seconds * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_srt_time_format() {
        assert_eq!(SubtitleEntry::format_srt_time(0), "00:00:00,000");
        assert_eq!(SubtitleEntry::format_srt_time(1000), "00:00:01,000");
        assert_eq!(SubtitleEntry::format_srt_time(61000), "00:01:01,000");
        assert_eq!(SubtitleEntry::format_srt_time(3661500), "01:01:01,500");
    }

    #[test]
    fn test_ass_time_format() {
        assert_eq!(SubtitleEntry::format_ass_time(0), "0:00:00.00");
        assert_eq!(SubtitleEntry::format_ass_time(1000), "0:00:01.00");
        assert_eq!(SubtitleEntry::format_ass_time(61000), "0:01:01.00");
        assert_eq!(SubtitleEntry::format_ass_time(3661500), "1:01:01.50");
    }

    #[test]
    fn test_srt_generation() {
        let gen = SrtGenerator::new();
        let entries = vec![
            SubtitleEntry::new(0, 2000, "Hello, world!"),
            SubtitleEntry::new(2500, 4000, "This is a test."),
        ];

        let output = gen.generate(&entries).unwrap();

        assert!(output.contains("1\n"));
        assert!(output.contains("00:00:00,000 --> 00:00:02,000"));
        assert!(output.contains("Hello, world!"));
        assert!(output.contains("2\n"));
        assert!(output.contains("00:00:02,500 --> 00:00:04,000"));
    }

    #[test]
    fn test_ass_generation() {
        let gen = AssGenerator::new();
        let entries = vec![SubtitleEntry::new(0, 2000, "Hello, world!")];

        let output = gen.generate(&entries).unwrap();

        assert!(output.contains("[Script Info]"));
        assert!(output.contains("[V4+ Styles]"));
        assert!(output.contains("[Events]"));
        assert!(output.contains("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,Hello, world!"));
    }

    #[test]
    fn test_parse_srt() {
        let content = r"1
00:00:00,000 --> 00:00:02,000
Hello, world!

2
00:00:02,500 --> 00:00:04,000
This is a test.
With multiple lines.

";
        let entries = parse_srt(content).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].start_ms, 0);
        assert_eq!(entries[0].end_ms, 2000);
        assert_eq!(entries[0].text, "Hello, world!");
        assert_eq!(entries[1].text, "This is a test.\nWith multiple lines.");
    }

    #[test]
    fn test_srt_with_speaker() {
        let gen = SrtGenerator::new().with_speaker_labels();
        let entries = vec![SubtitleEntry::new(0, 2000, "Hello!").with_speaker("John")];

        let output = gen.generate(&entries).unwrap();

        assert!(output.contains("[John] Hello!"));
    }
}
