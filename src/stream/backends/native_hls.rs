//! Native HLS streaming backend
//!
//! Fetches and concatenates HLS segments without external dependencies.
//! Supports:
//! - Multi-quality master playlists (quality selection)
//! - VOD playlists (finite segments)
//! - Live playlists (continuous refresh)
//! - Parallel segment fetching
//! - Retry on segment failure

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tracing::{debug, info};

use super::super::backend::{
    BackendType, ProgressCallback, StreamBackend, StreamConfig, StreamProgress,
};
use super::super::StreamQuality;

/// Native HLS streaming backend
pub struct NativeHlsBackend {
    client: Client,
    /// Maximum concurrent segment downloads
    max_concurrent: usize,
    /// Retry count for failed segments
    max_retries: u32,
}

impl NativeHlsBackend {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(16) // Keep more connections alive for speed
            .pool_idle_timeout(Duration::from_secs(60))
            .tcp_nodelay(true) // Reduce latency
            .build()?;

        Ok(Self {
            client,
            max_concurrent: 8, // Higher concurrency for faster VOD downloads
            max_retries: 3,
        })
    }

    #[must_use]
    pub fn with_concurrency(mut self, max: usize) -> Self {
        self.max_concurrent = max;
        self
    }

    /// Parse master playlist and return quality variants
    async fn parse_master_playlist(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
    ) -> Result<Vec<HlsVariant>> {
        let content = self.fetch_playlist(url, headers).await?;
        let base_url = url.rsplit_once('/').map_or("", |(base, _)| base);

        let mut variants = Vec::new();
        let mut lines = content.lines().peekable();

        while let Some(line) = lines.next() {
            if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
                let attrs = Self::parse_attributes(rest);
                let bandwidth = attrs
                    .get("BANDWIDTH")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let resolution = attrs.get("RESOLUTION").cloned();
                let codecs = attrs.get("CODECS").cloned();

                if let Some(uri_line) = lines.next() {
                    if !uri_line.starts_with('#') {
                        let uri = Self::resolve_url(base_url, uri_line);
                        let height = resolution
                            .as_ref()
                            .and_then(|r| r.split('x').nth(1))
                            .and_then(|h| h.parse().ok())
                            .unwrap_or(0);

                        variants.push(HlsVariant {
                            bandwidth,
                            height,
                            codecs,
                            uri,
                        });
                    }
                }
            }
        }

        // Sort by bandwidth (quality) descending
        variants.sort_by(|a, b| b.bandwidth.cmp(&a.bandwidth));

        Ok(variants)
    }

    /// Parse media playlist and return segments
    async fn parse_media_playlist(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
    ) -> Result<HlsPlaylist> {
        let content = self.fetch_playlist(url, headers).await?;
        let base_url = url.rsplit_once('/').map_or("", |(base, _)| base);

        let mut segments = Vec::new();
        let mut is_live = true;
        let mut media_sequence = 0u64;
        let mut target_duration = 10.0f64;
        let mut current_duration = 0.0f64;

        for line in content.lines() {
            if line.starts_with("#EXT-X-ENDLIST") {
                is_live = false;
            } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
                media_sequence = rest.parse().unwrap_or(0);
            } else if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
                target_duration = rest.parse().unwrap_or(10.0);
            } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
                current_duration = rest
                    .split(',')
                    .next()
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(target_duration);
            } else if !line.starts_with('#') && !line.is_empty() {
                let uri = Self::resolve_url(base_url, line);
                segments.push(HlsSegment {
                    sequence: media_sequence + segments.len() as u64,
                    duration: current_duration,
                    uri,
                });
            }
        }

        Ok(HlsPlaylist {
            segments,
            is_live,
            target_duration,
            media_sequence,
        })
    }

    async fn fetch_playlist(&self, url: &str, headers: &HashMap<String, String>) -> Result<String> {
        let mut req = self.client.get(url);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("Failed to fetch playlist: {}", resp.status()));
        }

        Ok(resp.text().await?)
    }

    async fn fetch_segment(&self, url: &str, headers: &HashMap<String, String>) -> Result<Vec<u8>> {
        let mut last_error = None;

        for attempt in 0..self.max_retries {
            let mut req = self.client.get(url);
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }

            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    return Ok(resp.bytes().await?.to_vec());
                }
                Ok(resp) => {
                    last_error = Some(anyhow!("Segment fetch failed: {}", resp.status()));
                }
                Err(e) => {
                    last_error = Some(e.into());
                }
            }

            if attempt < self.max_retries - 1 {
                tokio::time::sleep(Duration::from_millis(500 * (u64::from(attempt) + 1))).await;
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("Unknown segment fetch error")))
    }

    fn parse_attributes(attr_str: &str) -> HashMap<String, String> {
        let mut attrs = HashMap::new();
        let mut chars = attr_str.chars().peekable();

        while chars.peek().is_some() {
            // Parse key
            let key: String = chars.by_ref().take_while(|&c| c != '=').collect();

            if key.is_empty() {
                break;
            }

            // Parse value (handle quoted values)
            let value = if chars.peek() == Some(&'"') {
                chars.next(); // consume opening quote
                let v: String = chars.by_ref().take_while(|&c| c != '"').collect();
                chars.next(); // consume comma if present
                v
            } else {
                chars.by_ref().take_while(|&c| c != ',').collect()
            };

            attrs.insert(key.trim().to_string(), value.trim().to_string());
        }

        attrs
    }

    fn resolve_url(base: &str, relative: &str) -> String {
        if relative.starts_with("http://") || relative.starts_with("https://") {
            relative.to_string()
        } else if relative.starts_with('/') {
            // Absolute path - need to extract origin from base
            if let Some(idx) = base.find("://") {
                if let Some(end) = base[idx + 3..].find('/') {
                    format!("{}{}", &base[..idx + 3 + end], relative)
                } else {
                    format!("{base}{relative}")
                }
            } else {
                relative.to_string()
            }
        } else {
            format!("{base}/{relative}")
        }
    }

    fn select_variant<'a>(
        &self,
        variants: &'a [HlsVariant],
        quality: &StreamQuality,
    ) -> Option<&'a HlsVariant> {
        if variants.is_empty() {
            return None;
        }

        match quality {
            StreamQuality::Best => variants.first(),
            StreamQuality::Worst => variants.last(),
            StreamQuality::Specific(height) => {
                // Find closest match
                variants
                    .iter()
                    .min_by_key(|v| (v.height as i32 - *height as i32).abs())
            }
        }
    }

    async fn stream_live_with_duration<W: AsyncWrite + Unpin + Send>(
        &self,
        playlist_url: &str,
        headers: &HashMap<String, String>,
        output: &mut W,
        progress: &Option<ProgressCallback>,
        start_time: std::time::Instant,
        duration_secs: Option<u64>,
    ) -> Result<()> {
        let mut last_sequence = 0u64;
        let mut bytes_downloaded = 0u64;
        let mut segments_completed = 0u32;

        loop {
            // Check if we've reached duration limit
            if let Some(max_dur) = duration_secs {
                if start_time.elapsed().as_secs() >= max_dur {
                    info!("Duration limit reached ({max_dur}s), stopping live stream");
                    break;
                }
            }

            let playlist = self.parse_media_playlist(playlist_url, headers).await?;

            // Find new segments
            let new_segments: Vec<_> = playlist
                .segments
                .iter()
                .filter(|s| s.sequence > last_sequence)
                .collect();

            if !new_segments.is_empty() {
                debug!("Found {} new segments", new_segments.len());

                for seg in new_segments {
                    let data = self.fetch_segment(&seg.uri, headers).await?;
                    bytes_downloaded += data.len() as u64;
                    segments_completed += 1;
                    last_sequence = seg.sequence;

                    output.write_all(&data).await?;

                    if let Some(ref cb) = progress {
                        cb(StreamProgress {
                            bytes_downloaded,
                            segments_completed,
                            segments_total: None,
                            elapsed_seconds: start_time.elapsed().as_secs_f64(),
                        });
                    }

                    // Check duration limit after each segment
                    if let Some(max_dur) = duration_secs {
                        if start_time.elapsed().as_secs() >= max_dur {
                            info!("Duration limit reached ({max_dur}s), stopping live stream");
                            return Ok(());
                        }
                    }
                }
            }

            // Check if stream ended
            if !playlist.is_live {
                break;
            }

            // Wait before next poll (half of target duration is typical)
            tokio::time::sleep(Duration::from_secs_f64(playlist.target_duration / 2.0)).await;
        }

        Ok(())
    }

    /// Internal streaming with optional duration limit
    async fn stream_to_internal<W: AsyncWrite + Unpin + Send>(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        output: &mut W,
        progress: Option<ProgressCallback>,
        duration_secs: Option<u64>,
    ) -> Result<()> {
        let headers = &config.headers;
        let start_time = std::time::Instant::now();

        // Check if master playlist (has variants) or media playlist (has segments)
        let content = self.fetch_playlist(manifest_url, headers).await?;
        let is_master = content.contains("#EXT-X-STREAM-INF:");

        let media_url = if is_master {
            let variants = self.parse_master_playlist(manifest_url, headers).await?;
            debug!("Found {} quality variants", variants.len());

            let variant = self
                .select_variant(&variants, &config.quality)
                .ok_or_else(|| anyhow!("No suitable quality variant found"))?;

            info!(
                "Selected variant: {}p @ {} bps",
                variant.height, variant.bandwidth
            );
            variant.uri.clone()
        } else {
            manifest_url.to_string()
        };

        let playlist = self.parse_media_playlist(&media_url, headers).await?;
        info!(
            "Playlist: {} segments, live={}",
            playlist.segments.len(),
            playlist.is_live
        );

        let total_segments = if playlist.is_live {
            None
        } else {
            Some(playlist.segments.len() as u32)
        };
        let mut bytes_downloaded = 0u64;
        let mut segments_completed = 0u32;

        // For VOD: fetch all segments in order with limited concurrency
        // For live: continuously poll and fetch new segments
        if playlist.is_live {
            self.stream_live_with_duration(
                &media_url,
                headers,
                output,
                &progress,
                start_time,
                duration_secs,
            )
            .await?;
        } else {
            // Calculate max segments if duration is limited
            let max_segments = duration_secs.and_then(|dur| {
                if playlist.segments.is_empty() {
                    None
                } else {
                    // Estimate segments from target duration
                    let avg_seg_duration = playlist.target_duration;
                    Some((dur as f64 / avg_seg_duration).ceil() as usize)
                }
            });

            let segments_to_fetch = if let Some(max) = max_segments {
                playlist.segments.iter().take(max).collect::<Vec<_>>()
            } else {
                playlist.segments.iter().collect::<Vec<_>>()
            };

            // Fetch segments with concurrency
            for chunk in segments_to_fetch.chunks(self.max_concurrent) {
                let futures: Vec<_> = chunk
                    .iter()
                    .map(|seg| self.fetch_segment(&seg.uri, headers))
                    .collect();

                let results = futures::future::join_all(futures).await;

                for result in results {
                    let data = result?;
                    bytes_downloaded += data.len() as u64;
                    segments_completed += 1;

                    output.write_all(&data).await?;

                    if let Some(ref cb) = progress {
                        cb(StreamProgress {
                            bytes_downloaded,
                            segments_completed,
                            segments_total: total_segments,
                            elapsed_seconds: start_time.elapsed().as_secs_f64(),
                        });
                    }
                }
            }
        }

        output.flush().await?;
        Ok(())
    }
}

#[async_trait]
impl StreamBackend for NativeHlsBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::Native
    }

    fn can_handle(&self, manifest_url: &str, encrypted: bool) -> bool {
        // Native backend handles unencrypted HLS only
        !encrypted && manifest_url.contains(".m3u8")
    }

    async fn stream_to<W: AsyncWrite + Unpin + Send>(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        output: &mut W,
        progress: Option<ProgressCallback>,
    ) -> Result<()> {
        self.stream_to_internal(manifest_url, config, output, progress, None)
            .await
    }

    async fn stream_to_file(
        &self,
        manifest_url: &str,
        config: &StreamConfig,
        path: &std::path::Path,
        progress: Option<ProgressCallback>,
        duration_secs: Option<u64>,
    ) -> Result<()> {
        let file = tokio::fs::File::create(path).await?;
        let mut writer = tokio::io::BufWriter::new(file);
        self.stream_to_internal(manifest_url, config, &mut writer, progress, duration_secs)
            .await
    }
}

#[derive(Debug, Clone)]
struct HlsVariant {
    bandwidth: u64,
    height: u32,
    #[allow(dead_code)]
    codecs: Option<String>,
    uri: String,
}

#[derive(Debug)]
struct HlsPlaylist {
    segments: Vec<HlsSegment>,
    is_live: bool,
    target_duration: f64,
    #[allow(dead_code)]
    media_sequence: u64,
}

#[derive(Debug, Clone)]
struct HlsSegment {
    sequence: u64,
    #[allow(dead_code)]
    duration: f64,
    uri: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_url() {
        assert_eq!(
            NativeHlsBackend::resolve_url("https://example.com/path", "video.ts"),
            "https://example.com/path/video.ts"
        );
        assert_eq!(
            NativeHlsBackend::resolve_url("https://example.com/path", "/video.ts"),
            "https://example.com/video.ts"
        );
        assert_eq!(
            NativeHlsBackend::resolve_url(
                "https://example.com/path",
                "https://cdn.example.com/video.ts"
            ),
            "https://cdn.example.com/video.ts"
        );
    }

    #[test]
    fn test_parse_attributes() {
        let attrs = NativeHlsBackend::parse_attributes("BANDWIDTH=1280000,RESOLUTION=720x480");
        assert_eq!(attrs.get("BANDWIDTH"), Some(&"1280000".to_string()));
        assert_eq!(attrs.get("RESOLUTION"), Some(&"720x480".to_string()));

        let attrs2 = NativeHlsBackend::parse_attributes(
            "CODECS=\"avc1.4d401f,mp4a.40.2\",BANDWIDTH=2000000",
        );
        assert_eq!(
            attrs2.get("CODECS"),
            Some(&"avc1.4d401f,mp4a.40.2".to_string())
        );
    }
}
