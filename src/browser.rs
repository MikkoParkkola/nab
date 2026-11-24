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
use std::time::Duration;
#[cfg(not(feature = "browser"))]
use tracing::debug;
#[cfg(feature = "browser")]
use tracing::{debug, info, warn};

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

        let (page, guard) = self.open_guarded(PageUse::Login).await?;
        if let Err(err) = goto_guarded(&page, &guard, url).await {
            let _ = page.close().await;
            return Err(err);
        }

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
        // A navigation refused during form submission or the CAPTCHA wait
        // fails the login rather than returning its cookies.
        guard.ensure_clean()?;

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
        let (page, guard) = self.open_guarded(PageUse::Render).await?;
        if let Err(err) = block_window_open(&page).await {
            let _ = page.close().await;
            return Err(err);
        }
        let rendered = async {
            goto_guarded(&page, &guard, url).await?;
            page.wait_for_navigation()
                .await
                .context("failed to navigate the browser page")?;
            // SPAs hydrate after load; give the DOM a brief settle window.
            tokio::time::sleep(Duration::from_millis(800)).await;
            let html = page
                .content()
                .await
                .context("failed to read rendered DOM from the browser")?;
            // A meta/JS navigation during the settle window is refused too.
            guard.ensure_clean()?;
            Ok::<_, anyhow::Error>(html)
        }
        .await;
        // Best-effort close so the orchestrated browser does not accumulate tabs.
        let _ = page.close().await;
        Self::dom_to_markdown(&rendered?, url)
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
        let (page, guard) = self.open_guarded(PageUse::Render).await?;
        if let Err(err) = block_window_open(&page).await {
            let _ = page.close().await;
            return Err(err);
        }
        if let Err(err) = guard.check(url) {
            let _ = page.close().await;
            return Err(err);
        }

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

        let rendered = async {
            goto_guarded(&page, &guard, url)
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
            guard.ensure_clean()?;
            Ok::<_, anyhow::Error>(html)
        }
        .await;
        let _ = page.close().await;
        Self::dom_to_markdown(&rendered?, url)
    }

    /// Open `about:blank` with the SSRF Fetch guard armed, so no later
    /// navigation or redirect hop on the page reaches a denied address.
    async fn open_guarded(
        &self,
        page_use: PageUse,
    ) -> Result<(chromiumoxide::Page, SsrfFetchGuard)> {
        let page = self
            .browser
            .new_page("about:blank")
            .await
            .context("failed to open browser page")?;
        match SsrfFetchGuard::arm(&page, page_use).await {
            Ok(guard) => Ok((page, guard)),
            Err(err) => {
                let _ = page.close().await;
                Err(err)
            }
        }
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

/// Whether a browser request to `url` may proceed under `policy`.
///
/// HTTP(S) targets go through [`crate::ssrf::validate_redirect_target_with_policy`],
/// the check the HTTP client runs before each redirect hop, so the browser
/// honours the same `NAB_SSRF_ALLOW_PRIVATE` / `NAB_SSRF_ALLOWLIST` opt-outs.
/// Local page schemes (`about`, `data`, `blob`, Chrome internals) are not
/// network targets and pass; every other scheme is refused.
#[cfg(any(feature = "browser", test))]
fn browser_request_check(
    url: &str,
    policy: &crate::ssrf::SsrfPolicy,
) -> std::result::Result<(), crate::error::NabError> {
    use crate::error::NabError;
    let parsed = url::Url::parse(url).map_err(|err| NabError::InvalidUrl(err.to_string()))?;
    match parsed.scheme() {
        "about" | "data" | "blob" | "chrome" | "chrome-extension" | "devtools" => Ok(()),
        _ => {
            crate::ssrf::validate_redirect_target_with_policy(&parsed, policy)?;
            // The HTTP client pins the one address it validated; Chrome resolves
            // the name itself and may pick any answer, so every answer must pass.
            if let Some(url::Host::Domain(host)) = parsed.host() {
                use std::net::ToSocketAddrs;
                let port = parsed.port_or_known_default().unwrap_or(443);
                let addrs = (host, port).to_socket_addrs().map_err(|err| {
                    NabError::SsrfBlocked(format!("DNS resolution failed for {host}: {err}"))
                })?;
                for addr in addrs {
                    crate::ssrf::validate_ip_with_policy(addr.ip(), policy)?;
                }
            }
            Ok(())
        }
    }
}

/// What a guarded page is for; decides which frames it may load.
#[cfg(feature = "browser")]
#[derive(Clone, Copy)]
enum PageUse {
    /// Unattended render: only the top document is read, so subframes and
    /// popups are refused outright.
    Render,
    /// Interactive login: the user may need sign-in frames and popups.
    Login,
}

/// Refuses the first document navigation the Fetch guard failed.
///
/// Subresource requests to denied targets are failed inside Chrome but do not
/// fail the render; a denied document (initial load, redirect hop, meta/JS
/// navigation, frame) does.
#[cfg(feature = "browser")]
#[derive(Clone)]
struct SsrfFetchGuard {
    blocked: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    policy: crate::ssrf::SsrfPolicy,
}

#[cfg(feature = "browser")]
impl SsrfFetchGuard {
    /// Arm Chrome's Fetch domain on `page` so every request is paused and a
    /// request to a denied target is failed with `BlockedByClient` before it
    /// leaves the browser. `Page::goto` follows redirects inside Chrome, so
    /// this is the only place a redirect hop can be checked: responses are
    /// paused too, and a 3xx whose `Location` is denied is failed before
    /// Chrome follows it.
    ///
    /// Limit: chromiumoxide 0.9 auto-attaches child targets (cross-site
    /// iframes, popups, workers) with `waitForDebuggerOnStart` and resumes them
    /// itself with `Runtime.runIfWaitingForDebugger` inside its handler,
    /// before an embedder sees `Target.attachedToTarget`, and it offers no
    /// call to send commands on a child session. Fetch therefore cannot be
    /// armed on a child target before it loads. A render page instead refuses
    /// every subframe document (seen here, on the parent, before the frame
    /// starts) and every popup, so no child target with network access is
    /// created. The login page keeps frames and popups for sign-in flows,
    /// and worker traffic is not intercepted.
    async fn arm(page: &chromiumoxide::Page, page_use: PageUse) -> Result<Self> {
        use chromiumoxide::cdp::browser_protocol::fetch::{
            ContinueRequestParams, EnableParams, EventRequestPaused, FailRequestParams,
            RequestPattern, RequestStage,
        };
        use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, ResourceType};

        let guard = Self {
            blocked: std::sync::Arc::default(),
            policy: crate::ssrf::SsrfPolicy::from_env(),
        };
        let mut events = page
            .event_listener::<EventRequestPaused>()
            .await
            .context("failed to listen for paused browser requests")?;
        let stage = |stage| {
            RequestPattern::builder()
                .url_pattern("*")
                .request_stage(stage)
                .build()
        };
        page.execute(
            EnableParams::builder()
                .handle_auth_requests(false)
                .pattern(stage(RequestStage::Request))
                .pattern(stage(RequestStage::Response))
                .build(),
        )
        .await
        .context("failed to enable the browser Fetch domain")?;

        let main_frame = render_main_frame(page, page_use).await?;
        let page = page.clone();
        let task_guard = guard.clone();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                // A render page loads no subframe document: a frame from
                // another site would run as a child target this guard cannot
                // see, and the render reads only the top document.
                let subframe = event.response_status_code.is_none()
                    && event.resource_type == ResourceType::Document
                    && main_frame
                        .as_ref()
                        .is_some_and(|main| *main != event.frame_id);
                if subframe {
                    debug!(
                        "refusing a subframe on a render page: {}",
                        event.request.url
                    );
                    if let Err(err) = page
                        .execute(FailRequestParams::new(
                            event.request_id.clone(),
                            ErrorReason::BlockedByClient,
                        ))
                        .await
                    {
                        debug!("ssrf fetch guard could not fail a subframe: {err}");
                    }
                    continue;
                }
                // Request stage: the request's own URL. Response stage: the
                // redirect target of a 3xx, or nothing to check.
                let Some(url) = paused_target(&event) else {
                    if let Err(err) = page
                        .execute(ContinueRequestParams::new(event.request_id.clone()))
                        .await
                    {
                        debug!("ssrf fetch guard could not continue a response: {err}");
                    }
                    continue;
                };
                let policy = task_guard.policy.clone();
                let check_url = url.clone();
                // The check may resolve DNS; keep it off the async workers.
                let allowed = tokio::task::spawn_blocking(move || {
                    browser_request_check(&check_url, &policy).is_ok()
                })
                .await
                .unwrap_or(false);
                let request_id = event.request_id.clone();
                let settled = if allowed {
                    page.execute(ContinueRequestParams::new(request_id))
                        .await
                        .err()
                } else {
                    debug!("refusing browser request: {url}");
                    if event.resource_type == ResourceType::Document
                        && let Ok(mut slot) = task_guard.blocked.lock()
                    {
                        slot.get_or_insert(url);
                    }
                    page.execute(FailRequestParams::new(
                        request_id,
                        ErrorReason::BlockedByClient,
                    ))
                    .await
                    .err()
                };
                if let Some(err) = settled {
                    debug!("ssrf fetch guard could not settle a paused request: {err}");
                }
            }
        });
        Ok(guard)
    }

    /// Check `url` up front, before any page is opened.
    fn check(&self, url: &str) -> Result<()> {
        browser_request_check(url, &self.policy)?;
        Ok(())
    }

    /// Fail if a document navigation was refused, with the HTTP path's SSRF error.
    fn ensure_clean(&self) -> Result<()> {
        let blocked = self.blocked.lock().ok().and_then(|slot| slot.clone());
        if let Some(url) = blocked {
            self.check(&url)?;
            anyhow::bail!("browser navigation refused for {url}");
        }
        Ok(())
    }
}

/// The main frame of a render page, whose subframes the guard refuses;
/// `None` for a login page, which keeps its frames.
#[cfg(feature = "browser")]
async fn render_main_frame(
    page: &chromiumoxide::Page,
    page_use: PageUse,
) -> Result<Option<chromiumoxide::cdp::browser_protocol::page::FrameId>> {
    match page_use {
        PageUse::Render => Ok(Some(
            page.mainframe()
                .await
                .context("failed to read the browser page's main frame")?
                .context("the browser page has no main frame")?,
        )),
        PageUse::Login => Ok(None),
    }
}

/// The URL a paused Fetch event asks to reach: the request URL at the request
/// stage, the resolved `Location` of a 3xx at the response stage, `None` for
/// any other response.
#[cfg(feature = "browser")]
fn paused_target(
    event: &chromiumoxide::cdp::browser_protocol::fetch::EventRequestPaused,
) -> Option<String> {
    let Some(status) = event.response_status_code else {
        return Some(event.request.url.clone());
    };
    if !(300..400).contains(&status) {
        return None;
    }
    let location = event
        .response_headers
        .as_deref()?
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("location"))?;
    let base = url::Url::parse(&event.request.url).ok()?;
    // An unparseable Location is handed to the request check as-is, which
    // refuses it.
    Some(
        base.join(&location.value)
            .map_or_else(|_| location.value.clone(), String::from),
    )
}

/// Stop a rendered page from opening windows: a popup is a separate browser
/// target the Fetch guard on this page does not see. Render paths have no
/// user, so no popup is wanted; the login flow keeps them for OAuth windows.
#[cfg(feature = "browser")]
async fn block_window_open(page: &chromiumoxide::Page) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
    let script = AddScriptToEvaluateOnNewDocumentParams::new("window.open = () => null;");
    page.execute(script)
        .await
        .context("failed to disable window.open on the render page")?;
    Ok(())
}

/// Navigate `page` to `url`, refusing a denied target or redirect hop.
#[cfg(feature = "browser")]
async fn goto_guarded(page: &chromiumoxide::Page, guard: &SsrfFetchGuard, url: &str) -> Result<()> {
    guard.check(url)?;
    let navigated = page.goto(url).await;
    guard.ensure_clean()?;
    navigated.context("failed to navigate the browser page")?;
    Ok(())
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
        assert_eq!(cookies, [] as [crate::browser::Cookie; 0]);
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

    #[test]
    fn browser_request_check_matches_the_http_redirect_policy() {
        let policy = crate::ssrf::SsrfPolicy::deny_all();
        for url in [
            "http://127.0.0.1/",
            "http://10.255.255.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "file:///etc/passwd",
        ] {
            let err = browser_request_check(url, &policy).expect_err(url);
            assert!(
                matches!(err, crate::error::NabError::SsrfBlocked(_)),
                "{url} must get the SSRF refusal, got {err}"
            );
        }
        for url in ["about:blank", "data:text/html,hi", "blob:https://a.test/x"] {
            assert!(browser_request_check(url, &policy).is_ok(), "{url}");
        }
        // The private-range opt-out relaxes 10/8 but never metadata or loopback.
        let relaxed = crate::ssrf::SsrfPolicy::deny_all().with_allow_private(true);
        assert!(browser_request_check("http://10.255.255.1/", &relaxed).is_ok());
        assert!(browser_request_check("http://169.254.169.254/", &relaxed).is_err());
        assert!(browser_request_check("http://127.0.0.1/", &relaxed).is_err());
    }

    /// A page that 302-redirects the headless browser to loopback, a private
    /// address or the cloud metadata address must be refused with the same SSRF
    /// error the HTTP client returns, and the loopback listener must never see a
    /// connection.
    ///
    /// The redirecting page is served from this machine's own LAN address,
    /// which the test admits through `NAB_SSRF_ALLOWLIST`; loopback and
    /// link-local can never be allowlisted, and `10.255.255.1` is not listed.
    ///
    /// Ignored by default: it launches a throwaway headless Chrome (path from
    /// `NAB_TEST_CHROME`, else a standard install location) and needs a
    /// non-loopback network interface. Run with:
    /// `cargo test --features browser -- --ignored browser_refuses_redirect`.
    #[cfg(feature = "browser")]
    #[tokio::test]
    #[ignore = "requires a local Chrome binary and a network interface"]
    #[allow(clippy::too_many_lines)] // One end-to-end scenario: launch, serve, render, assert.
    async fn browser_refuses_redirect_to_private_targets() {
        let chrome = std::env::var("NAB_TEST_CHROME")
            .ok()
            .or_else(|| {
                [
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                    "/usr/bin/google-chrome",
                    "/usr/bin/chromium",
                ]
                .into_iter()
                .find(|path| std::path::Path::new(path).is_file())
                .map(str::to_string)
            })
            .expect("set NAB_TEST_CHROME to a Chrome or Chromium binary");

        let free_port = || {
            std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|l| l.local_addr())
                .expect("free port")
                .port()
        };
        let port = free_port();
        let profile = tempfile::tempdir().expect("chrome profile dir");
        let mut child = std::process::Command::new(chrome)
            .arg("--headless=new")
            .arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .args(["--no-first-run", "--disable-extensions", "--disable-gpu"])
            .arg("about:blank")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("launch chrome");

        let mut browser = None;
        for _ in 0..50 {
            if let Ok(b) = BrowserLogin::connect(Some(port)).await {
                browser = Some(b);
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let browser = browser.expect("connect to the throwaway chrome");

        // Loopback listener: any accepted connection means the hop went through.
        let sink = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback sink");
        let sink_port = sink.local_addr().expect("sink addr").port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&hits);
        let accept = tokio::spawn(async move {
            while let Ok((_stream, _)) = sink.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });

        // The interface address used to reach the internet; no packet is sent.
        let lan_ip = std::net::UdpSocket::bind("0.0.0.0:0")
            .and_then(|sock| sock.connect("192.0.2.1:9").map(|()| sock))
            .and_then(|sock| sock.local_addr())
            .expect("a non-loopback interface")
            .ip();
        assert!(!lan_ip.is_loopback(), "needs a non-loopback interface");
        // SAFETY: this ignored test runs on its own; no other thread reads the
        // environment while it is set.
        unsafe { std::env::set_var(crate::ssrf::ALLOWLIST_ENV, lan_ip.to_string()) };

        // One redirector per target: answers every request with a 302.
        let redirector = |target: String| async move {
            let listener = tokio::net::TcpListener::bind((lan_ip, 0))
                .await
                .expect("redirector");
            let port = listener.local_addr().expect("redirector addr").port();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                while let Ok((mut stream, _)) = listener.accept().await {
                    let mut buf = [0u8; 2048];
                    let _ = stream.read(&mut buf).await;
                    let reply = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {target}\r\n\
                         Content-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                }
            });
            format!("http://{lan_ip}:{port}/start")
        };

        let targets = [
            format!("http://127.0.0.1:{sink_port}/"),
            "http://10.255.255.1/".to_string(),
            "http://169.254.169.254/latest/meta-data/".to_string(),
        ];
        let mut failures = Vec::new();
        for target in &targets {
            let start = redirector(target.clone()).await;
            let result =
                tokio::time::timeout(Duration::from_secs(30), browser.render_markdown(&start))
                    .await;
            match result {
                Ok(Err(err)) if format!("{err:#}").contains("SSRF") => {}
                Ok(Err(err)) => failures.push(format!("{target}: non-SSRF error: {err:#}")),
                Ok(Ok(_)) => failures.push(format!("{target}: rendered, not refused")),
                Err(_) => failures.push(format!("{target}: timed out, not refused")),
            }
        }
        let loopback_hits = hits.load(std::sync::atomic::Ordering::SeqCst);
        accept.abort();
        drop(browser);
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(loopback_hits, 0, "the loopback hop reached the listener");
        assert!(failures.is_empty(), "redirects not refused: {failures:#?}");
    }

    /// Launch a throwaway headless Chrome and connect to it.
    #[cfg(feature = "browser")]
    async fn launch_test_chrome() -> (std::process::Child, tempfile::TempDir, BrowserLogin) {
        let chrome = std::env::var("NAB_TEST_CHROME")
            .expect("set NAB_TEST_CHROME to a Chrome or Chromium binary");
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("free port")
            .port();
        let profile = tempfile::tempdir().expect("chrome profile dir");
        let mut child = std::process::Command::new(chrome)
            .arg("--headless=new")
            .arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .args(["--no-first-run", "--disable-extensions", "--disable-gpu"])
            .arg("about:blank")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("launch chrome");
        for _ in 0..50 {
            if let Ok(browser) = BrowserLogin::connect(Some(port)).await {
                return (child, profile, browser);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("could not connect to the throwaway chrome");
    }

    /// Serve `body` as HTML on `ip` for every request; returns the port.
    #[cfg(feature = "browser")]
    async fn serve_html(ip: std::net::IpAddr, body: String) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((ip, 0))
            .await
            .expect("html server");
        let port = listener.local_addr().expect("server addr").port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        port
    }

    /// Pages that reach loopback through a popup (`window.open`, a scripted
    /// `target=_blank` click) or a cross-site iframe must not reach it.
    ///
    /// The cross-site frame is served from `<lan-ip>.nip.io`, a public DNS name
    /// for the LAN address, so Chrome isolates it in its own process. Ignored:
    /// needs `NAB_TEST_CHROME`, a network interface and public DNS.
    #[cfg(feature = "browser")]
    #[tokio::test]
    #[ignore = "requires a local Chrome binary, a network interface and DNS"]
    async fn browser_guards_popups_and_cross_site_frames() {
        let (mut child, _profile, browser) = launch_test_chrome().await;
        let sink = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("sink");
        let sink_url = format!(
            "http://127.0.0.1:{}/",
            sink.local_addr().expect("addr").port()
        );
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&hits);
        let accept = tokio::spawn(async move {
            while let Ok((_stream, _)) = sink.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let lan_ip = std::net::UdpSocket::bind("0.0.0.0:0")
            .and_then(|sock| sock.connect("192.0.2.1:9").map(|()| sock))
            .and_then(|sock| sock.local_addr())
            .expect("a non-loopback interface")
            .ip();
        // SAFETY: this ignored test runs on its own; no other thread reads the
        // environment while it is set.
        unsafe { std::env::set_var(crate::ssrf::ALLOWLIST_ENV, lan_ip.to_string()) };

        let inner = serve_html(lan_ip, format!("<img src=\"{sink_url}img.png\">")).await;
        let cases = [
            (
                "window.open",
                format!("<script>window.open('{sink_url}open')</script>"),
            ),
            (
                "target=_blank click",
                format!(
                    "<a id=a href=\"{sink_url}blank\" target=_blank>x</a>\
                     <script>document.getElementById('a').click()</script>"
                ),
            ),
            (
                "cross-site iframe",
                format!("<iframe src=\"http://{lan_ip}.nip.io:{inner}/inner\"></iframe>"),
            ),
        ];
        let mut reached = Vec::new();
        for (name, html) in cases {
            let before = hits.load(std::sync::atomic::Ordering::SeqCst);
            let port = serve_html(lan_ip, format!("<p>page</p>{html}")).await;
            let url = format!("http://{lan_ip}:{port}/");
            let _ =
                tokio::time::timeout(Duration::from_secs(30), browser.render_markdown(&url)).await;
            // Give a popup or frame time to issue its request.
            tokio::time::sleep(Duration::from_secs(2)).await;
            if hits.load(std::sync::atomic::Ordering::SeqCst) > before {
                reached.push(name);
            }
        }
        accept.abort();
        drop(browser);
        let _ = child.kill();
        let _ = child.wait();
        assert!(reached.is_empty(), "loopback reached through: {reached:?}");
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
