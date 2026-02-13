//! Streaming media support for nab.
//!
//! This module provides a two-layer architecture:
//!
//! - **Providers** ([`StreamProvider`]) know how to extract metadata
//!   (manifest URLs, titles, durations) from streaming services like
//!   Yle Areena, SVT Play, NRK TV, DR TV, or generic HLS/DASH URLs.
//!
//! - **Backends** ([`StreamBackend`]) handle the actual data transfer:
//!   a pure-Rust native HLS fetcher, an ffmpeg bridge, or a streamlink
//!   bridge for sites with complex DRM.

pub mod backend;
pub mod backends;
pub mod provider;
pub mod providers;

pub use backend::{BackendType, StreamBackend};
pub use provider::{StreamInfo, StreamProvider, StreamQuality};
