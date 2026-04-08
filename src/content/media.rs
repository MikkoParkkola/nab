//! Media URL detection and transcription for `nab fetch`.
//!
//! When `nab fetch` encounters a URL that resolves to audio or video content,
//! this module extracts the audio via yt-dlp and transcribes it via the
//! analyze pipeline (FluidAudio on macOS arm64, sherpa-onnx or whisper-rs
//! fallback elsewhere).
//!
//! The output is LLM-friendly markdown with a metadata header and
//! timestamped transcript segments.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::content::media::is_media_url;
//!
//! assert!(is_media_url("https://www.youtube.com/watch?v=abc123"));
//! assert!(!is_media_url("https://example.com/article"));
//! ```

use std::path::Path;

use anyhow::{Context, Result};
use tokio::process::Command;
use tracing::{info, warn};

use crate::analyze::{AsrBackend, TranscribeOptions, TranscriptionResult, default_backend};

// ─── URL detection ─────────────────────────────────────────────────────────────

/// Hostnames that host audio or video content requiring extraction + transcription.
const MEDIA_HOSTS: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "vimeo.com",
    "soundcloud.com",
    "spotify.com",
    "podcasts.apple.com",
    "anchor.fm",
    "overcast.fm",
    "twitch.tv",
    "dailymotion.com",
    "rumble.com",
    "bitchute.com",
];

/// File extensions (before the query string) that indicate a direct media file.
const MEDIA_EXTENSIONS: &[&str] = &[
    ".mp3", ".mp4", ".m4a", ".wav", ".ogg", ".flac", ".webm", ".opus", ".aac", ".wma", ".avi",
    ".mkv", ".mov",
];

/// Returns `true` when `url` points to audio or video content that `nab fetch`
/// should transcribe rather than render as HTML.
///
/// Matches on:
/// - Known video/audio hosting platforms (case-insensitive host check).
/// - Direct media file extensions in the URL path (before the query string).
///
/// # Examples
///
/// ```rust
/// use nab::content::media::is_media_url;
///
/// assert!(is_media_url("https://www.youtube.com/watch?v=abc"));
/// assert!(is_media_url("https://example.com/audio.mp3?token=xyz"));
/// assert!(!is_media_url("https://example.com/article.html"));
/// ```
#[must_use]
pub fn is_media_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    if MEDIA_HOSTS.iter().any(|h| lower.contains(h)) {
        return true;
    }
    let path_part = lower.split('?').next().unwrap_or(&lower);
    MEDIA_EXTENSIONS.iter().any(|ext| path_part.ends_with(ext))
}

// ─── Metadata ──────────────────────────────────────────────────────────────────

/// Metadata extracted from a media URL before transcription.
#[derive(Debug, Clone)]
pub struct MediaMetadata {
    /// Media title as reported by yt-dlp (e.g., video title, podcast episode name).
    pub title: Option<String>,
    /// Channel or artist name.
    pub uploader: Option<String>,
    /// Duration in `H:MM:SS` or `M:SS` format.
    pub duration_string: Option<String>,
    /// The original URL that was fetched.
    pub url: String,
}

// ─── Result ────────────────────────────────────────────────────────────────────

/// The output of [`fetch_media_as_markdown`].
pub struct MediaFetchResult {
    /// LLM-ready markdown with metadata header and timestamped segments.
    pub markdown: String,
    /// Metadata extracted from the media source before transcription.
    pub metadata: MediaMetadata,
    /// Raw transcription result including all segments, RTFx, model info, etc.
    pub transcription: TranscriptionResult,
}

// ─── Public API ────────────────────────────────────────────────────────────────

/// Extract audio from `url` via yt-dlp, transcribe it, and return markdown.
///
/// # Arguments
///
/// * `url` — Any URL accepted by yt-dlp (YouTube, SoundCloud, Vimeo, direct
///   `.mp3`/`.mp4`/etc. links, podcast RSS episodes, …).
/// * `language` — Optional BCP-47 language hint (e.g. `"fi"`, `"en-US"`).
///   Pass `None` to let the model auto-detect the language.
/// * `diarize` — When `true`, also run speaker diarization and annotate each
///   segment with a speaker label (requires FluidAudio diarizer).
///
/// # Errors
///
/// Returns an error when:
/// - `yt-dlp` is not installed or cannot extract audio from the URL.
/// - `ffmpeg` is not installed or conversion to 16 kHz mono WAV fails.
/// - No ASR backend is available (run `nab models fetch fluidaudio` first).
/// - The ASR backend's `transcribe` call fails.
pub async fn fetch_media_as_markdown(
    url: &str,
    language: Option<&str>,
    diarize: bool,
) -> Result<MediaFetchResult> {
    let metadata = extract_metadata(url).await;

    let temp_dir = tempfile::tempdir().context("create temp dir")?;
    let wav_path = temp_dir.path().join("audio.wav");

    download_audio(url, &wav_path)
        .await
        .context("audio download via yt-dlp/ffmpeg")?;

    let backend = default_backend();
    if !backend.is_available() {
        anyhow::bail!(
            "No ASR backend available. Run `nab models fetch fluidaudio` first \
             (macOS Apple Silicon) or `nab models fetch sherpa-onnx` (all platforms)."
        );
    }

    let opts = TranscribeOptions {
        language: language.map(String::from),
        word_timestamps: true,
        diarize,
        ..Default::default()
    };

    info!(url, backend = backend.name(), "transcribing media");
    let result = backend
        .transcribe(&wav_path, opts)
        .await
        .context("transcription failed")?;

    let markdown = format_transcript_markdown(&metadata, &result);

    Ok(MediaFetchResult {
        markdown,
        metadata,
        transcription: result,
    })
}

// ─── Internals ─────────────────────────────────────────────────────────────────

/// Attempt to extract title, uploader, and duration from `url` via yt-dlp.
///
/// All fields are `None` on failure — metadata extraction is best-effort.
async fn extract_metadata(url: &str) -> MediaMetadata {
    let output = Command::new("yt-dlp")
        .args([
            "--no-playlist",
            "--skip-download",
            "--print",
            "%(title)s\n%(uploader)s\n%(duration_string)s",
            url,
        ])
        .output()
        .await;

    let stdout = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        Ok(o) => {
            warn!(
                "yt-dlp metadata failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return MediaMetadata {
                title: None,
                uploader: None,
                duration_string: None,
                url: url.to_owned(),
            };
        }
        Err(e) => {
            warn!("yt-dlp not found or spawn failed: {e}");
            return MediaMetadata {
                title: None,
                uploader: None,
                duration_string: None,
                url: url.to_owned(),
            };
        }
    };

    let mut lines = stdout.lines();
    let title = lines.next().map(str::trim).filter(|s| !s.is_empty() && *s != "NA").map(String::from);
    let uploader = lines.next().map(str::trim).filter(|s| !s.is_empty() && *s != "NA").map(String::from);
    let duration_string = lines.next().map(str::trim).filter(|s| !s.is_empty() && *s != "NA").map(String::from);

    MediaMetadata {
        title,
        uploader,
        duration_string,
        url: url.to_owned(),
    }
}

/// Download audio from `url` to `wav_path` (16 kHz mono WAV).
///
/// Tries `yt-dlp` first with browser cookies for sites that require auth
/// (YouTube PO token, Spotify, etc.). Falls back to `uvx --from "yt-dlp[default]"
/// yt-dlp` when the system yt-dlp binary is absent.
async fn download_audio(url: &str, wav_path: &Path) -> Result<()> {
    let temp_base = wav_path
        .parent()
        .context("wav_path has no parent directory")?;
    let temp_audio = temp_base.join("audio_raw.%(ext)s");

    // Build base yt-dlp args: best audio, save as temp_audio.
    let common_args: &[&str] = &[
        "--no-playlist",
        "-f",
        "bestaudio",
        "--cookies-from-browser",
        "brave",
        "-o",
        temp_audio.to_str().context("non-UTF-8 temp path")?,
        url,
    ];

    let ytdlp_status = run_ytdlp("yt-dlp", common_args).await;

    if ytdlp_status.is_err() {
        info!("system yt-dlp failed, retrying via uvx");
        // uvx wraps: uvx --from "yt-dlp[default]" yt-dlp <args>
        let mut uvx_args = vec!["--from", "yt-dlp[default]", "yt-dlp"];
        uvx_args.extend(common_args);
        run_ytdlp("uvx", &uvx_args)
            .await
            .context("yt-dlp via uvx also failed")?;
    }

    // Find the file yt-dlp produced (extension is unknown ahead of time).
    let downloaded = find_downloaded_audio(temp_base)
        .context("no audio file produced by yt-dlp — check URL and yt-dlp installation")?;

    convert_to_wav(&downloaded, wav_path).await
}

/// Invoke `binary` with `args` and return `Ok(())` if it exits successfully.
async fn run_ytdlp(binary: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(binary)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .with_context(|| format!("spawn {binary}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{binary} exited with {status}"))
    }
}

/// Find the audio file written by yt-dlp in `dir` (skips `audio.wav` itself).
fn find_downloaded_audio(dir: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir).ok()?.find_map(|entry| {
        let path = entry.ok()?.path();
        let name = path.file_name()?.to_str()?;
        // Exclude our own output file and the raw template.
        if name == "audio.wav" || name.contains("audio_raw.%(ext)s") {
            return None;
        }
        if name.starts_with("audio_raw.") && path.is_file() {
            return Some(path);
        }
        None
    })
}

/// Convert any audio file to 16 kHz mono WAV via ffmpeg.
async fn convert_to_wav(input: &Path, output: &Path) -> Result<()> {
    let status = Command::new("ffmpeg")
        .args([
            "-i",
            input.to_str().context("non-UTF-8 input path")?,
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
            output.to_str().context("non-UTF-8 output path")?,
            "-y",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .context("spawn ffmpeg")?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("ffmpeg conversion failed: {status}"))
    }
}

// ─── Markdown formatting ───────────────────────────────────────────────────────

/// Format a [`TranscriptionResult`] as LLM-friendly markdown.
///
/// Produces:
/// - A `# Title` heading (URL as fallback).
/// - A metadata block: source URL, uploader, duration, model, RTFx, language.
/// - A `## Transcript` section with `**[M:SS]** segment text` lines.
/// - Speaker labels (`**[M:SS] SPEAKER_00**`) when diarization was run.
#[must_use]
pub fn format_transcript_markdown(
    metadata: &MediaMetadata,
    result: &TranscriptionResult,
) -> String {
    let mut out = String::with_capacity(result.segments.len() * 120 + 512);

    // ── Header ──────────────────────────────────────────────────────────────
    let heading = metadata
        .title
        .as_deref()
        .unwrap_or(metadata.url.as_str());
    out.push_str(&format!("# {heading}\n\n"));

    out.push_str(&format!("**Source**: {}\n", metadata.url));
    if let Some(ref uploader) = metadata.uploader {
        out.push_str(&format!("**Uploader**: {uploader}\n"));
    }
    if let Some(ref dur) = metadata.duration_string {
        out.push_str(&format!("**Duration**: {dur}\n"));
    }
    out.push_str(&format!("**Model**: {} | **RTFx**: {:.0}×\n", result.model, result.rtfx));
    out.push_str(&format!("**Language**: {}\n", result.language));

    out.push_str("\n---\n\n## Transcript\n\n");

    // ── Segments ────────────────────────────────────────────────────────────
    for seg in &result.segments {
        let timestamp = format_seconds(seg.start);
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        match seg.speaker.as_deref() {
            Some(speaker) => out.push_str(&format!("**[{timestamp}] {speaker}** {text}\n\n")),
            None => out.push_str(&format!("**[{timestamp}]** {text}\n\n")),
        }
    }

    // Trim trailing whitespace.
    let trimmed = out.trim_end();
    trimmed.to_owned()
}

/// Convert `seconds` (f64) to `M:SS` or `H:MM:SS` timestamp string.
fn format_seconds(seconds: f64) -> String {
    let total = seconds as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::analyze::asr_backend::TranscriptSegment;

    use super::*;

    fn make_result(segments: Vec<TranscriptSegment>) -> TranscriptionResult {
        TranscriptionResult {
            segments,
            language: "en".to_string(),
            duration_seconds: 120.0,
            model: "parakeet-tdt-0.6b-v3".to_string(),
            backend: "fluidaudio".to_string(),
            rtfx: 131.0,
            processing_time_seconds: 0.92,
            speakers: None,
            footnotes: None,
            active_reading: None,
        }
    }

    fn make_metadata(title: Option<&str>, url: &str) -> MediaMetadata {
        MediaMetadata {
            title: title.map(String::from),
            uploader: Some("Test Channel".to_string()),
            duration_string: Some("2:00".to_string()),
            url: url.to_string(),
        }
    }

    // ── is_media_url ──────────────────────────────────────────────────────

    #[test]
    fn is_media_url_youtube_watch() {
        // GIVEN a standard YouTube watch URL
        // WHEN checked
        // THEN it is recognised as media
        assert!(is_media_url("https://www.youtube.com/watch?v=Cn8HBj8QAbk"));
    }

    #[test]
    fn is_media_url_youtu_be_shortlink() {
        // GIVEN a youtu.be short URL
        assert!(is_media_url("https://youtu.be/Cn8HBj8QAbk"));
    }

    #[test]
    fn is_media_url_vimeo() {
        // GIVEN a Vimeo video URL
        assert!(is_media_url("https://vimeo.com/123456789"));
    }

    #[test]
    fn is_media_url_soundcloud() {
        // GIVEN a SoundCloud track URL
        assert!(is_media_url("https://soundcloud.com/artist/track"));
    }

    #[test]
    fn is_media_url_direct_mp3() {
        // GIVEN a direct .mp3 link (no query string)
        assert!(is_media_url("https://example.com/podcast/episode.mp3"));
    }

    #[test]
    fn is_media_url_direct_mp4() {
        // GIVEN a direct .mp4 link
        assert!(is_media_url("https://cdn.example.com/video.mp4"));
    }

    #[test]
    fn is_media_url_direct_mp3_with_query_params() {
        // GIVEN a direct media link with a query string token
        // WHEN checked
        // THEN the extension before '?' is matched correctly
        assert!(is_media_url(
            "https://example.com/video.mp4?token=abc&expires=9999"
        ));
    }

    #[test]
    fn is_media_url_html_page_returns_false() {
        // GIVEN a plain HTML article URL
        // THEN it is NOT recognised as media
        assert!(!is_media_url("https://example.com/article"));
    }

    #[test]
    fn is_media_url_pdf_returns_false() {
        // GIVEN a PDF link
        // THEN it is NOT recognised as media
        assert!(!is_media_url("https://example.com/paper.pdf"));
    }

    // ── format_transcript_markdown ────────────────────────────────────────

    #[test]
    fn format_transcript_markdown_includes_header_with_title() {
        // GIVEN a result with a known title
        let meta = make_metadata(Some("My Podcast Episode"), "https://example.com/ep1.mp3");
        let result = make_result(vec![]);

        // WHEN formatted
        let md = format_transcript_markdown(&meta, &result);

        // THEN the title appears as a heading
        assert!(md.contains("# My Podcast Episode"), "got:\n{md}");
        assert!(md.contains("**Source**: https://example.com/ep1.mp3"), "got:\n{md}");
        assert!(md.contains("**Uploader**: Test Channel"), "got:\n{md}");
        assert!(md.contains("**Duration**: 2:00"), "got:\n{md}");
    }

    #[test]
    fn format_transcript_markdown_uses_url_when_no_title() {
        // GIVEN metadata with no title
        let meta = MediaMetadata {
            title: None,
            uploader: None,
            duration_string: None,
            url: "https://example.com/audio.mp3".to_string(),
        };
        let result = make_result(vec![]);

        // WHEN formatted
        let md = format_transcript_markdown(&meta, &result);

        // THEN the URL is used as the heading
        assert!(md.contains("# https://example.com/audio.mp3"), "got:\n{md}");
    }

    #[test]
    fn format_transcript_markdown_includes_timestamps() {
        // GIVEN a result with two segments at known offsets
        let segments = vec![
            TranscriptSegment {
                text: "Hello world.".to_string(),
                start: 0.0,
                end: 2.5,
                confidence: 0.98,
                language: None,
                speaker: None,
                words: None,
            },
            TranscriptSegment {
                text: "Second segment.".to_string(),
                start: 15.0,
                end: 18.0,
                confidence: 0.96,
                language: None,
                speaker: None,
                words: None,
            },
        ];
        let meta = make_metadata(Some("Test"), "https://example.com/video.mp4");
        let result = make_result(segments);

        // WHEN formatted
        let md = format_transcript_markdown(&meta, &result);

        // THEN each segment has a correct timestamp marker
        assert!(md.contains("**[0:00]** Hello world."), "got:\n{md}");
        assert!(md.contains("**[0:15]** Second segment."), "got:\n{md}");
    }

    #[test]
    fn format_transcript_markdown_includes_speaker_labels_when_diarized() {
        // GIVEN a diarized segment with a speaker label
        let segments = vec![TranscriptSegment {
            text: "Welcome to the show.".to_string(),
            start: 0.5,
            end: 3.0,
            confidence: 0.99,
            language: None,
            speaker: Some("SPEAKER_00".to_string()),
            words: None,
        }];
        let meta = make_metadata(Some("Interview"), "https://youtube.com/watch?v=abc");
        let result = make_result(segments);

        // WHEN formatted
        let md = format_transcript_markdown(&meta, &result);

        // THEN the speaker label appears inline with the timestamp
        assert!(
            md.contains("**[0:00] SPEAKER_00** Welcome to the show."),
            "got:\n{md}"
        );
    }

    #[test]
    fn format_seconds_minutes_only() {
        assert_eq!(format_seconds(75.9), "1:15");
    }

    #[test]
    fn format_seconds_hours() {
        assert_eq!(format_seconds(3661.0), "1:01:01");
    }

    #[test]
    fn format_seconds_zero() {
        assert_eq!(format_seconds(0.0), "0:00");
    }
}
