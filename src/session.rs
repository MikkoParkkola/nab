//! Persistent named sessions for the `nab-mcp` server.
//!
//! A [`SessionStore`] holds up to [`MAX_SESSIONS`] named sessions. Each session
//! owns an isolated [`reqwest::cookie::Jar`] and a [`reqwest::Client`] built
//! from that jar, so cookies set by `Set-Cookie` response headers are
//! automatically retained and sent on subsequent requests within the same
//! session.
//!
//! **LRU eviction**: when the store is full (32 sessions), the
//! least-recently-used session is dropped before inserting the new one.
//!
//! **Cookie seeding**: when a `Cookie:` request header string is provided at
//! session creation, each `name=value` pair is synthesised into a
//! `Set-Cookie` header scoped to the seed URL's domain/path and inserted
//! into the jar. This lets browser cookies (extracted from Brave/Chrome/etc.)
//! bootstrap an authenticated session.
//!
//! # Example
//!
//! ```rust,no_run
//! use nab::session::SessionStore;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let store = SessionStore::new();
//! let entry = store.get_or_create("github", None, None).await?;
//! // Use entry.client for requests — cookies persist automatically.
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use reqwest::Client;
use tokio::sync::RwLock;

use crate::fingerprint::{BrowserProfile, random_profile};

/// Maximum number of concurrent named sessions held in memory.
pub const MAX_SESSIONS: usize = 32;

/// A single named session: a dedicated HTTP client with its own cookie jar
/// and a pinned browser fingerprint.
#[derive(Clone)]
pub struct SessionEntry {
    /// HTTP client built against this session's cookie jar.
    pub client: Client,
    /// The browser fingerprint pinned at session creation.
    pub profile: BrowserProfile,
    /// Shared cookie jar (also owned by `client`).
    pub jar: Arc<reqwest::cookie::Jar>,
    /// Monotonic timestamp of the last access (for LRU eviction).
    pub last_used: Instant,
    /// Monotonic timestamp of session creation.
    pub created_at: Instant,
}

/// Thread-safe store of named [`SessionEntry`] values with LRU eviction.
pub struct SessionStore {
    inner: Arc<RwLock<HashMap>>,
}

/// Internal alias to keep the type tidy.
type HashMap = std::collections::HashMap<String, SessionEntry>;

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Return the session named `name`, creating it if it does not exist.
    ///
    /// `seed_cookies` — optional `Cookie:` header string (`"SID=abc; HSID=def"`)
    /// used to pre-populate the jar. Only applied at *creation*; ignored on
    /// subsequent calls for the same session name.
    ///
    /// `seed_url` — the URL whose domain/path is used when synthesising
    /// `Set-Cookie` headers for the seed cookies. Defaults to `"/"` path and
    /// the domain parsed from the string when `None`.
    ///
    /// Returns an error if `name` fails validation.
    pub async fn get_or_create(
        &self,
        name: &str,
        seed_cookies: Option<&str>,
        seed_url: Option<&str>,
    ) -> Result<SessionEntry> {
        validate_session_name(name)?;

        // Fast path: session already exists.
        {
            let mut map = self.inner.write().await;
            if let Some(entry) = map.get_mut(name) {
                entry.last_used = Instant::now();
                return Ok(entry.clone());
            }
        }

        // Slow path: build a new session.
        let entry = build_session_entry(seed_cookies, seed_url)?;

        let mut map = self.inner.write().await;

        // Re-check after acquiring write lock — another task may have raced us.
        if let Some(existing) = map.get_mut(name) {
            existing.last_used = Instant::now();
            return Ok(existing.clone());
        }

        // Evict LRU if at capacity.
        if map.len() >= MAX_SESSIONS {
            evict_lru(&mut map);
        }

        map.insert(name.to_owned(), entry.clone());
        Ok(entry)
    }

    /// Update `last_used` for an existing session; no-op if not found.
    pub async fn touch(&self, name: &str) {
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get_mut(name) {
            entry.last_used = Instant::now();
        }
    }

    /// Number of active sessions (for tests / diagnostics).
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    /// True when the store has no sessions.
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    /// True when a session with the given name currently exists.
    pub async fn contains(&self, name: &str) -> bool {
        self.inner.read().await.contains_key(name)
    }
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Validate a session name: 1-64 chars, ASCII alphanumeric + `-` + `_` only.
pub(crate) fn validate_session_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!(
            "session name must be 1-64 characters, got {} chars",
            name.len()
        );
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!(
            "session name must contain only alphanumeric characters, hyphens, or underscores: \
             '{name}'"
        );
    }
    Ok(())
}

/// Build a new [`SessionEntry`] with an optional seed cookie string.
fn build_session_entry(seed_cookies: Option<&str>, seed_url: Option<&str>) -> Result<SessionEntry> {
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let profile = random_profile();
    let headers = profile.to_headers();

    if let Some(cookie_str) = seed_cookies {
        seed_jar(&jar, cookie_str, seed_url);
    }

    let client = Client::builder()
        .pool_max_idle_per_host(5)
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(60))
        .tcp_nodelay(true)
        .use_rustls_tls()
        .brotli(true)
        .zstd(true)
        .gzip(true)
        .deflate(true)
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10))
        .http2_adaptive_window(true)
        .cookie_provider(jar.clone())
        .build()?;

    let now = Instant::now();
    Ok(SessionEntry {
        client,
        profile,
        jar,
        last_used: now,
        created_at: now,
    })
}

/// Parse a `Cookie:` header string and add each pair to `jar` scoped to `seed_url`.
///
/// `Cookie:` format: `"name1=value1; name2=value2"`.
/// `reqwest::cookie::Jar::add_cookie_str` expects `Set-Cookie` format:
/// `"name=value; Domain=example.com; Path=/"`.
fn seed_jar(jar: &reqwest::cookie::Jar, cookie_str: &str, seed_url: Option<&str>) {
    let url_str = seed_url.unwrap_or("https://localhost/");
    let Ok(url) = url_str.parse::<url::Url>() else {
        return;
    };

    let domain = url.host_str().unwrap_or("localhost");
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };

    for pair in cookie_str.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        // Must contain `=` to be a valid name=value pair.
        if !pair.contains('=') {
            continue;
        }
        let set_cookie = format!("{pair}; Domain={domain}; Path={path}");
        jar.add_cookie_str(&set_cookie, &url);
    }
}

/// Remove the session with the oldest `last_used` timestamp from `map`.
fn evict_lru(map: &mut HashMap) {
    let lru_key = map
        .iter()
        .min_by_key(|(_, v)| v.last_used)
        .map(|(k, _)| k.clone());

    if let Some(key) = lru_key {
        map.remove(&key);
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    // ── name validation ──────────────────────────────────────────────────────

    #[test]
    fn validate_name_accepts_alphanumeric() {
        assert!(validate_session_name("abc123").is_ok());
    }

    #[test]
    fn validate_name_accepts_hyphens_and_underscores() {
        assert!(validate_session_name("my-session_v2").is_ok());
    }

    #[test]
    fn validate_name_accepts_single_char() {
        assert!(validate_session_name("a").is_ok());
    }

    #[test]
    fn validate_name_accepts_64_chars() {
        let name = "a".repeat(64);
        assert!(validate_session_name(&name).is_ok());
    }

    #[test]
    fn validate_name_rejects_empty() {
        let err = validate_session_name("").unwrap_err();
        assert!(err.to_string().contains("1-64"));
    }

    #[test]
    fn validate_name_rejects_65_chars() {
        let name = "a".repeat(65);
        let err = validate_session_name(&name).unwrap_err();
        assert!(err.to_string().contains("65 chars"));
    }

    #[test]
    fn validate_name_rejects_slash() {
        let err = validate_session_name("../../etc").unwrap_err();
        assert!(err.to_string().contains("alphanumeric"));
    }

    #[test]
    fn validate_name_rejects_dot() {
        assert!(validate_session_name("session.name").is_err());
    }

    #[test]
    fn validate_name_rejects_space() {
        assert!(validate_session_name("my session").is_err());
    }

    #[test]
    fn validate_name_rejects_at_sign() {
        assert!(validate_session_name("user@host").is_err());
    }

    // ── session creation ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_or_create_creates_new_session() {
        let store = SessionStore::new();
        let entry = store.get_or_create("test", None, None).await.unwrap();
        assert!(!entry.profile.user_agent.is_empty());
    }

    #[tokio::test]
    async fn get_or_create_returns_same_session_on_repeat_call() {
        let store = SessionStore::new();
        let _e1 = store.get_or_create("s1", None, None).await.unwrap();
        let _e2 = store.get_or_create("s1", None, None).await.unwrap();
        // Store should still have exactly one entry.
        assert_eq!(store.len().await, 1);
    }

    #[tokio::test]
    async fn get_or_create_different_names_gives_different_jars() {
        let store = SessionStore::new();
        let e1 = store.get_or_create("s1", None, None).await.unwrap();
        let e2 = store.get_or_create("s2", None, None).await.unwrap();
        // Two distinct Arc pointers means different jars.
        assert!(!Arc::ptr_eq(&e1.jar, &e2.jar));
        assert_eq!(store.len().await, 2);
    }

    #[tokio::test]
    async fn get_or_create_invalid_name_returns_error() {
        let store = SessionStore::new();
        assert!(store.get_or_create("bad name", None, None).await.is_err());
        assert!(store.get_or_create("", None, None).await.is_err());
        assert!(store.get_or_create("../../etc", None, None).await.is_err());
    }

    // ── LRU eviction ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn lru_eviction_at_capacity() {
        let store = SessionStore::new();

        // Fill to capacity.
        for i in 0..MAX_SESSIONS {
            store
                .get_or_create(&format!("session-{i}"), None, None)
                .await
                .unwrap();
        }
        assert_eq!(store.len().await, MAX_SESSIONS);

        // The first session is now the least-recently-used.
        // Creating a new one should evict it.
        store
            .get_or_create("new-session", None, None)
            .await
            .unwrap();
        assert_eq!(store.len().await, MAX_SESSIONS);
        assert!(!store.contains("session-0").await);
        assert!(store.contains("new-session").await);
    }

    #[tokio::test]
    async fn lru_eviction_preserves_recently_touched_session() {
        let store = SessionStore::new();

        // Fill to capacity.
        for i in 0..MAX_SESSIONS {
            store
                .get_or_create(&format!("session-{i}"), None, None)
                .await
                .unwrap();
        }

        // Touch session-0 so it is no longer LRU.
        // We need a brief pause to ensure the timestamp ordering is stable.
        tokio::time::sleep(Duration::from_millis(1)).await;
        store.touch("session-0").await;

        // session-1 is now the LRU (it was created right after session-0 and
        // has not been touched since).
        store.get_or_create("evict-test", None, None).await.unwrap();
        assert_eq!(store.len().await, MAX_SESSIONS);
        assert!(
            store.contains("session-0").await,
            "session-0 should survive"
        );
        assert!(
            !store.contains("session-1").await,
            "session-1 should be evicted"
        );
    }

    // ── touch ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn touch_updates_last_used() {
        let store = SessionStore::new();
        store.get_or_create("s", None, None).await.unwrap();

        let before = {
            let map = store.inner.read().await;
            map["s"].last_used
        };

        tokio::time::sleep(Duration::from_millis(2)).await;
        store.touch("s").await;

        let after = {
            let map = store.inner.read().await;
            map["s"].last_used
        };

        assert!(after > before, "last_used should advance after touch");
    }

    #[tokio::test]
    async fn touch_on_nonexistent_session_is_noop() {
        let store = SessionStore::new();
        // Should not panic.
        store.touch("does-not-exist").await;
        assert!(store.is_empty().await);
    }

    // ── cookie seeding ───────────────────────────────────────────────────────

    #[test]
    fn seed_jar_populates_from_cookie_header() {
        let jar = reqwest::cookie::Jar::default();
        let url: url::Url = "https://example.com/path".parse().unwrap();
        seed_jar(&jar, "SID=abc; HSID=def", Some("https://example.com/path"));

        // Verify the jar returns cookies for the seeded domain.
        use reqwest::cookie::CookieStore as _;
        let headers = jar.cookies(&url);
        assert!(headers.is_some(), "jar should have cookies for example.com");
        let hdr = headers.unwrap().to_str().unwrap().to_string();
        assert!(hdr.contains("SID=abc") || hdr.contains("HSID=def"));
    }

    #[test]
    fn seed_jar_handles_single_cookie() {
        let jar = reqwest::cookie::Jar::default();
        seed_jar(&jar, "token=xyz", Some("https://api.example.com/"));

        use reqwest::cookie::CookieStore as _;
        let url: url::Url = "https://api.example.com/endpoint".parse().unwrap();
        let headers = jar.cookies(&url);
        assert!(headers.is_some());
    }

    #[test]
    fn seed_jar_ignores_empty_pairs() {
        // Should not panic on malformed/empty input.
        let jar = reqwest::cookie::Jar::default();
        seed_jar(&jar, "; ; ;", Some("https://example.com/"));
        // No crash = pass.
    }

    #[test]
    fn seed_jar_ignores_pairs_without_equals() {
        let jar = reqwest::cookie::Jar::default();
        seed_jar(&jar, "notavalidcookie", Some("https://example.com/"));
        // No crash = pass.
    }

    #[test]
    fn seed_jar_uses_localhost_for_invalid_url() {
        // Should not panic even if seed_url is garbage.
        let jar = reqwest::cookie::Jar::default();
        seed_jar(&jar, "a=b", Some("not-a-url"));
        // No crash = pass.
    }

    #[test]
    fn seed_jar_with_no_seed_url_uses_localhost() {
        let jar = reqwest::cookie::Jar::default();
        seed_jar(&jar, "a=b", None);
        // No crash = pass.
    }

    // ── concurrent access ────────────────────────────────────────────────────

    #[tokio::test]
    async fn concurrent_get_or_create_is_safe() {
        use std::sync::Arc;

        let store = Arc::new(SessionStore::new());
        let mut handles = Vec::new();

        for i in 0..20 {
            let store_clone = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                store_clone
                    .get_or_create(&format!("concurrent-{i}"), None, None)
                    .await
                    .unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(store.len().await, 20);
    }

    #[tokio::test]
    async fn concurrent_same_session_name_creates_exactly_one() {
        use std::sync::Arc;

        let store = Arc::new(SessionStore::new());
        let mut handles = Vec::new();

        for _ in 0..10 {
            let store_clone = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                store_clone
                    .get_or_create("shared", None, None)
                    .await
                    .unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(store.len().await, 1);
    }

    // ── contains / is_empty helpers ──────────────────────────────────────────

    #[tokio::test]
    async fn is_empty_true_when_no_sessions() {
        let store = SessionStore::new();
        assert!(store.is_empty().await);
    }

    #[tokio::test]
    async fn contains_false_for_unknown_session() {
        let store = SessionStore::new();
        assert!(!store.contains("ghost").await);
    }

    #[tokio::test]
    async fn contains_true_after_creation() {
        let store = SessionStore::new();
        store.get_or_create("present", None, None).await.unwrap();
        assert!(store.contains("present").await);
    }

    // ── build_session_entry ──────────────────────────────────────────────────

    #[test]
    fn build_session_entry_without_seed_cookies_succeeds() {
        let entry = build_session_entry(None, None).unwrap();
        assert!(!entry.profile.user_agent.is_empty());
    }

    #[test]
    fn build_session_entry_with_seed_cookies_succeeds() {
        let entry =
            build_session_entry(Some("sid=abc; token=xyz"), Some("https://example.com/")).unwrap();
        assert!(!entry.profile.user_agent.is_empty());
        // Jar should have content for the seeded domain.
        use reqwest::cookie::CookieStore as _;
        let url: url::Url = "https://example.com/".parse().unwrap();
        assert!(entry.jar.cookies(&url).is_some());
    }
}
