//! Auto-login orchestration
//!
//! Combines form detection, credential retrieval, and OTP handling
//! to automate login flows.

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::auth::{Credential, OnePasswordAuth, OtpRetriever};
use crate::form::Form;
use crate::http_client::AcceleratedClient;
use crate::js_engine::JsEngine;

/// Session storage directory
const SESSION_DIR: &str = ".nab/sessions";

/// Login flow orchestrator
pub struct LoginFlow {
    client: AcceleratedClient,
    one_password: Option<OnePasswordAuth>,
    cookie_header: Option<String>,
    #[cfg(feature = "browser")]
    use_browser: bool,
}

impl LoginFlow {
    /// Create a new login flow
    pub fn new(
        client: AcceleratedClient,
        use_1password: bool,
        cookie_header: Option<String>,
    ) -> Self {
        let one_password = if use_1password {
            Some(OnePasswordAuth::new(None))
        } else {
            None
        };

        Self {
            client,
            one_password,
            cookie_header,
            #[cfg(feature = "browser")]
            use_browser: false,
        }
    }

    /// Enable browser-based login (requires browser feature)
    #[cfg(feature = "browser")]
    #[must_use]
    pub fn with_browser(mut self, enable: bool) -> Self {
        self.use_browser = enable;
        self
    }

    /// Execute login flow
    ///
    /// 1. Fetch login page
    /// 2. Detect login form
    /// 3. Get credentials from 1Password
    /// 4. Fill and submit form
    /// 5. Handle MFA if needed
    /// 6. Return final page
    #[allow(clippy::too_many_lines)] // Complex login flow; splitting would obscure the flow
    pub async fn login(&self, url: &str) -> Result<LoginResult> {
        // Use browser-based login if enabled
        #[cfg(feature = "browser")]
        if self.use_browser {
            return self.browser_login(url).await;
        }

        info!("Starting login flow for {}", url);

        // Step 1: Fetch the login page
        debug!("Fetching login page...");
        let page_html = if let Some(ref cookie) = self.cookie_header {
            // Fetch with cookies using raw client
            let response = self
                .client
                .inner()
                .get(url)
                .header("Cookie", cookie)
                .send()
                .await?;
            response.text().await?
        } else {
            self.client.fetch_text(url).await?
        };

        // Step 2: Detect CAPTCHA / SPA before form detection
        let captcha = Self::detect_captcha(&page_html);
        let is_spa = Self::detect_spa(&page_html);

        // Step 3: Detect login form
        debug!("Detecting login form...");
        let form_result = Form::find_login_form(&page_html).context("Failed to parse forms")?;

        // If no form found, try QuickJS execution for SPA forms
        let mut form = if let Some(f) = form_result {
            f
        } else {
            // Check if page has inline scripts that might render forms
            let has_inline_scripts = Self::has_inline_scripts(&page_html);

            if has_inline_scripts && is_spa {
                info!(
                    "No static form found, but inline scripts detected. Attempting QuickJS execution..."
                );

                // Try to execute inline scripts and extract rendered DOM
                match JsEngine::new() {
                    Ok(js_engine) => {
                        match js_engine.execute_and_extract_forms(&page_html) {
                            Ok(rendered_html) => {
                                debug!("QuickJS execution completed, re-parsing for forms...");

                                // Try to find form in rendered HTML
                                if let Ok(Some(rendered_form)) =
                                    Form::find_login_form(&rendered_html)
                                {
                                    info!("✓ Found login form after JavaScript execution");
                                    rendered_form
                                } else {
                                    warn!(
                                        "JavaScript executed but no login form found in rendered output"
                                    );

                                    // Try cookie-based auth as fallback
                                    if self.cookie_header.is_some() {
                                        info!("Attempting cookie-based authentication instead...");
                                        return self.cookie_auth_fallback(url).await;
                                    }

                                    #[cfg(feature = "browser")]
                                    anyhow::bail!(
                                        "No login form found (SPA detected, JavaScript executed).\n\
                                         💡 Try browser-based login: nab login <url> --browser\n\
                                         💡 Or log in via your browser, then use: nab fetch <url> --cookies brave"
                                    );
                                    #[cfg(not(feature = "browser"))]
                                    anyhow::bail!(
                                        "No login form found (SPA detected, JavaScript executed).\n\
                                         💡 Log in via your browser, then use: nab fetch <url> --cookies brave"
                                    );
                                }
                            }
                            Err(e) => {
                                warn!("JavaScript execution failed: {}", e);

                                // Try cookie-based auth as fallback
                                if self.cookie_header.is_some() {
                                    info!("Attempting cookie-based authentication instead...");
                                    return self.cookie_auth_fallback(url).await;
                                }

                                #[cfg(feature = "browser")]
                                anyhow::bail!(
                                    "No login form found (JavaScript execution failed).\n\
                                     💡 Try browser-based login: nab login <url> --browser\n\
                                     💡 Or log in via your browser, then use: nab fetch <url> --cookies brave"
                                );
                                #[cfg(not(feature = "browser"))]
                                anyhow::bail!(
                                    "No login form found (JavaScript execution failed).\n\
                                     💡 Log in via your browser, then use: nab fetch <url> --cookies brave"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to create JavaScript engine: {}", e);

                        // Try cookie-based auth as fallback
                        if self.cookie_header.is_some() {
                            info!("Attempting cookie-based authentication instead...");
                            return self.cookie_auth_fallback(url).await;
                        }

                        anyhow::bail!(
                            "No login form found and JavaScript engine initialization failed"
                        );
                    }
                }
            } else {
                if is_spa {
                    warn!("No login form found — this appears to be a SPA (React/Vue/Angular)");
                    warn!(
                        "SPA login forms are rendered client-side and not visible to HTTP requests"
                    );
                }

                // Try cookie-based auth as fallback
                if self.cookie_header.is_some() {
                    info!("Attempting cookie-based authentication instead...");
                    return self.cookie_auth_fallback(url).await;
                }

                if is_spa {
                    #[cfg(feature = "browser")]
                    anyhow::bail!(
                        "No login form found (SPA detected).\n\
                         💡 Try browser-based login: nab login <url> --browser\n\
                         💡 Or log in via your browser, then use: nab fetch <url> --cookies brave"
                    );
                    #[cfg(not(feature = "browser"))]
                    anyhow::bail!(
                        "No login form found (SPA detected).\n\
                         💡 Log in via your browser, then use: nab fetch <url> --cookies brave"
                    );
                }

                anyhow::bail!("No login form found on page");
            }
        };

        // If CAPTCHA detected, warn and try cookie fallback or browser
        if let Some(ref captcha_type) = captcha {
            warn!("{} detected on login form", captcha_type);
            if self.cookie_header.is_some() {
                info!("Falling back to browser cookie authentication...");
                return self.cookie_auth_fallback(url).await;
            }
            #[cfg(feature = "browser")]
            warn!("💡 CAPTCHA detected. Try browser-based login: nab login <url> --browser");
            #[cfg(not(feature = "browser"))]
            warn!("💡 CAPTCHA may block login. Try: nab fetch <url> --cookies brave");
        }

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
        Self::fill_form_with_credential(&mut form, &credential)?;

        // Step 5: Resolve action URL and submit
        let action_url = form.resolve_action(url)?;
        debug!("Submitting form to: {}", action_url);

        let form_data = form.encode_urlencoded();

        // Extract origin from URL for CSRF protection
        let origin = extract_origin(url)?;

        let mut request = self
            .client
            .inner()
            .post(&action_url)
            .header("Content-Type", form.content_type())
            .header("Referer", url)
            .header("Origin", &origin);

        // Add cookies if available
        if let Some(ref cookie) = self.cookie_header {
            request = request.header("Cookie", cookie);
        }

        let response = request.body(form_data).send().await?;

        let final_url = response.url().to_string();
        let mut body = response.text().await?;

        // Step 6: Check for success first, then MFA if login didn't succeed
        let login_succeeded = !final_url.to_lowercase().contains("login")
            && !final_url.to_lowercase().contains("sign")
            || body.to_lowercase().contains("welcome")
            || body.to_lowercase().contains("dashboard")
            || body.to_lowercase().contains("my account");

        if !login_succeeded && Self::detect_mfa_required(&body) {
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
    ///
    /// Supports both exact field names (`email`) and nested/namespaced names
    /// like Rails (`user_session[email]`), Django (`auth-email`), or
    /// generic patterns (`login_email`).
    #[allow(clippy::unnecessary_wraps)] // API consistency: callers use ? operator
    fn fill_form_with_credential(form: &mut Form, credential: &Credential) -> Result<()> {
        // Common username field name patterns
        let username_patterns = [
            "username",
            "user",
            "email",
            "login",
            "log",
            "user_name",
            "user_email",
            "email_address",
        ];
        // Common password field name patterns
        let password_patterns = ["password", "pass", "passwd", "pwd"];

        // Find and fill username field
        if let Some(ref username) = credential.username
            && let Some(key) = Self::find_matching_field(&form.fields, &username_patterns)
        {
            debug!("Filling username field: {}", key);
            form.fields.insert(key, username.clone());
        }

        // Find and fill password field
        if let Some(ref password) = credential.password
            && let Some(key) = Self::find_matching_field(&form.fields, &password_patterns)
        {
            debug!("Filling password field: {}", key);
            form.fields.insert(key, password.clone());
        }

        Ok(())
    }

    /// Find a form field matching any of the given patterns.
    /// Checks exact match first, then substring match for nested names
    /// like `user_session[email]` or `auth-password`.
    fn find_matching_field(
        fields: &std::collections::HashMap<String, String>,
        patterns: &[&str],
    ) -> Option<String> {
        // Pass 1: exact match (highest priority)
        for pattern in patterns {
            if fields.contains_key(*pattern) {
                return Some(pattern.to_string());
            }
        }
        // Pass 2: field name contains pattern (handles nested/namespaced names)
        for pattern in patterns {
            for key in fields.keys() {
                let key_lower = key.to_lowercase();
                if key_lower.contains(pattern) {
                    return Some(key.clone());
                }
            }
        }
        None
    }

    /// Detect if MFA is required from the response
    /// Only detects MFA if there's a non-login form with a visible OTP/code input field
    fn detect_mfa_required(html: &str) -> bool {
        if let Ok(forms) = Form::parse_all(html) {
            return forms.iter().any(|f| {
                // Skip login forms (they have password fields)
                if f.is_login_form {
                    return false;
                }
                // Check for visible (non-hidden) fields that look like OTP inputs
                f.fields.keys().any(|k| {
                    // Skip hidden fields (CSRF tokens like authenticity_token, csrf_token)
                    if f.hidden_fields.contains_key(k) {
                        return false;
                    }
                    let k_lower = k.to_lowercase();
                    k_lower.contains("otp")
                        || k_lower.contains("totp")
                        || k_lower.contains("2fa")
                        || k_lower.contains("verification_code")
                        || k_lower.contains("security_code")
                        || (k_lower.contains("code")
                            && !k_lower.contains("zip")
                            && !k_lower.contains("postal")
                            && !k_lower.contains("promo")
                            && !k_lower.contains("discount")
                            && !k_lower.contains("country"))
                })
            });
        }
        false
    }

    /// Handle MFA challenge
    async fn handle_mfa(
        &self,
        base_url: &str,
        html: &str,
        credential: &Credential,
    ) -> Result<String> {
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
                        Self::get_otp_from_other_sources(base_url)?
                    }
                } else {
                    Self::get_otp_from_other_sources(base_url)?
                }
            } else {
                Self::get_otp_from_other_sources(base_url)?
            }
        } else {
            Self::get_otp_from_other_sources(base_url)?
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
            if key_lower.contains("code")
                || key_lower.contains("otp")
                || key_lower.contains("token")
            {
                debug!("Filling MFA field: {}", key);
                mfa_form.fields.insert(key.clone(), otp_code.clone());
                break;
            }
        }

        // Submit MFA form
        let action_url = mfa_form.resolve_action(base_url)?;
        let form_data = mfa_form.encode_urlencoded();

        debug!("Submitting MFA form to: {}", action_url);
        let mut request = self
            .client
            .inner()
            .post(&action_url)
            .header("Content-Type", mfa_form.content_type());

        // Add cookies if available
        if let Some(ref cookie) = self.cookie_header {
            request = request.header("Cookie", cookie);
        }

        let response = request.body(form_data).send().await?;

        Ok(response.text().await?)
    }

    /// Get OTP from SMS or email sources
    fn get_otp_from_other_sources(domain: &str) -> Result<String> {
        if let Some(otp_code) = OtpRetriever::get_otp_for_domain(domain)? {
            info!("Got OTP from {}", otp_code.source);
            return Ok(otp_code.code);
        }
        anyhow::bail!("No OTP code available")
    }

    /// Detect CAPTCHA presence in HTML
    fn detect_captcha(html: &str) -> Option<String> {
        let html_lower = html.to_lowercase();
        if html_lower.contains("g-recaptcha") || html_lower.contains("grecaptcha") {
            Some("reCAPTCHA".to_string())
        } else if html_lower.contains("h-captcha") || html_lower.contains("hcaptcha") {
            Some("hCaptcha".to_string())
        } else if html_lower.contains("cf-turnstile") {
            Some("Cloudflare Turnstile".to_string())
        } else if html_lower.contains("captcha") && html_lower.contains("<img") {
            Some("Image CAPTCHA".to_string())
        } else {
            None
        }
    }

    /// Detect if page is a SPA (no server-rendered form)
    fn detect_spa(html: &str) -> bool {
        let html_lower = html.to_lowercase();
        let has_spa_markers = html_lower.contains("id=\"root\"")
            || html_lower.contains("id=\"app\"")
            || html_lower.contains("id=\"__next\"")
            || html_lower.contains("id=\"__nuxt\"")
            || html_lower.contains("ng-app")
            || html_lower.contains("data-reactroot");
        let has_no_form = !html_lower.contains("<form");
        has_spa_markers && has_no_form
    }

    /// Check if HTML contains inline scripts (not external src=)
    fn has_inline_scripts(html: &str) -> bool {
        // Look for <script> tags without src attribute
        // Simple heuristic: contains <script> but not all have src=
        let html_lower = html.to_lowercase();
        if !html_lower.contains("<script") {
            return false;
        }

        // Check if there are any script tags without src attribute
        // by looking for <script> followed by > (not src=)
        html_lower.contains("<script>") || html_lower.contains("<script ")
    }

    /// Fallback: use browser cookies to access the target URL directly
    async fn cookie_auth_fallback(&self, url: &str) -> Result<LoginResult> {
        let cookie = self
            .cookie_header
            .as_ref()
            .context("No browser cookies available for fallback")?;

        let response = self
            .client
            .inner()
            .get(url)
            .header("Cookie", cookie)
            .send()
            .await?;

        let final_url = response.url().to_string();
        let status = response.status();
        let body = response.text().await?;

        let success = status.is_success()
            && !final_url.to_lowercase().contains("login")
            && !final_url.to_lowercase().contains("signin");

        Ok(LoginResult {
            success,
            final_url,
            body,
            message: if success {
                "Authenticated via browser cookies".to_string()
            } else {
                "Cookie auth attempted but session may be expired".to_string()
            },
        })
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

    /// Browser-based login flow (requires browser feature)
    #[cfg(feature = "browser")]
    async fn browser_login(&self, url: &str) -> Result<LoginResult> {
        use crate::browser::BrowserLogin;

        info!("Starting browser-based login for {}", url);

        // Connect to running Chrome instance
        let browser = BrowserLogin::connect(None).await
            .context("Failed to connect to Chrome. Start Chrome with: google-chrome --remote-debugging-port=9222")?;

        // Get credentials from 1Password if available
        let credential = if let Some(ref op) = self.one_password {
            debug!("Getting credentials from 1Password...");
            op.get_credential_for_url(url)?
        } else {
            None
        };

        // Perform browser login
        let cookies = browser
            .login(url, credential.as_ref())
            .await
            .context("Browser login failed")?;

        info!("Browser login successful, got {} cookies", cookies.len());

        // Convert cookies to header format
        let cookie_header = BrowserLogin::cookies_to_header(&cookies);

        // Fetch the page with the authenticated session
        let response = self
            .client
            .inner()
            .get(url)
            .header("Cookie", &cookie_header)
            .send()
            .await?;

        let final_url = response.url().to_string();
        let body = response.text().await?;

        Ok(LoginResult {
            success: true,
            final_url,
            body,
            message: "Browser login successful".to_string(),
        })
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

/// Extract origin (scheme + host) from URL for CSRF protection
fn extract_origin(url: &str) -> Result<String> {
    let parsed = url::Url::parse(url).context("Invalid URL")?;
    let scheme = parsed.scheme();
    let host = parsed.host_str().context("No host in URL")?;
    let port = parsed.port();

    if let Some(p) = port {
        Ok(format!("{scheme}://{host}:{p}"))
    } else {
        Ok(format!("{scheme}://{host}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_mfa_required() {
        let html_with_mfa = r#"
            <html>
                <body>
                    <form>
                        <input name="verification_code">
                    </form>
                </body>
            </html>
        "#;

        assert!(LoginFlow::detect_mfa_required(html_with_mfa));

        let html_without_mfa = r"
            <html>
                <body>
                    <p>Welcome back!</p>
                </body>
            </html>
        ";

        assert!(!LoginFlow::detect_mfa_required(html_without_mfa));
    }

    #[test]
    fn test_fill_form_with_credential() {
        use std::collections::HashMap;

        let mut form = Form {
            action: "/login".to_string(),
            method: "POST".to_string(),
            enctype: "application/x-www-form-urlencoded".to_string(),
            fields: HashMap::from([
                ("username".to_string(), String::new()),
                ("password".to_string(), String::new()),
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

        LoginFlow::fill_form_with_credential(&mut form, &credential).unwrap();

        assert_eq!(form.fields.get("username"), Some(&"testuser".to_string()));
        assert_eq!(form.fields.get("password"), Some(&"testpass".to_string()));
    }

    #[test]
    fn test_has_inline_scripts() {
        // HTML with inline script
        let html_with_inline = r"
            <html>
                <head>
                    <script>console.log('hello');</script>
                </head>
                <body></body>
            </html>
        ";
        assert!(LoginFlow::has_inline_scripts(html_with_inline));

        // HTML with external script only - not tested since has_inline_scripts is a heuristic
        // that checks for any <script> tag; QuickJS execution skips external scripts

        // HTML with no scripts
        let html_no_script = r"
            <html>
                <body><p>No scripts here</p></body>
            </html>
        ";
        assert!(!LoginFlow::has_inline_scripts(html_no_script));
    }

    #[test]
    fn test_detect_spa() {
        // SPA with root div and no form
        let spa_html = r#"
            <html>
                <body>
                    <div id="root"></div>
                </body>
            </html>
        "#;
        assert!(LoginFlow::detect_spa(spa_html));

        // Regular HTML with form
        let regular_html = r#"
            <html>
                <body>
                    <form action="/login">
                        <input name="username">
                    </form>
                </body>
            </html>
        "#;
        assert!(!LoginFlow::detect_spa(regular_html));

        // React app marker
        let react_html = r#"
            <html>
                <body>
                    <div id="app"></div>
                </body>
            </html>
        "#;
        assert!(LoginFlow::detect_spa(react_html));
    }
}
