//! Stable error type hierarchy for the `nab` public API.
//!
//! All public functions at library boundaries return [`NabError`] so that
//! consumers can match on specific failure modes without parsing strings.
//! Internal code continues to use [`anyhow::Error`] for ergonomic propagation;
//! only the outermost public surface converts via `.map_err(NabError::from)` or
//! the explicit conversion helpers.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::error::NabError;
//! use nab::AcceleratedClient;
//!
//! # async fn example() -> Result<(), NabError> {
//! let client = AcceleratedClient::new().map_err(NabError::from)?;
//! # Ok(())
//! # }
//! ```

/// Stable error type for all public `nab` API surfaces.
///
/// Variants cover the distinct failure domains that consumers may need
/// to handle differently. Use [`NabError::Other`] for unclassified internal
/// errors propagated from [`anyhow`].
#[derive(Debug, thiserror::Error)]
pub enum NabError {
    /// The supplied URL is syntactically invalid or uses an unsupported scheme.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// The request was blocked by SSRF protection (private/reserved IP range).
    #[error("SSRF blocked: {0}")]
    SsrfBlocked(String),

    /// A site-specific provider (Twitter, Reddit, etc.) returned an error.
    #[error("provider error: {0}")]
    ProviderError(String),

    /// Content type conversion (HTML→Markdown, PDF, etc.) failed.
    #[error("conversion error: {0}")]
    ConversionError(String),

    /// Authentication or credential retrieval failed.
    #[error("auth error: {0}")]
    AuthError(String),

    /// Automated login flow failed (form detection, submission, etc.).
    #[error("login error: {0}")]
    LoginError(String),

    /// Session store operation failed.
    #[error("session error: {0}")]
    SessionError(String),

    /// Network-level error (connection refused, timeout, TLS, etc.).
    #[error("network error: {0}")]
    NetworkError(String),

    /// Response body exceeded the configured budget.
    #[error("budget exceeded: limit={limit}, actual={actual}")]
    BudgetExceeded {
        /// Configured byte or token limit.
        limit: usize,
        /// Actual size that triggered the limit.
        actual: usize,
    },

    /// The fetched page tripped the Cloudflare AI Labyrinth (or similar)
    /// bot-trap detector. The body is suppressed to avoid leaking
    /// scraper-like behaviour back to the trap.
    #[error("labyrinth detected: score={score:.1}, verdict={verdict}")]
    LabyrinthDetected {
        /// Total weighted score from the labyrinth detector.
        score: f32,
        /// Human-readable verdict (`Suspicious` or `Trap`).
        verdict: String,
    },

    /// Catch-all for unclassified errors propagated from internal anyhow chains.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
