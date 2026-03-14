use anyhow::Result;

use nab::CookieSource;

use super::fetch::resolve_browser_name;

/// Configuration for the `nab stream` command.
#[allow(clippy::struct_excessive_bools)] // 1:1 map of CLI boolean flags
pub struct StreamCmdConfig {
    pub source: String,
    pub id: String,
    pub output: String,
    pub quality: String,
    pub force_native: bool,
    pub force_ffmpeg: bool,
    pub info_only: bool,
    pub list_episodes: bool,
    pub cookies: String,
    pub duration: Option<String>,
    pub ffmpeg_opts: Option<String>,
    pub player: Option<String>,
}

#[allow(clippy::too_many_lines)]
pub async fn cmd_stream(cfg: &StreamCmdConfig) -> Result<()> {
    use nab::stream::{
        StreamBackend, StreamProvider, StreamQuality,
        backend::StreamConfig,
        backends::{FfmpegBackend, NativeHlsBackend},
        providers::{GenericHlsProvider, YleProvider},
    };
    use std::collections::HashMap;
    use std::process::Stdio;
    use tokio::io::{AsyncWriteExt, stdout};

    // Parse quality
    let stream_quality = match cfg.quality.to_lowercase().as_str() {
        "best" => StreamQuality::Best,
        "worst" => StreamQuality::Worst,
        q => q
            .parse::<u32>()
            .map(StreamQuality::Specific)
            .unwrap_or(StreamQuality::Best),
    };

    // Select provider based on source
    let provider: Box<dyn StreamProvider> = match cfg.source.to_lowercase().as_str() {
        "yle" => Box::new(YleProvider::new()?),
        "generic" | "hls" | "dash" => Box::new(GenericHlsProvider::new()),
        url if url.starts_with("http") => {
            if url.contains("areena.yle.fi") || url.contains("arenan.yle.fi") {
                Box::new(YleProvider::new()?)
            } else {
                Box::new(GenericHlsProvider::new())
            }
        }
        _ => {
            if cfg.id.contains("areena.yle.fi") || cfg.id.starts_with("1-") {
                Box::new(YleProvider::new()?)
            } else if std::path::Path::new(cfg.id.split('?').next().unwrap_or(&cfg.id))
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("m3u8") || e.eq_ignore_ascii_case("mpd"))
            {
                Box::new(GenericHlsProvider::new())
            } else {
                anyhow::bail!(
                    "Unknown source: {}. Use 'yle', 'generic', or a direct URL.",
                    cfg.source
                );
            }
        }
    };

    eprintln!("🎬 Provider: {}", provider.name());

    // List episodes mode
    if cfg.list_episodes {
        eprintln!("📋 Listing episodes for: {}", cfg.id);
        let series = provider.list_series(&cfg.id).await?;
        println!("Series: {}", series.title);
        println!("Episodes: {}", series.episodes.len());
        for ep in &series.episodes {
            let duration = ep
                .duration_seconds
                .map(|d| format!(" ({}:{:02})", d / 60, d % 60))
                .unwrap_or_default();
            let ep_num = ep
                .episode_number
                .map(|n| format!("E{n}"))
                .unwrap_or_default();
            let season = ep
                .season_number
                .map(|n| format!("S{n}"))
                .unwrap_or_default();
            println!("  {} {}{}: {}{}", ep.id, season, ep_num, ep.title, duration);
        }
        return Ok(());
    }

    // Get stream info
    eprintln!("📡 Fetching stream info for: {}", cfg.id);
    let stream_info = provider.get_stream_info(&cfg.id).await?;

    // Info only mode
    if cfg.info_only {
        println!("Title: {}", stream_info.title);
        if let Some(ref desc) = stream_info.description {
            println!("Description: {desc}");
        }
        if let Some(dur) = stream_info.duration_seconds {
            println!("Duration: {}:{:02}", dur / 60, dur % 60);
        }
        println!("Live: {}", stream_info.is_live);
        println!("Manifest: {}", stream_info.manifest_url);
        if let Some(ref thumb) = stream_info.thumbnail_url {
            println!("Thumbnail: {thumb}");
        }
        return Ok(());
    }

    eprintln!("📺 {}", stream_info.title);
    if stream_info.is_live {
        eprintln!("   🔴 LIVE");
    }
    if let Some(dur) = stream_info.duration_seconds {
        eprintln!("   Duration: {}:{:02}", dur / 60, dur % 60);
    }

    // Build stream config
    let mut headers = HashMap::new();
    headers.insert("Referer".to_string(), "https://areena.yle.fi".to_string());
    headers.insert("Origin".to_string(), "https://areena.yle.fi".to_string());

    if provider.name() == "yle" {
        use rand::Rng;
        let mut rng = rand::rng();
        let ip = format!(
            "91.{}.{}.{}",
            rng.random_range(152..160),
            rng.random_range(0..256),
            rng.random_range(1..255)
        );
        headers.insert("X-Forwarded-For".to_string(), ip);

        if cfg.cookies.to_lowercase() == "none" {
            eprintln!(
                "🌍 Using Finnish IP for geo access. Add --cookies to enable authenticated content."
            );
        } else {
            eprintln!("🔐 Using browser session + Finnish IP for Yle");
        }
    }

    // Extract cookies from browser
    let browser_name = resolve_browser_name(&cfg.cookies);

    if let Some(browser) = browser_name {
        eprintln!("🍪 Extracting cookies from {browser}...");
        let cookie_source = match browser.to_lowercase().as_str() {
            "brave" => CookieSource::Brave,
            "firefox" => CookieSource::Firefox,
            "safari" => CookieSource::Safari,
            _ => CookieSource::Chrome, // chrome, edge, or unknown -> Chrome format
        };

        match cookie_source.get_cookies("yle.fi") {
            Ok(cookie_map) if !cookie_map.is_empty() => {
                let cookie_str: String = cookie_map
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                headers.insert("Cookie".to_string(), cookie_str);
                eprintln!("   ✅ Found {} cookies for yle.fi", cookie_map.len());
            }
            Ok(_) => {
                eprintln!("   ⚠️  No cookies found for yle.fi. Are you logged in?");
            }
            Err(e) => {
                eprintln!("   ⚠️  Cookie extraction failed: {e}");
            }
        }
    }

    let config = StreamConfig {
        quality: stream_quality,
        headers,
        cookies: if cfg.cookies.to_lowercase() == "none" {
            None
        } else {
            Some(cfg.cookies.clone())
        },
    };

    // For Yle, get fresh manifest URL via yle-dl
    let manifest_url = if provider.name() == "yle" {
        eprintln!("🔄 Getting fresh manifest URL via yle-dl...");
        let yle_provider = YleProvider::new()?;
        match yle_provider.get_fresh_manifest_url(&cfg.id).await {
            Ok(url) => {
                eprintln!("   ✅ Got fresh URL");
                url
            }
            Err(e) => {
                eprintln!("   ⚠️  yle-dl failed: {e}");
                eprintln!("   Using preview API URL (may fail)");
                stream_info.manifest_url.clone()
            }
        }
    } else {
        stream_info.manifest_url.clone()
    };
    let manifest_url = &manifest_url;
    let is_dash = manifest_url.contains(".mpd");
    let is_encrypted = false;

    let use_ffmpeg = cfg.force_ffmpeg || is_dash || is_encrypted || cfg.ffmpeg_opts.is_some();
    let use_native = cfg.force_native && !is_dash && !is_encrypted;

    if use_ffmpeg && !use_native {
        eprintln!("🔧 Backend: ffmpeg");
        let mut backend = FfmpegBackend::new()?;

        if let Some(opts) = &cfg.ffmpeg_opts {
            backend = backend.with_transcode_opts(opts);
        }

        if !backend.check_available().await {
            anyhow::bail!("ffmpeg not found in PATH. Install ffmpeg or use --native.");
        }

        let progress_cb = |p: nab::stream::backend::StreamProgress| {
            // Precision loss acceptable: MB display does not need full u64 precision
            #[allow(clippy::cast_precision_loss)]
            let mb: f64 = p.bytes_downloaded as f64 / 1_000_000.0;
            eprint!("\r   📥 {mb:.1} MB, {:.1}s elapsed    ", p.elapsed_seconds);
        };

        if let Some(player_cmd) = &cfg.player {
            eprintln!("🎬 Piping to: {player_cmd}");
            let player_args = get_player_stdin_args(player_cmd);
            let mut child = tokio::process::Command::new(player_cmd.as_str())
                .args(&player_args)
                .stdin(Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| anyhow::anyhow!("Failed to spawn {player_cmd}: {e}"))?;

            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow::anyhow!("Failed to get stdin for {player_cmd}"))?;

            if let Some(dur_str) = &cfg.duration {
                let secs = parse_duration(dur_str)?;
                backend
                    .stream_with_duration(
                        manifest_url,
                        &config,
                        &mut stdin,
                        secs,
                        Some(Box::new(progress_cb)),
                    )
                    .await?;
            } else {
                backend
                    .stream_to(
                        manifest_url,
                        &config,
                        &mut stdin,
                        Some(Box::new(progress_cb)),
                    )
                    .await?;
            }

            drop(stdin);
            child.wait().await?;
        } else if cfg.output == "-" {
            let mut stdout = stdout();
            if let Some(dur_str) = &cfg.duration {
                let secs = parse_duration(dur_str)?;
                backend
                    .stream_with_duration(
                        manifest_url,
                        &config,
                        &mut stdout,
                        secs,
                        Some(Box::new(progress_cb)),
                    )
                    .await?;
            } else {
                backend
                    .stream_to(
                        manifest_url,
                        &config,
                        &mut stdout,
                        Some(Box::new(progress_cb)),
                    )
                    .await?;
            }
            stdout.flush().await?;
        } else {
            let path = std::path::Path::new(&cfg.output);
            let duration_parsed = cfg.duration.as_deref().map(parse_duration).transpose()?;
            backend
                .stream_to_file(
                    manifest_url,
                    &config,
                    path,
                    Some(Box::new(progress_cb)),
                    duration_parsed,
                )
                .await?;
        }
    } else {
        eprintln!("🔧 Backend: native");
        let backend = NativeHlsBackend::new()?;

        if !backend.can_handle(manifest_url, is_encrypted) {
            anyhow::bail!("Native backend cannot handle this stream. Try --ffmpeg.");
        }

        let progress_cb = |p: nab::stream::backend::StreamProgress| {
            let total = p
                .segments_total
                .map(|t| format!("/{t}"))
                .unwrap_or_default();
            // Precision loss acceptable: MB display does not need full u64 precision
            #[allow(clippy::cast_precision_loss)]
            let mb: f64 = p.bytes_downloaded as f64 / 1_000_000.0;
            eprint!(
                "\r   📥 {mb:.1} MB, {}{} segments, {:.1}s    ",
                p.segments_completed, total, p.elapsed_seconds
            );
        };

        if let Some(player_cmd) = &cfg.player {
            eprintln!("🎬 Piping to: {player_cmd}");
            let player_args = get_player_stdin_args(player_cmd);
            let mut child = tokio::process::Command::new(player_cmd.as_str())
                .args(&player_args)
                .stdin(Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| anyhow::anyhow!("Failed to spawn {player_cmd}: {e}"))?;

            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow::anyhow!("Failed to get stdin for {player_cmd}"))?;

            backend
                .stream_to(
                    manifest_url,
                    &config,
                    &mut stdin,
                    Some(Box::new(progress_cb)),
                )
                .await?;

            drop(stdin);
            child.wait().await?;
        } else if cfg.output == "-" {
            let mut stdout = stdout();
            backend
                .stream_to(
                    manifest_url,
                    &config,
                    &mut stdout,
                    Some(Box::new(progress_cb)),
                )
                .await?;
            stdout.flush().await?;
        } else {
            let path = std::path::Path::new(&cfg.output);
            let duration_parsed = cfg.duration.as_deref().map(parse_duration).transpose()?;
            backend
                .stream_to_file(
                    manifest_url,
                    &config,
                    path,
                    Some(Box::new(progress_cb)),
                    duration_parsed,
                )
                .await?;
        }
    }

    eprintln!("\n✅ Stream complete");
    Ok(())
}

/// Get arguments for media players to read from stdin
fn get_player_stdin_args(player: &str) -> Vec<&'static str> {
    match player {
        "vlc" => vec!["-", "--intf", "dummy", "--play-and-exit"],
        "ffplay" => vec!["-i", "-"],
        "iina" => vec!["--stdin"],
        _ => vec!["-"], // mpv, mplayer, and unknown players use "-" for stdin
    }
}

/// Parse duration string like "1h", "30m", "1h30m", "90" (seconds)
fn parse_duration(s: &str) -> Result<u64> {
    let s = s.trim().to_lowercase();

    if let Ok(secs) = s.parse::<u64>() {
        return Ok(secs);
    }

    let mut total_secs = 0u64;
    let mut current_num = String::new();

    for c in s.chars() {
        if c.is_ascii_digit() {
            current_num.push(c);
        } else {
            let num: u64 = current_num.parse().unwrap_or(0);
            current_num.clear();

            match c {
                'h' => total_secs += num * 3600,
                'm' => total_secs += num * 60,
                's' => total_secs += num,
                _ => {}
            }
        }
    }

    if !current_num.is_empty() {
        total_secs += current_num.parse::<u64>().unwrap_or(0);
    }

    if total_secs == 0 {
        anyhow::bail!("Invalid duration: {s}. Use format like '1h', '30m', '1h30m', or seconds.");
    }

    Ok(total_secs)
}
