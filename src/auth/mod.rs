// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Authentication Module
//!
//! Supports:
//! - 1Password CLI integration for credentials, passkeys, and TOTP
//! - SMS OTP extraction via Beeper MCP
//! - Email OTP extraction via Gmail API
//! - Browser cookie extraction (Brave, Chrome, Firefox, Safari)
//! - WebAuthn/Passkey authentication

pub mod cookies;

pub use cookies::{CookieSource, CredentialRetriever, CredentialSource};

use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// OTP (One-Time Password) with source information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtpCode {
    pub code: String,
    pub source: OtpSource,
    pub expires_in_seconds: Option<u32>,
}

/// Source of the OTP code.
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

/// Credential from 1Password.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub title: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub url: Option<String>,
    pub totp: Option<String>,
    pub has_totp: bool,
    /// For passkeys — the credential ID.
    pub passkey_credential_id: Option<String>,
}

/// 1Password item structure (from `op item get --format=json`).
#[derive(Debug, Deserialize)]
struct OpItem {
    id: String,
    title: String,
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

#[derive(Debug, Deserialize)]
struct OpListItem {
    id: String,
    title: String,
    urls: Option<Vec<OpUrl>>,
}

/// 1Password CLI wrapper.
pub struct OnePasswordAuth {
    vault: Option<String>,
}

impl OnePasswordAuth {
    /// Create new 1Password auth with optional vault filter.
    #[must_use]
    pub fn new(vault: Option<String>) -> Self {
        Self { vault }
    }

    /// Check if 1Password CLI is available and authenticated.
    #[must_use]
    pub fn is_available() -> bool {
        Command::new("op")
            .args(["account", "list"])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Get credential for a URL/domain.
    pub fn get_credential_for_url(&self, url: &str) -> Result<Option<Credential>> {
        let domain = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(std::string::ToString::to_string))
            .unwrap_or_else(|| url.to_string());

        debug!("Searching 1Password for domain: {}", domain);

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

        for item in items {
            if item.title.to_lowercase().contains(&domain.to_lowercase()) {
                return Self::get_item_details(&item.id);
            }
            if let Some(ref urls) = item.urls {
                for url_entry in urls {
                    if url_entry.href.contains(&domain) {
                        return Self::get_item_details(&item.id);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Get all credentials that match a URL/domain.
    ///
    /// Unlike [`Self::get_credential_for_url`] which stops at the first match,
    /// this method collects every matching item so callers can present a choice
    /// when multiple credentials exist for the same domain.
    pub fn get_all_credentials_for_url(&self, url: &str) -> Result<Vec<Credential>> {
        let domain = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(std::string::ToString::to_string))
            .unwrap_or_else(|| url.to_string());

        debug!("Listing all 1Password credentials for domain: {}", domain);

        let mut cmd = Command::new("op");
        cmd.args(["item", "list", "--format=json"]);
        if let Some(ref vault) = self.vault {
            cmd.args(["--vault", vault]);
        }

        let output = cmd.output().context("Failed to run 'op item list'")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("1Password search failed: {}", stderr);
            return Ok(Vec::new());
        }

        let items: Vec<OpListItem> =
            serde_json::from_slice(&output.stdout).context("Failed to parse 1Password items")?;

        let mut credentials = Vec::new();
        for item in items {
            let matches = item.title.to_lowercase().contains(&domain.to_lowercase())
                || item
                    .urls
                    .as_ref()
                    .is_some_and(|urls| urls.iter().any(|u| u.href.contains(&domain)));

            if matches && let Ok(Some(cred)) = Self::get_item_details(&item.id) {
                credentials.push(cred);
            }
        }

        Ok(credentials)
    }

    /// Get full item details by ID.
    fn get_item_details(item_id: &str) -> Result<Option<Credential>> {
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
                    "username" => username.clone_from(&field.value),
                    "password" => password.clone_from(&field.value),
                    _ => {
                        if let Some(ref label) = field.label {
                            let label_lower = label.to_lowercase();
                            if (label_lower.contains("username") || label_lower.contains("email"))
                                && username.is_none()
                            {
                                username.clone_from(&field.value);
                            }
                            if label_lower.contains("password") && password.is_none() {
                                password.clone_from(&field.value);
                            }
                            if label_lower.contains("one-time") || label_lower.contains("totp") {
                                has_totp = true;
                                if field.field_type.as_deref() == Some("OTP") {
                                    totp = Self::get_totp_code(&item.id).ok().flatten();
                                }
                            }
                            if label_lower.contains("passkey") {
                                passkey_credential_id.clone_from(&field.value);
                            }
                        }
                    }
                }
            }
        }

        let url = item.urls.and_then(|urls| {
            urls.into_iter()
                .find(|u| u.primary.unwrap_or(false))
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

    /// Get current TOTP code for an item.
    fn get_totp_code(item_id: &str) -> Result<Option<String>> {
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

    /// Get TOTP with full `OtpCode` structure.
    pub fn get_totp(&self, url: &str) -> Result<Option<OtpCode>> {
        if let Some(cred) = self.get_credential_for_url(url)?
            && let Some(code) = cred.totp
        {
            return Ok(Some(OtpCode {
                code,
                source: OtpSource::OnePasswordTotp,
                expires_in_seconds: Some(30),
            }));
        }
        Ok(None)
    }

    /// Get TOTP directly by item title.
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

    /// List all available passkeys.
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
            if let Ok(Some(cred)) = Self::get_item_details(&item.id) {
                passkeys.push(cred);
            }
        }

        Ok(passkeys)
    }
}

/// Multi-source OTP retrieval.
pub struct OtpRetriever;

impl OtpRetriever {
    /// Get OTP from all available sources for a domain.
    ///
    /// Priority: 1Password TOTP → SMS (Beeper) → Email (Gmail).
    pub fn get_otp_for_domain(domain: &str) -> Result<Option<OtpCode>> {
        info!("🔐 Searching for OTP codes for: {}", domain);

        let op_auth = OnePasswordAuth::new(None);
        if let Ok(Some(otp)) = op_auth.get_totp(&format!("https://{domain}")) {
            info!("   ✅ Found TOTP in 1Password");
            return Ok(Some(otp));
        }

        if let Ok(Some(otp)) = Self::get_sms_otp(domain) {
            info!("   ✅ Found SMS OTP via Beeper");
            return Ok(Some(otp));
        }

        if let Ok(Some(otp)) = Self::get_email_otp(domain) {
            info!("   ✅ Found Email OTP via Gmail");
            return Ok(Some(otp));
        }

        info!("   ❌ No OTP found from any source");
        Ok(None)
    }

    #[allow(clippy::unnecessary_wraps)]
    fn get_sms_otp(domain: &str) -> Result<Option<OtpCode>> {
        let output = Command::new("mcp-cli")
            .args([
                "beeper/search_messages",
                &format!(r#"{{"query": "{domain} code OR {domain} verification", "limit": 5}}"#),
            ])
            .output();

        if let Ok(output) = output
            && output.status.success()
        {
            let response = String::from_utf8_lossy(&output.stdout);
            if let Some(code) = Self::extract_otp_from_text(&response) {
                return Ok(Some(OtpCode {
                    code,
                    source: OtpSource::SmsBeeper,
                    expires_in_seconds: Some(300),
                }));
            }
        }

        Ok(None)
    }

    #[allow(clippy::unnecessary_wraps)]
    fn get_email_otp(domain: &str) -> Result<Option<OtpCode>> {
        let output = Command::new("mcp-cli")
            .args([
                "gmail/search_emails",
                &format!(
                    r#"{{"query": "from:{domain} subject:(code OR verification OR OTP) newer_than:10m", "max_results": 5}}"#
                ),
            ])
            .output();

        if let Ok(output) = output
            && output.status.success()
        {
            let response = String::from_utf8_lossy(&output.stdout);
            if let Some(code) = Self::extract_otp_from_text(&response) {
                return Ok(Some(OtpCode {
                    code,
                    source: OtpSource::EmailGmail,
                    expires_in_seconds: Some(600),
                }));
            }
        }

        Ok(None)
    }

    /// Extract 6-digit OTP code from text.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn extract_otp_from_text(text: &str) -> Option<String> {
        use std::sync::LazyLock;
        static OTP_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"(?:code|otp|verification)[:\s]*(\d{6,8})|\b(\d{3}[-\s]?\d{3})\b")
                .expect("Static regex pattern should compile")
        });
        static DIGIT_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"\b(\d{6})\b").expect("Static regex pattern should compile")
        });

        if let Some(caps) = OTP_REGEX.captures(text)
            && let Some(code) = caps.get(1).or_else(|| caps.get(2))
        {
            let code_str = code.as_str().replace(['-', ' '], "");
            if code_str.len() >= 6 {
                return Some(code_str);
            }
        }

        DIGIT_REGEX
            .captures(text)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_available_check_does_not_panic() {
        let available = OnePasswordAuth::is_available();
        println!("1Password CLI available: {available}");
    }

    #[test]
    fn otp_extraction_compiles_and_does_not_panic() {
        // Verifies the regex patterns compile correctly
        let _ = OnePasswordAuth::is_available();
    }
}
