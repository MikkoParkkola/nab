//! `nab` - Ultra-minimal browser engine
//!
//! # Features
//!
//! - **HTTP Acceleration**: HTTP/2 multiplexing, TLS 1.3, Brotli/Zstd compression
//! - **Browser Fingerprinting**: Realistic Chrome/Firefox/Safari profiles
//! - **Authentication**: 1Password CLI integration, cookie extraction
//! - **JavaScript**: `QuickJS` engine with minimal DOM
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::AcceleratedClient;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let client = AcceleratedClient::new()?;
//!     let html = client.fetch_text("https://example.com").await?;
//!     println!("Fetched {} bytes", html.len());
//!     Ok(())
//! }
//! ```

pub mod error;

/// Internal implementation modules — not stable public API.
#[doc(hidden)]
pub mod analyze;
/// Internal implementation modules — not stable public API.
#[doc(hidden)]
pub mod annotate;
pub mod api_discovery;
pub mod arena;
pub mod auth;
#[cfg(feature = "browser")]
pub mod browser;
pub mod browser_detect;
pub mod content;
/// Internal implementation modules — not stable public API.
#[doc(hidden)]
pub mod fetch_bridge;
pub mod fingerprint;
pub mod form;
pub mod http3_client;
pub mod http_client;
#[cfg(feature = "impersonate")]
pub mod impersonate_client;
/// Internal implementation modules — not stable public API.
#[doc(hidden)]
pub mod js_engine;
pub mod login;
pub mod mfa;
pub mod plugin;
pub mod prefetch;
pub mod rate_limit;
pub mod session;
pub mod site;
pub mod ssrf;
pub mod stream;
/// Internal implementation modules — not stable public API.
#[doc(hidden)]
pub mod util;
pub mod watch;
pub mod webmcp;
pub mod websocket;

pub use error::NabError;

pub use api_discovery::{ApiDiscovery, ApiEndpoint};
pub use auth::{
    CookieSource, Credential, CredentialRetriever, CredentialSource, OnePasswordAuth, OtpCode,
    OtpRetriever, OtpSource,
};
#[cfg(feature = "browser")]
pub use browser::{BrowserLogin, Cookie};
pub use browser_detect::{BrowserType, detect_default_browser};
pub use fingerprint::{
    BrowserProfile, chrome_profile, firefox_profile, random_profile, safari_profile,
};
pub use form::{Form, parse_field_args};
pub use http_client::{AcceleratedClient, SafeFetchConfig, SafeFetchResponse, TOR_PROXY_URL};
pub use http3_client::Http3Client;
#[cfg(feature = "http3")]
pub use http3_client::Http3Response;
pub use login::{LoginFlow, LoginResult, get_session_dir};
pub use mfa::{MfaHandler, MfaResult, MfaType, NotificationConfig, detect_mfa_type};
pub use prefetch::{EarlyHintLink, EarlyHints, PrefetchManager, extract_link_hints};
pub use session::{MAX_SESSIONS, SessionStore};
pub use ssrf::{
    DEFAULT_MAX_BODY_SIZE, DEFAULT_MAX_REDIRECTS, extract_mapped_ipv4, is_denied_ipv4,
    is_denied_ipv6, validate_ip, validate_redirect_target, validate_url,
};
pub use stream::{StreamBackend, StreamInfo, StreamProvider};
pub use watch::{AddOptions, Watch, WatchEvent, WatchId, WatchManager, WatchOptions};
pub use webmcp::{
    DiscoveryResult, McpManifest, McpTool, extract_link_href, parse_manifest_bytes,
    resolve_manifest_url, well_known_url,
};
pub use websocket::{JsonRpcWebSocket, WebSocket, WebSocketMessage};

/// Version of nab
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
