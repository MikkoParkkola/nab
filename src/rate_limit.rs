//! Per-domain rate limiter for concurrent HTTP fetching.
//!
//! Enforces a minimum delay between consecutive requests to the same domain,
//! preventing rate-limit errors while maximising throughput across different
//! domains.
//!
//! # Example
//!
//! ```rust
//! use std::time::Duration;
//! use nab::rate_limit::DomainRateLimiter;
//!
//! # async fn example() {
//! let limiter = DomainRateLimiter::new(Duration::from_millis(500));
//!
//! // First request to example.com proceeds immediately.
//! limiter.wait("example.com").await;
//!
//! // Second request to example.com waits ~500ms.
//! limiter.wait("example.com").await;
//!
//! // Request to other.com proceeds immediately (different domain).
//! limiter.wait("other.com").await;
//! # }
//! ```

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// A rate limiter that enforces a minimum interval between requests to the
/// same domain.
///
/// Thread-safe and designed for use with `tokio::spawn`.  Different domains
/// are independent — only requests to the *same* domain are throttled.
pub struct DomainRateLimiter {
    min_interval: Duration,
    last_request: Mutex<HashMap<String, Instant>>,
}

impl DomainRateLimiter {
    /// Create a new limiter with the given minimum interval between
    /// same-domain requests.
    #[must_use]
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last_request: Mutex::new(HashMap::new()),
        }
    }

    /// Wait until the rate limit allows a request to `domain`.
    ///
    /// If this is the first request to `domain`, returns immediately.
    /// Otherwise, sleeps until `min_interval` has elapsed since the last
    /// request to the same domain.
    pub async fn wait(&self, domain: &str) {
        let sleep_duration = {
            let mut map = self.last_request.lock().await;
            let now = Instant::now();

            if let Some(last) = map.get(domain) {
                let elapsed = now.duration_since(*last);
                if elapsed < self.min_interval {
                    let wait = self.min_interval.saturating_sub(elapsed);
                    // Update the timestamp to when we'll actually send.
                    map.insert(domain.to_owned(), now + wait);
                    Some(wait)
                } else {
                    map.insert(domain.to_owned(), now);
                    None
                }
            } else {
                map.insert(domain.to_owned(), now);
                None
            }
        };

        if let Some(d) = sleep_duration {
            tokio::time::sleep(d).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_request_proceeds_immediately() {
        let limiter = DomainRateLimiter::new(Duration::from_secs(10));
        let start = Instant::now();
        limiter.wait("example.com").await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn second_request_to_same_domain_is_delayed() {
        let limiter = DomainRateLimiter::new(Duration::from_millis(100));
        limiter.wait("example.com").await;

        let start = Instant::now();
        limiter.wait("example.com").await;
        assert!(start.elapsed() >= Duration::from_millis(80));
    }

    #[tokio::test]
    async fn different_domains_are_independent() {
        let limiter = DomainRateLimiter::new(Duration::from_secs(10));
        limiter.wait("a.com").await;

        let start = Instant::now();
        limiter.wait("b.com").await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn delay_expires_after_interval() {
        let limiter = DomainRateLimiter::new(Duration::from_millis(50));
        limiter.wait("example.com").await;

        tokio::time::sleep(Duration::from_millis(60)).await;

        let start = Instant::now();
        limiter.wait("example.com").await;
        assert!(start.elapsed() < Duration::from_millis(20));
    }
}
