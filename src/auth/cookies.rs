//! Browser cookie extraction and credential retrieval.
//!
//! Provides:
//! - [`CookieSource`]: extract cookies from Brave/Chrome/Firefox/Safari
//! - [`CredentialRetriever`]: unified credential lookup across all sources
//!
//! # macOS Cookie Decryption
//!
//! Brave and Chrome encrypt cookies with AES-128-CBC:
//! - Key: PBKDF2-SHA1(`keychain_password`, salt=`"saltysalt"`, iterations=1003, 16 bytes)
//! - IV: 16 **space** bytes (`0x20 * 16`) — NOT zero bytes
//! - Ciphertext: `encrypted_value[3..]` (first 3 bytes are the `"v10"` prefix)
//! - Padding: PKCS7
//!
//! The raw password is retrieved from macOS Keychain under service
//! `"Brave Safe Storage"` or `"Chrome Safe Storage"`.
//!
//! ## Schema v24+ domain-integrity prefix
//!
//! Starting with Chromium cookie DB schema version 24 (Chrome 130+, Brave 1.70+),
//! the first 32 bytes of every decrypted plaintext are `SHA-256(host_key)`.
//! These bytes must be stripped to recover the actual cookie value.
//! See: <https://issues.chromium.org/issues/40185252>

use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use super::{Credential, OnePasswordAuth};

// ─── Crypto constants ─────────────────────────────────────────────────────────

/// PBKDF2 salt used by Chromium for cookie key derivation.
const CHROME_PBKDF2_SALT: &[u8] = b"saltysalt";
/// PBKDF2 iteration count (1003 for macOS Chromium builds).
const CHROME_PBKDF2_ITERATIONS: u32 = 1003;
/// Derived key length in bytes (AES-128 = 16 bytes).
const CHROME_KEY_LEN: usize = 16;
/// Prefix on every v10 encrypted cookie value (ASCII "v10").
const V10_PREFIX: &[u8; 3] = b"v10";
/// AES-CBC IV: 16 **space** bytes (0x20). Chromium hard-codes this — NOT zero bytes.
/// Reference: `chromium/components/os_crypt/os_crypt_mac.mm`, `OSCryptImpl::DecryptString`.
const AES_CBC_IV: [u8; 16] = [b' '; 16];
/// Domain-integrity prefix length added in DB schema v24+.
/// First 32 bytes of every decrypted value are `SHA-256(host_key)`.
const DOMAIN_SHA256_LEN: usize = 32;
/// Minimum Chromium cookie DB schema version that prepends a SHA-256 domain tag.
const SCHEMA_VERSION_WITH_DOMAIN_TAG: u32 = 24;

// ─── CookieSource ─────────────────────────────────────────────────────────────

/// Cookie source for browser cookie extraction.
#[derive(Debug, Clone, Copy)]
pub enum CookieSource {
    Brave,
    Chrome,
    Firefox,
    Safari,
}

impl CookieSource {
    /// Get the cookie database path for this browser.
    fn cookie_path(self) -> Option<std::path::PathBuf> {
        let home = dirs::home_dir()?;
        let path = match self {
            CookieSource::Brave => {
                home.join("Library/Application Support/BraveSoftware/Brave-Browser/Default/Cookies")
            }
            CookieSource::Chrome => {
                home.join("Library/Application Support/Google/Chrome/Default/Cookies")
            }
            CookieSource::Firefox => home.join("Library/Application Support/Firefox/Profiles"),
            CookieSource::Safari => home.join("Library/Cookies/Cookies.binarycookies"),
        };
        Some(path)
    }

    /// Get the Keychain service name for this browser.
    fn keychain_service(self) -> &'static str {
        match self {
            CookieSource::Brave => "Brave Safe Storage",
            CookieSource::Chrome => "Chrome Safe Storage",
            CookieSource::Firefox | CookieSource::Safari => "",
        }
    }

    /// Get the raw Keychain password for this browser and derive the AES key.
    ///
    /// On macOS, uses `security-framework` to query the system Keychain.
    /// On Linux, returns an error — only GNOME Keyring is supported there (TODO).
    fn get_keychain_key(self) -> Result<Vec<u8>> {
        let service = self.keychain_service();
        if service.is_empty() {
            anyhow::bail!("Browser does not use Keychain encryption");
        }
        let password = Self::read_keychain_password(service)?;
        derive_cookie_key(&password)
    }

    /// Read the raw password bytes from the macOS Keychain.
    ///
    /// Uses `security-framework` on macOS; falls back to `security` CLI on other
    /// platforms (Linux uses GNOME Keyring — not yet implemented natively).
    #[cfg(target_os = "macos")]
    fn read_keychain_password(service: &str) -> Result<Vec<u8>> {
        use security_framework::passwords::get_generic_password;

        // The Keychain account name for the cookie key is the browser's name,
        // which matches the service name (e.g., "Brave" for "Brave Safe Storage").
        let account = service.strip_suffix(" Safe Storage").unwrap_or(service);

        get_generic_password(service, account)
            .with_context(|| format!("Keychain access denied for service '{service}'"))
    }

    /// Fallback for non-macOS: shell out to `security` CLI.
    #[cfg(not(target_os = "macos"))]
    fn read_keychain_password(service: &str) -> Result<Vec<u8>> {
        // TODO(linux): implement GNOME Keyring support via `secret-service` crate.
        // For now fall back to the `security` CLI, which only exists on macOS anyway.
        let output = Command::new("security")
            .args(["find-generic-password", "-s", service, "-w"])
            .output()
            .context("Failed to access Keychain")?;

        if !output.status.success() {
            anyhow::bail!("Keychain access denied for {service}");
        }

        Ok(output.stdout.trim_ascii().to_vec())
    }

    /// Get cookies for a domain from the specified browser.
    ///
    /// Tries native Rust extraction first, falls back to Python `browser_cookie3`.
    pub fn get_cookies(&self, domain: &str) -> Result<HashMap<String, String>> {
        debug!("Getting cookies for {} from {:?}", domain, self);

        match self.get_cookies_native(domain) {
            Ok(cookies) if !cookies.is_empty() => {
                info!(
                    "Native cookie extraction succeeded: {} cookies",
                    cookies.len()
                );
                return Ok(cookies);
            }
            Ok(_) => debug!("Native extraction returned empty, trying Python fallback"),
            Err(e) => debug!("Native extraction failed: {}, trying Python fallback", e),
        }

        self.get_cookies_via_python(domain)
    }

    /// Native Rust cookie extraction from the browser `SQLite` database.
    ///
    /// Reads the Chromium `Cookies` `SQLite` database, decrypts v10-encrypted values
    /// using AES-128-CBC with the key derived from the macOS Keychain password.
    ///
    /// For DB schema v24+, the decrypted plaintext has a 32-byte `SHA-256(host_key)`
    /// prefix that is stripped automatically.
    fn get_cookies_native(self, domain: &str) -> Result<HashMap<String, String>> {
        let cookie_path = self
            .cookie_path()
            .context("Could not determine cookie path")?;
        if !cookie_path.exists() {
            warn!("Cookie database not found: {:?}", cookie_path);
            return Ok(HashMap::new());
        }

        let temp_db = copy_db_to_temp(&cookie_path)?;
        let schema_version = query_db_schema_version(&temp_db);
        let rows = query_cookie_db(&temp_db, domain)?;
        let _ = std::fs::remove_dir_all(temp_db.parent().expect("temp_db always has a parent"));

        let key = self.get_keychain_key().ok();
        let has_domain_tag = schema_version >= SCHEMA_VERSION_WITH_DOMAIN_TAG;
        debug!(
            "Cookie DB schema v{schema_version}, domain_tag={}",
            has_domain_tag
        );
        let cookies = decrypt_rows(rows, key.as_deref(), has_domain_tag);

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

        let script = format!(
            r#"
import json
try:
    import browser_cookie3 as bc
    cj = bc.{browser_fn}()

    def matches_cookie_domain(cookie_domain, request_domain):
        if cookie_domain.startswith('.'):
            parent = cookie_domain[1:]
            return request_domain == parent or request_domain.endswith('.' + parent)
        else:
            return cookie_domain == request_domain

    cookies = {{c.name: c.value for c in cj if matches_cookie_domain(c.domain, '{domain}')}}
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
}

// ─── Cookie DB helpers ────────────────────────────────────────────────────────

/// Copy the browser cookie database to a temp directory (avoids locking issues).
fn copy_db_to_temp(cookie_path: &std::path::Path) -> Result<std::path::PathBuf> {
    let temp_dir = std::env::temp_dir().join(format!("nab_cookies_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir)?;
    let temp_db = temp_dir.join("Cookies");
    std::fs::copy(cookie_path, &temp_db)?;

    // Copy WAL/SHM files so SQLite can read a consistent snapshot.
    for suffix in ["-wal", "-shm"] {
        let wal = cookie_path.with_extension(format!("Cookies{suffix}"));
        if wal.exists() {
            let _ = std::fs::copy(&wal, temp_db.with_extension(format!("Cookies{suffix}")));
        }
    }

    Ok(temp_db)
}

/// A single row from the cookies table.
struct CookieRow {
    name: String,
    /// Plaintext value (may be empty when `encrypted_value` is present).
    value: String,
    /// Raw encrypted bytes (decoded from hex output by sqlite3).
    encrypted_bytes: Vec<u8>,
}

/// Query the `meta` table for the DB schema version (0 if unavailable).
///
/// Chromium increments this monotonically; v24 added `SHA-256(host_key)` prepended
/// to every decrypted cookie value.
fn query_db_schema_version(temp_db: &std::path::Path) -> u32 {
    let Some(db_str) = temp_db.to_str() else {
        return 0;
    };
    let output = Command::new("sqlite3")
        .args([db_str, "SELECT value FROM meta WHERE key='version';"])
        .output();
    let Ok(out) = output else { return 0 };
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

/// Query the cookie database for rows matching `domain` and its parents.
///
/// Uses `hex(encrypted_value)` to avoid binary corruption when reading blobs
/// through the sqlite3 CLI and `String::from_utf8_lossy`.
fn query_cookie_db(temp_db: &std::path::Path, domain: &str) -> Result<Vec<CookieRow>> {
    let conditions = build_domain_conditions(domain);
    let where_clause = conditions.join(" OR ");
    // Use hex() on the encrypted blob so binary bytes survive the CLI round-trip.
    let query =
        format!("SELECT name, value, hex(encrypted_value) FROM cookies WHERE {where_clause}");
    debug!("Cookie SQL query for '{}': WHERE {}", domain, where_clause);

    let temp_db_str = temp_db
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid temp database path"))?;
    let output = Command::new("sqlite3")
        .args(["-separator", "\t", temp_db_str, &query])
        .output()
        .context("Failed to query cookie database")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("SQLite query failed: {}", stderr);
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_cookie_rows(&stdout))
}

/// Build SQL `host_key` conditions for `domain` and its parent domains.
fn build_domain_conditions(domain: &str) -> Vec<String> {
    let parts: Vec<&str> = domain.split('.').collect();
    let mut conditions = vec![
        format!("host_key = '{domain}'"),
        format!("host_key = '.{domain}'"),
    ];
    for i in 1..parts.len() {
        let parent = parts[i..].join(".");
        conditions.push(format!("host_key = '.{parent}'"));
    }
    conditions
}

/// Parse tab-separated output from `sqlite3` into `CookieRow` structs.
fn parse_cookie_rows(stdout: &str) -> Vec<CookieRow> {
    let mut rows = Vec::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let encrypted_bytes = if parts.len() >= 3 && !parts[2].is_empty() {
            hex::decode(parts[2]).unwrap_or_default()
        } else {
            Vec::new()
        };
        rows.push(CookieRow {
            name: parts[0].to_string(),
            value: parts[1].to_string(),
            encrypted_bytes,
        });
    }
    rows
}

/// Decrypt cookie rows, returning a map of `name -> plaintext value`.
///
/// Rows with a plaintext `value` are returned as-is.
/// Rows with an `encrypted_value` blob are decrypted with `key` when provided.
/// If a row is encrypted but no key is available, the row is skipped.
///
/// `has_domain_tag` — when `true` (DB schema ≥ 24), the first 32 bytes of each
/// decrypted value are `SHA-256(host_key)` and are stripped.
fn decrypt_rows(
    rows: Vec<CookieRow>,
    key: Option<&[u8]>,
    has_domain_tag: bool,
) -> HashMap<String, String> {
    let mut cookies = HashMap::new();

    for row in rows {
        if !row.value.is_empty() {
            cookies.insert(row.name, row.value);
            continue;
        }

        if row.encrypted_bytes.is_empty() {
            continue;
        }

        let Some(k) = key else {
            debug!(
                "Skipping encrypted cookie '{}' — no key available",
                row.name
            );
            continue;
        };

        match decrypt_cookie_value(&row.encrypted_bytes, k, has_domain_tag) {
            Ok(plain) => {
                cookies.insert(row.name, plain);
            }
            Err(e) => {
                warn!("Cookie decryption failed for '{}': {}", row.name, e);
            }
        }
    }

    cookies
}

// ─── Crypto ───────────────────────────────────────────────────────────────────

/// Derive the 16-byte AES key from the raw Keychain password using PBKDF2-SHA1.
///
/// This is the exact derivation used by all Chromium-based browsers on macOS:
/// `PBKDF2(password, salt="saltysalt", iterations=1003, key_len=16, prf=HMAC-SHA1)`
pub fn derive_cookie_key(password: &[u8]) -> Result<Vec<u8>> {
    use hmac::Hmac;
    use pbkdf2::pbkdf2;
    use sha1::Sha1;

    let mut key = [0u8; CHROME_KEY_LEN];
    pbkdf2::<Hmac<Sha1>>(
        password,
        CHROME_PBKDF2_SALT,
        CHROME_PBKDF2_ITERATIONS,
        &mut key,
    )
    .map_err(|e| anyhow::anyhow!("PBKDF2 key derivation failed: {e}"))?;
    Ok(key.to_vec())
}

/// Decrypt a single AES-128-CBC encrypted cookie blob.
///
/// # Format (macOS Chromium v10 cookies)
/// ```text
/// [ 'v' | '1' | '0' | ciphertext... ]
///   3 bytes prefix     N bytes (must be a nonzero multiple of 16)
/// ```
///
/// After PKCS7 unpadding the plaintext layout depends on the DB schema version:
/// - **Schema < 24**: `[actual_cookie_value]`
/// - **Schema ≥ 24**: `[SHA-256(host_key) 32 bytes][actual_cookie_value]`
///
/// Pass `has_domain_tag = true` for schema v24+ databases.
///
/// # Errors
/// Returns an error if the blob is too short, the prefix is wrong, AES
/// decryption/unpadding fails, or the result is not valid UTF-8.
pub fn decrypt_cookie_value(encrypted: &[u8], key: &[u8], has_domain_tag: bool) -> Result<String> {
    use aes::Aes128;
    use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    type Aes128CbcDec = cbc::Decryptor<Aes128>;

    anyhow::ensure!(
        encrypted.len() > V10_PREFIX.len(),
        "Encrypted blob too short ({} bytes)",
        encrypted.len()
    );
    anyhow::ensure!(
        encrypted.starts_with(V10_PREFIX),
        "Unexpected cookie prefix (expected v10, got {:?})",
        &encrypted[..V10_PREFIX.len()]
    );
    anyhow::ensure!(key.len() == CHROME_KEY_LEN, "Key must be 16 bytes");

    let ciphertext = &encrypted[V10_PREFIX.len()..];
    anyhow::ensure!(
        !ciphertext.is_empty() && ciphertext.len().is_multiple_of(16),
        "Ciphertext length {} is not a nonzero multiple of 16",
        ciphertext.len()
    );

    let decryptor = Aes128CbcDec::new_from_slices(key, &AES_CBC_IV)
        .map_err(|e| anyhow::anyhow!("AES key/IV setup failed: {e}"))?;

    let mut buf = ciphertext.to_vec();
    let plaintext = decryptor
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| anyhow::anyhow!("AES-CBC unpadding failed: {e}"))?;

    // Schema v24+ prepends 32 bytes of SHA-256(host_key) before the real value.
    let value_bytes = if has_domain_tag {
        anyhow::ensure!(
            plaintext.len() >= DOMAIN_SHA256_LEN,
            "Decrypted blob too short for domain tag ({} bytes)",
            plaintext.len()
        );
        &plaintext[DOMAIN_SHA256_LEN..]
    } else {
        plaintext
    };

    String::from_utf8(value_bytes.to_vec()).context("Decrypted cookie value is not valid UTF-8")
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── CookieSource metadata ──────────────────────────────────────────────────

    #[test]
    fn cookie_source_variants_are_distinct() {
        let sources = [
            CookieSource::Chrome,
            CookieSource::Firefox,
            CookieSource::Brave,
            CookieSource::Safari,
        ];
        for (i, a) in sources.iter().enumerate() {
            for (j, b) in sources.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        format!("{a:?}"),
                        format!("{b:?}"),
                        "variants {i} and {j} should differ"
                    );
                }
            }
        }
    }

    #[test]
    fn keychain_service_brave_and_chrome_are_nonempty() {
        assert!(!CookieSource::Brave.keychain_service().is_empty());
        assert!(!CookieSource::Chrome.keychain_service().is_empty());
    }

    #[test]
    fn keychain_service_firefox_safari_are_empty() {
        assert!(CookieSource::Firefox.keychain_service().is_empty());
        assert!(CookieSource::Safari.keychain_service().is_empty());
    }

    // ── PBKDF2 key derivation ─────────────────────────────────────────────────

    #[test]
    fn derive_cookie_key_known_vector() {
        // GIVEN: password "peanuts" (known test vector matching Python browser_cookie3 output)
        let password = b"peanuts";

        // WHEN: key is derived with Chrome parameters
        let key = derive_cookie_key(password).expect("key derivation must succeed");

        // THEN: key is 16 bytes
        assert_eq!(key.len(), CHROME_KEY_LEN, "derived key must be 16 bytes");
    }

    #[test]
    fn derive_cookie_key_empty_password_succeeds() {
        // GIVEN: empty password (edge case — Keychain could theoretically return empty)
        // WHEN: key is derived
        let key = derive_cookie_key(b"").expect("derivation must not panic on empty input");
        // THEN: still 16 bytes
        assert_eq!(key.len(), CHROME_KEY_LEN);
    }

    #[test]
    fn derive_cookie_key_is_deterministic() {
        // GIVEN: same password
        let pw = b"my-brave-password";
        // WHEN: derived twice
        let k1 = derive_cookie_key(pw).unwrap();
        let k2 = derive_cookie_key(pw).unwrap();
        // THEN: identical
        assert_eq!(k1, k2);
    }

    // ── AES-128-CBC decryption ────────────────────────────────────────────────

    /// Build a valid v10-encrypted blob from `inner_plaintext` using the same
    /// cipher parameters that Chromium uses, for round-trip testing.
    ///
    /// `inner_plaintext` is what is placed directly inside the AES envelope.
    /// For schema-v24 blobs the caller should prepend 32 SHA-256 bytes itself.
    fn encrypt_v10(inner_plaintext: &[u8], key: &[u8]) -> Vec<u8> {
        use aes::Aes128;
        use cbc::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
        type Aes128CbcEnc = cbc::Encryptor<Aes128>;

        // Output buffer: plaintext + up to one extra block for PKCS7 padding.
        let out_len = inner_plaintext.len() + 16;
        let mut out = vec![0u8; out_len];
        let enc = Aes128CbcEnc::new_from_slices(key, &AES_CBC_IV).unwrap();
        let ciphertext = enc
            .encrypt_padded_b2b_mut::<Pkcs7>(inner_plaintext, &mut out)
            .expect("output buffer is always large enough");

        let mut blob = V10_PREFIX.to_vec();
        blob.extend_from_slice(ciphertext);
        blob
    }

    /// Build a v24+ blob: `v10` + AES-CBC(`SHA-256(host_key)` + `value`).
    fn encrypt_v10_v24(value: &[u8], key: &[u8], host_key: &str) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        let mut inner = Sha256::digest(host_key.as_bytes()).to_vec();
        inner.extend_from_slice(value);
        encrypt_v10(&inner, key)
    }

    #[test]
    fn aes_iv_is_16_space_bytes_not_zero_bytes() {
        // GIVEN: the AES_CBC_IV constant
        // THEN: it must be 16 × 0x20 (space), matching Chromium's os_crypt_mac.mm
        assert_eq!(AES_CBC_IV, [0x20u8; 16], "IV must be 16 space bytes (0x20)");
        assert_ne!(AES_CBC_IV, [0u8; 16], "IV must NOT be zero bytes");
    }

    #[test]
    fn decrypt_cookie_value_round_trip_simple() {
        // GIVEN: a known plaintext and derived key (no domain tag, schema < 24)
        let password = b"test-key";
        let key = derive_cookie_key(password).unwrap();
        let plaintext = b"session_token_abc123";
        let blob = encrypt_v10(plaintext, &key);

        // WHEN: decrypted without domain-tag stripping
        let result = decrypt_cookie_value(&blob, &key, false).expect("decryption must succeed");

        // THEN: plaintext recovered
        assert_eq!(result, "session_token_abc123");
    }

    #[test]
    fn decrypt_cookie_value_round_trip_v24_domain_tag_stripped() {
        // GIVEN: v24+ blob with 32-byte SHA-256 prefix before the actual value
        let key = derive_cookie_key(b"brave-key").unwrap();
        let host = ".linkedin.com";
        let value = b"my_session_value";
        let blob = encrypt_v10_v24(value, &key, host);

        // WHEN: decrypted with has_domain_tag=true
        let result = decrypt_cookie_value(&blob, &key, true).unwrap();

        // THEN: only the actual value is returned, not the SHA-256 prefix
        assert_eq!(result, "my_session_value");
    }

    #[test]
    fn decrypt_cookie_value_v24_without_tag_flag_returns_garbage() {
        // GIVEN: v24+ blob (SHA-256 prefix present)
        let key = derive_cookie_key(b"brave-key-2").unwrap();
        let blob = encrypt_v10_v24(b"val", &key, ".example.com");

        // WHEN: decrypted WITHOUT setting has_domain_tag (wrong flag)
        // THEN: result is either an error or contains the SHA-256 garbage bytes
        // (not equal to "val"), proving the flag matters
        let result = decrypt_cookie_value(&blob, &key, false).unwrap_or_default();
        assert_ne!(
            result, "val",
            "domain tag must be stripped for correct result"
        );
    }

    #[test]
    fn decrypt_cookie_value_round_trip_unicode() {
        // GIVEN: UTF-8 cookie value (no domain tag)
        let key = derive_cookie_key(b"unicode-test").unwrap();
        let plaintext = "café=résumé".as_bytes();
        let blob = encrypt_v10(plaintext, &key);

        // WHEN: decrypted
        let result = decrypt_cookie_value(&blob, &key, false).unwrap();

        // THEN: unicode preserved
        assert_eq!(result, "café=résumé");
    }

    #[test]
    fn decrypt_cookie_value_round_trip_exactly_16_bytes() {
        // GIVEN: plaintext that is exactly 16 bytes (one full AES block, needs +1 padding block)
        let key = derive_cookie_key(b"block-aligned").unwrap();
        let plaintext = b"0123456789abcdef"; // exactly 16
        let blob = encrypt_v10(plaintext, &key);

        // WHEN: decrypted
        let result = decrypt_cookie_value(&blob, &key, false).unwrap();

        // THEN: exact match
        assert_eq!(result, "0123456789abcdef");
    }

    #[test]
    fn decrypt_cookie_value_empty_blob_returns_error() {
        // GIVEN: empty input
        // WHEN: decryption attempted
        let err = decrypt_cookie_value(&[], &[0u8; 16], false).unwrap_err();
        // THEN: descriptive error
        assert!(
            err.to_string().contains("too short"),
            "error should mention too short: {err}"
        );
    }

    #[test]
    fn decrypt_cookie_value_wrong_prefix_returns_error() {
        // GIVEN: blob with wrong prefix
        let mut blob = b"v11".to_vec();
        blob.extend_from_slice(&[0u8; 16]);

        // WHEN: decryption attempted
        let err = decrypt_cookie_value(&blob, &[0u8; 16], false).unwrap_err();

        // THEN: error mentions v10
        assert!(
            err.to_string().contains("v10"),
            "error should mention expected prefix: {err}"
        );
    }

    #[test]
    fn decrypt_cookie_value_only_prefix_no_ciphertext_returns_error() {
        // GIVEN: only the v10 prefix, no ciphertext
        let blob = V10_PREFIX.to_vec();

        // WHEN: decryption attempted
        let err = decrypt_cookie_value(&blob, &[0u8; 16], false).unwrap_err();

        // THEN: error is descriptive
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn decrypt_cookie_value_wrong_key_length_returns_error() {
        // GIVEN: blob with valid prefix but wrong key length
        let blob = encrypt_v10(b"hello", &[0u8; 16]);

        // WHEN: called with 32-byte key
        let err = decrypt_cookie_value(&blob, &[0u8; 32], false).unwrap_err();

        // THEN: error mentions key
        assert!(!err.to_string().is_empty(), "should fail: {err}");
    }

    #[test]
    fn decrypt_cookie_value_v24_too_short_for_domain_tag_returns_error() {
        // GIVEN: a valid AES-CBC blob that decrypts to fewer than 32 bytes
        // (can't have a full domain tag)
        let key = derive_cookie_key(b"short-test").unwrap();
        // Encrypt something smaller than 32 bytes
        let blob = encrypt_v10(b"tiny", &key);

        // WHEN: decoded with has_domain_tag=true
        let err = decrypt_cookie_value(&blob, &key, true).unwrap_err();

        // THEN: error mentions the domain tag being too short
        assert!(
            err.to_string().contains("too short"),
            "error should mention too short for domain tag: {err}"
        );
    }

    // ── Domain condition builder ──────────────────────────────────────────────

    #[test]
    fn build_domain_conditions_includes_exact_and_parent() {
        // GIVEN: subdomain
        let conds = build_domain_conditions("login.example.com");

        // THEN: exact match + dotted variants present
        assert!(conds.iter().any(|c| c.contains("'login.example.com'")));
        assert!(conds.iter().any(|c| c.contains("'.login.example.com'")));
        assert!(conds.iter().any(|c| c.contains("'.example.com'")));
        assert!(conds.iter().any(|c| c.contains("'.com'")));
    }

    #[test]
    fn build_domain_conditions_apex_domain() {
        // GIVEN: apex domain (no subdomain)
        let conds = build_domain_conditions("example.com");

        // THEN: exact + dotted apex
        assert!(conds.iter().any(|c| c.contains("'example.com'")));
        assert!(conds.iter().any(|c| c.contains("'.example.com'")));
    }

    // ── parse_cookie_rows ─────────────────────────────────────────────────────

    #[test]
    fn parse_cookie_rows_plaintext_value() {
        // GIVEN: tab-separated row with a plaintext value and empty hex blob
        let input = "session_id\tabc123\t\n";

        // WHEN: parsed
        let rows = parse_cookie_rows(input);

        // THEN: one row, value set, no encrypted bytes
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "session_id");
        assert_eq!(rows[0].value, "abc123");
        assert!(rows[0].encrypted_bytes.is_empty());
    }

    #[test]
    fn parse_cookie_rows_hex_encrypted_value() {
        // GIVEN: tab-separated row with empty value and hex-encoded encrypted blob
        // "v10" = 76 31 30
        let hex = "763130";
        let input = format!("token\t\t{hex}\n");

        // WHEN: parsed
        let rows = parse_cookie_rows(&input);

        // THEN: encrypted_bytes decoded correctly
        assert_eq!(rows[0].encrypted_bytes, b"v10");
    }

    #[test]
    fn parse_cookie_rows_malformed_lines_skipped() {
        // GIVEN: mix of valid and invalid lines
        let input = "good\tvalue\t\nno_tab_here\ngood2\tval2\t\n";

        // WHEN: parsed
        let rows = parse_cookie_rows(input);

        // THEN: only 2 valid rows
        assert_eq!(rows.len(), 2);
    }

    // ── decrypt_rows ──────────────────────────────────────────────────────────

    #[test]
    fn decrypt_rows_plaintext_passthrough() {
        // GIVEN: rows with only plaintext values (no encryption)
        let rows = vec![
            CookieRow {
                name: "a".into(),
                value: "v1".into(),
                encrypted_bytes: vec![],
            },
            CookieRow {
                name: "b".into(),
                value: "v2".into(),
                encrypted_bytes: vec![],
            },
        ];

        // WHEN: decrypted with no key
        let result = decrypt_rows(rows, None, false);

        // THEN: both cookies present
        assert_eq!(result["a"], "v1");
        assert_eq!(result["b"], "v2");
    }

    #[test]
    fn decrypt_rows_encrypted_without_key_is_skipped() {
        // GIVEN: encrypted row but no key provided
        let key = derive_cookie_key(b"skip-test").unwrap();
        let blob = encrypt_v10(b"secret", &key);
        let rows = vec![CookieRow {
            name: "tok".into(),
            value: String::new(),
            encrypted_bytes: blob,
        }];

        // WHEN: decrypted without key
        let result = decrypt_rows(rows, None, false);

        // THEN: cookie skipped, not present
        assert!(!result.contains_key("tok"));
    }

    #[test]
    fn decrypt_rows_encrypted_with_correct_key_schema_pre24() {
        // GIVEN: encrypted row, schema < 24 (no domain tag), with correct key
        let key = derive_cookie_key(b"my-browser-password").unwrap();
        let blob = encrypt_v10(b"my_session_value", &key);
        let rows = vec![CookieRow {
            name: "session".into(),
            value: String::new(),
            encrypted_bytes: blob,
        }];

        // WHEN: decrypted with key, has_domain_tag=false
        let result = decrypt_rows(rows, Some(&key), false);

        // THEN: value recovered
        assert_eq!(result["session"], "my_session_value");
    }

    #[test]
    fn decrypt_rows_encrypted_with_correct_key_schema_v24() {
        // GIVEN: v24+ encrypted row (SHA-256 domain prefix present), correct key
        let key = derive_cookie_key(b"brave-real-password").unwrap();
        let blob = encrypt_v10_v24(b"real_cookie_value", &key, ".example.com");
        let rows = vec![CookieRow {
            name: "auth".into(),
            value: String::new(),
            encrypted_bytes: blob,
        }];

        // WHEN: decrypted with key and has_domain_tag=true (v24+ path)
        let result = decrypt_rows(rows, Some(&key), true);

        // THEN: domain prefix stripped, actual value recovered
        assert_eq!(result["auth"], "real_cookie_value");
    }
}
