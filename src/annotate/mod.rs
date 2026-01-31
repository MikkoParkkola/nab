//! Video annotation and overlay system for microfetch
//!
//! Provides synchronized subtitles and analysis commentary for video streams.
//!
//! # Features
//!
//! - **Subtitle generation** - Whisper transcription to SRT/ASS format
//! - **Analysis overlay** - Behavioral/emotional analysis as on-screen text
//! - **Speaker labels** - Diarization-based speaker identification
//! - **ffmpeg compositing** - Burn overlays into video streams
//!
//! # Example
//!
//! ```rust,no_run
//! use microfetch::annotate::{AnnotationPipeline, PipelineConfig};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let pipeline = AnnotationPipeline::new(PipelineConfig::default())?;
//!     pipeline.process_file("input.mp4", "output.mp4").await?;
//!     Ok(())
//! }
//! ```

pub mod compositor;
pub mod overlay;
pub mod pipeline;
pub mod subtitle;

pub use compositor::{Compositor, CompositorConfig, CompositorOutput};
pub use overlay::{
    AnalysisOverlay, OverlayEntry, OverlayPosition, OverlayStyle, OverlayTrack, SpeakerLabelOverlay,
};
pub use pipeline::{
    AnalysisConfig, AnnotationPipeline, PipelineConfig, PipelineResult, TranscriptionConfig,
};
pub use subtitle::{
    AssGenerator, SrtGenerator, SubtitleEntry, SubtitleFormat, SubtitleGenerator, SubtitleStyle,
};
