use std::path::PathBuf;

use anyhow::Result;

use crate::AnalyzeOutputFormat;

/// Configuration for the `nab analyze` command.
pub struct AnalyzeConfig {
    pub video: String,
    pub audio_only: bool,
    pub diarize: bool,
    pub format: AnalyzeOutputFormat,
    pub output: Option<PathBuf>,
    pub dgx: bool,
    pub api_key: Option<String>,
}

pub async fn cmd_analyze(cfg: &AnalyzeConfig) -> Result<()> {
    use nab::analyze::{
        AnalysisPipeline, PipelineConfig as AnalysisConfig, VisionBackend,
        report::{AnalysisReport, ReportFormat},
    };

    eprintln!("🎬 Analyzing: {}", cfg.video);

    // Auto-detect audio-only files by extension
    let lower = cfg.video.to_lowercase();
    let is_audio_file = [".wav", ".mp3", ".flac", ".m4a", ".aac", ".ogg"]
        .iter()
        .any(|ext| lower.ends_with(ext));

    let audio_only = cfg.audio_only || is_audio_file;

    if is_audio_file {
        eprintln!("   Detected audio-only file, skipping video analysis");
    }

    // Build configuration
    let mut config = AnalysisConfig::default();

    if cfg.dgx {
        config.dgx_host = Some("spark".to_string());
        eprintln!("   GPU: DGX Spark (nvfp4 quantization)");
    }

    config.enable_diarization = cfg.diarize;
    if cfg.diarize {
        eprintln!("   Diarization: enabled");
    }

    if audio_only {
        eprintln!("   Mode: audio-only (transcription)");
    } else if let Some(key) = &cfg.api_key {
        config.vision_backend = VisionBackend::ClaudeApi {
            api_key: key.clone(),
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
        pipeline.analyze_audio_only(&cfg.video).await?
    } else {
        pipeline.analyze(&cfg.video).await?
    };
    let elapsed = start.elapsed();

    eprintln!(
        "\n✅ Analysis complete: {} segments in {:.1}s",
        analysis.segments.len(),
        elapsed.as_secs_f64()
    );

    let report_format = match cfg.format {
        AnalyzeOutputFormat::Json => ReportFormat::Json,
        AnalyzeOutputFormat::Markdown => ReportFormat::Markdown,
        AnalyzeOutputFormat::Srt => ReportFormat::Srt,
    };

    let report = AnalysisReport::generate(&analysis, report_format)?;

    if let Some(path) = &cfg.output {
        std::fs::write(path, &report)?;
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
