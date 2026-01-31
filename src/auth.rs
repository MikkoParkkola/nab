//! Authentication Module
//!
//! Supports:
//! - 1Password CLI integration for credentials, passkeys, and TOTP
//! - SMS OTP extraction via Beeper MCP
//! - Email OTP extraction via Gmail API
//! - Browser cookie extraction (Brave, Chrome, Firefox, Safari)
//! - WebAuthn/Passkey authentication

use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// OTP (One-Time Password) with source information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtpCode {
    pub code: String,
    pub source: OtpSource,
    pub expires_in_seconds: Option<u32>,
}

/// Source of the OTP code
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OtpSource {
    /// TOTP from 1Password
    OnePasswordTotp,
    /// SMS received via Beeper
    SmsBeeper,
    /// Email OTP via Gmail
    EmailGmail,
    /// Unknown source
    Unknown,
}

impl std::fmt::Display for OtpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OtpSource::OnePasswordTotp => write!(f, "1Password TOTP"),
            OtpSource::SmsBeeper => write!(f, "SMS (Beeper)"),
            OtpSource::EmailGmail => write!(f, "Email (Gmail)"),
            OtpSource::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Credential from 1Password
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub title: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub url: Option<String>,
    pub totp: Option<String>,
    pub has_totp: bool,
    /// For passkeys - the credential ID
    pub passkey_credential_id: Option<String>,
}

/// 1Password item structure (from `op item get --format=json`)
#[derive(Debug, Deserialize)]
struct OpItem {
    id: String,
    title: String,
    /// Category field from 1Password API - retained for serde completeness but not currently used
    #[allow(dead_code)]
    category: String,
    urls: Option<Vec<OpUrl>>,
    fields: Option<Vec<OpField>>,
}

#[derive(Debug, Deserialize)]
struct OpUrl {
    href: String,
    primary: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct OpField {
    id: String,
    label: Option<String>,
    value: Option<String>,
    #[serde(rename = "type")]
    field_type: Option<String>,
}

/// 1Password CLI wrapper
pub struct OnePasswordAuth {
    /// Vault to search (optional)
    vault: Option<String>,
}

impl OnePasswordAuth {
    /// Create new 1Password auth with optional vault filter
    #[must_use]
    pub fn new(vault: Option<String>) -> Self {
        Self { vault }
    }

    /// Check if 1Password CLI is available and authenticated
    #[must_use]
    pub fn is_available() -> bool {
        Command::new("op")
            .args(["account", "list"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get credential for a URL/domain
    pub fn get_credential_for_url(&self, url: &str) -> Result<Option<Credential>> {
        // Extract domain from URL
        let domain = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(std::string::ToString::to_string))
            .unwrap_or_else(|| url.to_string());

        debug!("Searching 1Password for domain: {}", domain);

        // Search for items matching the domain
        let mut cmd = Command::new("op");
        cmd.args(["item", "list", "--format=json"]);

        if let Some(ref vault) = self.vault {
            cmd.args(["--vault", vault]);
        }

        let output = cmd.output().context("Failed to run 'op item list'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("1Password search failed: {}", stderr);
            return Ok(None);
        }

        let items: Vec<OpListItem> =
            serde_json::from_slice(&output.stdout).context("Failed to parse 1Password items")?;

        // Find matching item by URL or title
        for item in items {
            // Check if title contains domain
            if item.title.to_lowercase().contains(&domain.to_lowercase()) {
                return self.get_item_details(&item.id);
            }

            // Check URLs
            if let Some(ref urls) = item.urls {
                for url_entry in urls {
                    if url_entry.href.contains(&domain) {
                        return self.get_item_details(&item.id);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Get full item details by ID
    fn get_item_details(&self, item_id: &str) -> Result<Option<Credential>> {
        debug!("Getting 1Password item details: {}", item_id);

        let output = Command::new("op")
            .args(["item", "get", item_id, "--format=json"])
            .output()
            .context("Failed to run 'op item get'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to get 1Password item: {}", stderr);
            return Ok(None);
        }

        let item: OpItem =
            serde_json::from_slice(&output.stdout).context("Failed to parse 1Password item")?;

        let mut username = None;
        let mut password = None;
        let mut totp = None;
        let mut has_totp = false;
        let mut passkey_credential_id = None;

        if let Some(fields) = item.fields {
            for field in &fields {
                match field.id.as_str() {
                    "username" => username = field.value.clone(),
                    "password" => password = field.value.clone(),
                    _ => {
                        // Check by label
                        if let Some(ref label) = field.label {
                            let label_lower = label.to_lowercase();
                            if (label_lower.contains("username") || label_lower.contains("email"))
                                && username.is_none()
                            {
                                username = field.value.clone();
                            }
                            if label_lower.contains("password") && password.is_none() {
                                password = field.value.clone();
                            }
                            if label_lower.contains("one-time") || label_lower.contains("totp") {
                                has_totp = true;
                                // For TOTP, we need to get the current code
                                if field.field_type.as_deref() == Some("OTP") {
                                    totp = self.get_totp_code(&item.id).ok().flatten();
                                }
                            }
                            if label_lower.contains("passkey") {
                                passkey_credential_id = field.value.clone();
                            }
                        }
                    }
                }
            }
        }

        let url = item.urls.and_then(|urls| {
            urls.into_iter()
                .find(|u| u.primary.unwrap_or(false))
                .or(None)
                .map(|u| u.href)
        });

        Ok(Some(Credential {
            title: item.title,
            username,
            password,
            url,
            totp,
            has_totp,
            passkey_credential_id,
        }))
    }

    /// Get current TOTP code for an item (internal)
    fn get_totp_code(&self, item_id: &str) -> Result<Option<String>> {
        let output = Command::new("op")
            .args(["item", "get", item_id, "--otp"])
            .output()
            .context("Failed to get TOTP")?;

        if output.status.success() {
            let code = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !code.is_empty() {
                return Ok(Some(code));
            }
        }

        Ok(None)
    }

    /// Get TOTP with full `OtpCode` structure
    pub fn get_totp(&self, url: &str) -> Result<Option<OtpCode>> {
        if let Some(cred) = self.get_credential_for_url(url)? {
            if let Some(code) = cred.totp {
                return Ok(Some(OtpCode {
                    code,
                    source: OtpSource::OnePasswordTotp,
                    expires_in_seconds: Some(30), // TOTP typically 30 seconds
                }));
            }
        }
        Ok(None)
    }

    /// Get TOTP directly by item title
    pub fn get_totp_by_title(&self, title: &str) -> Result<Option<OtpCode>> {
        let output = Command::new("op")
            .args(["item", "get", title, "--otp"])
            .output()
            .context("Failed to get TOTP")?;

        if output.status.success() {
            let code = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !code.is_empty() {
                return Ok(Some(OtpCode {
                    code,
                    source: OtpSource::OnePasswordTotp,
                    expires_in_seconds: Some(30),
                }));
            }
        }

        Ok(None)
    }

    /// List all available passkeys
    pub fn list_passkeys(&self) -> Result<Vec<Credential>> {
        let mut cmd = Command::new("op");
        cmd.args(["item", "list", "--categories=Passkey", "--format=json"]);

        if let Some(ref vault) = self.vault {
            cmd.args(["--vault", vault]);
        }

        let output = cmd.output().context("Failed to list passkeys")?;

        if !output.status.success() {
            return Ok(vec![]);
        }

        let items: Vec<OpListItem> = serde_json::from_slice(&output.stdout).unwrap_or_default();

        let mut passkeys = Vec::new();
        for item in items {
            if let Ok(Some(cred)) = self.get_item_details(&item.id) {
                passkeys.push(cred);
            }
        }

        Ok(passkeys)
    }
}

#[derive(Debug, Deserialize)]
struct OpListItem {
    id: String,
    title: String,
    urls: Option<Vec<OpUrl>>,
}

/// Multi-source OTP retrieval
pub struct OtpRetriever;

impl OtpRetriever {
    /// Get OTP from all available sources for a domain
    /// Checks: 1Password TOTP → SMS (Beeper) → Email (Gmail)
    pub fn get_otp_for_domain(domain: &str) -> Result<Option<OtpCode>> {
        info!("🔐 Searching for OTP codes for: {}", domain);

        // 1. Try 1Password TOTP first (fastest, most reliable)
        let op_auth = OnePasswordAuth::new(None);
        if let Ok(Some(otp)) = op_auth.get_totp(&format!("https://{domain}")) {
            info!("   ✅ Found TOTP in 1Password");
            return Ok(Some(otp));
        }

        // 2. Try SMS via Beeper MCP
        if let Ok(Some(otp)) = Self::get_sms_otp(domain) {
            info!("   ✅ Found SMS OTP via Beeper");
            return Ok(Some(otp));
        }

        // 3. Try Email via Gmail
        if let Ok(Some(otp)) = Self::get_email_otp(domain) {
            info!("   ✅ Found Email OTP via Gmail");
            return Ok(Some(otp));
        }

        info!("   ❌ No OTP found from any source");
        Ok(None)
    }

    /// Extract OTP from recent SMS messages via Beeper MCP
    fn get_sms_otp(domain: &str) -> Result<Option<OtpCode>> {
        // Call mcp-cli to query Beeper for recent SMS
        let output = Command::new("mcp-cli")
            .args([
                "beeper/search_messages",
                &format!(r#"{{"query": "{domain} code OR {domain} verification", "limit": 5}}"#),
            ])
            .output();

        if let Ok(output) = output {
            if output.status.success() {
                let response = String::from_utf8_lossy(&output.stdout);
                // Extract OTP code from message (6-digit pattern)
                if let Some(code) = Self::extract_otp_from_text(&response) {
                    return Ok(Some(OtpCode {
                        code,
                        source: OtpSource::SmsBeeper,
                        expires_in_seconds: Some(300), // SMS codes typically 5 minutes
                    }));
                }
            }
        }

        Ok(None)
    }

    /// Extract OTP from recent emails via Gmail API
    fn get_email_otp(domain: &str) -> Result<Option<OtpCode>> {
        // Call mcp-cli to query Gmail for recent verification emails
        let output = Command::new("mcp-cli")
            .args([
                "gmail/search_emails",
                &format!(
                    r#"{{"query": "from:{domain} subject:(code OR verification OR OTP) newer_than:10m", "max_results": 5}}"#
                ),
            ])
            .output();

        if let Ok(output) = output {
            if output.status.success() {
                let response = String::from_utf8_lossy(&output.stdout);
                // Extract OTP code from email body
                if let Some(code) = Self::extract_otp_from_text(&response) {
                    return Ok(Some(OtpCode {
                        code,
                        source: OtpSource::EmailGmail,
                        expires_in_seconds: Some(600), // Email codes typically 10 minutes
                    }));
                }
            }
        }

        Ok(None)
    }

    /// Extract 6-digit OTP code from text
    fn extract_otp_from_text(text: &str) -> Option<String> {
        // Common OTP patterns:
        // - 6 digits: 123456
        // - 6 digits with spaces: 123 456
        // - 6 digits with dash: 123-456
        // - 8 digits for some services
        use std::sync::LazyLock;
        static OTP_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"(?:code|otp|verification)[:\s]*(\d{6,8})|\b(\d{3}[-\s]?\d{3})\b")
                .unwrap()
        });

        if let Some(caps) = OTP_REGEX.captures(text) {
            if let Some(code) = caps.get(1).or_else(|| caps.get(2)) {
                let code_str = code.as_str().replace(['-', ' '], "");
                if code_str.len() >= 6 {
                    return Some(code_str);
                }
            }
        }

        // Fallback: find any 6-digit sequence
        static DIGIT_REGEX: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"\b(\d{6})\b").unwrap());

        DIGIT_REGEX
            .captures(text)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }
}

/// Cookie source for browser cookie extraction
#[derive(Debug, Clone, Copy)]
pub enum CookieSource {
    Brave,
    Chrome,
    Firefox,
    Safari,
}

impl CookieSource {
    /// Get the cookie database path for this browser
    fn cookie_path(&self) -> Option<std::path::PathBuf> {
        let home = dirs::home_dir()?;
        let path = match self {
            CookieSource::Brave => {
                home.join("Library/Application Support/BraveSoftware/Brave-Browser/Default/Cookies")
            }
            CookieSource::Chrome => {
                home.join("Library/Application Support/Google/Chrome/Default/Cookies")
            }
            CookieSource::Firefox => home.join("Library/Application Support/Firefox/Profiles"), // Needs profile detection
            CookieSource::Safari => home.join("Library/Cookies/Cookies.binarycookies"),
        };
        Some(path)
    }

    /// Get the Keychain service name for this browser
    fn keychain_service(&self) -> &'static str {
        match self {
            CookieSource::Brave => "Brave Safe Storage",
            CookieSource::Chrome => "Chrome Safe Storage",
            CookieSource::Firefox => "",
            CookieSource::Safari => "",
        }
    }

    /// Get encryption key from macOS Keychain
    fn get_keychain_key(&self) -> Result<Vec<u8>> {
        let service = self.keychain_service();
        if service.is_empty() {
            anyhow::bail!("Browser does not use Keychain encryption");
        }

        let output = Command::new("security")
            .args(["find-generic-password", "-s", service, "-w"])
            .output()
            .context("Failed to access Keychain")?;

        if !output.status.success() {
            anyhow::bail!("Keychain access denied for {service}");
        }

        Ok(output.stdout.trim_ascii().to_vec())
    }

    /// Get cookies for a domain from the specified browser
    ///
    /// Tries native Rust extraction first, falls back to Python `browser_cookie3`
    pub fn get_cookies(&self, domain: &str) -> Result<HashMap<String, String>> {
        debug!("Getting cookies for {} from {:?}", domain, self);

        // Try native Rust extraction first
        match self.get_cookies_native(domain) {
            Ok(cookies) if !cookies.is_empty() => {
                info!(
                    "Native cookie extraction succeeded: {} cookies",
                    cookies.len()
                );
                return Ok(cookies);
            }
            Ok(_) => {
                debug!("Native extraction returned empty, trying Python fallback");
            }
            Err(e) => {
                debug!("Native extraction failed: {}, trying Python fallback", e);
            }
        }

        // Fallback to Python browser_cookie3 (handles all encryption edge cases)
        self.get_cookies_via_python(domain)
    }

    /// Native Rust cookie extraction - tries to extract cookies without Python dependency
    fn get_cookies_native(&self, domain: &str) -> Result<HashMap<String, String>> {
        let cookie_path = self
            .cookie_path()
            .context("Could not determine cookie path")?;
        if !cookie_path.exists() {
            warn!("Cookie database not found: {:?}", cookie_path);
            return Ok(HashMap::new());
        }

        // Copy database to temp file (browser may have it locked)
        let temp_dir =
            std::env::temp_dir().join(format!("microfetch_cookies_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir)?;
        let temp_db = temp_dir.join("Cookies");

        std::fs::copy(&cookie_path, &temp_db)?;

        // Also copy WAL/SHM if present
        for suffix in ["-wal", "-shm"] {
            let wal = cookie_path.with_extension(format!("Cookies{suffix}"));
            if wal.exists() {
                let _ = std::fs::copy(&wal, temp_db.with_extension(format!("Cookies{suffix}")));
            }
        }

        // Read cookies using sqlite3 CLI (avoids linking sqlite)
        // Cookie matching rules:
        // - Cookie on .example.com matches sub.example.com, example.com, etc.
        // - Cookie on example.com matches only example.com exactly
        // - We need to match: exact domain, .domain (parent), and any .parent where domain is subdomain

        // Extract base domain parts for subdomain matching
        let domain_parts: Vec<&str> = domain.split('.').collect();
        let mut conditions = vec![
            format!("host_key = '{domain}'"),  // Exact match
            format!("host_key = '.{domain}'"), // Parent domain with dot
        ];

        // Add parent domain matches (e.g., for areena.yle.fi, also match .yle.fi, .fi)
        for i in 1..domain_parts.len() {
            let parent = domain_parts[i..].join(".");
            conditions.push(format!("host_key = '.{parent}'"));
        }

        let where_clause = conditions.join(" OR ");
        let query =
            format!("SELECT name, value, encrypted_value FROM cookies WHERE {where_clause}");

        debug!("Cookie SQL query for '{}': WHERE {}", domain, where_clause);

        let output = Command::new("sqlite3")
            .args(["-separator", "\t", temp_db.to_str().unwrap(), &query])
            .output()
            .context("Failed to query cookie database")?;

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("SQLite query failed: {}", stderr);
            return Ok(HashMap::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut cookies = HashMap::new();

        // Get decryption key if needed
        let key = self.get_keychain_key().ok();

        for line in stdout.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 2 {
                let name = parts[0].to_string();
                let value = parts[1].to_string();

                // If value is empty and we have encrypted_value, try to decrypt
                if value.is_empty() && parts.len() >= 3 {
                    if let Some(k) = key.as_ref() {
                        // Try native decryption - if it fails, we'll fall back to Python at the caller
                        if let Ok(decrypted) = self.decrypt_cookie_value(parts[2], k) {
                            cookies.insert(name, decrypted);
                            continue;
                        }
                        // If decryption fails, return error to trigger Python fallback
                        anyhow::bail!("Cookie decryption failed for encrypted values");
                    }
                }

                if !value.is_empty() {
                    cookies.insert(name, value);
                }
            }
        }

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

    /// Decrypt a Chrome/Brave encrypted cookie value
    fn decrypt_cookie_value(&self, _encrypted_hex: &str, _key: &[u8]) -> Result<String> {
        // Chrome uses AES-128-CBC with PBKDF2-derived key
        // For simplicity, fall back to Python for decryption
        anyhow::bail!("Encrypted cookie - use Python fallback")
    }

    /// Fallback: Get cookies via Python `browser_cookie3`
    fn get_cookies_via_python(&self, domain: &str) -> Result<HashMap<String, String>> {
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
    # Don't use domain_name parameter - it doesn't support subdomain matching
    # Instead, fetch all cookies and filter ourselves
    cj = bc.{browser_fn}()

    # Cookie domain matching rules:
    # - Cookie on .example.com matches sub.example.com, example.com, etc.
    # - Cookie on example.com (no dot) matches only example.com exactly
    def matches_cookie_domain(cookie_domain, request_domain):
        if cookie_domain.startswith('.'):
            # Parent domain with leading dot - matches request domain and all subdomains
            parent = cookie_domain[1:]  # Remove leading dot
            return request_domain == parent or request_domain.endswith('.' + parent)
        else:
            # No leading dot - exact match only
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

    /// Get cookie header string for HTTP requests
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

// ═══════════════════════════════════════════════════════════════════════════════
// Keychain & Browser Password Support
// ═══════════════════════════════════════════════════════════════════════════════

/// Source for retrieving credentials
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

/// Unified credential retriever - tries multiple sources
pub struct CredentialRetriever;

impl CredentialRetriever {
    /// Get credentials for a URL from all available sources
    ///
    /// Priority: 1Password > Keychain > Browser passwords
    pub fn get_credential_for_url(url: &str) -> Result<Option<Credential>> {
        // Extract domain
        let domain = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(std::string::ToString::to_string))
            .unwrap_or_default();

        if domain.is_empty() {
            return Ok(None);
        }

        // Try 1Password first (most secure, has TOTP)
        if OnePasswordAuth::is_available() {
            let auth = OnePasswordAuth::new(None);
            if let Ok(Some(cred)) = auth.get_credential_for_url(url) {
                info!("Found credential in 1Password: {}", cred.title);
                return Ok(Some(cred));
            }
        }

        // Try macOS Keychain
        if let Some(cred) = Self::get_keychain_credential(&domain)? {
            info!("Found credential in Keychain");
            return Ok(Some(cred));
        }

        // Try browser passwords
        if let Some(cred) = Self::get_browser_credential(&domain)? {
            info!("Found credential in browser");
            return Ok(Some(cred));
        }

        Ok(None)
    }

    /// Get credential from macOS Keychain
    fn get_keychain_credential(domain: &str) -> Result<Option<Credential>> {
        // Use security command to find internet password
        let output = Command::new("security")
            .args([
                "find-internet-password",
                "-s",
                domain,
                "-g", // Show password
            ])
            .output();

        if let Ok(output) = output {
            if output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);

                // Parse username from stdout
                let username = stdout
                    .lines()
                    .find(|l| l.contains("\"acct\""))
                    .and_then(|l| l.split('"').nth(3))
                    .map(String::from);

                // Parse password from stderr (yes, really - security outputs it there)
                let password = stderr
                    .lines()
                    .find(|l| l.starts_with("password:"))
                    .and_then(|l| {
                        if l.contains('"') {
                            l.split('"').nth(1).map(String::from)
                        } else {
                            // Hex encoded or empty
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
        }

        Ok(None)
    }

    /// Get credential from browser password manager
    fn get_browser_credential(domain: &str) -> Result<Option<Credential>> {
        // Try Brave first, then Chrome
        for browser in ["brave", "chrome"] {
            if let Some(cred) = Self::get_chromium_password(browser, domain)? {
                return Ok(Some(cred));
            }
        }
        Ok(None)
    }

    /// Get password from Chromium-based browser (Brave/Chrome)
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

        // Copy to temp file (browser locks it)
        let temp_dir =
            std::env::temp_dir().join(format!("microfetch_logins_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir)?;
        let temp_db = temp_dir.join("Login Data");
        std::fs::copy(&login_data_path, &temp_db)?;

        // Query with sqlite3
        let query = format!(
            "SELECT origin_url, username_value FROM logins WHERE origin_url LIKE '%{domain}%' LIMIT 1"
        );

        let output = Command::new("sqlite3")
            .args(["-separator", "\t", temp_db.to_str().unwrap(), &query])
            .output();

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);

        if let Ok(output) = output {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(line) = stdout.lines().next() {
                    let parts: Vec<&str> = line.split('\t').collect();
                    if parts.len() >= 2 {
                        // Note: Password is encrypted - would need Keychain key to decrypt
                        // For now, just return username (password requires same decryption as cookies)
                        return Ok(Some(Credential {
                            title: format!("{browser} password: {domain}"),
                            username: Some(parts[1].to_string()),
                            password: None, // Encrypted - would need decryption
                            url: Some(parts[0].to_string()),
                            totp: None,
                            has_totp: false,
                            passkey_credential_id: None,
                        }));
                    }
                }
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_op_available() {
        // This test will pass if 1Password CLI is installed
        let available = OnePasswordAuth::is_available();
        println!("1Password CLI available: {}", available);
    }
}
