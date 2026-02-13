use anyhow::Result;

use crate::OverlayStyleArg;

pub async fn cmd_annotate(
    video: &str,
    output: &str,
    subtitles: bool,
    speaker_labels: bool,
    analysis: bool,
    style: OverlayStyleArg,
    hwaccel: bool,
) -> Result<()> {
    use nab::annotate::{AnalysisConfig, AnnotationPipeline, PipelineConfig};

    eprintln!("🎬 Annotating: {video}");
    eprintln!("   Output: {output}");

    let mut config = match style {
        OverlayStyleArg::Minimal => PipelineConfig::default(),
        OverlayStyleArg::Detailed => PipelineConfig::high_quality().with_speaker_labels(true),
        OverlayStyleArg::Debug => PipelineConfig::high_quality()
            .with_speaker_labels(true)
            .with_analysis(true),
    };

    if subtitles || (!speaker_labels && !analysis) {
        config.subtitles = true;
        eprintln!("   Subtitles: enabled");
    }

    if speaker_labels {
        config.speaker_labels = true;
        config.transcription = config.transcription.with_diarization();
        eprintln!("   Speaker labels: enabled");
    }

    if analysis {
        config.analysis_overlay = true;
        config.analysis = AnalysisConfig::full();
        eprintln!("   Analysis overlay: enabled");
    }

    if hwaccel {
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

    eprintln!("   Style: {style:?}");

    let pipeline = AnnotationPipeline::new(config)?;

    let start = std::time::Instant::now();
    let result = pipeline.process_file(video, output).await?;
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
