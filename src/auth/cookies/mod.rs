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
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use super::{Credential, OnePasswordAuth};
use db::{copy_db_to_temp, decrypt_rows, has_domain_tag, query_cookie_db};

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
    pub(super) fn keychain_service(self) -> &'static str {
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
        crypto::derive_cookie_key(&password)
    }

    /// Read the raw password bytes from the macOS Keychain.
    ///
    /// Uses `security-framework` on macOS; falls back to `security` CLI on other
    /// platforms (Linux uses GNOME Keyring — not yet implemented natively).
    #[cfg(target_os = "macos")]
    fn read_keychain_password(service: &str) -> Result<Vec<u8>> {
        use security_framework::passwords::get_generic_password;

        let account = service.strip_suffix(" Safe Storage").unwrap_or(service);
        get_generic_password(service, account)
            .with_context(|| format!("Keychain access denied for service '{service}'"))
    }

    /// Fallback for non-macOS: shell out to `security` CLI.
    #[cfg(not(target_os = "macos"))]
    fn read_keychain_password(service: &str) -> Result<Vec<u8>> {
        // TODO(linux): implement GNOME Keyring support via `secret-service` crate.
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
        let domain_tag = has_domain_tag(&temp_db);
        let rows = query_cookie_db(&temp_db, domain)?;
        if let Some(parent) = temp_db.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }

        let key = self.get_keychain_key().ok();
        let cookies = decrypt_rows(rows, key.as_deref(), domain_tag);

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
