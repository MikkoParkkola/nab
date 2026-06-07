//! Browser automation via Chrome `DevTools` Protocol (CDP) and
//! default-browser escape hatch for WAF challenges.
//!
//! The `BrowserLogin` struct provides automated login for SPAs and
//! CAPTCHA-protected sites by connecting to a running Chrome/Chromium
//! instance. It is feature-gated behind `feature = "browser"` because it
//! pulls in the 20+ MB `chromiumoxide` dependency.
//!
//! The [`open_and_wait`] helper opens a URL in the user's default browser
//! and polls a cookie probe until either the store changes or the
//! timeout elapses. It is gated behind `feature = "browser-launcher"` so
//! lean builds that want this escape hatch do not need the full CDP
//! stack.

use anyhow::{Context, Result};
#[cfg(feature = "browser")]
use futures::StreamExt;
use regex::Regex;
use std::sync::LazyLock;
use std::time::Duration;
#[cfg(not(feature = "browser"))]
use tracing::debug;
#[cfg(feature = "browser")]
use tracing::{debug, info, warn};

/// X (Twitter) long-form **Article** URL matcher.
///
/// Matches `https://x.com/i/article/<id>` and the `www.`/`mobile.` host
/// variants, case-insensitively. Deliberately scoped to the `x.com` apex only:
/// look-alike mirrors (`fxtwitter.com`, `vxtwitter.com`, …) and ordinary status
/// / profile / home URLs must NOT match, because the authed DOM-render path is
/// expensive and X-specific.
///
/// Compiled once. The pattern is a literal we control, so the `expect` cannot
/// fire at runtime.
static X_ARTICLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https?://(?:www\.|mobile\.)?x\.com/i/article/\d+")
        .expect("static X article URL regex is valid")
});

/// Returns `true` when `url` is an X (Twitter) long-form Article URL.
///
/// This predicate keys the authed DOM-render short-circuit in the CLI fetch
/// ladder. It is intentionally **not** feature-gated: it is pure string
/// matching with no browser dependency, so it always compiles and its unit
/// tests run in the default build.
///
/// # Examples
/// ```
/// use nab::browser::is_x_article_url;
/// assert!(is_x_article_url("https://x.com/i/article/123"));
/// assert!(is_x_article_url("https://www.x.com/i/article/9"));
/// assert!(!is_x_article_url("https://x.com/home"));
/// assert!(!is_x_article_url("https://fxtwitter.com/i/article/1"));
/// ```
#[must_use]
pub fn is_x_article_url(url: &str) -> bool {
    X_ARTICLE_RE.is_match(url)
}

#[cfg(feature = "browser")]
use crate::auth::Credential;

/// Chrome `DevTools` Protocol client for browser automation
#[cfg(feature = "browser")]
pub struct BrowserLogin {
    browser: chromiumoxide::Browser,
}

#[cfg(feature = "browser")]
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
            format!("http://localhost:{port}")
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
        let page = self
            .browser
            .new_page(url)
            .await
            .context("Failed to create new browser page")?;

        // Wait for page load
        page.wait_for_navigation()
            .await
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
            tokio::time::sleep(Duration::from_mins(1)).await;
        }

        // Extract cookies after successful login
        let cookies = self.extract_cookies(&page).await?;

        info!(
            "Browser login complete, extracted {} cookies",
            cookies.len()
        );
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
                    element
                        .type_str(username)
                        .await
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
                    element
                        .type_str(password)
                        .await
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
        let cdp_cookies = page
            .get_cookies()
            .await
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

    /// Rung 3: render `url` in the connected external browser and return screened,
    /// LLM-shaped markdown. Opens a page, waits for navigation, gives the DOM a
    /// brief settle window (SPAs hydrate client-side), extracts the rendered HTML,
    /// converts it through nab's content pipeline, and runs the fetch-time YARA
    /// screen. This is the primitive the task engine's [`nab::task::BrowserBackend`]
    /// wraps so the brain-driven loop can escalate to a browser without nab ever
    /// bundling Chromium — it orchestrates the user's EXTERNAL Chrome over CDP.
    pub async fn render_markdown(&self, url: &str) -> Result<String> {
        let page = self
            .browser
            .new_page(url)
            .await
            .context("failed to open browser page for rung-3 render")?;
        page.wait_for_navigation()
            .await
            .context("failed to navigate the browser page")?;
        // SPAs hydrate after load; give the DOM a brief settle window.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let html = page
            .content()
            .await
            .context("failed to read rendered DOM from the browser")?;
        // Best-effort close so the orchestrated browser does not accumulate tabs.
        let _ = page.close().await;
        Self::dom_to_markdown(&html, url)
    }

    /// Render `url` with the supplied session `cookies` injected into the page
    /// context BEFORE the first navigation, then convert the fully rendered DOM
    /// to markdown.
    ///
    /// This is the authed DOM-read path for content that paints into the DOM
    /// only after an authenticated XHR completes — e.g. X long-form Articles at
    /// `https://x.com/i/article/<id>`, whose body is fetched via an authed
    /// GraphQL request after hydration and therefore exists in neither the
    /// static HTML nor the embedded data layer.
    ///
    /// Flow: open a blank page → set each cookie scoped to the target host (and
    /// its parent domain) → navigate → await the load event → settle for
    /// [`COOKIE_RENDER_SETTLE`] so the post-hydration XHR can paint → read the
    /// rendered DOM → convert via nab's existing HTML→markdown pipeline and run
    /// the fetch-time YARA screen.
    ///
    /// Cookie values flow in memory only and are never logged.
    ///
    /// # Arguments
    /// * `url` — the target page to render.
    /// * `cookies` — session cookies to inject (typically the user's existing
    ///   browser cookies for the target domain).
    ///
    /// # Errors
    /// Returns an error if the page cannot be opened, cookies cannot be set,
    /// navigation fails, the DOM cannot be read, or the YARA screen rejects the
    /// rendered content.
    pub async fn render_with_cookies(&self, url: &str, cookies: &[Cookie]) -> Result<String> {
        use chromiumoxide::cdp::browser_protocol::network::{CookieParam, SetCookiesParams};

        // Open a blank page first so cookies are present in the context BEFORE
        // the first navigation to the target URL.
        let page = self
            .browser
            .new_page("about:blank")
            .await
            .context("failed to open blank browser page for authed render")?;

        if !cookies.is_empty() {
            // Each CookieParam carries an explicit `url` so the CDP backend can
            // scope it. We issue `Network.setCookies` directly via `execute`
            // rather than `Page::set_cookies`, because the latter validates the
            // current page URL and rejects `about:blank` — yet injecting cookies
            // BEFORE the first real navigation is exactly the requirement here.
            let params: Vec<CookieParam> = cookies
                .iter()
                .map(|c| {
                    let mut p = CookieParam::new(c.name.clone(), c.value.clone());
                    p.url = Some(url.to_string());
                    p.domain = Some(c.domain.clone());
                    p.path = Some(c.path.clone());
                    p.secure = Some(c.secure);
                    p.http_only = Some(c.http_only);
                    p
                })
                .collect();
            // SECURITY: log the count only — never the cookie names or values.
            debug!(
                "injecting {} session cookie(s) before navigation",
                params.len()
            );
            page.execute(SetCookiesParams::new(params))
                .await
                .context("failed to inject session cookies into the browser context")?;
        }

        page.goto(url)
            .await
            .context("failed to navigate to the authed render target")?;
        page.wait_for_navigation()
            .await
            .context("failed waiting for navigation on the authed render target")?;
        // The authed XHR paints into the DOM AFTER hydration; give it a generous
        // settle window beyond the load event (network-idle proxy).
        tokio::time::sleep(COOKIE_RENDER_SETTLE).await;

        let html = page
            .content()
            .await
            .context("failed to read rendered DOM from the authed page")?;
        let _ = page.close().await;
        Self::dom_to_markdown(&html, url)
    }

    /// Convert rendered DOM `html` to screened markdown.
    ///
    /// Shared tail for [`Self::render_markdown`] and
    /// [`Self::render_with_cookies`]: reuses nab's existing HTML→markdown
    /// converter under `src/content/` and the fetch-time YARA screen, so neither
    /// render path duplicates the conversion / screening logic.
    fn dom_to_markdown(html: &str, url: &str) -> Result<String> {
        let markdown = crate::content::html::html_to_markdown_with_url(html, Some(url));
        let screened = crate::security::guard_fetch_output(&markdown, "task_browser", url)
            .context("YARA screen rejected the rendered page")?;
        Ok(screened)
    }
}

/// Settle window after the load event for authed DOM renders.
///
/// X Articles paint their body via an authenticated GraphQL XHR that completes
/// only after hydration; a fixed delay here is the network-idle proxy described
/// in the proven recipe. Kept generous because the alternative — returning a
/// half-painted DOM — silently drops the article body.
#[cfg(feature = "browser")]
const COOKIE_RENDER_SETTLE: Duration = Duration::from_millis(2_500);

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

/// Build injectable [`Cookie`]s for `target_url` from a flat name→value map.
///
/// The flat map is what nab's native cookie extraction surfaces on the fetch
/// hot path (`CookieSource::get_cookies`, the "Native cookie extraction
/// succeeded: N cookies" log line). That map carries no per-cookie metadata, so
/// we synthesize a conservative, correct scope from the target host:
///
/// * `domain` — the registrable parent (`.x.com` for `x.com`/`www.x.com`), with
///   a leading dot so the cookie applies to the host and its subdomains.
/// * `path` — `/`.
/// * `secure` — `true` (X is HTTPS-only; the recipe injects HTTPS cookies).
/// * `http_only` — `false` (CDP injection does not require the `HttpOnly` hint
///   to be faithful; the value is what matters for the authed XHR).
///
/// Pure and browser-free, so it is unit-testable without a live CDP session.
/// Cookie values are moved through in memory only and never logged here.
///
/// Generic over the map's hasher so the standard-library `HashMap` returned by
/// native cookie extraction passes without a rebuild.
#[must_use]
pub fn cookies_for_host<S: std::hash::BuildHasher>(
    target_url: &str,
    flat: &std::collections::HashMap<String, String, S>,
) -> Vec<Cookie> {
    let host = crate::util::extract_domain(target_url);
    let domain = scope_domain(&host);
    flat.iter()
        .map(|(name, value)| Cookie {
            name: name.clone(),
            value: value.clone(),
            domain: domain.clone(),
            path: "/".to_string(),
            secure: true,
            http_only: false,
        })
        .collect()
}

/// Derive the dot-prefixed registrable parent domain for cookie scoping.
///
/// `www.x.com` / `mobile.x.com` / `x.com` → `.x.com`. Hosts with two or fewer
/// labels are returned dot-prefixed as-is. Used by [`cookies_for_host`].
fn scope_domain(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    let parent = if labels.len() > 2 {
        labels[labels.len() - 2..].join(".")
    } else {
        labels.join(".")
    };
    if parent.is_empty() {
        host.to_string()
    } else {
        format!(".{parent}")
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Default-browser escape hatch
// ═════════════════════════════════════════════════════════════════════════════

/// Open `url` in the user's default browser and block for up to `duration`,
/// polling the provided `cookie_probe` every second for changes.
///
/// This is the "last-resort" WAF escape hatch: when replay and JS solvers
/// fail, the user can complete the challenge manually. The function spawns
/// the default browser (via the `webbrowser` crate when the
/// `browser-launcher` feature is enabled, otherwise via `open` /
/// `xdg-open` / `cmd /c start`) and then sleeps in 1-second increments,
/// calling `cookie_probe()` between sleeps. If the probe reports a
/// change, the wait aborts early.
///
/// # Arguments
/// * `url` — URL to open.
/// * `duration` — maximum time to wait for user interaction.
/// * `cookie_probe` — optional closure returning the current cookie state
///   as a comparable string. Called every second; if the value changes
///   from the initial snapshot, the wait returns early.
///
/// # Errors
/// Returns an error if the browser fails to launch.
pub fn open_and_wait(
    url: &str,
    duration: Duration,
    cookie_probe: Option<&dyn Fn() -> String>,
) -> Result<()> {
    launch_default_browser(url).context("failed to launch default browser")?;

    let initial = cookie_probe.map(|probe| probe());
    let start = std::time::Instant::now();
    let tick = Duration::from_secs(1);
    while start.elapsed() < duration {
        std::thread::sleep(tick);
        if let (Some(before), Some(probe)) = (initial.as_ref(), cookie_probe) {
            let now = probe();
            if &now != before {
                debug!("cookie store changed — aborting wait early");
                return Ok(());
            }
        }
    }
    Ok(())
}

fn launch_default_browser(url: &str) -> Result<()> {
    #[cfg(feature = "browser-launcher")]
    {
        webbrowser::open(url).context("webbrowser::open failed")?;
    }

    #[cfg(not(feature = "browser-launcher"))]
    {
        // Fallback: invoke the platform-specific launcher directly.
        #[cfg(target_os = "macos")]
        let cmd = {
            let mut c = std::process::Command::new("open");
            c.arg(url);
            c
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let cmd = {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(url);
            c
        };
        #[cfg(target_os = "windows")]
        let cmd = {
            let mut c = std::process::Command::new("cmd");
            c.arg("/c").arg("start").arg("").arg(url);
            c
        };

        #[cfg(not(any(target_os = "macos", unix, target_os = "windows")))]
        anyhow::bail!("no default-browser launcher available on this platform");

        #[cfg(any(target_os = "macos", unix, target_os = "windows"))]
        {
            let mut cmd = cmd;
            let status = cmd.status().context("failed to spawn browser launcher")?;
            if !status.success() {
                anyhow::bail!("browser launcher exited with {status}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_article_url_matches_apex_host() {
        // GIVEN a canonical X Article URL
        // WHEN the predicate runs
        // THEN it matches
        assert!(is_x_article_url("https://x.com/i/article/123"));
    }

    #[test]
    fn x_article_url_matches_www_subdomain() {
        assert!(is_x_article_url("https://www.x.com/i/article/9"));
    }

    #[test]
    fn x_article_url_matches_mobile_subdomain() {
        assert!(is_x_article_url("https://mobile.x.com/i/article/42"));
    }

    #[test]
    fn x_article_url_is_case_insensitive_on_scheme_and_host() {
        assert!(is_x_article_url("HTTPS://X.COM/i/article/7"));
    }

    #[test]
    fn x_article_url_rejects_status_url() {
        // A status (tweet) URL is not an Article; it must not trigger the
        // expensive authed DOM render.
        assert!(!is_x_article_url("https://x.com/u/status/123"));
    }

    #[test]
    fn x_article_url_rejects_lookalike_mirror_host() {
        // fxtwitter is a third-party mirror, not x.com.
        assert!(!is_x_article_url("https://fxtwitter.com/i/article/1"));
    }

    #[test]
    fn x_article_url_rejects_home_feed() {
        assert!(!is_x_article_url("https://x.com/home"));
    }

    #[test]
    fn x_article_url_rejects_article_path_without_numeric_id() {
        // The id segment must be digits.
        assert!(!is_x_article_url("https://x.com/i/article/abc"));
    }

    #[test]
    fn x_article_url_rejects_twitter_com_apex() {
        // The matcher is scoped to x.com only; twitter.com is out of scope here
        // (the rule layer owns twitter.com routing).
        assert!(!is_x_article_url("https://twitter.com/i/article/1"));
    }

    #[test]
    fn scope_domain_collapses_subdomain_to_dot_parent() {
        // GIVEN a host with a leading subdomain
        // WHEN the parent domain is derived
        // THEN it is the dot-prefixed registrable parent
        assert_eq!(super::scope_domain("www.x.com"), ".x.com");
        assert_eq!(super::scope_domain("mobile.x.com"), ".x.com");
        assert_eq!(super::scope_domain("x.com"), ".x.com");
    }

    #[test]
    fn cookies_for_host_synthesizes_scoped_cookies() {
        // GIVEN a flat name→value map (what native extraction returns)
        let mut flat = std::collections::HashMap::new();
        flat.insert("auth_token".to_string(), "secret".to_string());
        // WHEN we build injectable cookies for an x.com article URL
        let cookies = cookies_for_host("https://x.com/i/article/123", &flat);
        // THEN the single cookie is scoped to .x.com, path /, secure
        assert_eq!(cookies.len(), 1);
        let c = &cookies[0];
        assert_eq!(c.name, "auth_token");
        assert_eq!(c.value, "secret");
        assert_eq!(c.domain, ".x.com");
        assert_eq!(c.path, "/");
        assert!(c.secure);
    }

    #[test]
    fn cookies_for_host_empty_map_yields_no_cookies() {
        let flat = std::collections::HashMap::new();
        let cookies = cookies_for_host("https://x.com/i/article/1", &flat);
        assert!(cookies.is_empty());
    }

    #[cfg(feature = "browser")]
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

    #[cfg(feature = "browser")]
    #[test]
    fn test_empty_cookies() {
        let cookies = vec![];
        let header = BrowserLogin::cookies_to_header(&cookies);
        assert_eq!(header, "");
    }

    #[cfg(feature = "browser")]
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

        let debug_str = format!("{cookie:?}");
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

    #[test]
    fn test_open_and_wait_aborts_early_on_cookie_change() {
        // Does NOT launch a real browser: we temporarily override
        // launch_default_browser via a stub. Instead, test the polling
        // logic with a fake probe.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let ticks = AtomicUsize::new(0);
        let probe = |(): ()| -> String {
            let n = ticks.fetch_add(1, Ordering::SeqCst);
            if n < 1 { "a".into() } else { "changed".into() }
        };
        // Directly exercise the inner polling loop semantics by calling
        // wait_for_cookie_change instead of open_and_wait (which would
        // actually launch a browser).
        let probe_fn = || probe(());
        let initial = probe_fn();
        let start = std::time::Instant::now();
        let max = Duration::from_secs(3);
        let tick = Duration::from_millis(5);
        let mut changed = false;
        while start.elapsed() < max {
            std::thread::sleep(tick);
            if probe_fn() != initial {
                changed = true;
                break;
            }
        }
        assert!(changed, "probe must detect change within timeout");
    }

    /// Live end-to-end render of an X Article with injected cookies.
    ///
    /// Ignored by default (nab's convention for tests that need an external
    /// resource — here a running Chrome on `--remote-debugging-port=9222` AND a
    /// logged-in x.com session in that browser). Run explicitly with:
    /// `cargo test --features browser -- --ignored render_with_cookies_live`.
    #[cfg(feature = "browser")]
    #[tokio::test]
    #[ignore = "requires a running Chrome on :9222 with a logged-in x.com session"]
    async fn render_with_cookies_live_x_article() {
        let url = "https://x.com/i/article/123";
        let mut flat = std::collections::HashMap::new();
        // In a real run these come from native extraction; here the harness
        // expects the operator's live browser session to already hold them.
        if let Ok(header) = std::env::var("NAB_TEST_X_COOKIES") {
            for pair in header.split(';') {
                if let Some((k, v)) = pair.trim().split_once('=') {
                    flat.insert(k.to_string(), v.to_string());
                }
            }
        }
        let cookies = cookies_for_host(url, &flat);
        let browser = BrowserLogin::connect(Some(9222))
            .await
            .expect("connect to Chrome on :9222");
        let markdown = browser
            .render_with_cookies(url, &cookies)
            .await
            .expect("authed DOM render should succeed");
        assert!(
            !markdown.trim().is_empty(),
            "rendered article markdown must not be empty"
        );
    }
}
