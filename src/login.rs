//! Auto-login orchestration
//!
//! Combines form detection, credential retrieval, and OTP handling
//! to automate login flows.

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::auth::{Credential, OnePasswordAuth, OtpRetriever};
use crate::form::Form;
use crate::http_client::AcceleratedClient;

/// Session storage directory
const SESSION_DIR: &str = ".nab/sessions";

/// Login flow orchestrator
pub struct LoginFlow {
    client: AcceleratedClient,
    one_password: Option<OnePasswordAuth>,
}

impl LoginFlow {
    /// Create a new login flow
    pub fn new(client: AcceleratedClient, use_1password: bool) -> Self {
        let one_password = if use_1password {
            Some(OnePasswordAuth::new(None))
        } else {
            None
        };

        Self {
            client,
            one_password,
        }
    }

    /// Execute login flow
    ///
    /// 1. Fetch login page
    /// 2. Detect login form
    /// 3. Get credentials from 1Password
    /// 4. Fill and submit form
    /// 5. Handle MFA if needed
    /// 6. Return final page
    pub async fn login(&self, url: &str) -> Result<LoginResult> {
        info!("Starting login flow for {}", url);

        // Step 1: Fetch the login page
        debug!("Fetching login page...");
        let page_html = self.client.fetch_text(url).await?;

        // Step 2: Detect login form
        debug!("Detecting login form...");
        let mut form = Form::find_login_form(&page_html)
            .context("Failed to parse forms")?
            .context("No login form found on page")?;

        info!("Found login form: {} {}", form.method, form.action);

        // Step 3: Get credentials
        let credential = if let Some(ref op) = self.one_password {
            debug!("Getting credentials from 1Password...");
            op.get_credential_for_url(url)?
                .context("No credentials found in 1Password for this URL")?
        } else {
            anyhow::bail!("1Password authentication required but not enabled");
        };

        info!("Found credential: {}", credential.title);

        // Step 4: Fill form with credentials
        self.fill_form_with_credential(&mut form, &credential)?;

        // Step 5: Resolve action URL and submit
        let action_url = form.resolve_action(url)?;
        debug!("Submitting form to: {}", action_url);

        let form_data = form.encode_urlencoded();
        let response = self
            .client
            .inner()
            .post(&action_url)
            .header("Content-Type", form.content_type())
            .body(form_data)
            .send()
            .await?;

        let final_url = response.url().to_string();
        let mut body = response.text().await?;

        // Step 6: Check for MFA/2FA requirement
        if self.detect_mfa_required(&body) {
            info!("MFA required, attempting to get OTP...");
            body = self.handle_mfa(url, &body, &credential).await?;
        }

        Ok(LoginResult {
            success: true,
            final_url,
            body,
            message: "Login successful".to_string(),
        })
    }

    /// Fill form fields with credential data
    fn fill_form_with_credential(&self, form: &mut Form, credential: &Credential) -> Result<()> {
        // Common username field names
        let username_fields = ["username", "user", "email", "login", "user_name"];
        // Common password field names
        let password_fields = ["password", "pass", "passwd", "pwd"];

        // Find and fill username field
        for field_name in &username_fields {
            if form.fields.contains_key(*field_name) {
                if let Some(ref username) = credential.username {
                    debug!("Filling username field: {}", field_name);
                    form.fields.insert(field_name.to_string(), username.clone());
                    break;
                }
            }
        }

        // Find and fill password field
        for field_name in &password_fields {
            if form.fields.contains_key(*field_name) {
                if let Some(ref password) = credential.password {
                    debug!("Filling password field: {}", field_name);
                    form.fields.insert(field_name.to_string(), password.clone());
                    break;
                }
            }
        }

        Ok(())
    }

    /// Detect if MFA is required from the response
    fn detect_mfa_required(&self, html: &str) -> bool {
        let mfa_indicators = [
            "two-factor",
            "2fa",
            "mfa",
            "verification",
            "authenticator",
            "security code",
            "otp",
            "one-time password",
        ];

        let html_lower = html.to_lowercase();
        mfa_indicators.iter().any(|indicator| html_lower.contains(indicator))
    }

    /// Handle MFA challenge
    async fn handle_mfa(&self, base_url: &str, html: &str, credential: &Credential) -> Result<String> {
        // Try to get OTP code
        let otp_code = if credential.has_totp {
            // Try 1Password TOTP first
            if let Some(ref op) = self.one_password {
                debug!("Getting TOTP from 1Password...");
                if let Ok(Some(cred)) = op.get_credential_for_url(base_url) {
                    if let Some(totp) = cred.totp {
                        info!("Got TOTP from 1Password");
                        totp
                    } else {
                        self.get_otp_from_other_sources(base_url).await?
                    }
                } else {
                    self.get_otp_from_other_sources(base_url).await?
                }
            } else {
                self.get_otp_from_other_sources(base_url).await?
            }
        } else {
            self.get_otp_from_other_sources(base_url).await?
        };

        // Find MFA form
        let forms = Form::parse_all(html)?;
        let mut mfa_form = forms
            .into_iter()
            .find(|f| {
                f.fields.keys().any(|k| {
                    let k_lower = k.to_lowercase();
                    k_lower.contains("code") || k_lower.contains("otp") || k_lower.contains("token")
                })
            })
            .context("No MFA form found")?;

        // Fill OTP code
        for key in mfa_form.fields.clone().keys() {
            let key_lower = key.to_lowercase();
            if key_lower.contains("code") || key_lower.contains("otp") || key_lower.contains("token") {
                debug!("Filling MFA field: {}", key);
                mfa_form.fields.insert(key.clone(), otp_code.clone());
                break;
            }
        }

        // Submit MFA form
        let action_url = mfa_form.resolve_action(base_url)?;
        let form_data = mfa_form.encode_urlencoded();

        debug!("Submitting MFA form to: {}", action_url);
        let response = self
            .client
            .inner()
            .post(&action_url)
            .header("Content-Type", mfa_form.content_type())
            .body(form_data)
            .send()
            .await?;

        Ok(response.text().await?)
    }

    /// Get OTP from SMS or email sources
    async fn get_otp_from_other_sources(&self, domain: &str) -> Result<String> {
        if let Some(otp_code) = OtpRetriever::get_otp_for_domain(domain)? {
            info!("Got OTP from {}", otp_code.source);
            return Ok(otp_code.code);
        }
        anyhow::bail!("No OTP code available")
    }

    /// Save session cookies to disk
    pub fn save_session(&self, _url: &str, _save: bool) -> Result<()> {
        // Session cookie saving will use the cookie jar from AcceleratedClient
        // The reqwest client already handles cookies automatically
        // For now, we can skip explicit session saving since cookies are
        // maintained in memory during the client lifetime

        // Future: implement persistent session storage in ~/.nab/sessions/
        warn!("Session saving not yet implemented (cookies maintained in memory)");
        Ok(())
    }
}

/// Result of a login attempt
#[derive(Debug, Clone)]
pub struct LoginResult {
    pub success: bool,
    pub final_url: String,
    pub body: String,
    pub message: String,
}

/// Get session directory path
pub fn get_session_dir() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    Ok(home.join(SESSION_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_mfa_required() {
        let client = AcceleratedClient::new().unwrap();
        let flow = LoginFlow::new(client, false);

        let html_with_mfa = r#"
            <html>
                <body>
                    <form>
                        <input name="verification_code">
                    </form>
                </body>
            </html>
        "#;

        assert!(flow.detect_mfa_required(html_with_mfa));

        let html_without_mfa = r#"
            <html>
                <body>
                    <p>Welcome back!</p>
                </body>
            </html>
        "#;

        assert!(!flow.detect_mfa_required(html_without_mfa));
    }

    #[test]
    fn test_fill_form_with_credential() {
        use std::collections::HashMap;

        let client = AcceleratedClient::new().unwrap();
        let flow = LoginFlow::new(client, false);

        let mut form = Form {
            action: "/login".to_string(),
            method: "POST".to_string(),
            enctype: "application/x-www-form-urlencoded".to_string(),
            fields: HashMap::from([
                ("username".to_string(), "".to_string()),
                ("password".to_string(), "".to_string()),
            ]),
            hidden_fields: HashMap::new(),
            is_login_form: true,
        };

        let credential = Credential {
            title: "Test Site".to_string(),
            username: Some("testuser".to_string()),
            password: Some("testpass".to_string()),
            url: None,
            totp: None,
            has_totp: false,
            passkey_credential_id: None,
        };

        flow.fill_form_with_credential(&mut form, &credential).unwrap();

        assert_eq!(form.fields.get("username"), Some(&"testuser".to_string()));
        assert_eq!(form.fields.get("password"), Some(&"testpass".to_string()));
    }
}
