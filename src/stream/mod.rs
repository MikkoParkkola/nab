//! Streaming media support for microfetch
//!
//! Supports multiple providers (Yle, `YouTube`, generic HLS) with
//! native and ffmpeg backends.

pub mod provider;
pub mod backend;
pub mod providers;
pub mod backends;

pub use provider::{StreamProvider, StreamInfo, StreamQuality};
pub use backend::{StreamBackend, BackendType};
