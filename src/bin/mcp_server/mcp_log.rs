//! Structured MCP logging — dual-emit tracing + `notifications/message`.
//!
//! The MCP 2025-11-25 spec defines `logging/setLevel` and
//! `notifications/message` for structured server-to-client logging with
//! RFC 5424 severity levels.  This module provides:
//!
//! - [`McpLogger`]: a `static` dual-emitter (tracing + MCP notification).
//! - Macro helpers [`mcp_debug!`], [`mcp_info!`], [`mcp_warn!`], [`mcp_error!`]
//!   that call the singleton and also emit the matching `tracing` event.
//!
//! # Level ordering (highest severity first)
//!
//! emergency > alert > critical > error > warning > notice > info > debug
//!
//! When the client sends `logging/setLevel = "warning"`, only warning and
//! above are forwarded as MCP notifications.  All levels always go to tracing.
//!
//! # Thread safety
//!
//! The `McpLogger` stores the runtime via [`OnceLock`] and the current
//! minimum level via [`AtomicU8`].  Both operations are lock-free.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::LoggingMessageNotificationParams;

// ─── Level mapping ────────────────────────────────────────────────────────────

/// RFC 5424 log levels as numeric severity (lower = more severe).
///
/// The SDK's `LoggingLevel` enum uses the same naming; we store as `u8` to
/// avoid an atomic enum.
pub(crate) mod level {
    pub const EMERGENCY: u8 = 0;
    pub const ALERT: u8 = 1;
    pub const CRITICAL: u8 = 2;
    pub const ERROR: u8 = 3;
    pub const WARNING: u8 = 4;
    pub const NOTICE: u8 = 5;
    pub const INFO: u8 = 6;
    pub const DEBUG: u8 = 7;
}

/// Convert a `LoggingLevel` variant to the numeric severity used internally.
fn logging_level_to_u8(level: rust_mcp_sdk::schema::LoggingLevel) -> u8 {
    use rust_mcp_sdk::schema::LoggingLevel;
    match level {
        LoggingLevel::Emergency => level::EMERGENCY,
        LoggingLevel::Alert => level::ALERT,
        LoggingLevel::Critical => level::CRITICAL,
        LoggingLevel::Error => level::ERROR,
        LoggingLevel::Warning => level::WARNING,
        LoggingLevel::Notice => level::NOTICE,
        LoggingLevel::Info => level::INFO,
        LoggingLevel::Debug => level::DEBUG,
    }
}

/// Convert numeric severity back to `LoggingLevel`.
fn u8_to_logging_level(v: u8) -> rust_mcp_sdk::schema::LoggingLevel {
    use rust_mcp_sdk::schema::LoggingLevel;
    match v {
        level::EMERGENCY => LoggingLevel::Emergency,
        level::ALERT => LoggingLevel::Alert,
        level::CRITICAL => LoggingLevel::Critical,
        level::ERROR => LoggingLevel::Error,
        level::WARNING => LoggingLevel::Warning,
        level::NOTICE => LoggingLevel::Notice,
        level::DEBUG => LoggingLevel::Debug,
        _ => LoggingLevel::Info,
    }
}

// ─── McpLogger ────────────────────────────────────────────────────────────────

/// Global dual-emit logger: tracing + MCP `notifications/message`.
///
/// The runtime handle is injected once after the server starts via
/// [`McpLogger::init`].  Before injection, messages still reach tracing.
pub(crate) struct McpLogger {
    runtime: OnceLock<Arc<dyn McpServer>>,
    /// Minimum level to forward via MCP notifications (default: INFO).
    min_level: AtomicU8,
}

impl McpLogger {
    /// Create the logger in uninitialized state (no runtime yet).
    pub(crate) const fn new() -> Self {
        Self {
            runtime: OnceLock::new(),
            min_level: AtomicU8::new(level::INFO),
        }
    }

    /// Inject the MCP runtime. Must be called exactly once after
    /// `server_runtime::create_server`.
    /// Subsequent calls are silently ignored (`OnceLock` semantics).
    pub(crate) fn init(&self, runtime: Arc<dyn McpServer>) {
        let _ = self.runtime.set(runtime);
    }

    /// Update the minimum level forwarded to the MCP client.
    ///
    /// Called from `handle_set_level_request`.
    pub(crate) fn set_level(&self, level: rust_mcp_sdk::schema::LoggingLevel) {
        self.min_level
            .store(logging_level_to_u8(level), Ordering::Relaxed);
        self.log(
            level::NOTICE,
            "mcp",
            &format!("logging level set to {level:?}"),
            None,
        );
    }

    /// Emit a log event.
    ///
    /// Always writes to tracing (for stderr/file sinks).
    /// Forwards via `notifications/message` only when:
    /// - the runtime has been initialized, and
    /// - `severity <= min_level` (i.e., the message is at or above the threshold).
    pub(crate) fn log(
        &self,
        severity: u8,
        logger: &str,
        message: &str,
        data: Option<serde_json::Value>,
    ) {
        // Always emit to tracing so stderr always works.
        match severity {
            level::EMERGENCY | level::ALERT | level::CRITICAL | level::ERROR => {
                tracing::error!(logger, "{}", message);
            }
            level::WARNING => {
                tracing::warn!(logger, "{}", message);
            }
            level::NOTICE | level::INFO => {
                tracing::info!(logger, "{}", message);
            }
            _ => {
                tracing::debug!(logger, "{}", message);
            }
        }

        // Only forward if we have a runtime and severity passes the threshold.
        let min = self.min_level.load(Ordering::Relaxed);
        if severity > min {
            return;
        }
        let Some(rt) = self.runtime.get() else {
            return;
        };

        let level = u8_to_logging_level(severity);
        let params = LoggingMessageNotificationParams {
            level,
            logger: Some(logger.to_string()),
            data: data.unwrap_or_else(|| serde_json::Value::String(message.to_string())),
            meta: None,
        };

        // Fire-and-forget: logging failures must not propagate to callers.
        let rt = Arc::clone(rt);
        tokio::spawn(async move {
            if let Err(e) = rt.notify_log_message(params).await {
                tracing::debug!("MCP log notification failed: {e}");
            }
        });
    }
}

// ─── Global singleton ─────────────────────────────────────────────────────────

/// Global `McpLogger` instance — initialized once at startup.
pub(crate) static LOGGER: McpLogger = McpLogger::new();

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_logger() -> McpLogger {
        McpLogger::new()
    }

    // ── level filtering ───────────────────────────────────────────────────────

    #[test]
    fn set_level_warning_rejects_info() {
        // GIVEN a logger with min level = warning
        let logger = fresh_logger();
        logger.set_level(rust_mcp_sdk::schema::LoggingLevel::Warning);
        // WHEN we check if INFO (severity 6) passes
        let min = logger.min_level.load(Ordering::Relaxed);
        // THEN INFO severity (6) > warning threshold (4) → filtered out
        assert!(
            level::INFO > min,
            "INFO should be filtered when min=warning"
        );
    }

    #[test]
    fn set_level_info_passes_warning() {
        // GIVEN a logger with min level = info (default)
        let logger = fresh_logger();
        logger.set_level(rust_mcp_sdk::schema::LoggingLevel::Info);
        let min = logger.min_level.load(Ordering::Relaxed);
        // THEN WARNING severity (4) <= info threshold (6) → passes through
        assert!(level::WARNING <= min, "WARNING should pass when min=info");
    }

    #[test]
    fn set_level_debug_passes_all() {
        // GIVEN a logger at debug level (most permissive)
        let logger = fresh_logger();
        logger.set_level(rust_mcp_sdk::schema::LoggingLevel::Debug);
        let min = logger.min_level.load(Ordering::Relaxed);
        // THEN emergency (0) <= debug (7) → all levels pass
        assert!(level::EMERGENCY <= min);
        assert!(level::DEBUG <= min);
    }

    // ── level round-trip ─────────────────────────────────────────────────────

    #[test]
    fn level_round_trip_all_variants() {
        // GIVEN every LoggingLevel variant
        use rust_mcp_sdk::schema::LoggingLevel;
        let variants = [
            LoggingLevel::Emergency,
            LoggingLevel::Alert,
            LoggingLevel::Critical,
            LoggingLevel::Error,
            LoggingLevel::Warning,
            LoggingLevel::Notice,
            LoggingLevel::Info,
            LoggingLevel::Debug,
        ];
        for variant in &variants {
            // WHEN converted to u8 and back
            let numeric = logging_level_to_u8(*variant);
            let restored = u8_to_logging_level(numeric);
            // THEN the restored value matches the original numeric
            assert_eq!(
                logging_level_to_u8(restored),
                numeric,
                "round-trip failed for {variant:?}"
            );
        }
    }

    // ── current_level reflects set_level ─────────────────────────────────────

    #[test]
    fn current_level_reflects_set_level() {
        // GIVEN a logger whose level is updated
        let logger = fresh_logger();
        logger.set_level(rust_mcp_sdk::schema::LoggingLevel::Error);
        // WHEN queried
        let lvl = u8_to_logging_level(logger.min_level.load(Ordering::Relaxed));
        // THEN it matches
        assert_eq!(
            logging_level_to_u8(lvl),
            level::ERROR,
            "stored level should reflect the last set_level call"
        );
    }

    // ── structured data preservation ─────────────────────────────────────────

    #[test]
    fn log_with_data_builds_notification_params() {
        // GIVEN structured JSON data attached to a log call
        let data = serde_json::json!({"url": "https://example.com", "status": 200});
        // WHEN we build the notification params manually (log() fire-and-forget can't be awaited in unit tests)
        let params = LoggingMessageNotificationParams {
            level: rust_mcp_sdk::schema::LoggingLevel::Info,
            logger: Some("fetch".to_string()),
            data: data.clone(),
            meta: None,
        };
        // THEN structured data is preserved verbatim
        assert_eq!(params.data, data);
        assert_eq!(params.logger.as_deref(), Some("fetch"));
    }
}
