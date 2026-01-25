//! Analysis report generation
//!
//! Generates human-readable reports from analysis output.

use serde::{Deserialize, Serialize};
use std::fmt::Write as FmtWrite;
use std::path::Path;

use super::{AnalysisOutput, Result};

/// Report output format
#[derive(Debug, Clone, Copy, Default)]
pub enum ReportFormat {
    /// JSON (default, machine-readable)
    #[default]
    Json,
    /// Markdown (human-readable)
    Markdown,
    /// Plain text transcript
    Transcript,
    /// SRT subtitles
    Srt,
    /// `WebVTT` subtitles
    Vtt,
}

/// Analysis report generator
pub struct AnalysisReport;

impl AnalysisReport {
    /// Generate report in specified format
    pub fn generate(output: &AnalysisOutput, format: ReportFormat) -> Result<String> {
        match format {
            ReportFormat::Json => Self::to_json(output),
            ReportFormat::Markdown => Self::to_markdown(output),
            ReportFormat::Transcript => Self::to_transcript(output),
            ReportFormat::Srt => Self::to_srt(output),
            ReportFormat::Vtt => Self::to_vtt(output),
        }
    }

    /// Save report to file
    pub fn save(output: &AnalysisOutput, format: ReportFormat, path: &Path) -> Result<()> {
        let content = Self::generate(output, format)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Generate JSON output
    fn to_json(output: &AnalysisOutput) -> Result<String> {
        Ok(serde_json::to_string_pretty(output)?)
    }

    /// Generate Markdown report
    fn to_markdown(output: &AnalysisOutput) -> Result<String> {
        let mut md = String::new();

        writeln!(md, "# Video Analysis Report\n")?;

        // Metadata section
        if let Some(ref meta) = output.metadata {
            writeln!(md, "## Metadata\n")?;
            writeln!(md, "- **Duration**: {:.1}s", meta.duration)?;
            writeln!(md, "- **Resolution**: {}x{}", meta.width, meta.height)?;
            writeln!(md, "- **Frame Rate**: {:.2} fps", meta.fps)?;
            if let Some(channels) = meta.audio_channels {
                writeln!(md, "- **Audio Channels**: {channels}")?;
            }
            writeln!(md)?;
        }

        // Summary statistics
        writeln!(md, "## Summary\n")?;

        let total_segments = output.segments.len();
        let speakers: std::collections::HashSet<_> = output
            .segments
            .iter()
            .filter_map(|s| s.speaker.as_ref())
            .collect();

        writeln!(md, "- **Total Segments**: {total_segments}")?;
        writeln!(md, "- **Unique Speakers**: {}", speakers.len())?;

        // Emotion distribution
        let mut emotion_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for seg in &output.segments {
            if let Some(ref emo) = seg.emotion {
                *emotion_counts.entry(&emo.primary).or_insert(0) += 1;
            }
        }

        if !emotion_counts.is_empty() {
            writeln!(md, "\n### Emotion Distribution\n")?;
            let mut emotions: Vec<_> = emotion_counts.iter().collect();
            emotions.sort_by(|a, b| b.1.cmp(a.1));

            for (emotion, count) in emotions {
                let pct = (*count as f64 / total_segments as f64) * 100.0;
                writeln!(md, "- **{emotion}**: {count} ({pct:.1}%)")?;
            }
        }

        // Flags
        let flagged: Vec<_> = output
            .segments
            .iter()
            .filter(|s| !s.flags.is_empty())
            .collect();

        if !flagged.is_empty() {
            writeln!(md, "\n### Flagged Segments\n")?;
            for seg in flagged {
                writeln!(
                    md,
                    "- **{:.1}s-{:.1}s**: {:?}",
                    seg.start, seg.end, seg.flags
                )?;
            }
        }

        writeln!(md)?;

        // Full transcript with annotations
        writeln!(md, "## Transcript\n")?;

        for seg in &output.segments {
            let time = format!("[{:.1}s - {:.1}s]", seg.start, seg.end);
            let speaker = seg.speaker.as_deref().unwrap_or("Unknown");
            let text = seg.transcript.as_deref().unwrap_or("");

            let mut annotation = String::new();

            if let Some(ref emo) = seg.emotion {
                write!(
                    annotation,
                    " ({} {:.0}%)",
                    emo.primary,
                    emo.confidence * 100.0
                )?;
            }

            if let Some(ref vis) = seg.visual {
                if vis.action != "unknown" && vis.action != "none" {
                    write!(annotation, " [{}]", vis.action)?;
                }
            }

            writeln!(md, "**{time}** {speaker}{annotation}\n> {text}\n")?;
        }

        Ok(md)
    }

    /// Generate plain text transcript
    fn to_transcript(output: &AnalysisOutput) -> Result<String> {
        let mut transcript = String::new();

        for seg in &output.segments {
            let speaker = seg.speaker.as_deref().unwrap_or("Speaker");
            let text = seg.transcript.as_deref().unwrap_or("");

            writeln!(transcript, "{speaker}: {text}")?;
        }

        Ok(transcript)
    }

    /// Generate SRT subtitles
    fn to_srt(output: &AnalysisOutput) -> Result<String> {
        let mut srt = String::new();

        for (i, seg) in output.segments.iter().enumerate() {
            let start = Self::format_srt_time(seg.start);
            let end = Self::format_srt_time(seg.end);
            let text = seg.transcript.as_deref().unwrap_or("");

            writeln!(srt, "{}", i + 1)?;
            writeln!(srt, "{start} --> {end}")?;

            // Include speaker if available
            if let Some(ref speaker) = seg.speaker {
                writeln!(srt, "[{speaker}] {text}")?;
            } else {
                writeln!(srt, "{text}")?;
            }

            writeln!(srt)?;
        }

        Ok(srt)
    }

    /// Generate `WebVTT` subtitles
    fn to_vtt(output: &AnalysisOutput) -> Result<String> {
        let mut vtt = String::new();

        writeln!(vtt, "WEBVTT\n")?;

        for (i, seg) in output.segments.iter().enumerate() {
            let start = Self::format_vtt_time(seg.start);
            let end = Self::format_vtt_time(seg.end);
            let text = seg.transcript.as_deref().unwrap_or("");

            writeln!(vtt, "{}", i + 1)?;
            writeln!(vtt, "{start} --> {end}")?;

            if let Some(ref speaker) = seg.speaker {
                writeln!(vtt, "<v {speaker}>{text}")?;
            } else {
                writeln!(vtt, "{text}")?;
            }

            writeln!(vtt)?;
        }

        Ok(vtt)
    }

    /// Format time for SRT (HH:MM:SS,mmm)
    fn format_srt_time(seconds: f64) -> String {
        let hours = (seconds / 3600.0) as u32;
        let minutes = ((seconds % 3600.0) / 60.0) as u32;
        let secs = (seconds % 60.0) as u32;
        let millis = ((seconds % 1.0) * 1000.0) as u32;

        format!("{hours:02}:{minutes:02}:{secs:02},{millis:03}")
    }

    /// Format time for VTT (HH:MM:SS.mmm)
    fn format_vtt_time(seconds: f64) -> String {
        let hours = (seconds / 3600.0) as u32;
        let minutes = ((seconds % 3600.0) / 60.0) as u32;
        let secs = (seconds % 60.0) as u32;
        let millis = ((seconds % 1.0) * 1000.0) as u32;

        format!("{hours:02}:{minutes:02}:{secs:02}.{millis:03}")
    }
}

/// Speaker statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerStats {
    pub speaker: String,
    pub total_time: f64,
    pub segment_count: usize,
    pub word_count: usize,
    pub dominant_emotion: Option<String>,
}

impl AnalysisReport {
    /// Generate per-speaker statistics
    #[must_use]
    pub fn speaker_stats(output: &AnalysisOutput) -> Vec<SpeakerStats> {
        use std::collections::HashMap;

        let mut stats: HashMap<String, SpeakerStats> = HashMap::new();

        for seg in &output.segments {
            let speaker = seg.speaker.clone().unwrap_or_else(|| "Unknown".to_string());

            let entry = stats.entry(speaker.clone()).or_insert(SpeakerStats {
                speaker,
                total_time: 0.0,
                segment_count: 0,
                word_count: 0,
                dominant_emotion: None,
            });

            entry.total_time += seg.end - seg.start;
            entry.segment_count += 1;

            if let Some(ref text) = seg.transcript {
                entry.word_count += text.split_whitespace().count();
            }
        }

        stats.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::{AnalysisSegment, EmotionAnalysis, VideoMetadata};

    fn sample_output() -> AnalysisOutput {
        AnalysisOutput {
            segments: vec![
                AnalysisSegment {
                    start: 0.0,
                    end: 5.0,
                    speaker: Some("Alice".to_string()),
                    transcript: Some("Hello, welcome to the show".to_string()),
                    emotion: Some(EmotionAnalysis {
                        primary: "happy".to_string(),
                        confidence: 0.85,
                        secondary: None,
                    }),
                    visual: None,
                    flags: vec![],
                },
                AnalysisSegment {
                    start: 5.5,
                    end: 10.0,
                    speaker: Some("Bob".to_string()),
                    transcript: Some("Thanks for having me".to_string()),
                    emotion: Some(EmotionAnalysis {
                        primary: "happy".to_string(),
                        confidence: 0.75,
                        secondary: None,
                    }),
                    visual: None,
                    flags: vec![],
                },
            ],
            metadata: Some(VideoMetadata {
                duration: 60.0,
                width: 1920,
                height: 1080,
                fps: 30.0,
                audio_channels: Some(2),
                audio_sample_rate: Some(48000),
            }),
        }
    }

    #[test]
    fn test_json_generation() {
        let output = sample_output();
        let json = AnalysisReport::generate(&output, ReportFormat::Json).unwrap();

        assert!(json.contains("Alice"));
        assert!(json.contains("Hello, welcome"));
    }

    #[test]
    fn test_markdown_generation() {
        let output = sample_output();
        let md = AnalysisReport::generate(&output, ReportFormat::Markdown).unwrap();

        assert!(md.contains("# Video Analysis Report"));
        assert!(md.contains("Alice"));
        assert!(md.contains("happy"));
    }

    #[test]
    fn test_srt_generation() {
        let output = sample_output();
        let srt = AnalysisReport::generate(&output, ReportFormat::Srt).unwrap();

        assert!(srt.contains("00:00:00,000 --> 00:00:05,000"));
        assert!(srt.contains("[Alice]"));
    }

    #[test]
    fn test_vtt_generation() {
        let output = sample_output();
        let vtt = AnalysisReport::generate(&output, ReportFormat::Vtt).unwrap();

        assert!(vtt.contains("WEBVTT"));
        assert!(vtt.contains("00:00:00.000 --> 00:00:05.000"));
    }

    #[test]
    fn test_speaker_stats() {
        let output = sample_output();
        let stats = AnalysisReport::speaker_stats(&output);

        assert_eq!(stats.len(), 2);

        let alice = stats.iter().find(|s| s.speaker == "Alice").unwrap();
        assert_eq!(alice.segment_count, 1);
        assert_eq!(alice.word_count, 5);
    }

    #[test]
    fn test_time_formatting() {
        assert_eq!(AnalysisReport::format_srt_time(0.0), "00:00:00,000");
        assert_eq!(AnalysisReport::format_srt_time(61.5), "00:01:01,500");
        assert_eq!(AnalysisReport::format_srt_time(3661.123), "01:01:01,123");

        assert_eq!(AnalysisReport::format_vtt_time(0.0), "00:00:00.000");
        assert_eq!(AnalysisReport::format_vtt_time(61.5), "00:01:01.500");
    }
}
