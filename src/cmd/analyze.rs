use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::AnalyzeOutputFormat;

/// Configuration for the `nab analyze` command.
#[allow(clippy::struct_excessive_bools)]
pub struct AnalyzeConfig {
    pub video: String,
    pub audio_only: bool,
    pub diarize: bool,
    pub format: AnalyzeOutputFormat,
    pub output: Option<PathBuf>,
    /// Reserved for Phase 3 DGX Spark offload support.
    pub dgx: bool,
    /// Reserved for Phase 4 Claude Vision API integration.
    pub api_key: Option<String>,
    /// Optional BCP-47 language hint (e.g. `"fi"`, `"en"`, `"zh"`).
    pub language: Option<String>,
    /// When `true`, log a notice that active reading requires the MCP server.
    ///
    /// The CLI cannot perform active reading because there is no MCP client
    /// to satisfy `sampling/createMessage`.  Setting this flag prints a helpful
    /// message rather than silently doing nothing.
    pub active_reading: bool,
}

/// Audio-only file extensions that bypass video frame extraction.
const AUDIO_EXTENSIONS: &[&str] = &[".wav", ".mp3", ".flac", ".m4a", ".aac", ".ogg"];

pub async fn cmd_analyze(cfg: &AnalyzeConfig) -> Result<()> {
    use nab::analyze::{AudioExtractor, TranscribeOptions, default_backend};
    // dgx and api_key are reserved for Phase 3/4 — suppress lint until then.
    let _ = (cfg.dgx, cfg.api_key.as_deref());

    if cfg.active_reading {
        eprintln!(
            "Note: --active-reading is only available via the nab MCP server \
             (nab-mcp). The CLI cannot perform sampling/createMessage calls. \
             Proceeding with passive transcription."
        );
    }

    eprintln!("Analyzing: {}", cfg.video);

    // ── Auto-detect audio-only input ──────────────────────────────────────────
    let lower = cfg.video.to_lowercase();
    let is_audio_file = AUDIO_EXTENSIONS.iter().any(|ext| lower.ends_with(ext));
    let audio_only = cfg.audio_only || is_audio_file;

    if is_audio_file {
        eprintln!("  Detected audio-only file");
    }

    // ── Resolve audio path (extract if video) ─────────────────────────────────
    let input_path = std::path::Path::new(&cfg.video);
    let tmp_wav: Option<PathBuf>;

    let audio_path = if audio_only {
        tmp_wav = None;
        input_path.to_path_buf()
    } else {
        eprintln!("  Extracting audio track via ffmpeg...");
        let dest = std::env::temp_dir().join(format!("nab_analyze_{}.wav", std::process::id()));
        AudioExtractor::new()
            .extract(input_path, &dest)
            .await
            .context("ffmpeg audio extraction failed")?;
        tmp_wav = Some(dest.clone());
        dest
    };

    // ── Select backend ────────────────────────────────────────────────────────
    let backend = default_backend();
    eprintln!(
        "  Backend: {} (available={})",
        backend.name(),
        backend.is_available()
    );

    if !backend.is_available() {
        anyhow::bail!(
            "ASR backend '{}' is not available on this platform. \
             Install fluidaudiocli with `nab models fetch fluidaudio` or build from \
             https://github.com/FluidInference/FluidAudio",
            backend.name()
        );
    }

    if cfg.diarize {
        eprintln!("  Diarization: enabled");
    }

    // ── Transcribe ────────────────────────────────────────────────────────────
    let opts = TranscribeOptions {
        language: cfg.language.clone(),
        word_timestamps: true,
        diarize: cfg.diarize,
        max_duration_seconds: None,
        include_embeddings: false, // CLI does not expose embeddings; use MCP + match-speakers-with-hebb
    };

    let start = std::time::Instant::now();
    let result = backend
        .transcribe(&audio_path, opts)
        .await
        .context("transcription failed")?;
    let elapsed = start.elapsed();

    // ── Clean up temp file ────────────────────────────────────────────────────
    if let Some(ref tmp) = tmp_wav {
        let _ = std::fs::remove_file(tmp);
    }

    eprintln!(
        "\nComplete: {} segments in {:.1}s ({:.0}x realtime)",
        result.segments.len(),
        elapsed.as_secs_f64(),
        result.rtfx,
    );

    if let Some(ref speakers) = result.speakers {
        let unique: std::collections::HashSet<_> = speakers.iter().map(|s| &s.speaker).collect();
        eprintln!("  Speakers: {}", unique.len());
    }

    // ── Format output ─────────────────────────────────────────────────────────
    let formatted = match cfg.format {
        AnalyzeOutputFormat::Json => {
            serde_json::to_string_pretty(&result).context("JSON serialization failed")?
        }
        AnalyzeOutputFormat::Markdown => format_markdown(&result),
        AnalyzeOutputFormat::Srt => format_srt(&result),
    };

    if let Some(ref path) = cfg.output {
        std::fs::write(path, &formatted)
            .with_context(|| format!("writing to {}", path.display()))?;
        eprintln!("Saved to: {}", path.display());
    } else {
        println!("{formatted}");
    }

    Ok(())
}

// ─── Output formatters ────────────────────────────────────────────────────────

/// Format `TranscriptionResult` as Markdown with speaker labels.
fn format_markdown(result: &nab::analyze::TranscriptionResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Transcript\n\n**Language**: {} | **Model**: {} | **RTFx**: {:.0}x\n",
        result.language, result.model, result.rtfx
    );
    for seg in &result.segments {
        let speaker = seg.speaker.as_deref().unwrap_or("");
        if speaker.is_empty() {
            let _ = writeln!(
                out,
                "**[{:.1}s–{:.1}s]** {}\n",
                seg.start, seg.end, seg.text
            );
        } else {
            let _ = writeln!(
                out,
                "**[{:.1}s–{:.1}s] {}:** {}\n",
                seg.start, seg.end, speaker, seg.text
            );
        }
    }
    out
}

/// Format `TranscriptionResult` as SRT subtitles.
fn format_srt(result: &nab::analyze::TranscriptionResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for (i, seg) in result.segments.iter().enumerate() {
        let _ = writeln!(out, "{}", i + 1);
        let _ = writeln!(
            out,
            "{} --> {}",
            srt_timestamp(seg.start),
            srt_timestamp(seg.end)
        );
        let _ = writeln!(out, "{}\n", seg.text);
    }
    out
}

/// Format a time value in seconds as an SRT timestamp (`HH:MM:SS,mmm`).
fn srt_timestamp(secs: f64) -> String {
    let total_ms = std::time::Duration::try_from_secs_f64(secs.max(0.0))
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let ms = total_ms % 1000;
    let s = (total_ms / 1000) % 60;
    let m = (total_ms / 60_000) % 60;
    let h = total_ms / 3_600_000;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}
