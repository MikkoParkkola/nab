//! Shared response classification for auth walls, browser challenges, thin
//! shells, and other fetch-time diagnostics.

use super::quality::QualityScore;

/// Auth-related response class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRequiredKind {
    LoginRequired,
    SessionExpired,
}

/// Bot / browser challenge response class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserChallengeKind {
    Cloudflare,
    Vercel,
    Turnstile,
    Captcha,
    LinkedInBotDetection,
}

/// High-level fetch-time response diagnosis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseDiagnosticKind {
    AuthRequired(AuthRequiredKind),
    BrowserChallenge(BrowserChallengeKind),
    RateLimited,
}

/// Classified diagnostic for an HTTP response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseDiagnostic {
    pub kind: ResponseDiagnosticKind,
    pub status: u16,
}

impl ResponseDiagnostic {
    /// Stable machine-readable code for structured output.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self.kind {
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::LoginRequired) => {
                "login_required"
            }
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::SessionExpired) => {
                "session_expired"
            }
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Cloudflare) => {
                "cloudflare_challenge"
            }
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Vercel) => {
                "vercel_challenge"
            }
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Turnstile) => {
                "turnstile_challenge"
            }
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Captcha) => {
                "captcha_challenge"
            }
            ResponseDiagnosticKind::BrowserChallenge(
                BrowserChallengeKind::LinkedInBotDetection,
            ) => "linkedin_bot_detection",
            ResponseDiagnosticKind::RateLimited => "rate_limited",
        }
    }

    /// Human-readable summary for logs and diagnostics.
    #[must_use]
    pub fn summary(self) -> String {
        match self.kind {
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::LoginRequired) => format!(
                "Login wall or authenticated content detected (HTTP {}).",
                self.status
            ),
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::SessionExpired) => format!(
                "Session appears expired or timed out (HTTP {}).",
                self.status
            ),
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Cloudflare) => {
                format!(
                    "Cloudflare browser challenge detected (HTTP {}).",
                    self.status
                )
            }
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Vercel) => {
                "Vercel Security Checkpoint detected.".to_string()
            }
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Turnstile) => {
                "Cloudflare Turnstile challenge detected.".to_string()
            }
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Captcha) => {
                format!("CAPTCHA challenge detected (HTTP {}).", self.status)
            }
            ResponseDiagnosticKind::BrowserChallenge(
                BrowserChallengeKind::LinkedInBotDetection,
            ) => "LinkedIn bot detection (HTTP 999).".to_string(),
            ResponseDiagnosticKind::RateLimited => format!(
                "Rate limit or throttling response detected (HTTP {}).",
                self.status
            ),
        }
    }

    /// Suggested remediation phrased for both CLI and MCP callers.
    #[must_use]
    pub fn guidance(self) -> &'static str {
        match self.kind {
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::LoginRequired) => {
                "Sign in in a browser first, then retry with the default browser cookies or a named authenticated session. If you explicitly disabled cookies, re-enable them."
            }
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::SessionExpired) => {
                "Refresh the site in a browser to renew the session, then retry with the default browser cookies or a named authenticated session."
            }
            ResponseDiagnosticKind::BrowserChallenge(_) => {
                "Complete the browser challenge in a real browser first, then retry with the default browser cookies or a named session. Use an explicit browser override only if the default profile is not the authenticated one."
            }
            ResponseDiagnosticKind::RateLimited => {
                "Retry later, or use an authenticated browser/session path if the site rate-limits anonymous traffic."
            }
        }
    }

    /// Full user-facing diagnostic message.
    #[must_use]
    pub fn message(self) -> String {
        format!("{}\n{}", self.summary(), self.guidance())
    }
}

/// Shared response classes used by fetch diagnostics and MCP tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseClass {
    Unauthorized,
    LoginRequired,
    Forbidden,
    BotChallenge,
    RateLimited,
    ThinContent,
}

/// One classified response signal with a confidence estimate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResponseSignal {
    pub class: ResponseClass,
    pub confidence: f32,
    pub reason: &'static str,
}

/// Multi-signal response classification result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResponseClassification {
    signals: Vec<ResponseSignal>,
}

impl ResponseClassification {
    fn push(&mut self, signal: ResponseSignal) {
        if !self.has_class(signal.class) {
            self.signals.push(signal);
        }
    }

    /// Highest-priority response signal.
    #[must_use]
    pub fn primary(&self) -> Option<&ResponseSignal> {
        self.signals.first()
    }

    /// Whether the classification contains the given signal class.
    #[must_use]
    pub fn has_class(&self, class: ResponseClass) -> bool {
        self.signals.iter().any(|signal| signal.class == class)
    }
}

/// Inputs used for shared response classification.
#[derive(Debug, Clone, Copy)]
pub struct ResponseAnalysis<'a> {
    pub status: u16,
    pub body: &'a str,
    pub content_type: Option<&'a str>,
    pub html_bytes: Option<usize>,
    pub markdown_chars: Option<usize>,
    pub quality: Option<&'a QualityScore>,
}

/// Classify a raw HTTP response body into a higher-level auth/challenge signal.
#[must_use]
pub fn classify_http_response(status: u16, body: &str) -> Option<ResponseDiagnostic> {
    let body_lower = body.to_lowercase();
    classify_http_response_lower(status, &body_lower)
}

/// Classify a response using status/body plus optional HTML extraction signals.
#[must_use]
pub fn classify_response(analysis: ResponseAnalysis<'_>) -> ResponseClassification {
    let body_lower = analysis.body.to_lowercase();
    let mut classification = ResponseClassification::default();

    if analysis.status == 401 {
        classification.push(ResponseSignal {
            class: ResponseClass::Unauthorized,
            confidence: 0.97,
            reason: "http 401 unauthorized response",
        });
    } else if let Some(diagnostic) = classify_http_response_lower(analysis.status, &body_lower) {
        classification.push(map_diagnostic_signal(diagnostic));
    } else if matches!(analysis.status, 403 | 999) && looks_like_forbidden(&body_lower) {
        classification.push(ResponseSignal {
            class: ResponseClass::Forbidden,
            confidence: if analysis.status == 999 { 0.96 } else { 0.85 },
            reason: if analysis.status == 999 {
                "nonstandard anti-automation block status detected"
            } else {
                "forbidden or access-denied markers detected"
            },
        });
    }

    if let (Some(html_bytes), Some(markdown_chars)) = (analysis.html_bytes, analysis.markdown_chars)
        && classify_thin_content(
            analysis.content_type,
            html_bytes,
            markdown_chars,
            analysis.quality,
        )
        .is_some()
    {
        let confidence = analysis.quality.map_or(0.78_f32, |quality| {
            if quality.confidence < 0.5 {
                0.9_f32
            } else {
                0.8_f32
            }
        });
        classification.push(ResponseSignal {
            class: ResponseClass::ThinContent,
            confidence,
            reason: "markdown output is disproportionately small relative to the HTML body",
        });
    }

    classification
}

/// Thin-content diagnostic payload used by CLI / MCP fetch diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThinContentDiagnostic {
    pub html_bytes: usize,
    pub markdown_chars: usize,
    pub low_confidence: bool,
}

/// Shared thin-content classifier using the same narrow thresholds as HTML
/// extraction plus an optional low-confidence signal.
#[must_use]
pub fn classify_thin_content(
    content_type: Option<&str>,
    html_bytes: usize,
    markdown_chars: usize,
    quality: Option<&QualityScore>,
) -> Option<ThinContentDiagnostic> {
    let is_html = content_type.is_some_and(|value| value.contains("html"));
    if !is_html {
        return None;
    }

    if is_thin_content(html_bytes, markdown_chars) {
        return Some(ThinContentDiagnostic {
            html_bytes,
            markdown_chars,
            low_confidence: quality.is_some_and(|score| score.confidence < 0.5),
        });
    }

    if html_bytes >= 5_000
        && markdown_chars < 800
        && quality.is_some_and(|score| score.confidence < 0.35)
    {
        return Some(ThinContentDiagnostic {
            html_bytes,
            markdown_chars,
            low_confidence: true,
        });
    }

    None
}

fn classify_http_response_lower(status: u16, body_lower: &str) -> Option<ResponseDiagnostic> {
    if status == 999 {
        return Some(ResponseDiagnostic {
            kind: ResponseDiagnosticKind::BrowserChallenge(
                BrowserChallengeKind::LinkedInBotDetection,
            ),
            status,
        });
    }

    if looks_like_turnstile(body_lower) {
        return Some(ResponseDiagnostic {
            kind: ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Turnstile),
            status,
        });
    }

    if status == 429 && looks_like_vercel_checkpoint(body_lower) {
        return Some(ResponseDiagnostic {
            kind: ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Vercel),
            status,
        });
    }

    if matches!(status, 403 | 503) && looks_like_cloudflare_challenge(body_lower) {
        return Some(ResponseDiagnostic {
            kind: ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Cloudflare),
            status,
        });
    }

    if matches!(status, 403 | 429 | 503) && looks_like_captcha_interstitial(body_lower) {
        return Some(ResponseDiagnostic {
            kind: ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Captcha),
            status,
        });
    }

    if matches!(status, 419 | 440) || looks_like_session_expired(body_lower) {
        return Some(ResponseDiagnostic {
            kind: ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::SessionExpired),
            status,
        });
    }

    if status == 429 && looks_like_rate_limit(body_lower) {
        return Some(ResponseDiagnostic {
            kind: ResponseDiagnosticKind::RateLimited,
            status,
        });
    }

    if (status == 403
        && (looks_like_login_wall(body_lower) || looks_like_password_gate(body_lower)))
        || (looks_like_login_wall(body_lower) && looks_like_password_gate(body_lower))
    {
        return Some(ResponseDiagnostic {
            kind: ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::LoginRequired),
            status,
        });
    }

    None
}

fn map_diagnostic_signal(diagnostic: ResponseDiagnostic) -> ResponseSignal {
    match diagnostic.kind {
        ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::LoginRequired) => ResponseSignal {
            class: ResponseClass::LoginRequired,
            confidence: if diagnostic.status == 200 { 0.83 } else { 0.95 },
            reason: "login-wall markers and password-gate signals detected",
        },
        ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::SessionExpired) => ResponseSignal {
            class: ResponseClass::Unauthorized,
            confidence: 0.94,
            reason: "session-expired markers detected",
        },
        ResponseDiagnosticKind::BrowserChallenge(_) => ResponseSignal {
            class: ResponseClass::BotChallenge,
            confidence: 0.97,
            reason: "browser-challenge or CAPTCHA markers detected",
        },
        ResponseDiagnosticKind::RateLimited => ResponseSignal {
            class: ResponseClass::RateLimited,
            confidence: 0.91,
            reason: "rate-limit markers detected",
        },
    }
}

fn looks_like_vercel_checkpoint(body_lower: &str) -> bool {
    contains_any(
        body_lower,
        &[
            "vercel security checkpoint",
            "we're verifying your browser",
            "we are verifying your browser",
        ],
    )
}

fn looks_like_cloudflare_challenge(body_lower: &str) -> bool {
    contains_any(
        body_lower,
        &[
            "cf-browser-verification",
            "cf-chl-",
            "cf-challenge",
            "checking your browser before accessing",
            "just a moment...",
            "cloudflare ray id",
        ],
    )
}

fn looks_like_turnstile(body_lower: &str) -> bool {
    contains_any(
        body_lower,
        &["cf-turnstile", "turnstile.js", "challenge-platform"],
    )
}

fn looks_like_captcha(body_lower: &str) -> bool {
    contains_any(
        body_lower,
        &["g-recaptcha", "grecaptcha", "h-captcha", "hcaptcha"],
    ) || (body_lower.contains("captcha") && body_lower.contains("<img"))
}

fn looks_like_captcha_interstitial(body_lower: &str) -> bool {
    looks_like_captcha(body_lower)
        && contains_any(
            body_lower,
            &[
                "verify you are human",
                "are you human",
                "security check",
                "browser verification",
                "checking your browser",
                "please enable javascript and cookies to continue",
            ],
        )
}

fn looks_like_rate_limit(body_lower: &str) -> bool {
    contains_any(
        body_lower,
        &[
            "too many requests",
            "rate limit",
            "rate-limit",
            "throttled",
            "request limit reached",
        ],
    )
}

fn looks_like_forbidden(body_lower: &str) -> bool {
    contains_any(
        body_lower,
        &[
            "access denied",
            "forbidden",
            "permission denied",
            "not authorized",
            "not authorised",
        ],
    )
}

fn looks_like_session_expired(body_lower: &str) -> bool {
    contains_any(
        body_lower,
        &[
            "session expired",
            "your session has expired",
            "session timed out",
            "please sign in again",
            "please log in again",
        ],
    )
}

fn looks_like_login_wall(body_lower: &str) -> bool {
    contains_any(
        body_lower,
        &[
            "login required",
            "log in to continue",
            "sign in to continue",
            "authentication required",
            "please authenticate",
            "continue with google",
            "continue with email",
            "sign in with",
        ],
    )
}

fn looks_like_password_gate(body_lower: &str) -> bool {
    contains_any(
        body_lower,
        &[
            "type=\"password\"",
            "autocomplete=\"current-password\"",
            "name=\"password\"",
            "id=\"password\"",
            "enter your password",
            "forgot password",
        ],
    )
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn is_thin_content(html_len: usize, markdown_len: usize) -> bool {
    const MIN_HTML_LEN: usize = 5_000;
    const MIN_MARKDOWN_LEN: usize = 800;
    const THIN_RATIO_PERCENT: usize = 2;

    if html_len < MIN_HTML_LEN || markdown_len >= MIN_MARKDOWN_LEN {
        return false;
    }

    let ratio_percent = (markdown_len * 100) / html_len.max(1);
    ratio_percent < THIN_RATIO_PERCENT
}

#[cfg(test)]
mod tests {
    use super::{
        AuthRequiredKind, BrowserChallengeKind, ResponseAnalysis, ResponseClass,
        ResponseDiagnosticKind, classify_http_response, classify_response,
    };

    #[test]
    fn classify_http_response_detects_vercel_checkpoint() {
        let body = "<html><body>Vercel Security Checkpoint</body></html>";
        let diagnostic = classify_http_response(429, body).expect("vercel classification");
        assert_eq!(
            diagnostic.kind,
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Vercel)
        );
        assert_eq!(diagnostic.code(), "vercel_challenge");
    }

    #[test]
    fn classify_http_response_detects_cloudflare_challenge() {
        let body = "<div id='cf-browser-verification'>Please wait...</div>";
        let diagnostic = classify_http_response(403, body).expect("cloudflare classification");
        assert_eq!(
            diagnostic.kind,
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Cloudflare)
        );
    }

    #[test]
    fn classify_http_response_detects_turnstile_challenge_on_200() {
        let body = "<div class='cf-turnstile'></div><script src='turnstile.js'></script>";
        let diagnostic = classify_http_response(200, body).expect("turnstile classification");
        assert_eq!(
            diagnostic.kind,
            ResponseDiagnosticKind::BrowserChallenge(BrowserChallengeKind::Turnstile)
        );
    }

    #[test]
    fn classify_http_response_detects_login_wall_with_password_form() {
        let body = r#"
            <html><body>
              <h1>Sign in to continue</h1>
              <form><input type="password" name="password"></form>
            </body></html>
        "#;
        let diagnostic = classify_http_response(200, body).expect("login wall classification");
        assert_eq!(
            diagnostic.kind,
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::LoginRequired)
        );
        assert_eq!(diagnostic.code(), "login_required");
    }

    #[test]
    fn classify_http_response_detects_session_expired() {
        let body = "<html><body>Your session has expired. Please sign in again.</body></html>";
        let diagnostic = classify_http_response(200, body).expect("session expired classification");
        assert_eq!(
            diagnostic.kind,
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::SessionExpired)
        );
    }

    #[test]
    fn classify_http_response_detects_rate_limit() {
        let body = "Too many requests. Rate limit reached.";
        let diagnostic = classify_http_response(429, body).expect("rate limit classification");
        assert_eq!(diagnostic.kind, ResponseDiagnosticKind::RateLimited);
    }

    #[test]
    fn classify_http_response_ignores_normal_html() {
        let body = "<html><body><article><h1>Hello</h1><p>World</p></article></body></html>";
        assert!(
            classify_http_response(200, body).is_none(),
            "expected no diagnostic for regular article HTML"
        );
    }

    #[test]
    fn classify_response_marks_thin_html_content() {
        let classification = classify_response(ResponseAnalysis {
            status: 200,
            body: "<html></html>",
            content_type: Some("text/html"),
            html_bytes: Some(20_000),
            markdown_chars: Some(120),
            quality: None,
        });
        assert!(classification.has_class(ResponseClass::ThinContent));
    }

    #[test]
    fn classify_response_maps_session_expired_to_unauthorized() {
        let classification = classify_response(ResponseAnalysis {
            status: 200,
            body: "<html><body>Your session has expired. Please sign in again.</body></html>",
            content_type: Some("text/html"),
            html_bytes: None,
            markdown_chars: None,
            quality: None,
        });
        assert_eq!(
            classification.primary().map(|signal| signal.class),
            Some(ResponseClass::Unauthorized)
        );
    }

    #[test]
    fn classify_response_maps_http_401_to_unauthorized() {
        let classification = classify_response(ResponseAnalysis {
            status: 401,
            body: "<html><body>Unauthorized</body></html>",
            content_type: Some("text/html"),
            html_bytes: None,
            markdown_chars: None,
            quality: None,
        });
        assert_eq!(
            classification.primary().map(|signal| signal.class),
            Some(ResponseClass::Unauthorized)
        );
    }

    #[test]
    fn classify_http_response_avoids_login_page_recaptcha_false_positive() {
        let body = r#"
            <html><body>
              <h1>Sign in to continue</h1>
              <form><input type="password" name="password"></form>
              <div class="g-recaptcha"></div>
            </body></html>
        "#;
        let diagnostic = classify_http_response(200, body).expect("login wall classification");
        assert_eq!(
            diagnostic.kind,
            ResponseDiagnosticKind::AuthRequired(AuthRequiredKind::LoginRequired)
        );
    }
}
