//! Streaming media support for microfetch
//!
//! Supports multiple providers (Yle, `YouTube`, generic HLS) with
//! native and ffmpeg backends.

pub mod backend;
pub mod backends;
pub mod provider;
pub mod providers;

pub use backend::{BackendType, StreamBackend};
pub use provider::{StreamInfo, StreamProvider, StreamQuality};
