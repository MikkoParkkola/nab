//! Browser automation via Chrome DevTools Protocol (CDP)
//!
//! Provides automated login for SPAs and CAPTCHA-protected sites
//! by connecting to a running Chrome/Chromium instance.

#![cfg(feature = "browser")]

use anyhow::{Context, Result};
use futures::StreamExt;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::auth::Credential;

/// Chrome DevTools Protocol client for browser automation
pub struct BrowserLogin {
    browser: chromiumoxide::Browser,
}

impl BrowserLogin {
    /// Connect to a running Chrome instance on the remote debugging port
    ///
    /// # Arguments
    /// * `port` - Remote debugging port (default: 9222)
    ///
    /// # Example
    /// ```no_run
    /// use nab::BrowserLogin;
    ///
    /// #[tokio::main]
    /// async fn main() -> anyhow::Result<()> {
    ///     let browser = BrowserLogin::connect(Some(9222)).await?;
    ///     Ok(())
    /// }
    /// ```
    pub async fn connect(port: Option<u16>) -> Result<Self> {
        let port = port.unwrap_or(9222);

        debug!("Connecting to Chrome on port {}", port);

        let (browser, mut handler) = chromiumoxide::Browser::connect(
            format!("http://localhost:{}", port)
        )
        .await
        .context("Failed to connect to Chrome. Make sure Chrome is running with --remote-debugging-port=9222")?;

        // Spawn handler to process CDP events
        tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if let Err(e) = event {
                    warn!("CDP handler error: {}", e);
                }
            }
        });

        info!("Connected to Chrome on port {}", port);
        Ok(Self { browser })
    }

    /// Automated login flow with browser interaction
    ///
    /// 1. Navigate to URL
    /// 2. Wait for page load
    /// 3. Fill form fields (if credential provided)
    /// 4. Pause for CAPTCHA/manual intervention if needed
    /// 5. Submit form
    /// 6. Extract cookies from successful session
    ///
    /// # Arguments
    /// * `url` - Login page URL
    /// * `credential` - Optional credentials (username/password)
    ///
    /// # Returns
    /// Cookies from the authenticated session
    pub async fn login(&self, url: &str, credential: Option<&Credential>) -> Result<Vec<Cookie>> {
        info!("Starting browser login for {}", url);

        // Create new page
        let page = self.browser.new_page(url).await
            .context("Failed to create new browser page")?;

        // Wait for page load
        page.wait_for_navigation().await
            .context("Failed to navigate to login page")?;

        debug!("Page loaded: {}", url);

        // If credentials provided, try to fill the form
        if let Some(cred) = credential {
            self.fill_login_form(&page, cred).await?;
        }

        // Check for CAPTCHA indicators
        let has_captcha = self.detect_captcha(&page).await?;

        if has_captcha {
            warn!("⚠️  CAPTCHA detected - please solve it in the browser window");
            warn!("   Waiting 60 seconds for manual intervention...");

            // Give user time to solve CAPTCHA
            tokio::time::sleep(Duration::from_secs(60)).await;
        }

        // Extract cookies after successful login
        let cookies = self.extract_cookies(&page).await?;

        info!("Browser login complete, extracted {} cookies", cookies.len());
        Ok(cookies)
    }

    /// Fill login form fields with credentials
    async fn fill_login_form(
        &self,
        page: &chromiumoxide::Page,
        credential: &Credential,
    ) -> Result<()> {
        debug!("Attempting to fill login form");

        // Common username field selectors
        let username_selectors = [
            "input[name='username']",
            "input[name='email']",
            "input[name='user']",
            "input[type='email']",
            "input[id='username']",
            "input[id='email']",
        ];

        // Common password field selectors
        let password_selectors = [
            "input[name='password']",
            "input[type='password']",
            "input[id='password']",
        ];

        // Fill username
        if let Some(ref username) = credential.username {
            for selector in username_selectors {
                if let Ok(element) = page.find_element(selector).await {
                    debug!("Found username field: {}", selector);
                    element.click().await?;
                    element.type_str(username).await
                        .context("Failed to type username")?;
                    break;
                }
            }
        }

        // Fill password
        if let Some(ref password) = credential.password {
            for selector in password_selectors {
                if let Ok(element) = page.find_element(selector).await {
                    debug!("Found password field: {}", selector);
                    element.click().await?;
                    element.type_str(password).await
                        .context("Failed to type password")?;
                    break;
                }
            }
        }

        // Try to find and click submit button
        let submit_selectors = [
            "button[type='submit']",
            "input[type='submit']",
            "button:has-text('Sign in')",
            "button:has-text('Log in')",
            "button:has-text('Login')",
        ];

        for selector in submit_selectors {
            if let Ok(element) = page.find_element(selector).await {
                debug!("Found submit button: {}", selector);
                element.click().await?;

                // Wait for navigation after submit
                tokio::time::sleep(Duration::from_secs(2)).await;
                break;
            }
        }

        Ok(())
    }

    /// Detect CAPTCHA presence on the page
    async fn detect_captcha(&self, page: &chromiumoxide::Page) -> Result<bool> {
        let captcha_selectors = [
            ".g-recaptcha",
            ".h-captcha",
            ".cf-turnstile",
            "iframe[src*='recaptcha']",
            "iframe[src*='hcaptcha']",
        ];

        for selector in captcha_selectors {
            if page.find_element(selector).await.is_ok() {
                debug!("CAPTCHA detected: {}", selector);
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Extract cookies from browser session
    ///
    /// Returns all cookies for the current page's domain
    pub async fn extract_cookies(&self, page: &chromiumoxide::Page) -> Result<Vec<Cookie>> {
        let cdp_cookies = page.get_cookies().await
            .context("Failed to get cookies from browser")?;

        let cookies = cdp_cookies
            .into_iter()
            .map(|c| Cookie {
                name: c.name,
                value: c.value,
                domain: c.domain,
                path: c.path,
                secure: c.secure,
                http_only: c.http_only,
            })
            .collect();

        Ok(cookies)
    }

    /// Format cookies as HTTP Cookie header value
    pub fn cookies_to_header(cookies: &[Cookie]) -> String {
        cookies
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Browser cookie
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cookies_to_header() {
        let cookies = vec![
            Cookie {
                name: "session".to_string(),
                value: "abc123".to_string(),
                domain: "example.com".to_string(),
                path: "/".to_string(),
                secure: true,
                http_only: true,
            },
            Cookie {
                name: "token".to_string(),
                value: "xyz789".to_string(),
                domain: "example.com".to_string(),
                path: "/".to_string(),
                secure: true,
                http_only: false,
            },
        ];

        let header = BrowserLogin::cookies_to_header(&cookies);
        assert_eq!(header, "session=abc123; token=xyz789");
    }

    #[test]
    fn test_empty_cookies() {
        let cookies = vec![];
        let header = BrowserLogin::cookies_to_header(&cookies);
        assert_eq!(header, "");
    }

    #[test]
    fn test_single_cookie() {
        let cookies = vec![Cookie {
            name: "auth".to_string(),
            value: "token123".to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: true,
            http_only: true,
        }];

        let header = BrowserLogin::cookies_to_header(&cookies);
        assert_eq!(header, "auth=token123");
    }

    #[test]
    fn test_cookie_equality() {
        let c1 = Cookie {
            name: "test".to_string(),
            value: "value".to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: true,
            http_only: true,
        };

        let c2 = c1.clone();
        assert_eq!(c1, c2);
    }

    #[test]
    fn test_cookie_debug() {
        let cookie = Cookie {
            name: "test".to_string(),
            value: "value".to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: true,
            http_only: true,
        };

        let debug_str = format!("{:?}", cookie);
        assert!(debug_str.contains("test"));
        assert!(debug_str.contains("value"));
    }

    #[test]
    fn test_cookie_clone() {
        let c1 = Cookie {
            name: "session".to_string(),
            value: "abc".to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
        };

        let c2 = c1.clone();
        assert_eq!(c1.name, c2.name);
        assert_eq!(c1.value, c2.value);
        assert_eq!(c1.domain, c2.domain);
    }
}
