//! `MicroFetch` - Ultra-minimal browser engine
//!
//! # Features
//!
//! - **HTTP Acceleration**: HTTP/2 multiplexing, TLS 1.3, Brotli/Zstd compression
//! - **Browser Fingerprinting**: Realistic Chrome/Firefox/Safari profiles
//! - **Authentication**: 1Password CLI integration, cookie extraction
//! - **JavaScript**: `QuickJS` engine with minimal DOM (planned)
//!
//! # Example
//!
//! ```rust,no_run
//! use microfetch::AcceleratedClient;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let client = AcceleratedClient::new()?;
//!     let html = client.fetch_text("https://example.com").await?;
//!     println!("Fetched {} bytes", html.len());
//!     Ok(())
//! }
//! ```

pub mod analyze;
pub mod annotate;
pub mod api_discovery;
pub mod auth;
pub mod browser_detect;
pub mod fetch_bridge;
pub mod fingerprint;
pub mod http3_client;
pub mod http_client;
pub mod js_engine;
pub mod mfa;
pub mod prefetch;
pub mod stream;
pub mod websocket;

pub use analyze::{
    AnalysisOutput, AnalysisPipeline, AnalysisSegment, PipelineConfig as AnalysisPipelineConfig,
};
pub use annotate::{
    AnalysisConfig as AnnotateAnalysisConfig, AnalysisOverlay, AnnotationPipeline, AssGenerator,
    Compositor, CompositorConfig, OverlayPosition, OverlayTrack,
    PipelineConfig as AnnotationPipelineConfig, PipelineResult, SpeakerLabelOverlay, SrtGenerator,
    SubtitleEntry, SubtitleFormat, TranscriptionConfig,
};
pub use api_discovery::{ApiDiscovery, ApiEndpoint};
pub use auth::{
    CookieSource, Credential, CredentialRetriever, CredentialSource, OnePasswordAuth, OtpCode,
    OtpRetriever, OtpSource,
};
pub use browser_detect::{detect_default_browser, BrowserType};
pub use fetch_bridge::{inject_fetch_sync, FetchClient};
pub use fingerprint::{
    chrome_profile, firefox_profile, random_profile, safari_profile, BrowserProfile,
};
pub use http3_client::Http3Client;
#[cfg(feature = "http3")]
pub use http3_client::Http3Response;
pub use http_client::AcceleratedClient;
pub use js_engine::JsEngine;
pub use mfa::{detect_mfa_type, MfaHandler, MfaResult, MfaType, NotificationConfig};
pub use prefetch::{extract_link_hints, EarlyHintLink, EarlyHints, PrefetchManager};
pub use stream::{StreamBackend, StreamInfo, StreamProvider};
pub use websocket::{JsonRpcWebSocket, WebSocket, WebSocketMessage};

/// Version of microfetch
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
