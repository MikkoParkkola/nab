use std::path::PathBuf;

use anyhow::Result;

use crate::AnalyzeOutputFormat;

pub async fn cmd_analyze(
    video: &str,
    audio_only: bool,
    diarize: bool,
    format: AnalyzeOutputFormat,
    output: Option<PathBuf>,
    dgx: bool,
    api_key: Option<&str>,
) -> Result<()> {
    use nab::analyze::{
        report::{AnalysisReport, ReportFormat},
        AnalysisPipeline, PipelineConfig as AnalysisConfig, VisionBackend,
    };

    eprintln!("🎬 Analyzing: {video}");

    // Auto-detect audio-only files by extension
    let is_audio_file = video.to_lowercase().ends_with(".wav")
        || video.to_lowercase().ends_with(".mp3")
        || video.to_lowercase().ends_with(".flac")
        || video.to_lowercase().ends_with(".m4a")
        || video.to_lowercase().ends_with(".aac")
        || video.to_lowercase().ends_with(".ogg");

    let audio_only = audio_only || is_audio_file;

    if is_audio_file {
        eprintln!("   Detected audio-only file, skipping video analysis");
    }

    // Build configuration
    let mut config = AnalysisConfig::default();

    if dgx {
        config.dgx_host = Some("spark".to_string());
        eprintln!("   GPU: DGX Spark (nvfp4 quantization)");
    }

    config.enable_diarization = diarize;
    if diarize {
        eprintln!("   Diarization: enabled");
    }

    let _skip_vision = audio_only;
    if audio_only {
        eprintln!("   Mode: audio-only (transcription)");
    } else if let Some(key) = api_key {
        config.vision_backend = VisionBackend::ClaudeApi {
            api_key: key.to_string(),
        };
        eprintln!("   Vision: Claude API");
    } else if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        config.vision_backend = VisionBackend::ClaudeApi { api_key: key };
        eprintln!("   Vision: Claude API (from ANTHROPIC_API_KEY)");
    } else {
        config.vision_backend = VisionBackend::Local;
        eprintln!("   Vision: local models");
    }

    let pipeline = AnalysisPipeline::with_config(config)?;

    let start = std::time::Instant::now();
    let analysis = if audio_only {
        pipeline.analyze_audio_only(video).await?
    } else {
        pipeline.analyze(video).await?
    };
    let elapsed = start.elapsed();

    eprintln!(
        "\n✅ Analysis complete: {} segments in {:.1}s",
        analysis.segments.len(),
        elapsed.as_secs_f64()
    );

    let report_format = match format {
        AnalyzeOutputFormat::Json => ReportFormat::Json,
        AnalyzeOutputFormat::Markdown => ReportFormat::Markdown,
        AnalyzeOutputFormat::Srt => ReportFormat::Srt,
    };

    let report = AnalysisReport::generate(&analysis, report_format)?;

    if let Some(path) = output {
        std::fs::write(&path, &report)?;
        eprintln!("📄 Saved to: {}", path.display());
    } else {
        println!("{report}");
    }

    if let Some(ref meta) = analysis.metadata {
        eprintln!(
            "\n📊 Video: {}x{} @ {:.1}fps, {:.1}s",
            meta.width, meta.height, meta.fps, meta.duration
        );
    }

    let speakers: std::collections::HashSet<_> = analysis
        .segments
        .iter()
        .filter_map(|s| s.speaker.as_ref())
        .collect();

    if !speakers.is_empty() {
        eprintln!("   Speakers: {}", speakers.len());
    }

    Ok(())
}
