//! Overlay track generation for video annotation
//!
//! Supports multiple overlay types:
//! - Speaker labels (from diarization)
//! - Analysis commentary (behavioral/emotional)
//! - Custom text overlays

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::subtitle::{SubtitleEntry, SubtitleStyle};

/// Position for overlay text
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum OverlayPosition {
    /// Top left corner
    TopLeft,
    /// Top center
    TopCenter,
    /// Top right corner
    TopRight,
    /// Middle left
    MiddleLeft,
    /// Middle center
    #[default]
    MiddleCenter,
    /// Middle right
    MiddleRight,
    /// Bottom left corner
    BottomLeft,
    /// Bottom center (standard subtitle position)
    BottomCenter,
    /// Bottom right corner
    BottomRight,
    /// Custom position (x, y in pixels or percentage if < 1.0)
    Custom(f32, f32),
}

impl OverlayPosition {
    /// Convert to ASS alignment value (1-9, numpad style)
    #[must_use]
    pub fn to_ass_alignment(&self) -> u8 {
        match self {
            Self::BottomLeft => 1,
            Self::BottomCenter => 2,
            Self::BottomRight => 3,
            Self::MiddleLeft => 4,
            Self::MiddleCenter => 5,
            Self::MiddleRight => 6,
            Self::TopLeft => 7,
            Self::TopCenter => 8,
            Self::TopRight => 9,
            Self::Custom(_, _) => 5, // Default to center, use \pos for actual position
        }
    }

    /// Convert to ffmpeg drawtext coordinates
    #[must_use]
    pub fn to_drawtext_position(&self, margin: u32) -> (String, String) {
        let m = margin.to_string();
        match self {
            Self::TopLeft => (m.clone(), m),
            Self::TopCenter => ("(w-text_w)/2".to_string(), m),
            Self::TopRight => (format!("w-text_w-{m}"), m),
            Self::MiddleLeft => (m, "(h-text_h)/2".to_string()),
            Self::MiddleCenter => ("(w-text_w)/2".to_string(), "(h-text_h)/2".to_string()),
            Self::MiddleRight => (format!("w-text_w-{m}"), "(h-text_h)/2".to_string()),
            Self::BottomLeft => (m.clone(), format!("h-text_h-{m}")),
            Self::BottomCenter => ("(w-text_w)/2".to_string(), format!("h-text_h-{m}")),
            Self::BottomRight => (format!("w-text_w-{m}"), format!("h-text_h-{m}")),
            Self::Custom(x, y) => {
                if *x < 1.0 && *y < 1.0 {
                    // Percentage
                    (format!("w*{x}"), format!("h*{y}"))
                } else {
                    // Pixels
                    (format!("{x}"), format!("{y}"))
                }
            }
        }
    }
}

/// Style configuration for overlays
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayStyle {
    /// Font name
    pub font_name: String,
    /// Font size
    pub font_size: u32,
    /// Text color (hex: RRGGBB)
    pub color: String,
    /// Background color (hex: RRGGBB) - empty for transparent
    pub background_color: String,
    /// Background opacity (0.0 - 1.0)
    pub background_opacity: f32,
    /// Outline/border color
    pub outline_color: String,
    /// Outline width
    pub outline_width: f32,
    /// Shadow offset (0 for no shadow)
    pub shadow_offset: f32,
    /// Bold text
    pub bold: bool,
    /// Italic text
    pub italic: bool,
}

impl Default for OverlayStyle {
    fn default() -> Self {
        Self {
            font_name: "Arial".to_string(),
            font_size: 32,
            color: "FFFFFF".to_string(),
            background_color: "000000".to_string(),
            background_opacity: 0.5,
            outline_color: "000000".to_string(),
            outline_width: 2.0,
            shadow_offset: 1.0,
            bold: false,
            italic: false,
        }
    }
}

impl OverlayStyle {
    /// Style for speaker labels
    #[must_use]
    pub fn speaker_label() -> Self {
        Self {
            font_name: "Arial".to_string(),
            font_size: 28,
            color: "FFFF00".to_string(), // Yellow
            background_color: "000000".to_string(),
            background_opacity: 0.7,
            outline_color: "000000".to_string(),
            outline_width: 2.0,
            shadow_offset: 1.0,
            bold: true,
            italic: false,
        }
    }

    /// Style for analysis overlay
    #[must_use]
    pub fn analysis() -> Self {
        Self {
            font_name: "Consolas".to_string(),
            font_size: 20,
            color: "80FF80".to_string(), // Light green
            background_color: "000000".to_string(),
            background_opacity: 0.8,
            outline_color: "000000".to_string(),
            outline_width: 1.0,
            shadow_offset: 0.0,
            bold: false,
            italic: false,
        }
    }

    /// Convert to ASS `SubtitleStyle`
    #[must_use]
    pub fn to_ass_style(&self, name: &str, position: OverlayPosition) -> SubtitleStyle {
        // Convert hex RRGGBB to ASS AABBGGRR format
        let primary = format!(
            "&H00{}{}{}",
            &self.color[4..6],
            &self.color[2..4],
            &self.color[0..2]
        );

        let outline = format!(
            "&H00{}{}{}",
            &self.outline_color[4..6],
            &self.outline_color[2..4],
            &self.outline_color[0..2]
        );

        let alpha = ((1.0 - self.background_opacity) * 255.0) as u8;
        let back = format!(
            "&H{:02X}{}{}{}",
            alpha,
            &self.background_color[4..6],
            &self.background_color[2..4],
            &self.background_color[0..2]
        );

        SubtitleStyle {
            name: name.to_string(),
            font_name: self.font_name.clone(),
            font_size: self.font_size,
            primary_color: primary,
            outline_color: outline,
            back_color: back,
            bold: self.bold,
            italic: self.italic,
            outline: self.outline_width,
            shadow: self.shadow_offset,
            alignment: position.to_ass_alignment(),
            margin_l: 20,
            margin_r: 20,
            margin_v: 20,
        }
    }

    /// Generate ffmpeg drawtext filter parameters
    #[must_use]
    pub fn to_drawtext_params(&self, position: OverlayPosition) -> String {
        let (x, y) = position.to_drawtext_position(20);

        let mut params = vec![
            format!("fontfile=/System/Library/Fonts/{}.ttf", self.font_name),
            format!("fontsize={}", self.font_size),
            format!("fontcolor=0x{}", self.color),
            format!("x={x}"),
            format!("y={y}"),
        ];

        if self.outline_width > 0.0 {
            params.push(format!("borderw={}", self.outline_width as u32));
            params.push(format!("bordercolor=0x{}", self.outline_color));
        }

        if self.shadow_offset > 0.0 {
            params.push(format!("shadowx={}", self.shadow_offset as i32));
            params.push(format!("shadowy={}", self.shadow_offset as i32));
            params.push("shadowcolor=0x000000@0.5".to_string());
        }

        if !self.background_color.is_empty() && self.background_opacity > 0.0 {
            params.push(format!(
                "box=1:boxcolor=0x{}@{}:boxborderw=5",
                self.background_color, self.background_opacity
            ));
        }

        params.join(":")
    }
}

/// A single overlay entry with timing and content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayEntry {
    /// Start time in milliseconds
    pub start_ms: u64,
    /// End time in milliseconds
    pub end_ms: u64,
    /// Text content
    pub text: String,
    /// Position on screen
    pub position: OverlayPosition,
    /// Optional custom style (uses track default if None)
    pub style: Option<OverlayStyle>,
    /// Metadata (for analysis overlays)
    pub metadata: HashMap<String, String>,
}

impl OverlayEntry {
    /// Create a new overlay entry
    #[must_use]
    pub fn new(start_ms: u64, end_ms: u64, text: impl Into<String>) -> Self {
        Self {
            start_ms,
            end_ms,
            text: text.into(),
            position: OverlayPosition::default(),
            style: None,
            metadata: HashMap::new(),
        }
    }

    /// Set position
    #[must_use]
    pub fn with_position(mut self, position: OverlayPosition) -> Self {
        self.position = position;
        self
    }

    /// Set style
    #[must_use]
    pub fn with_style(mut self, style: OverlayStyle) -> Self {
        self.style = Some(style);
        self
    }

    /// Add metadata
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Convert to subtitle entry for ASS rendering
    #[must_use]
    pub fn to_subtitle_entry(&self, style_name: &str) -> SubtitleEntry {
        SubtitleEntry {
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            text: self.text.clone(),
            speaker: None,
            style: Some(style_name.to_string()),
        }
    }
}

/// An overlay track containing multiple entries
#[derive(Debug, Clone)]
pub struct OverlayTrack {
    /// Track name/identifier
    pub name: String,
    /// Default position for entries without explicit position
    pub default_position: OverlayPosition,
    /// Default style for entries without explicit style
    pub default_style: OverlayStyle,
    /// Track entries
    pub entries: Vec<OverlayEntry>,
}

impl OverlayTrack {
    /// Create a new overlay track
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            default_position: OverlayPosition::default(),
            default_style: OverlayStyle::default(),
            entries: Vec::new(),
        }
    }

    /// Set default position
    #[must_use]
    pub fn with_position(mut self, position: OverlayPosition) -> Self {
        self.default_position = position;
        self
    }

    /// Set default style
    #[must_use]
    pub fn with_style(mut self, style: OverlayStyle) -> Self {
        self.default_style = style;
        self
    }

    /// Add an entry
    pub fn add_entry(&mut self, entry: OverlayEntry) {
        self.entries.push(entry);
    }

    /// Add multiple entries
    pub fn add_entries(&mut self, entries: impl IntoIterator<Item = OverlayEntry>) {
        self.entries.extend(entries);
    }

    /// Sort entries by start time
    pub fn sort_by_time(&mut self) {
        self.entries.sort_by_key(|e| e.start_ms);
    }

    /// Convert track to subtitle entries for ASS rendering
    #[must_use]
    pub fn to_subtitle_entries(&self) -> Vec<SubtitleEntry> {
        self.entries
            .iter()
            .map(|e| e.to_subtitle_entry(&self.name))
            .collect()
    }

    /// Get the ASS style for this track
    #[must_use]
    pub fn to_ass_style(&self) -> SubtitleStyle {
        self.default_style
            .to_ass_style(&self.name, self.default_position)
    }
}

/// Speaker label overlay generator
#[derive(Debug, Clone)]
pub struct SpeakerLabelOverlay {
    /// Style for speaker labels
    pub style: OverlayStyle,
    /// Position for speaker labels
    pub position: OverlayPosition,
    /// Format string for speaker label (use {speaker} placeholder)
    pub format: String,
    /// Minimum duration for a speaker label in milliseconds
    pub min_duration_ms: u64,
}

impl Default for SpeakerLabelOverlay {
    fn default() -> Self {
        Self {
            style: OverlayStyle::speaker_label(),
            position: OverlayPosition::TopLeft,
            format: "{speaker}".to_string(),
            min_duration_ms: 500,
        }
    }
}

impl SpeakerLabelOverlay {
    /// Create a new speaker label overlay
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set format string
    #[must_use]
    pub fn with_format(mut self, format: impl Into<String>) -> Self {
        self.format = format.into();
        self
    }

    /// Set position
    #[must_use]
    pub fn with_position(mut self, position: OverlayPosition) -> Self {
        self.position = position;
        self
    }

    /// Generate overlay track from diarization segments
    ///
    /// Input: Vec of (`start_ms`, `end_ms`, `speaker_id`)
    #[must_use]
    pub fn generate(&self, segments: &[(u64, u64, String)]) -> OverlayTrack {
        let mut track = OverlayTrack::new("Speaker")
            .with_position(self.position)
            .with_style(self.style.clone());

        // Merge consecutive segments from the same speaker
        let mut merged: Vec<(u64, u64, String)> = Vec::new();

        for (start, end, speaker) in segments {
            if let Some(last) = merged.last_mut() {
                // Same speaker and close in time (within 500ms gap)
                if last.2 == *speaker && *start <= last.1 + 500 {
                    last.1 = (*end).max(last.1);
                    continue;
                }
            }
            merged.push((*start, *end, speaker.clone()));
        }

        for (start, end, speaker) in merged {
            let duration = end.saturating_sub(start);
            if duration < self.min_duration_ms {
                continue;
            }

            let text = self.format.replace("{speaker}", &speaker);
            track.add_entry(
                OverlayEntry::new(start, end, text)
                    .with_position(self.position)
                    .with_metadata("speaker", speaker),
            );
        }

        track.sort_by_time();
        track
    }
}

/// Analysis overlay generator for behavioral/emotional commentary
#[derive(Debug, Clone)]
pub struct AnalysisOverlay {
    /// Style for analysis text
    pub style: OverlayStyle,
    /// Position for analysis overlay
    pub position: OverlayPosition,
    /// Maximum characters per line
    pub max_line_length: usize,
    /// Maximum lines to display at once
    pub max_lines: usize,
}

impl Default for AnalysisOverlay {
    fn default() -> Self {
        Self {
            style: OverlayStyle::analysis(),
            position: OverlayPosition::TopRight,
            max_line_length: 40,
            max_lines: 4,
        }
    }
}

impl AnalysisOverlay {
    /// Create a new analysis overlay
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set position
    #[must_use]
    pub fn with_position(mut self, position: OverlayPosition) -> Self {
        self.position = position;
        self
    }

    /// Set style
    #[must_use]
    pub fn with_style(mut self, style: OverlayStyle) -> Self {
        self.style = style;
        self
    }

    /// Word wrap text to fit within `max_line_length`
    fn wrap_text(&self, text: &str) -> String {
        let mut lines = Vec::new();
        let mut current_line = String::new();

        for word in text.split_whitespace() {
            if current_line.is_empty() {
                current_line = word.to_string();
            } else if current_line.len() + 1 + word.len() <= self.max_line_length {
                current_line.push(' ');
                current_line.push_str(word);
            } else {
                lines.push(current_line);
                current_line = word.to_string();

                if lines.len() >= self.max_lines {
                    break;
                }
            }
        }

        if !current_line.is_empty() && lines.len() < self.max_lines {
            lines.push(current_line);
        }

        lines.join("\n")
    }

    /// Generate overlay track from analysis entries
    ///
    /// Input: Vec of (`start_ms`, `end_ms`, `analysis_text`, `optional_metadata`)
    #[must_use]
    pub fn generate(
        &self,
        entries: &[(u64, u64, String, HashMap<String, String>)],
    ) -> OverlayTrack {
        let mut track = OverlayTrack::new("Analysis")
            .with_position(self.position)
            .with_style(self.style.clone());

        for (start, end, text, metadata) in entries {
            let wrapped = self.wrap_text(text);
            let mut entry = OverlayEntry::new(*start, *end, wrapped).with_position(self.position);

            for (k, v) in metadata {
                entry = entry.with_metadata(k.clone(), v.clone());
            }

            track.add_entry(entry);
        }

        track.sort_by_time();
        track
    }

    /// Generate from emotion/sentiment analysis results
    ///
    /// Input: Vec of (`start_ms`, `end_ms`, emotion, confidence)
    #[must_use]
    pub fn generate_emotion_overlay(&self, emotions: &[(u64, u64, String, f32)]) -> OverlayTrack {
        let entries: Vec<_> = emotions
            .iter()
            .map(|(start, end, emotion, confidence)| {
                let text = format!("{emotion}: {:.0}%", confidence * 100.0);
                let mut meta = HashMap::new();
                meta.insert("type".to_string(), "emotion".to_string());
                meta.insert("confidence".to_string(), confidence.to_string());
                (*start, *end, text, meta)
            })
            .collect();

        self.generate(&entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_overlay_position_to_ass_alignment() {
        assert_eq!(OverlayPosition::BottomLeft.to_ass_alignment(), 1);
        assert_eq!(OverlayPosition::BottomCenter.to_ass_alignment(), 2);
        assert_eq!(OverlayPosition::TopLeft.to_ass_alignment(), 7);
        assert_eq!(OverlayPosition::TopRight.to_ass_alignment(), 9);
    }

    #[test]
    fn test_speaker_label_generation() {
        let overlay = SpeakerLabelOverlay::new().with_format("Speaker: {speaker}");

        let segments = vec![
            (0, 2000, "John".to_string()),
            (2500, 4000, "Jane".to_string()),
            (4000, 6000, "John".to_string()),
        ];

        let track = overlay.generate(&segments);

        assert_eq!(track.entries.len(), 3);
        assert_eq!(track.entries[0].text, "Speaker: John");
        assert_eq!(track.entries[1].text, "Speaker: Jane");
    }

    #[test]
    fn test_speaker_label_merge_consecutive() {
        let overlay = SpeakerLabelOverlay::new();

        let segments = vec![
            (0, 1000, "John".to_string()),
            (1000, 2000, "John".to_string()), // Should merge with previous
            (2500, 4000, "Jane".to_string()),
        ];

        let track = overlay.generate(&segments);

        assert_eq!(track.entries.len(), 2);
        assert_eq!(track.entries[0].end_ms, 2000);
    }

    #[test]
    fn test_analysis_text_wrap() {
        let overlay = AnalysisOverlay {
            max_line_length: 20,
            max_lines: 3,
            ..Default::default()
        };

        let wrapped = overlay.wrap_text("This is a very long text that needs to be wrapped");

        let lines: Vec<&str> = wrapped.lines().collect();
        assert!(lines.len() <= 3);
        for line in &lines {
            assert!(line.len() <= 25); // Allow some overflow for single words
        }
    }

    #[test]
    fn test_overlay_track_to_subtitle_entries() {
        let mut track = OverlayTrack::new("Test");
        track.add_entry(OverlayEntry::new(0, 2000, "Test entry"));

        let entries = track.to_subtitle_entries();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].style, Some("Test".to_string()));
    }

    #[test]
    fn test_drawtext_position() {
        let (x, y) = OverlayPosition::TopLeft.to_drawtext_position(20);
        assert_eq!(x, "20");
        assert_eq!(y, "20");

        let (x, y) = OverlayPosition::BottomCenter.to_drawtext_position(10);
        assert_eq!(x, "(w-text_w)/2");
        assert_eq!(y, "h-text_h-10");
    }
}
