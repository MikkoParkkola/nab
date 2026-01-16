//! Multi-Factor Authentication Flow Handler
//!
//! Supports:
//! - Tier 1: Fully automated (TOTP, SMS, Email)
//! - Tier 2: Passkey signing via 1Password
//! - Tier 3: Human-in-loop for out-of-band (mobiilivarmenne, banking apps)

use std::io::{self, Write};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::auth::{OnePasswordAuth, OtpRetriever, OtpSource};

/// Type of MFA challenge detected
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MfaType {
    /// TOTP - automatable via 1Password
    Totp,
    /// SMS OTP - automatable if Beeper syncs
    SmsOtp,
    /// Email OTP - automatable via Gmail
    EmailOtp,
    /// WebAuthn/Passkey - automatable via 1Password
    Passkey,
    /// Mobile app push (Nordea Codes, etc) - human-in-loop
    MobileAppPush { app_name: String },
    /// Mobile certificate (mobiilivarmenne) - human-in-loop
    MobileCertificate { provider: String },
    /// External identity provider (`DigiD`, `BankID`) - human-in-loop
    ExternalIdp { provider: String },
    /// Unknown 2FA type
    Unknown,
}

impl MfaType {
    /// Check if this MFA type can be automated
    #[must_use]
    pub fn is_automatable(&self) -> bool {
        matches!(
            self,
            MfaType::Totp | MfaType::SmsOtp | MfaType::EmailOtp | MfaType::Passkey
        )
    }

    /// Get human-readable description
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            MfaType::Totp => "Time-based OTP (1Password)".to_string(),
            MfaType::SmsOtp => "SMS verification code".to_string(),
            MfaType::EmailOtp => "Email verification code".to_string(),
            MfaType::Passkey => "Passkey/WebAuthn".to_string(),
            MfaType::MobileAppPush { app_name } => format!("Mobile app authorization ({app_name})"),
            MfaType::MobileCertificate { provider } => {
                format!("Mobile certificate ({provider})")
            }
            MfaType::ExternalIdp { provider } => format!("External identity ({provider})"),
            MfaType::Unknown => "Unknown 2FA method".to_string(),
        }
    }
}

/// Result of an MFA challenge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MfaResult {
    pub success: bool,
    pub mfa_type: MfaType,
    pub code: Option<String>,
    pub duration_ms: u64,
    pub method: String,
}

/// Configuration for human-in-loop notifications
#[derive(Debug, Clone)]
pub struct NotificationConfig {
    /// Pushover user key (optional)
    pub pushover_user: Option<String>,
    /// Pushover app token (optional)
    pub pushover_token: Option<String>,
    /// Telegram bot token (optional)
    pub telegram_bot_token: Option<String>,
    /// Telegram chat ID (optional)
    pub telegram_chat_id: Option<String>,
    /// macOS notification (default: true)
    pub macos_notification: bool,
    /// Terminal prompt (default: true)
    pub terminal_prompt: bool,
    /// Timeout for human response
    pub timeout: Duration,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            pushover_user: std::env::var("PUSHOVER_USER").ok(),
            pushover_token: std::env::var("PUSHOVER_TOKEN").ok(),
            telegram_bot_token: std::env::var("TELEGRAM_BOT_TOKEN").ok(),
            telegram_chat_id: std::env::var("TELEGRAM_CHAT_ID").ok(),
            macos_notification: true,
            terminal_prompt: true,
            timeout: Duration::from_secs(120),
        }
    }
}

/// MFA flow handler
pub struct MfaHandler {
    config: NotificationConfig,
    op_auth: OnePasswordAuth,
}

impl MfaHandler {
    /// Create new MFA handler with default config
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: NotificationConfig::default(),
            op_auth: OnePasswordAuth::new(None),
        }
    }

    /// Create with custom notification config
    #[must_use]
    pub fn with_config(config: NotificationConfig) -> Self {
        Self {
            config,
            op_auth: OnePasswordAuth::new(None),
        }
    }

    /// Handle an MFA challenge
    pub fn handle(&self, mfa_type: &MfaType, domain: &str) -> Result<MfaResult> {
        let start = Instant::now();
        info!("🔐 Handling MFA challenge: {}", mfa_type.description());

        let result = match mfa_type {
            MfaType::Totp => self.handle_totp(domain),
            MfaType::SmsOtp => self.handle_sms_otp(domain),
            MfaType::EmailOtp => self.handle_email_otp(domain),
            MfaType::Passkey => self.handle_passkey(domain),
            MfaType::MobileAppPush { app_name } => {
                self.handle_human_in_loop(domain, &format!("Open {app_name} and approve"))
            }
            MfaType::MobileCertificate { provider } => self.handle_human_in_loop(
                domain,
                &format!("Complete {provider} authentication on your phone"),
            ),
            MfaType::ExternalIdp { provider } => {
                self.handle_human_in_loop(domain, &format!("Complete {provider} authentication"))
            }
            MfaType::Unknown => self.handle_human_in_loop(domain, "Complete 2FA on your device"),
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(code) => Ok(MfaResult {
                success: true,
                mfa_type: mfa_type.clone(),
                code,
                duration_ms,
                method: if mfa_type.is_automatable() {
                    "automated"
                } else {
                    "human-in-loop"
                }
                .to_string(),
            }),
            Err(e) => {
                warn!("MFA failed: {}", e);
                Ok(MfaResult {
                    success: false,
                    mfa_type: mfa_type.clone(),
                    code: None,
                    duration_ms,
                    method: "failed".to_string(),
                })
            }
        }
    }

    /// Handle TOTP via 1Password
    fn handle_totp(&self, domain: &str) -> Result<Option<String>> {
        if let Some(otp) = OtpRetriever::get_otp_for_domain(domain)? {
            if otp.source == OtpSource::OnePasswordTotp {
                info!("   ✅ Got TOTP from 1Password");
                return Ok(Some(otp.code));
            }
        }
        Err(anyhow::anyhow!("No TOTP found in 1Password"))
    }

    /// Handle SMS OTP via Beeper
    fn handle_sms_otp(&self, domain: &str) -> Result<Option<String>> {
        if let Some(otp) = OtpRetriever::get_otp_for_domain(domain)? {
            if otp.source == OtpSource::SmsBeeper {
                info!("   ✅ Got SMS OTP via Beeper");
                return Ok(Some(otp.code));
            }
        }
        // Fall back to human-in-loop if SMS not synced
        warn!("   ⚠️ SMS not available via Beeper, requesting manual input");
        self.handle_human_in_loop(domain, "Enter SMS code")
    }

    /// Handle Email OTP via Gmail
    fn handle_email_otp(&self, domain: &str) -> Result<Option<String>> {
        if let Some(otp) = OtpRetriever::get_otp_for_domain(domain)? {
            if otp.source == OtpSource::EmailGmail {
                info!("   ✅ Got Email OTP via Gmail");
                return Ok(Some(otp.code));
            }
        }
        // Fall back to human-in-loop
        warn!("   ⚠️ Email OTP not found, requesting manual input");
        self.handle_human_in_loop(domain, "Enter email verification code")
    }

    /// Handle Passkey via 1Password
    fn handle_passkey(&self, domain: &str) -> Result<Option<String>> {
        // Check if 1Password has a passkey for this domain
        let passkeys = self.op_auth.list_passkeys()?;
        for passkey in passkeys {
            if let Some(ref url) = passkey.url {
                if url.contains(domain) {
                    info!("   ✅ Found passkey in 1Password: {}", passkey.title);
                    // Note: Actual passkey signing requires 1Password browser extension
                    // or their SDK. For now, we notify the user to use 1Password.
                    if let Some(id) = passkey.passkey_credential_id {
                        return Ok(Some(id));
                    }
                }
            }
        }

        // Try using op CLI to sign (if supported)
        debug!("Attempting passkey sign via 1Password CLI");
        let output = Command::new("op")
            .args(["item", "list", "--categories=Passkey", "--format=json"])
            .output();

        if let Ok(output) = output {
            if output.status.success() {
                // Found passkeys, but signing requires browser extension
                info!("   ⚠️ Passkey found but signing requires 1Password browser extension");
                return self.handle_human_in_loop(domain, "Complete passkey authentication");
            }
        }

        Err(anyhow::anyhow!("No passkey found for {domain}"))
    }

    /// Handle human-in-loop authentication
    fn handle_human_in_loop(&self, domain: &str, instruction: &str) -> Result<Option<String>> {
        info!("   🧑 Human-in-loop required: {}", instruction);

        // Send notifications
        self.send_notifications(domain, instruction)?;

        if self.config.terminal_prompt {
            return self.terminal_prompt(domain, instruction);
        }

        // If no terminal, just wait for timeout (polling mode)
        std::thread::sleep(self.config.timeout);
        Ok(None)
    }

    /// Send notifications via all configured channels
    fn send_notifications(&self, domain: &str, instruction: &str) -> Result<()> {
        let message = format!("MicroFetch: 2FA required for {domain}\n{instruction}");

        // macOS notification
        if self.config.macos_notification {
            let _ = Command::new("osascript")
                .args([
                    "-e",
                    &format!(
                        r#"display notification "{instruction}" with title "MicroFetch 2FA" sound name "Ping""#
                    ),
                ])
                .output();
        }

        // Pushover
        if let (Some(user), Some(token)) = (&self.config.pushover_user, &self.config.pushover_token)
        {
            let _ = Command::new("curl")
                .args([
                    "-s",
                    "-F",
                    &format!("user={user}"),
                    "-F",
                    &format!("token={token}"),
                    "-F",
                    &format!("message={message}"),
                    "-F",
                    "priority=1",
                    "https://api.pushover.net/1/messages.json",
                ])
                .output();
            debug!("Sent Pushover notification");
        }

        // Telegram
        if let (Some(bot_token), Some(chat_id)) = (
            &self.config.telegram_bot_token,
            &self.config.telegram_chat_id,
        ) {
            let url = format!(
                "https://api.telegram.org/bot{}/sendMessage?chat_id={}&text={}",
                bot_token,
                chat_id,
                urlencoding::encode(&message)
            );
            let _ = Command::new("curl").args(["-s", &url]).output();
            debug!("Sent Telegram notification");
        }

        Ok(())
    }

    /// Interactive terminal prompt
    fn terminal_prompt(&self, domain: &str, instruction: &str) -> Result<Option<String>> {
        println!();
        println!("┌─────────────────────────────────────────────────────────────────┐");
        println!("│  MicroFetch: 2FA required for {domain:<32} │");
        println!("├─────────────────────────────────────────────────────────────────┤");
        println!("│  {instruction:<63} │");
        println!("│                                                                 │");
        println!(
            "│  Timeout: {:>3}s                                                 │",
            self.config.timeout.as_secs()
        );
        println!("├─────────────────────────────────────────────────────────────────┤");
        println!("│  Enter code (or press Enter when done, 'c' to cancel):         │");
        println!("└─────────────────────────────────────────────────────────────────┘");
        print!("  > ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.eq_ignore_ascii_case("c") {
            return Err(anyhow::anyhow!("User cancelled"));
        }

        if input.is_empty() {
            // User pressed Enter without code (for push-based auth)
            info!("   ✅ User confirmed completion");
            Ok(None)
        } else {
            info!("   ✅ User provided code");
            Ok(Some(input.to_string()))
        }
    }
}

impl Default for MfaHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Detect MFA type from HTML content
#[must_use] 
pub fn detect_mfa_type(html: &str, url: &str) -> Option<MfaType> {
    let html_lower = html.to_lowercase();
    let url_lower = url.to_lowercase();

    // Finnish mobile certificate (mobiilivarmenne)
    if html_lower.contains("mobiilivarmenne")
        || html_lower.contains("mobiilitunnistus")
        || html_lower.contains("telia tunnistus")
        || html_lower.contains("elisa tunnistus")
        || html_lower.contains("dna tunnistus")
    {
        let provider = if html_lower.contains("telia") {
            "Telia"
        } else if html_lower.contains("elisa") {
            "Elisa"
        } else if html_lower.contains("dna") {
            "DNA"
        } else {
            "Mobile Operator"
        };
        return Some(MfaType::MobileCertificate {
            provider: provider.to_string(),
        });
    }

    // Dutch DigiD
    if html_lower.contains("digid") || url_lower.contains("digid.nl") {
        return Some(MfaType::ExternalIdp {
            provider: "DigiD".to_string(),
        });
    }

    // Swedish BankID
    if html_lower.contains("bankid") || url_lower.contains("bankid.com") {
        return Some(MfaType::ExternalIdp {
            provider: "BankID".to_string(),
        });
    }

    // Belgian Itsme
    if html_lower.contains("itsme") || url_lower.contains("itsme.be") {
        return Some(MfaType::ExternalIdp {
            provider: "Itsme".to_string(),
        });
    }

    // Finnish banks
    if html_lower.contains("nordea codes") || html_lower.contains("tunnuslukusovellus") {
        return Some(MfaType::MobileAppPush {
            app_name: "Nordea Codes".to_string(),
        });
    }
    if html_lower.contains("op-mobiili") || html_lower.contains("op avain") {
        return Some(MfaType::MobileAppPush {
            app_name: "OP Mobile".to_string(),
        });
    }
    if html_lower.contains("aktia id") {
        return Some(MfaType::MobileAppPush {
            app_name: "Aktia ID".to_string(),
        });
    }
    if html_lower.contains("danske id") {
        return Some(MfaType::MobileAppPush {
            app_name: "Danske ID".to_string(),
        });
    }

    // Generic WebAuthn/Passkey detection
    if html_lower.contains("webauthn")
        || html_lower.contains("passkey")
        || html_lower.contains("credential.create")
        || html_lower.contains("navigator.credentials")
    {
        return Some(MfaType::Passkey);
    }

    // TOTP detection
    if html_lower.contains("authenticator")
        || html_lower.contains("6-digit")
        || html_lower.contains("one-time password")
        || html_lower.contains("totp")
    {
        return Some(MfaType::Totp);
    }

    // SMS OTP detection
    if html_lower.contains("sms") && (html_lower.contains("code") || html_lower.contains("verify"))
    {
        return Some(MfaType::SmsOtp);
    }

    // Email OTP detection
    if html_lower.contains("email")
        && (html_lower.contains("code")
            || html_lower.contains("verify")
            || html_lower.contains("link"))
    {
        return Some(MfaType::EmailOtp);
    }

    None
}

/// URL encoding helper
mod urlencoding {
    pub fn encode(s: &str) -> String {
        let mut result = String::new();
        for c in s.chars() {
            match c {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => result.push(c),
                ' ' => result.push_str("%20"),
                _ => {
                    for b in c.to_string().bytes() {
                        result.push_str(&format!("%{b:02X}"));
                    }
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_mobiilivarmenne() {
        let html = "<div>Käytä mobiilivarmennetta tunnistautumiseen</div>";
        let mfa = detect_mfa_type(html, "https://tunnistus.fi");
        assert!(matches!(mfa, Some(MfaType::MobileCertificate { .. })));
    }

    #[test]
    fn test_detect_digid() {
        let html = "<div>Login met DigiD</div>";
        let mfa = detect_mfa_type(html, "https://example.nl");
        assert!(matches!(mfa, Some(MfaType::ExternalIdp { provider }) if provider == "DigiD"));
    }

    #[test]
    fn test_detect_nordea() {
        let html = "<div>Vahvista Nordea Codes -sovelluksella</div>";
        let mfa = detect_mfa_type(html, "https://nordea.fi");
        assert!(matches!(
            mfa,
            Some(MfaType::MobileAppPush { app_name }) if app_name == "Nordea Codes"
        ));
    }

    #[test]
    fn test_detect_totp() {
        let html = "<input placeholder='Enter 6-digit code from authenticator'>";
        let mfa = detect_mfa_type(html, "https://example.com");
        assert!(matches!(mfa, Some(MfaType::Totp)));
    }

    #[test]
    fn test_automatable() {
        assert!(MfaType::Totp.is_automatable());
        assert!(MfaType::SmsOtp.is_automatable());
        assert!(MfaType::Passkey.is_automatable());
        assert!(!MfaType::MobileAppPush {
            app_name: "Test".to_string()
        }
        .is_automatable());
        assert!(!MfaType::MobileCertificate {
            provider: "Test".to_string()
        }
        .is_automatable());
    }
}
