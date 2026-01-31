//! Multimodal fusion engine
//!
//! Combines audio transcripts, speaker diarization, and visual analysis
//! into unified time-aligned segments.

#![allow(dead_code)] // Timeline event types reserved for future segment building

use super::{
    AnalysisSegment, EmotionAnalysis, ExtractedFrame, Result, SpeakerSegment, TranscriptSegment,
    VisualAnalysis, VisualContext,
};

/// Fusion engine for combining modalities
pub struct FusionEngine {
    /// Tolerance for timestamp alignment (seconds)
    alignment_tolerance: f64,
}

/// Fused segment with all modalities
#[derive(Debug, Clone)]
pub struct FusedSegment {
    pub start: f64,
    pub end: f64,
    pub transcript: Option<TranscriptSegment>,
    pub speaker: Option<String>,
    pub visual: Option<VisualAnalysis>,
    pub flags: Vec<String>,
}

impl FusionEngine {
    #[must_use]
    pub fn new() -> Self {
        Self {
            alignment_tolerance: 0.5, // 500ms tolerance
        }
    }

    #[must_use]
    pub fn with_tolerance(tolerance: f64) -> Self {
        Self {
            alignment_tolerance: tolerance,
        }
    }

    /// Fuse all modalities into unified segments
    pub fn fuse(
        &self,
        transcripts: &[TranscriptSegment],
        speakers: Option<&[SpeakerSegment]>,
        _frames: &[ExtractedFrame],
        visual_analyses: &[VisualAnalysis],
    ) -> Result<Vec<AnalysisSegment>> {
        // Build timeline from all sources
        let mut timeline_events: Vec<TimelineEvent> = Vec::new();

        // Add transcript boundaries
        for (i, t) in transcripts.iter().enumerate() {
            timeline_events.push(TimelineEvent {
                timestamp: t.start,
                event_type: EventType::TranscriptStart(i),
            });
            timeline_events.push(TimelineEvent {
                timestamp: t.end,
                event_type: EventType::TranscriptEnd(i),
            });
        }

        // Add speaker boundaries
        if let Some(speakers) = speakers {
            for (i, s) in speakers.iter().enumerate() {
                timeline_events.push(TimelineEvent {
                    timestamp: s.start,
                    event_type: EventType::SpeakerStart(i),
                });
                timeline_events.push(TimelineEvent {
                    timestamp: s.end,
                    event_type: EventType::SpeakerEnd(i),
                });
            }
        }

        // Sort by timestamp
        timeline_events.sort_by(|a, b| a.timestamp.partial_cmp(&b.timestamp).unwrap());

        // Build segments based on transcript boundaries (primary)
        let mut segments = Vec::new();

        for transcript in transcripts {
            // Find matching speaker
            let speaker = speakers.and_then(|spks| {
                self.find_speaker_for_segment(spks, transcript.start, transcript.end)
            });

            // Find closest visual analysis
            let visual =
                self.find_visual_for_segment(visual_analyses, transcript.start, transcript.end);

            // Build emotion from visual analysis
            let emotion = visual.as_ref().and_then(|v| {
                v.emotion.as_ref().map(|e| EmotionAnalysis {
                    primary: e.primary.clone(),
                    confidence: e.confidence,
                    secondary: e.secondary.clone(),
                })
            });

            // Build visual context
            let visual_context = visual.as_ref().map(|v| VisualContext {
                action: v.action.clone(),
                gaze: v.gaze.clone(),
                objects: v.objects.clone(),
                scene: v.scene.clone(),
            });

            // Detect flags
            let mut flags = Vec::new();

            // Flag: emotion mismatch between audio sentiment and visual
            if let (Some(vis), Some(trans)) = (&visual, transcript.words.as_ref()) {
                if let Some(ref emo) = vis.emotion {
                    // Simple sentiment heuristics
                    let has_negative_words = trans.iter().any(|w| {
                        let word = w.word.to_lowercase();
                        word.contains("not")
                            || word.contains("never")
                            || word.contains("hate")
                            || word.contains("terrible")
                    });

                    if has_negative_words && emo.primary == "happy" {
                        flags.push("sentiment_mismatch".to_string());
                    }
                }
            }

            segments.push(AnalysisSegment {
                start: transcript.start,
                end: transcript.end,
                speaker,
                transcript: Some(transcript.text.clone()),
                emotion,
                visual: visual_context,
                flags,
            });
        }

        Ok(segments)
    }

    /// Find the speaker for a time segment
    fn find_speaker_for_segment(
        &self,
        speakers: &[SpeakerSegment],
        start: f64,
        end: f64,
    ) -> Option<String> {
        // Find speaker with most overlap
        let mut best_speaker = None;
        let mut best_overlap = 0.0;

        for speaker in speakers {
            let overlap_start = start.max(speaker.start);
            let overlap_end = end.min(speaker.end);
            let overlap = (overlap_end - overlap_start).max(0.0);

            if overlap > best_overlap {
                best_overlap = overlap;
                best_speaker = Some(speaker.speaker.clone());
            }
        }

        best_speaker
    }

    /// Find the closest visual analysis for a time segment
    fn find_visual_for_segment<'a>(
        &self,
        analyses: &'a [VisualAnalysis],
        start: f64,
        end: f64,
    ) -> Option<&'a VisualAnalysis> {
        let midpoint = f64::midpoint(start, end);
        let tolerance = self.alignment_tolerance;

        analyses
            .iter()
            .filter(|a| a.timestamp >= start - tolerance && a.timestamp <= end + tolerance)
            .min_by(|a, b| {
                let dist_a = (a.timestamp - midpoint).abs();
                let dist_b = (b.timestamp - midpoint).abs();
                dist_a.partial_cmp(&dist_b).unwrap()
            })
    }

    /// Interpolate visual analysis between keyframes
    #[must_use]
    pub fn interpolate_visual(
        &self,
        analyses: &[VisualAnalysis],
        timestamp: f64,
    ) -> Option<VisualAnalysis> {
        if analyses.is_empty() {
            return None;
        }

        // Find surrounding frames
        let mut before: Option<&VisualAnalysis> = None;
        let mut after: Option<&VisualAnalysis> = None;

        for analysis in analyses {
            if analysis.timestamp <= timestamp {
                before = Some(analysis);
            }
            if analysis.timestamp > timestamp && after.is_none() {
                after = Some(analysis);
                break;
            }
        }

        match (before, after) {
            (Some(b), Some(a)) => {
                // Interpolate - for now just use the closer one
                let dist_before = timestamp - b.timestamp;
                let dist_after = a.timestamp - timestamp;

                if dist_before <= dist_after {
                    Some(b.clone())
                } else {
                    Some(a.clone())
                }
            }
            (Some(b), None) => Some(b.clone()),
            (None, Some(a)) => Some(a.clone()),
            (None, None) => None,
        }
    }

    /// Merge adjacent segments with same speaker and similar content
    #[must_use]
    pub fn merge_similar_segments(
        &self,
        segments: Vec<AnalysisSegment>,
        gap_threshold: f64,
    ) -> Vec<AnalysisSegment> {
        if segments.is_empty() {
            return Vec::new();
        }

        let mut merged = Vec::new();
        let mut current = segments[0].clone();

        for seg in segments.into_iter().skip(1) {
            let same_speaker = current.speaker == seg.speaker;
            let small_gap = seg.start - current.end <= gap_threshold;
            let same_emotion = current.emotion.as_ref().map(|e| &e.primary)
                == seg.emotion.as_ref().map(|e| &e.primary);

            if same_speaker && small_gap && same_emotion {
                // Merge
                current.end = seg.end;
                if let (Some(ref mut t1), Some(t2)) = (&mut current.transcript, seg.transcript) {
                    t1.push(' ');
                    t1.push_str(&t2);
                }
                current.flags.extend(seg.flags);
            } else {
                merged.push(current);
                current = seg;
            }
        }
        merged.push(current);

        merged
    }
}

impl Default for FusionEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Timeline event for segment building
#[derive(Debug, Clone)]
struct TimelineEvent {
    timestamp: f64,
    event_type: EventType,
}

#[derive(Debug, Clone)]
enum EventType {
    TranscriptStart(usize),
    TranscriptEnd(usize),
    SpeakerStart(usize),
    SpeakerEnd(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fusion_basic() {
        let engine = FusionEngine::new();

        let transcripts = vec![TranscriptSegment {
            start: 0.0,
            end: 2.0,
            text: "Hello world".to_string(),
            words: None,
            language: None,
            confidence: None,
        }];

        let speakers = vec![SpeakerSegment {
            speaker: "SPEAKER_1".to_string(),
            start: 0.0,
            end: 5.0,
            confidence: None,
        }];

        let frames = vec![];
        let visual = vec![];

        let result = engine
            .fuse(&transcripts, Some(&speakers), &frames, &visual)
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].speaker, Some("SPEAKER_1".to_string()));
        assert_eq!(result[0].transcript, Some("Hello world".to_string()));
    }

    #[test]
    fn test_speaker_overlap() {
        let engine = FusionEngine::new();

        let speakers = vec![
            SpeakerSegment {
                speaker: "A".to_string(),
                start: 0.0,
                end: 3.0,
                confidence: None,
            },
            SpeakerSegment {
                speaker: "B".to_string(),
                start: 2.5,
                end: 5.0,
                confidence: None,
            },
        ];

        // Segment mostly in A's range
        let result = engine.find_speaker_for_segment(&speakers, 1.0, 2.5);
        assert_eq!(result, Some("A".to_string()));

        // Segment mostly in B's range
        let result = engine.find_speaker_for_segment(&speakers, 3.0, 4.5);
        assert_eq!(result, Some("B".to_string()));
    }

    #[test]
    fn test_merge_segments() {
        let engine = FusionEngine::new();

        let segments = vec![
            AnalysisSegment {
                start: 0.0,
                end: 2.0,
                speaker: Some("A".to_string()),
                transcript: Some("Hello".to_string()),
                emotion: None,
                visual: None,
                flags: vec![],
            },
            AnalysisSegment {
                start: 2.1,
                end: 4.0,
                speaker: Some("A".to_string()),
                transcript: Some("world".to_string()),
                emotion: None,
                visual: None,
                flags: vec![],
            },
        ];

        let merged = engine.merge_similar_segments(segments, 0.5);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].transcript, Some("Hello world".to_string()));
    }
}
