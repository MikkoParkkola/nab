use anyhow::Result;

use crate::OverlayStyleArg;

/// All parameters for a `nab annotate` invocation.
#[allow(clippy::struct_excessive_bools)] // 1:1 map of CLI boolean flags
pub struct AnnotateConfig {
    pub video: String,
    pub output: String,
    pub subtitles: bool,
    pub speaker_labels: bool,
    pub analysis: bool,
    pub style: OverlayStyleArg,
    pub hwaccel: bool,
}

pub async fn cmd_annotate(cfg: &AnnotateConfig) -> Result<()> {
    use nab::annotate::{AnalysisConfig, AnnotationPipeline, PipelineConfig};

    eprintln!("🎬 Annotating: {}", cfg.video);
    eprintln!("   Output: {}", cfg.output);

    let mut config = match cfg.style {
        OverlayStyleArg::Minimal => PipelineConfig::default(),
        OverlayStyleArg::Detailed => PipelineConfig::high_quality().with_speaker_labels(true),
        OverlayStyleArg::Debug => PipelineConfig::high_quality()
            .with_speaker_labels(true)
            .with_analysis(true),
    };

    if cfg.subtitles || (!cfg.speaker_labels && !cfg.analysis) {
        config.subtitles = true;
        eprintln!("   Subtitles: enabled");
    }

    if cfg.speaker_labels {
        config.speaker_labels = true;
        config.transcription = config.transcription.with_diarization();
        eprintln!("   Speaker labels: enabled");
    }

    if cfg.analysis {
        config.analysis_overlay = true;
        config.analysis = AnalysisConfig::full();
        eprintln!("   Analysis overlay: enabled");
    }

    if cfg.hwaccel {
        #[cfg(target_os = "macos")]
        {
            config.compositor = config.compositor.with_hwaccel("videotoolbox");
            eprintln!("   Hardware acceleration: VideoToolbox");
        }
        #[cfg(not(target_os = "macos"))]
        {
            config.compositor = config.compositor.with_hwaccel("nvenc");
            eprintln!("   Hardware acceleration: NVENC");
        }
    }

    eprintln!("   Style: {:?}", cfg.style);

    let pipeline = AnnotationPipeline::new(config)?;

    let start = std::time::Instant::now();
    let result = pipeline.process_file(&cfg.video, &cfg.output).await?;
    let elapsed = start.elapsed();

    eprintln!("\n✅ Annotation complete in {:.1}s", elapsed.as_secs_f64());

    if let Some(ref path) = result.output_path {
        eprintln!("   Output: {}", path.display());
    }

    eprintln!("   Subtitles: {} entries", result.subtitle_count);
    eprintln!("   Speakers detected: {}", result.speakers.len());

    if let Some(ref lang) = result.detected_language {
        eprintln!("   Language: {lang}");
    }

    Ok(())
}
