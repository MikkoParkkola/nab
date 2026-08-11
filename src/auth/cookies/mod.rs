// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Browser cookie extraction and credential retrieval.
//!
//! Provides:
//! - [`CookieSource`]: extract cookies from Brave/Chrome/Firefox/Safari
//! - [`CredentialRetriever`]: unified credential lookup across all sources
//!
//! # macOS Cookie Decryption
//!
//! Brave and Chrome encrypt cookies with AES-128-CBC.
//! See [`crypto`] for the full decryption algorithm and constants.

pub use crypto::{decrypt_cookie_value, derive_cookie_key};

mod crypto;
mod db;
pub mod fallback;
pub mod storage_state;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;
#[cfg(target_os = "macos")]
use std::sync::OnceLock;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use super::{Credential, OnePasswordAuth};
use db::{copy_db_to_temp, decrypt_rows, domain_candidates, has_domain_tag, query_cookie_db};
use db::{decrypt_rich_rows, query_cookie_db_rich};
use storage_state::{PlaywrightCookie, SameSite};

/// Controls whether browser-cookie extraction may ask macOS to authorize a
/// Keychain read. Automated callers should set this to `never` so a background
/// fetch cannot open GUI dialogs.
pub const KEYCHAIN_INTERACTION_ENV: &str = "NAB_KEYCHAIN_INTERACTION";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeychainInteraction {
    Allow,
    Never,
}

#[derive(Debug, Clone)]
enum CachedKeychainKey {
    Derived(Vec<u8>),
    InteractiveFailure(String),
}

#[derive(Debug, Default)]
struct KeychainKeyCache {
    by_service: HashMap<&'static str, CachedKeychainKey>,
}

#[cfg(target_os = "macos")]
static KEYCHAIN_KEY_CACHE: OnceLock<Mutex<KeychainKeyCache>> = OnceLock::new();

fn keychain_interaction_from_env_value(value: Option<&str>) -> KeychainInteraction {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None | Some("" | "allow" | "true" | "1" | "yes" | "on") => KeychainInteraction::Allow,
        Some(_) => KeychainInteraction::Never,
    }
}

fn keychain_interaction_from_env_os_value(value: Option<&std::ffi::OsStr>) -> KeychainInteraction {
    match value {
        None => KeychainInteraction::Allow,
        Some(value) => value.to_str().map_or(KeychainInteraction::Never, |value| {
            keychain_interaction_from_env_value(Some(value))
        }),
    }
}

fn configured_keychain_interaction() -> KeychainInteraction {
    keychain_interaction_from_env_os_value(std::env::var_os(KEYCHAIN_INTERACTION_ENV).as_deref())
}

fn resolve_cookie_lookup<F>(
    native: Result<HashMap<String, String>>,
    interaction: KeychainInteraction,
    fallback: F,
) -> Result<HashMap<String, String>>
where
    F: FnOnce() -> Result<HashMap<String, String>>,
{
    match native {
        Ok(cookies) if !cookies.is_empty() => Ok(cookies),
        Err(err) if interaction == KeychainInteraction::Never => Err(err),
        Ok(cookies) if interaction == KeychainInteraction::Never => Ok(cookies),
        Ok(_) | Err(_) => fallback(),
    }
}

fn resolve_cookie_lookup_for_source<F>(
    source: CookieSource,
    native: Result<HashMap<String, String>>,
    interaction: KeychainInteraction,
    fallback: F,
) -> Result<HashMap<String, String>>
where
    F: FnOnce() -> Result<HashMap<String, String>>,
{
    if source.native_cookie_result_is_authoritative() {
        return native;
    }
    resolve_cookie_lookup(native, interaction, fallback)
}

#[cfg(target_os = "macos")]
static KEYCHAIN_UI_LOCK: Mutex<()> = Mutex::new(());

fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn load_cookie_key_cached<F>(
    cache: &Mutex<KeychainKeyCache>,
    service: &'static str,
    interaction: KeychainInteraction,
    load: F,
) -> Result<Vec<u8>>
where
    F: FnOnce() -> Result<Vec<u8>>,
{
    let mut cache = lock_ignoring_poison(cache);
    if let Some(entry) = cache.by_service.get(service) {
        return match entry {
            CachedKeychainKey::Derived(key) => Ok(key.clone()),
            CachedKeychainKey::InteractiveFailure(message) => Err(anyhow::anyhow!(message.clone())),
        };
    }

    match load() {
        Ok(key) => {
            cache
                .by_service
                .insert(service, CachedKeychainKey::Derived(key.clone()));
            Ok(key)
        }
        Err(error) => {
            if interaction == KeychainInteraction::Allow {
                cache.by_service.insert(
                    service,
                    CachedKeychainKey::InteractiveFailure(error.to_string()),
                );
            }
            Err(error)
        }
    }
}

fn load_first_usable_cookie_store<T, F, E>(
    paths: &[std::path::PathBuf],
    mut load: F,
    is_empty: E,
) -> Result<T>
where
    T: Default,
    F: FnMut(&std::path::Path) -> Result<T>,
    E: Fn(&T) -> bool,
{
    let mut first_error = None;
    for path in paths {
        match load(path) {
            Ok(value) if !is_empty(&value) => return Ok(value),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Ok(_) | Err(_) => {}
        }
    }
    first_error.map_or_else(|| Ok(T::default()), Err)
}

fn cookie_rows_need_key(rows: &[db::CookieRow]) -> bool {
    rows.iter()
        .any(|row| row.value.is_empty() && !row.encrypted_bytes.is_empty())
}

fn rich_cookie_rows_need_key(rows: &[db::RichCookieRow]) -> bool {
    rows.iter()
        .any(|row| row.value.is_empty() && !row.encrypted_bytes.is_empty())
}

fn load_cookie_key_if_needed<F>(
    needed: bool,
    interaction: KeychainInteraction,
    load: F,
) -> Result<Option<Vec<u8>>>
where
    F: FnOnce() -> Result<Vec<u8>>,
{
    if !needed {
        return Ok(None);
    }
    match load() {
        Ok(key) => Ok(Some(key)),
        Err(err) if interaction == KeychainInteraction::Never => Err(err),
        Err(_) => Ok(None),
    }
}

fn load_cookie_domain_tag_if_needed<F>(
    needed: bool,
    interaction: KeychainInteraction,
    load: F,
) -> Result<Option<bool>>
where
    F: FnOnce() -> Result<bool>,
{
    if !needed {
        return Ok(None);
    }
    match load() {
        Ok(has_domain_tag) => Ok(Some(has_domain_tag)),
        Err(err) if interaction == KeychainInteraction::Never => Err(err),
        Err(_) => Ok(None),
    }
}

// ─── CookieSource ─────────────────────────────────────────────────────────────

/// Cookie source for browser cookie extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CookieSource {
    Brave,
    Chrome,
    Firefox,
    Safari,
}

impl CookieSource {
    /// Map a browser identifier to the closest supported cookie source.
    ///
    /// `nab` currently has dedicated cookie-store implementations for Brave,
    /// Chrome-family browsers, Firefox, and Safari. Chromium-family names that
    /// do not yet have their own dedicated store implementation (for example
    /// `edge` and `dia`) intentionally fall back to the Chrome-family path.
    /// Unknown names also fall back to Chrome so CLI auto-detect fallback,
    /// browser-family matching, and MCP helper behavior stay aligned.
    #[must_use]
    pub fn from_browser_name(browser: &str) -> Self {
        match browser.to_lowercase().as_str() {
            "brave" => Self::Brave,
            "firefox" => Self::Firefox,
            "safari" => Self::Safari,
            _ => Self::Chrome,
        }
    }

    /// Get the cookie database path for this browser.
    #[cfg(test)]
    fn cookie_path(self) -> Option<std::path::PathBuf> {
        platform_default_cookie_path(self)
    }

    fn cookie_paths(self) -> Vec<std::path::PathBuf> {
        platform_cookie_paths(self)
    }

    /// Get the Keychain service name for this browser.
    pub(super) fn keychain_service(self) -> &'static str {
        match self {
            CookieSource::Brave => "Brave Safe Storage",
            CookieSource::Chrome => "Chrome Safe Storage",
            CookieSource::Firefox | CookieSource::Safari => "",
        }
    }

    fn native_cookie_result_is_authoritative(self) -> bool {
        #[cfg(target_os = "macos")]
        {
            !self.keychain_service().is_empty()
        }

        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// Get the raw Keychain password for this browser and derive the AES key.
    ///
    /// On macOS, uses `security-framework` to query the system Keychain.
    /// On Linux, returns an error — only GNOME Keyring is supported there (TODO).
    fn get_keychain_key_with_interaction(
        self,
        interaction: KeychainInteraction,
    ) -> Result<Vec<u8>> {
        let service = self.keychain_service();
        if service.is_empty() {
            anyhow::bail!("Browser does not use Keychain encryption");
        }

        let load = || {
            let password = Self::read_keychain_password(service, interaction)?;
            crypto::derive_cookie_key(&password)
        };

        #[cfg(target_os = "macos")]
        {
            let cache = KEYCHAIN_KEY_CACHE.get_or_init(|| Mutex::new(KeychainKeyCache::default()));
            load_cookie_key_cached(cache, service, interaction, load)
        }

        #[cfg(not(target_os = "macos"))]
        {
            load()
        }
    }

    /// Read the raw password bytes from the macOS Keychain.
    ///
    /// Uses `security-framework` on macOS. Other platforms intentionally return an
    /// error so cookie extraction can fall back to Python `browser_cookie3`.
    #[cfg(target_os = "macos")]
    fn read_keychain_password(service: &str, interaction: KeychainInteraction) -> Result<Vec<u8>> {
        use security_framework::os::macos::keychain::SecKeychain;
        use security_framework::passwords::get_generic_password;

        // SecKeychainSetUserInteractionAllowed is process-global. Serialize
        // non-interactive reads so one lookup cannot restore UI while another
        // is still in progress. If the caller had already disabled UI, do not
        // create security-framework's RAII lock because its Drop always
        // re-enables interaction rather than restoring the previous state.
        let _serial_guard = lock_ignoring_poison(&KEYCHAIN_UI_LOCK);
        let _interaction_guard = if interaction == KeychainInteraction::Never
            && SecKeychain::user_interaction_allowed()
                .context("Could not read Keychain interaction policy")?
        {
            Some(
                SecKeychain::disable_user_interaction()
                    .context("Could not disable Keychain interaction")?,
            )
        } else {
            None
        };

        let account = service.strip_suffix(" Safe Storage").unwrap_or(service);
        get_generic_password(service, account)
            .with_context(|| format!("Keychain access denied for service '{service}'"))
    }

    /// Non-macOS platforms do not have native keychain support in `nab` yet.
    #[cfg(not(target_os = "macos"))]
    fn read_keychain_password(
        _service: &str,
        _interaction: KeychainInteraction,
    ) -> Result<Vec<u8>> {
        anyhow::bail!(
            "Native keychain lookup is only supported on macOS; using Python cookie fallback"
        )
    }

    /// Get cookies for a domain from the specified browser.
    ///
    /// Tries native Rust extraction first and normally falls back to Python
    /// `browser_cookie3`. On macOS, native Chromium extraction covers the
    /// established Chrome/Brave channels and their profile databases, so it is
    /// authoritative even when no cookies are found: retrying through Python
    /// could display a duplicate Keychain prompt for the same browser service.
    /// When [`KEYCHAIN_INTERACTION_ENV`] forbids UI, the Python fallback is also
    /// skipped because it can perform a second Keychain read.
    pub fn get_cookies(&self, domain: &str) -> Result<HashMap<String, String>> {
        self.get_cookies_with_interaction(domain, configured_keychain_interaction())
    }

    fn get_cookies_with_interaction(
        self,
        domain: &str,
        interaction: KeychainInteraction,
    ) -> Result<HashMap<String, String>> {
        debug!("Getting cookies for {} from {:?}", domain, self);

        let native = self.get_cookies_native(domain, interaction);
        let native_is_authoritative = self.native_cookie_result_is_authoritative();
        match &native {
            Ok(cookies) if !cookies.is_empty() => {
                info!(
                    "Native cookie extraction succeeded: {} cookies",
                    cookies.len()
                );
            }
            Ok(_) if native_is_authoritative => {
                debug!("Native Chromium extraction completed; Python fallback disabled");
            }
            Ok(_) if interaction == KeychainInteraction::Never => {
                debug!("Native extraction returned empty; prompt-capable fallback disabled");
            }
            Ok(_) => debug!("Native extraction returned empty, trying Python fallback"),
            Err(e) if native_is_authoritative || interaction == KeychainInteraction::Never => {
                debug!("Native extraction failed; prompt-capable fallback disabled: {e}");
            }
            Err(e) => debug!("Native extraction failed: {e}, trying Python fallback"),
        }

        resolve_cookie_lookup_for_source(self, native, interaction, || {
            self.get_cookies_via_python(domain)
        })
    }

    /// Native Rust cookie extraction from the browser `SQLite` database.
    ///
    /// Reads the Chromium `Cookies` `SQLite` database, decrypts v10-encrypted values
    /// using AES-128-CBC with the key derived from the macOS Keychain password.
    ///
    /// For DB schema v24+, the decrypted plaintext has a 32-byte `SHA-256(host_key)`
    /// prefix that is stripped automatically.
    fn get_cookies_native(
        self,
        domain: &str,
        interaction: KeychainInteraction,
    ) -> Result<HashMap<String, String>> {
        let cookie_paths = self.cookie_paths();
        let existing_paths = cookie_paths
            .iter()
            .filter(|path| path.exists())
            .cloned()
            .collect::<Vec<_>>();
        if existing_paths.is_empty() {
            warn!(
                "Cookie database not found in candidates: {:?}",
                cookie_paths
            );
            return Ok(HashMap::new());
        }

        load_first_usable_cookie_store(
            &existing_paths,
            |cookie_path| self.get_cookies_native_from_path(domain, interaction, cookie_path),
            HashMap::is_empty,
        )
    }

    fn get_cookies_native_from_path(
        self,
        domain: &str,
        interaction: KeychainInteraction,
        cookie_path: &std::path::Path,
    ) -> Result<HashMap<String, String>> {
        let temp_db = copy_db_to_temp(cookie_path)?;
        let extraction = (|| {
            let rows = query_cookie_db(&temp_db, domain)?;
            let needs_key = cookie_rows_need_key(&rows);
            let domain_tag = load_cookie_domain_tag_if_needed(needs_key, interaction, || {
                has_domain_tag(&temp_db)
            })?;
            Ok::<_, anyhow::Error>((domain_tag, rows))
        })();
        if let Some(parent) = temp_db.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        let (domain_tag, rows) = extraction?;

        if rows.is_empty() {
            return Ok(HashMap::new());
        }

        let key = load_cookie_key_if_needed(domain_tag.is_some(), interaction, || {
            self.get_keychain_key_with_interaction(interaction)
        })?;
        let cookies = decrypt_rows(rows, key.as_deref(), domain_tag.unwrap_or(false));

        if cookies.is_empty() {
            debug!("Native extraction: 0 cookies for {}", domain);
        } else {
            info!(
                "Native extraction: {} cookies for {}",
                cookies.len(),
                domain
            );
        }
        Ok(cookies)
    }

    /// Get cookies via Python `browser_cookie3` (fallback when native path fails).
    fn get_cookies_via_python(self, domain: &str) -> Result<HashMap<String, String>> {
        let browser_fn = match self {
            CookieSource::Brave => "brave",
            CookieSource::Chrome => "chrome",
            CookieSource::Firefox => "firefox",
            CookieSource::Safari => "safari",
        };
        let domain_candidates_json = serde_json::to_string(&domain_candidates(domain))?;

        let script = format!(
            r#"
import json
try:
    import browser_cookie3 as bc
    cj = bc.{browser_fn}()
    request_domains = {domain_candidates_json}

    def matches_cookie_domain(cookie_domain, request_domains):
        for request_domain in request_domains:
            if matches_single_cookie_domain(cookie_domain, request_domain):
                return True
        return False

    def matches_single_cookie_domain(cookie_domain, request_domain):
        if cookie_domain.startswith('.'):
            parent = cookie_domain[1:]
            return request_domain == parent or request_domain.endswith('.' + parent)
        else:
            return cookie_domain == request_domain

    cookies = {{c.name: c.value for c in cj if matches_cookie_domain(c.domain, request_domains)}}
    print(json.dumps(cookies))
except Exception as e:
    print(json.dumps({{"__error__": str(e)}}))
"#
        );

        let output = Command::new("python3")
            .args(["-c", &script])
            .output()
            .context("Failed to run Python cookie extraction")?;

        if !output.status.success() {
            return Ok(HashMap::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let cookies: HashMap<String, String> = serde_json::from_str(&stdout).unwrap_or_default();

        if cookies.contains_key("__error__") {
            return Ok(HashMap::new());
        }

        Ok(cookies)
    }

    /// Get cookie header string for HTTP requests.
    pub fn get_cookie_header(&self, domain: &str) -> Result<String> {
        let cookies = self.get_cookies(domain)?;
        let header = cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        Ok(header)
    }

    /// Extract cookies for a domain as faithful Playwright `storage_state`
    /// cookies — with real `domain`/`path`/`expires`/`httpOnly`/`secure`/
    /// `sameSite` metadata.
    ///
    /// Chromium-family browsers (Brave/Chrome/Edge/…) get full fidelity from the
    /// native `SQLite` path: every row is preserved individually, so two same-named
    /// cookies on different host keys both survive (unlike [`Self::get_cookies`],
    /// which collapses by name for the fetch hot path).
    ///
    /// Firefox/Safari and the Python fallback currently surface only name/value;
    /// those are returned with safe defaults (`domain` = the queried domain,
    /// `path` = `/`, session expiry, `secure`, `Lax`) — best-effort, as recorded
    /// in `docs/design/2026-06-01-nab-task-engine-browser-modes.md`.
    ///
    /// # Errors
    /// Propagates filesystem/Keychain errors from the native extraction path.
    pub fn get_cookies_rich(&self, domain: &str) -> Result<Vec<PlaywrightCookie>> {
        debug!("Getting rich cookies for {} from {:?}", domain, self);

        let interaction = configured_keychain_interaction();

        match self.get_cookies_rich_native(domain, interaction) {
            Ok(cookies) if !cookies.is_empty() => {
                info!("Native rich extraction: {} cookies", cookies.len());
                return Ok(cookies);
            }
            Ok(cookies) if interaction == KeychainInteraction::Never => return Ok(cookies),
            Ok(_) => debug!("Native rich extraction empty, falling back to name/value"),
            Err(e) if interaction == KeychainInteraction::Never => return Err(e),
            Err(e) => debug!("Native rich extraction failed: {e}, falling back to name/value"),
        }

        // Fallback: name/value only (Firefox/Safari/Python). Synthesize safe
        // metadata defaults so the storage_state is still schema-valid.
        let flat = self.get_cookies(domain)?;
        Ok(flat
            .into_iter()
            .map(|(name, value)| synthesize_cookie(name, value, domain))
            .collect())
    }

    /// Native Rust rich extraction from the Chromium `SQLite` database.
    fn get_cookies_rich_native(
        self,
        domain: &str,
        interaction: KeychainInteraction,
    ) -> Result<Vec<PlaywrightCookie>> {
        let cookie_paths = self.cookie_paths();
        let existing_paths = cookie_paths
            .iter()
            .filter(|path| path.exists())
            .cloned()
            .collect::<Vec<_>>();
        if existing_paths.is_empty() {
            warn!(
                "Cookie database not found in candidates: {:?}",
                cookie_paths
            );
            return Ok(Vec::new());
        }

        load_first_usable_cookie_store(
            &existing_paths,
            |cookie_path| self.get_cookies_rich_native_from_path(domain, interaction, cookie_path),
            Vec::is_empty,
        )
    }

    fn get_cookies_rich_native_from_path(
        self,
        domain: &str,
        interaction: KeychainInteraction,
        cookie_path: &std::path::Path,
    ) -> Result<Vec<PlaywrightCookie>> {
        let temp_db = copy_db_to_temp(cookie_path)?;
        let extraction = (|| {
            let rows = query_cookie_db_rich(&temp_db, domain)?;
            let needs_key = rich_cookie_rows_need_key(&rows);
            let domain_tag = load_cookie_domain_tag_if_needed(needs_key, interaction, || {
                has_domain_tag(&temp_db)
            })?;
            Ok::<_, anyhow::Error>((domain_tag, rows))
        })();
        if let Some(parent) = temp_db.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        let (domain_tag, rows) = extraction?;

        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let key = load_cookie_key_if_needed(domain_tag.is_some(), interaction, || {
            self.get_keychain_key_with_interaction(interaction)
        })?;
        let cookies = decrypt_rich_rows(rows, key.as_deref(), domain_tag.unwrap_or(false));
        Ok(cookies)
    }
}

/// Build a best-effort Playwright cookie from a bare name/value pair.
///
/// Used for sources that surface only name/value (Firefox, Safari, the Python
/// `browser_cookie3` fallback). Defaults are conservative: the queried `domain`
/// (leading dot preserved if present), root `path`, session expiry, `secure`,
/// and `Lax` sameSite. Documented as best-effort in the browser-modes ADR.
fn synthesize_cookie(name: String, value: String, domain: &str) -> PlaywrightCookie {
    PlaywrightCookie {
        name,
        value,
        domain: domain.to_string(),
        path: "/".to_string(),
        expires: -1.0,
        http_only: false,
        secure: true,
        same_site: SameSite::Lax,
    }
}

fn chromium_cookie_paths_under(roots: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut profiles = entries
            .flatten()
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(std::fs::FileType::is_dir)
                    .map(|_| entry.path())
            })
            .collect::<Vec<_>>();
        profiles.sort();
        for profile in profiles {
            for relative in ["Cookies", "Network/Cookies"] {
                let candidate = profile.join(relative);
                if candidate.is_file() {
                    paths.push(candidate);
                }
            }
        }
    }
    paths
}

#[cfg(target_os = "macos")]
fn macos_chromium_roots(
    app_support: &std::path::Path,
    source: CookieSource,
) -> Vec<std::path::PathBuf> {
    match source {
        CookieSource::Chrome => [
            "Google/Chrome",
            "Google/Chrome Beta",
            "Google/Chrome Dev",
            "Google/Chrome Canary",
            "Chromium",
        ]
        .map(|path| app_support.join(path))
        .to_vec(),
        CookieSource::Brave => [
            "BraveSoftware/Brave-Browser",
            "BraveSoftware/Brave-Browser-Beta",
            "BraveSoftware/Brave-Browser-Dev",
            "BraveSoftware/Brave-Browser-Nightly",
        ]
        .map(|path| app_support.join(path))
        .to_vec(),
        CookieSource::Firefox | CookieSource::Safari => Vec::new(),
    }
}

#[cfg(target_os = "macos")]
fn platform_cookie_paths(source: CookieSource) -> Vec<std::path::PathBuf> {
    let Some(app_support) = dirs::config_dir() else {
        return Vec::new();
    };
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let roots = match source {
        CookieSource::Brave | CookieSource::Chrome => macos_chromium_roots(&app_support, source),
        CookieSource::Firefox => return vec![app_support.join("Firefox/Profiles")],
        CookieSource::Safari => {
            return vec![home.join("Library/Cookies/Cookies.binarycookies")];
        }
    };

    let mut paths = chromium_cookie_paths_under(&roots);
    if paths.is_empty()
        && let Some(root) = roots.first()
    {
        paths.push(root.join("Default/Cookies"));
    }
    paths
}

#[cfg(all(test, target_os = "macos"))]
fn platform_default_cookie_path(source: CookieSource) -> Option<std::path::PathBuf> {
    let app_support = dirs::config_dir()?;
    let home = dirs::home_dir()?;
    Some(match source {
        CookieSource::Brave => app_support.join("BraveSoftware/Brave-Browser/Default/Cookies"),
        CookieSource::Chrome => app_support.join("Google/Chrome/Default/Cookies"),
        CookieSource::Firefox => app_support.join("Firefox/Profiles"),
        CookieSource::Safari => home.join("Library/Cookies/Cookies.binarycookies"),
    })
}

#[cfg(target_os = "linux")]
fn platform_cookie_paths(source: CookieSource) -> Vec<std::path::PathBuf> {
    let Some(config_dir) = dirs::config_dir() else {
        return Vec::new();
    };
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    match source {
        CookieSource::Brave => vec![config_dir.join("BraveSoftware/Brave-Browser/Default/Cookies")],
        CookieSource::Chrome => vec![config_dir.join("google-chrome/Default/Cookies")],
        CookieSource::Firefox => vec![home.join(".mozilla/firefox")],
        CookieSource::Safari => Vec::new(),
    }
}

#[cfg(all(test, target_os = "linux"))]
fn platform_default_cookie_path(source: CookieSource) -> Option<std::path::PathBuf> {
    platform_cookie_paths(source).into_iter().next()
}

#[cfg(target_os = "windows")]
fn platform_cookie_paths(source: CookieSource) -> Vec<std::path::PathBuf> {
    let Some(local_data) = dirs::data_local_dir() else {
        return Vec::new();
    };
    let Some(config_dir) = dirs::config_dir() else {
        return Vec::new();
    };

    match source {
        CookieSource::Brave => {
            vec![local_data.join("BraveSoftware/Brave-Browser/User Data/Default/Cookies")]
        }
        CookieSource::Chrome => {
            vec![local_data.join("Google/Chrome/User Data/Default/Cookies")]
        }
        CookieSource::Firefox => vec![config_dir.join("Mozilla/Firefox/Profiles")],
        CookieSource::Safari => Vec::new(),
    }
}

#[cfg(all(test, target_os = "windows"))]
fn platform_default_cookie_path(source: CookieSource) -> Option<std::path::PathBuf> {
    platform_cookie_paths(source).into_iter().next()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn platform_cookie_paths(_source: CookieSource) -> Vec<std::path::PathBuf> {
    Vec::new()
}

#[cfg(all(
    test,
    not(any(target_os = "macos", target_os = "linux", target_os = "windows"))
))]
fn platform_default_cookie_path(_source: CookieSource) -> Option<std::path::PathBuf> {
    None
}

// ─── Credential source ────────────────────────────────────────────────────────

/// Source for retrieving credentials.
#[derive(Debug, Clone, Copy)]
pub enum CredentialSource {
    /// macOS Keychain (Internet passwords)
    Keychain,
    /// 1Password CLI
    OnePassword,
    /// Browser password manager (Brave)
    BravePasswords,
    /// Browser password manager (Chrome)
    ChromePasswords,
}

/// Unified credential retriever — tries multiple sources in priority order.
pub struct CredentialRetriever;

impl CredentialRetriever {
    /// Get credentials for a URL from all available sources.
    ///
    /// Priority: 1Password > Keychain > Browser passwords.
    pub fn get_credential_for_url(url: &str) -> Result<Option<Credential>> {
        let domain = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(std::string::ToString::to_string))
            .unwrap_or_default();

        if domain.is_empty() {
            return Ok(None);
        }

        if OnePasswordAuth::is_available() {
            let auth = OnePasswordAuth::new(None);
            if let Ok(Some(cred)) = auth.get_credential_for_url(url) {
                info!("Found credential in 1Password: {}", cred.title);
                return Ok(Some(cred));
            }
        }

        if let Some(cred) = Self::get_keychain_credential(&domain)? {
            info!("Found credential in Keychain");
            return Ok(Some(cred));
        }

        if let Some(cred) = Self::get_browser_credential(&domain)? {
            info!("Found credential in browser");
            return Ok(Some(cred));
        }

        Ok(None)
    }

    /// Get credential from macOS Keychain.
    #[allow(clippy::unnecessary_wraps)]
    fn get_keychain_credential(domain: &str) -> Result<Option<Credential>> {
        let output = Command::new("security")
            .args(["find-internet-password", "-s", domain, "-g"])
            .output();

        if let Ok(output) = output
            && output.status.success()
        {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);

            let username = stdout
                .lines()
                .find(|l| l.contains("\"acct\""))
                .and_then(|l| l.split('"').nth(3))
                .map(String::from);

            let password = stderr
                .lines()
                .find(|l| l.starts_with("password:"))
                .and_then(|l| {
                    if l.contains('"') {
                        l.split('"').nth(1).map(String::from)
                    } else {
                        None
                    }
                });

            if username.is_some() || password.is_some() {
                return Ok(Some(Credential {
                    title: format!("Keychain: {domain}"),
                    username,
                    password,
                    url: Some(format!("https://{domain}")),
                    totp: None,
                    has_totp: false,
                    passkey_credential_id: None,
                }));
            }
        }

        Ok(None)
    }

    /// Get credential from browser password manager (Brave then Chrome).
    fn get_browser_credential(domain: &str) -> Result<Option<Credential>> {
        for browser in ["brave", "chrome"] {
            if let Some(cred) = Self::get_chromium_password(browser, domain)? {
                return Ok(Some(cred));
            }
        }
        Ok(None)
    }

    /// Get password from a Chromium-based browser (Brave/Chrome).
    fn get_chromium_password(browser: &str, domain: &str) -> Result<Option<Credential>> {
        let home = dirs::home_dir().context("No home directory")?;

        let login_data_path = match browser {
            "brave" => home
                .join("Library/Application Support/BraveSoftware/Brave-Browser/Default/Login Data"),
            "chrome" => home.join("Library/Application Support/Google/Chrome/Default/Login Data"),
            _ => return Ok(None),
        };

        if !login_data_path.exists() {
            return Ok(None);
        }

        let temp_dir = std::env::temp_dir().join(format!("nab_logins_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir)?;
        let temp_db = temp_dir.join("Login Data");
        std::fs::copy(&login_data_path, &temp_db)?;

        let query = format!(
            "SELECT origin_url, username_value FROM logins WHERE origin_url LIKE '%{domain}%' LIMIT 1"
        );
        let temp_db_str = temp_db
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid temp database path"))?;
        let output = Command::new("sqlite3")
            .args(["-separator", "\t", temp_db_str, &query])
            .output();

        let _ = std::fs::remove_dir_all(&temp_dir);

        if let Ok(output) = output
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = stdout.lines().next() {
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 2 {
                    return Ok(Some(Credential {
                        title: format!("{browser} password: {domain}"),
                        username: Some(parts[1].to_string()),
                        password: None,
                        url: Some(parts[0].to_string()),
                        totp: None,
                        has_totp: false,
                        passkey_credential_id: None,
                    }));
                }
            }
        }

        Ok(None)
    }
}
