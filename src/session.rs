//! Persistent named sessions for the `nab-mcp` server.
//!
//! A [`SessionStore`] holds up to [`MAX_SESSIONS`] named sessions. Each session
//! owns an isolated cookie store and a dedicated [`reqwest::Client`] built
//! against that store, so cookies set by `Set-Cookie` response headers are
//! automatically retained and sent on subsequent requests within the same
//! session.
//!
//! **Encrypted persistence**: on non-Windows platforms, named sessions are saved under
//! `~/.nab/sessions/<name>.json` as AES-256-GCM encrypted envelopes. The
//! per-file data key is derived with Argon2id from a locally generated master
//! secret stored at `~/.nab/session-key`. On Windows, sessions currently stay
//! in memory only until per-user ACL hardening is implemented.
//!
//! **LRU eviction**: when the in-memory store is full (32 sessions), the
//! least-recently-used session is flushed to disk before being dropped.
//!
//! **Cookie seeding**: when a `Cookie:` request header string is provided at
//! session creation, each `name=value` pair is synthesised into a response
//! cookie scoped to the seed URL's domain/path and inserted into the store.
//! This lets browser cookies (extracted from Brave/Chrome/etc.) bootstrap an
//! authenticated session.
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

use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit, OsRng, Payload, rand_core::RngCore};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use cookie_store::{Cookie as StoredCookie, CookieStore as PersistentCookieStore, RawCookie};
use reqwest::Client;
use reqwest_cookie_store::CookieStoreMutex;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tokio::sync::RwLock;

use crate::fingerprint::{BrowserProfile, random_profile};

#[cfg(not(test))]
const NAB_STATE_DIR: &str = ".nab";
const SESSION_DIR_NAME: &str = "sessions";
const SESSION_KEY_FILE: &str = "session-key";
const SESSION_FILE_VERSION: u8 = 1;
const SESSION_CIPHER: &str = "aes-256-gcm";
const SESSION_KDF: &str = "argon2id";
const SESSION_KEY_LEN: usize = 32;
const SESSION_SALT_LEN: usize = 16;
const SESSION_NONCE_LEN: usize = 12;
const ARGON2_MEMORY_COST_KIB: u32 = 64 * 1024;
const ARGON2_TIME_COST: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;

/// Maximum number of concurrent named sessions held in memory.
pub const MAX_SESSIONS: usize = 32;

type SessionJar = CookieStoreMutex;

/// A single named session: a dedicated HTTP client with its own cookie store
/// and a pinned browser fingerprint.
#[derive(Clone)]
pub struct SessionEntry {
    /// HTTP client built against this session's cookie store.
    pub client: Client,
    /// The browser fingerprint pinned at session creation.
    pub profile: BrowserProfile,
    /// Shared cookie store (also owned by `client`).
    pub jar: Arc<SessionJar>,
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

#[derive(Debug, Serialize, Deserialize)]
struct PersistedSessionEnvelope {
    version: u8,
    cipher: String,
    kdf: String,
    salt_hex: String,
    nonce_hex: String,
    ciphertext_hex: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedSessionPayload {
    profile: BrowserProfile,
    cookies: Vec<StoredCookie<'static>>,
}

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
    /// used to pre-populate the store. Only applied at *creation*; ignored on
    /// subsequent calls for the same session name.
    ///
    /// `seed_url` — the URL whose domain/path is used when synthesising response
    /// cookies for the seed cookies. Defaults to `https://localhost/` when `None`.
    ///
    /// Returns an error if `name` fails validation or a persisted session cannot
    /// be decrypted/loaded.
    pub async fn get_or_create(
        &self,
        name: &str,
        seed_cookies: Option<&str>,
        seed_url: Option<&str>,
    ) -> Result<SessionEntry> {
        validate_session_name(name)?;

        {
            let mut map = self.inner.write().await;
            if let Some(entry) = map.get_mut(name) {
                entry.last_used = Instant::now();
                return Ok(entry.clone());
            }
        }

        if let Some(entry) = load_session_entry(name)? {
            let mut map = self.inner.write().await;
            if let Some(existing) = map.get_mut(name) {
                existing.last_used = Instant::now();
                return Ok(existing.clone());
            }
            map.insert(name.to_owned(), entry.clone());
            return Ok(entry);
        }

        let entry = build_session_entry(seed_cookies, seed_url)?;

        let mut map = self.inner.write().await;
        if let Some(existing) = map.get_mut(name) {
            existing.last_used = Instant::now();
            return Ok(existing.clone());
        }

        persist_session_entry(name, &entry)?;
        if map.len() >= MAX_SESSIONS {
            evict_lru(&mut map);
        }

        map.insert(name.to_owned(), entry.clone());
        Ok(entry)
    }

    /// Persist the current in-memory state of `name` to its encrypted session file.
    pub async fn persist(&self, name: &str) -> Result<()> {
        validate_session_name(name)?;
        let entry = {
            let map = self.inner.read().await;
            map.get(name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("No active session named '{name}'"))?
        };
        persist_session_entry(name, &entry)
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

    /// True when a session with the given name currently exists in memory.
    pub async fn contains(&self, name: &str) -> bool {
        self.inner.read().await.contains_key(name)
    }
}

/// Return the on-disk session directory (`~/.nab/sessions`).
pub fn get_session_dir() -> Result<PathBuf> {
    Ok(session_dir_for_state_dir(&nab_state_dir()?))
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

#[cfg(test)]
static TEST_STATE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    std::env::temp_dir().join(format!(
        "nab-session-tests-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ))
});

#[cfg(not(test))]
fn nab_state_dir() -> Result<PathBuf> {
    if let Some(override_dir) = std::env::var_os("NAB_STATE_DIR") {
        return Ok(PathBuf::from(override_dir));
    }

    let home = dirs::home_dir().context("Could not find home directory")?;
    Ok(home.join(NAB_STATE_DIR))
}

// Allow: must match the non-test signature so callers compile under both cfgs.
#[cfg(test)]
#[allow(clippy::unnecessary_wraps)]
fn nab_state_dir() -> Result<PathBuf> {
    Ok(TEST_STATE_DIR.clone())
}

fn session_dir_for_state_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(SESSION_DIR_NAME)
}

fn session_file_path(session_dir: &Path, name: &str) -> PathBuf {
    session_dir.join(format!("{name}.json"))
}

fn session_key_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SESSION_KEY_FILE)
}

fn session_aad(name: &str) -> String {
    format!("nab-session:{name}:v{SESSION_FILE_VERSION}")
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("Failed to create directory '{}'", path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "Failed to tighten directory permissions on '{}'",
            path.display()
        )
    })?;
    Ok(())
}

fn ensure_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("Failed to tighten file permissions on '{}'", path.display()))?;
    Ok(())
}

fn read_master_secret_file(key_path: &Path) -> Result<[u8; SESSION_KEY_LEN]> {
    let secret =
        fs::read(key_path).with_context(|| format!("Failed to read '{}'", key_path.display()))?;
    anyhow::ensure!(
        secret.len() == SESSION_KEY_LEN,
        "Session master key at '{}' must be {} bytes, got {}",
        key_path.display(),
        SESSION_KEY_LEN,
        secret.len()
    );
    ensure_private_file(key_path)?;
    let mut out = [0u8; SESSION_KEY_LEN];
    out.copy_from_slice(&secret);
    Ok(out)
}

fn load_or_create_master_secret_in(state_dir: &Path) -> Result<[u8; SESSION_KEY_LEN]> {
    ensure_private_dir(state_dir)?;
    let key_path = session_key_path(state_dir);

    if key_path.exists() {
        return read_master_secret_file(&key_path);
    }

    let mut secret = [0u8; SESSION_KEY_LEN];
    OsRng.fill_bytes(&mut secret);

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);

    match options.open(&key_path) {
        Ok(mut file) => {
            file.write_all(&secret)?;
            file.flush()?;
            file.sync_all()?;
            ensure_private_file(&key_path)?;
            Ok(secret)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            read_master_secret_file(&key_path)
        }
        Err(error) => {
            Err(error).with_context(|| format!("Failed to create '{}'", key_path.display()))
        }
    }
}

fn derive_session_key(master_secret: &[u8], salt: &[u8]) -> Result<[u8; SESSION_KEY_LEN]> {
    let params = Params::new(
        ARGON2_MEMORY_COST_KIB,
        ARGON2_TIME_COST,
        ARGON2_PARALLELISM,
        Some(SESSION_KEY_LEN),
    )
    .map_err(|e| anyhow::anyhow!("Invalid Argon2id session key parameters: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = [0u8; SESSION_KEY_LEN];
    argon2
        .hash_password_into(master_secret, salt, &mut key)
        .map_err(|e| anyhow::anyhow!("Argon2id session key derivation failed: {e}"))?;
    Ok(key)
}

fn decode_fixed_hex<const N: usize>(label: &str, value: &str) -> Result<[u8; N]> {
    let decoded = hex::decode(value).with_context(|| format!("Failed to decode {label} hex"))?;
    anyhow::ensure!(
        decoded.len() == N,
        "{label} must decode to {N} bytes, got {}",
        decoded.len()
    );
    let mut out = [0u8; N];
    out.copy_from_slice(&decoded);
    Ok(out)
}

fn encrypt_session_payload(
    name: &str,
    payload: &PersistedSessionPayload,
    state_dir: &Path,
) -> Result<PersistedSessionEnvelope> {
    let master_secret = load_or_create_master_secret_in(state_dir)?;

    let mut salt = [0u8; SESSION_SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let key = derive_session_key(&master_secret, &salt)?;

    let mut nonce = [0u8; SESSION_NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);

    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| anyhow::anyhow!("AES-256-GCM key setup failed: {e}"))?;
    let plaintext =
        serde_json::to_vec(payload).context("Failed to serialize session payload to JSON")?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: session_aad(name).as_bytes(),
            },
        )
        .map_err(|_| anyhow::anyhow!("AES-256-GCM session encryption failed"))?;

    Ok(PersistedSessionEnvelope {
        version: SESSION_FILE_VERSION,
        cipher: SESSION_CIPHER.to_string(),
        kdf: SESSION_KDF.to_string(),
        salt_hex: hex::encode(salt),
        nonce_hex: hex::encode(nonce),
        ciphertext_hex: hex::encode(ciphertext),
    })
}

fn decrypt_session_payload(
    name: &str,
    envelope: &PersistedSessionEnvelope,
    state_dir: &Path,
) -> Result<PersistedSessionPayload> {
    anyhow::ensure!(
        envelope.version == SESSION_FILE_VERSION,
        "Unsupported session file version {} (expected {})",
        envelope.version,
        SESSION_FILE_VERSION
    );
    anyhow::ensure!(
        envelope.cipher == SESSION_CIPHER,
        "Unsupported session cipher '{}' (expected '{}')",
        envelope.cipher,
        SESSION_CIPHER
    );
    anyhow::ensure!(
        envelope.kdf == SESSION_KDF,
        "Unsupported session KDF '{}' (expected '{}')",
        envelope.kdf,
        SESSION_KDF
    );

    let salt = decode_fixed_hex::<SESSION_SALT_LEN>("session salt", &envelope.salt_hex)?;
    let nonce = decode_fixed_hex::<SESSION_NONCE_LEN>("session nonce", &envelope.nonce_hex)?;
    let ciphertext =
        hex::decode(&envelope.ciphertext_hex).context("Failed to decode session ciphertext hex")?;

    let master_secret = load_or_create_master_secret_in(state_dir)?;
    let key = derive_session_key(&master_secret, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| anyhow::anyhow!("AES-256-GCM key setup failed: {e}"))?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: session_aad(name).as_bytes(),
            },
        )
        .map_err(|_| anyhow::anyhow!("AES-256-GCM session decryption failed"))?;

    serde_json::from_slice(&plaintext).context("Failed to deserialize decrypted session payload")
}

fn build_reqwest_client(profile: &BrowserProfile, jar: Arc<SessionJar>) -> Result<Client> {
    Client::builder()
        .pool_max_idle_per_host(5)
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_mins(1))
        .tcp_nodelay(true)
        .use_rustls_tls()
        .brotli(true)
        .zstd(true)
        .gzip(true)
        .deflate(true)
        .default_headers(profile.to_headers())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10))
        .http2_adaptive_window(true)
        .cookie_provider(jar)
        .build()
        .context("Failed to build session reqwest client")
}

/// Build a new [`SessionEntry`] with an optional seed cookie string.
fn build_session_entry(seed_cookies: Option<&str>, seed_url: Option<&str>) -> Result<SessionEntry> {
    let jar = Arc::new(SessionJar::new(PersistentCookieStore::default()));
    let profile = random_profile();

    if let Some(cookie_str) = seed_cookies {
        seed_jar(&jar, cookie_str, seed_url)?;
    }

    let client = build_reqwest_client(&profile, Arc::clone(&jar))?;
    let now = Instant::now();
    Ok(SessionEntry {
        client,
        profile,
        jar,
        last_used: now,
        created_at: now,
    })
}

/// Build an isolated, non-persisted session entry for one-off call flows.
#[doc(hidden)]
pub fn build_transient_entry(
    seed_cookies: Option<&str>,
    seed_url: Option<&str>,
) -> Result<SessionEntry> {
    build_session_entry(seed_cookies, seed_url)
}

#[cfg(all(not(test), not(windows)))]
fn persist_session_entry(name: &str, entry: &SessionEntry) -> Result<()> {
    let state_dir = nab_state_dir()?;
    persist_session_entry_in(name, entry, &state_dir)
}

// Allow: must match the non-test/non-windows signature so callers compile under all cfgs.
#[cfg(any(test, windows))]
#[allow(clippy::unnecessary_wraps)]
fn persist_session_entry(_name: &str, _entry: &SessionEntry) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn persist_session_entry_in(name: &str, entry: &SessionEntry, state_dir: &Path) -> Result<()> {
    validate_session_name(name)?;
    ensure_private_dir(state_dir)?;
    let session_dir = session_dir_for_state_dir(state_dir);
    ensure_private_dir(&session_dir)?;

    let cookies = {
        let store = entry
            .jar
            .lock()
            .map_err(|_| anyhow::anyhow!("session cookie store lock poisoned"))?;
        store
            .iter_any()
            .filter(|cookie| !cookie.is_expired())
            .cloned()
            .collect::<Vec<_>>()
    };
    let payload = PersistedSessionPayload {
        profile: entry.profile.clone(),
        cookies,
    };
    let envelope = encrypt_session_payload(name, &payload, state_dir)?;
    let serialized =
        serde_json::to_vec_pretty(&envelope).context("Failed to serialize session envelope")?;

    let session_path = session_file_path(&session_dir, name);
    let mut tmp = NamedTempFile::new_in(&session_dir)
        .with_context(|| format!("Failed to create temp file in '{}'", session_dir.display()))?;
    tmp.write_all(&serialized)?;
    tmp.write_all(b"\n")?;
    tmp.flush()?;
    ensure_private_file(tmp.path())?;
    tmp.as_file().sync_all()?;
    tmp.persist(&session_path)
        .with_context(|| format!("Failed to persist '{}'", session_path.display()))?;
    ensure_private_file(&session_path)?;
    Ok(())
}

#[cfg(windows)]
fn persist_session_entry_in(_name: &str, _entry: &SessionEntry, _state_dir: &Path) -> Result<()> {
    Ok(())
}

#[cfg(all(not(test), not(windows)))]
fn load_session_entry(name: &str) -> Result<Option<SessionEntry>> {
    let state_dir = nab_state_dir()?;
    load_session_entry_in(name, &state_dir)
}

// Allow: must match the non-test/non-windows signature so callers compile under all cfgs.
#[cfg(any(test, windows))]
#[allow(clippy::unnecessary_wraps)]
fn load_session_entry(_name: &str) -> Result<Option<SessionEntry>> {
    Ok(None)
}

#[cfg(not(windows))]
fn load_session_entry_in(name: &str, state_dir: &Path) -> Result<Option<SessionEntry>> {
    validate_session_name(name)?;
    let session_dir = session_dir_for_state_dir(state_dir);
    let session_path = session_file_path(&session_dir, name);
    if !session_path.exists() {
        return Ok(None);
    }

    let envelope_bytes = fs::read(&session_path)
        .with_context(|| format!("Failed to read '{}'", session_path.display()))?;
    let envelope: PersistedSessionEnvelope = serde_json::from_slice(&envelope_bytes)
        .with_context(|| format!("Failed to parse '{}'", session_path.display()))?;
    let payload = decrypt_session_payload(name, &envelope, state_dir)?;
    let cookie_store = PersistentCookieStore::from_cookies(
        payload
            .cookies
            .into_iter()
            .map(Ok::<StoredCookie<'static>, anyhow::Error>),
        false,
    )
    .context("Failed to rebuild session cookie store from persisted payload")?;

    let jar = Arc::new(SessionJar::new(cookie_store));
    let client = build_reqwest_client(&payload.profile, Arc::clone(&jar))?;
    let now = Instant::now();
    Ok(Some(SessionEntry {
        client,
        profile: payload.profile,
        jar,
        last_used: now,
        created_at: now,
    }))
}

#[cfg(windows)]
fn load_session_entry_in(_name: &str, _state_dir: &Path) -> Result<Option<SessionEntry>> {
    Ok(None)
}

/// Parse a `Cookie:` header string and add each pair to `jar` scoped to `seed_url`.
///
/// `Cookie:` format: `"name1=value1; name2=value2"`.
/// The pairs are translated into response cookies scoped to the seed URL.
fn seed_jar(jar: &SessionJar, cookie_str: &str, seed_url: Option<&str>) -> Result<()> {
    let url_str = seed_url.unwrap_or("https://localhost/");
    let Ok(url) = url_str.parse::<url::Url>() else {
        return Ok(());
    };

    let domain = url.host_str().unwrap_or("localhost");
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };

    let mut store = jar
        .lock()
        .map_err(|_| anyhow::anyhow!("session cookie store lock poisoned"))?;
    for pair in cookie_str.split(';') {
        let pair = pair.trim();
        if pair.is_empty() || !pair.contains('=') {
            continue;
        }

        let set_cookie = format!("{pair}; Domain={domain}; Path={path}");
        let Ok(cookie) = RawCookie::parse(set_cookie).map(cookie_store::RawCookie::into_owned)
        else {
            continue;
        };
        store.store_response_cookies(std::iter::once(cookie), &url);
    }
    Ok(())
}

/// Remove the session with the oldest `last_used` timestamp from `map`.
fn evict_lru(map: &mut HashMap) {
    let lru_key = map
        .iter()
        .min_by_key(|(_, value)| value.last_used)
        .map(|(key, _)| key.clone());

    if let Some(key) = lru_key {
        map.remove(&key);
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use reqwest::cookie::CookieStore as _;

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
        let jar = SessionJar::new(PersistentCookieStore::default());
        let url: url::Url = "https://example.com/path".parse().unwrap();
        seed_jar(&jar, "SID=abc; HSID=def", Some("https://example.com/path")).unwrap();

        // Verify the jar returns cookies for the seeded domain.
        let headers = jar.cookies(&url);
        assert!(headers.is_some(), "jar should have cookies for example.com");
        let hdr = headers.unwrap().to_str().unwrap().to_string();
        assert!(hdr.contains("SID=abc") || hdr.contains("HSID=def"));
    }

    #[test]
    fn seed_jar_handles_single_cookie() {
        let jar = SessionJar::new(PersistentCookieStore::default());
        seed_jar(&jar, "token=xyz", Some("https://api.example.com/")).unwrap();

        let url: url::Url = "https://api.example.com/endpoint".parse().unwrap();
        let headers = jar.cookies(&url);
        assert!(headers.is_some());
    }

    #[test]
    fn seed_jar_ignores_empty_pairs() {
        // Should not panic on malformed/empty input.
        let jar = SessionJar::new(PersistentCookieStore::default());
        seed_jar(&jar, "; ; ;", Some("https://example.com/")).unwrap();
        // No crash = pass.
    }

    #[test]
    fn seed_jar_ignores_pairs_without_equals() {
        let jar = SessionJar::new(PersistentCookieStore::default());
        seed_jar(&jar, "notavalidcookie", Some("https://example.com/")).unwrap();
        // No crash = pass.
    }

    #[test]
    fn seed_jar_uses_localhost_for_invalid_url() {
        // Should not panic even if seed_url is garbage.
        let jar = SessionJar::new(PersistentCookieStore::default());
        seed_jar(&jar, "a=b", Some("not-a-url")).unwrap();
        // No crash = pass.
    }

    #[test]
    fn seed_jar_with_no_seed_url_uses_localhost() {
        let jar = SessionJar::new(PersistentCookieStore::default());
        seed_jar(&jar, "a=b", None).unwrap();
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
        let url: url::Url = "https://example.com/".parse().unwrap();
        assert!(entry.jar.cookies(&url).is_some());
    }

    // ── encrypted persistence ────────────────────────────────────────────────

    #[cfg(not(windows))]
    #[test]
    fn persist_and_load_session_round_trips_profile_and_cookies() {
        let state_dir = tempfile::tempdir().unwrap();
        let entry =
            build_session_entry(Some("sid=abc; token=xyz"), Some("https://example.com/app"))
                .unwrap();
        let original_profile = entry.profile.clone();

        persist_session_entry_in("saved", &entry, state_dir.path()).unwrap();
        let loaded = load_session_entry_in("saved", state_dir.path())
            .unwrap()
            .expect("persisted session should load");

        assert_eq!(loaded.profile.user_agent, original_profile.user_agent);
        assert_eq!(
            loaded.profile.accept_language,
            original_profile.accept_language
        );

        let url: url::Url = "https://example.com/app".parse().unwrap();
        let headers = loaded
            .jar
            .cookies(&url)
            .expect("loaded jar should contain cookies")
            .to_str()
            .unwrap()
            .to_string();
        assert!(headers.contains("sid=abc"));
        assert!(headers.contains("token=xyz"));
    }

    #[cfg(not(windows))]
    #[test]
    fn persisted_session_file_hides_cookie_values_in_ciphertext() {
        let state_dir = tempfile::tempdir().unwrap();
        let entry =
            build_session_entry(Some("sid=secret-token"), Some("https://example.com/")).unwrap();

        persist_session_entry_in("secret", &entry, state_dir.path()).unwrap();

        let session_path =
            session_file_path(&session_dir_for_state_dir(state_dir.path()), "secret");
        let stored = fs::read_to_string(session_path).unwrap();

        assert!(stored.contains("\"cipher\": \"aes-256-gcm\""));
        assert!(stored.contains("\"kdf\": \"argon2id\""));
        assert!(!stored.contains("sid=secret-token"));
        assert!(!stored.contains("secret-token"));
    }

    #[cfg(not(windows))]
    #[test]
    fn load_session_entry_returns_none_when_file_is_absent() {
        let state_dir = tempfile::tempdir().unwrap();
        assert!(
            load_session_entry_in("missing", state_dir.path())
                .unwrap()
                .is_none()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn load_or_create_master_secret_creates_32_byte_key() {
        let state_dir = tempfile::tempdir().unwrap();
        let key = load_or_create_master_secret_in(state_dir.path()).unwrap();
        let key_path = session_key_path(state_dir.path());

        assert_eq!(key.len(), SESSION_KEY_LEN);
        assert_eq!(fs::read(key_path).unwrap().len(), SESSION_KEY_LEN);
    }
}
