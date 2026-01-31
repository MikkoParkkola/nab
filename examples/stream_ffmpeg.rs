//! Example: Stream HLS/DASH using ffmpeg backend
//!
//! Usage: cargo run --example stream_ffmpeg <manifest_url>

use anyhow::Result;
use nab::stream::backend::{StreamBackend, StreamConfig, StreamProgress};
use nab::stream::backends::FfmpegBackend;
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <manifest_url>", args[0]);
        eprintln!("Example: {} https://example.com/master.m3u8", args[0]);
        std::process::exit(1);
    }

    let manifest_url = &args[1];

    // Create ffmpeg backend
    let backend = FfmpegBackend::new()?;

    // Check if ffmpeg is available
    if !backend.check_available().await {
        eprintln!("Error: ffmpeg not found in PATH");
        eprintln!("Install with: brew install ffmpeg (macOS) or apt install ffmpeg (Linux)");
        std::process::exit(1);
    }

    println!("Streaming {} via ffmpeg...", manifest_url);

    // Configure streaming
    let config = StreamConfig {
        quality: nab::stream::StreamQuality::Best,
        headers: std::collections::HashMap::new(),
        cookies: None,
    };

    // Progress callback
    let progress_cb = Box::new(|p: StreamProgress| {
        eprintln!(
            "\rDownloaded: {:.2} MB | Elapsed: {:.1}s",
            p.bytes_downloaded as f64 / 1_000_000.0,
            p.elapsed_seconds
        );
    });

    // Stream to stdout (can pipe to player: | mpv -)
    let mut stdout = tokio::io::stdout();
    backend
        .stream_to(manifest_url, &config, &mut stdout, Some(progress_cb))
        .await?;

    stdout.flush().await?;
    eprintln!("\nStreaming complete!");

    Ok(())
}
