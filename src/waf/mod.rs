//! Web Application Firewall (WAF) challenge handling.
//!
//! This module groups per-vendor challenge solvers behind a small
//! dispatcher API. The current release implements the **AWS WAF** replay
//! solver (see [`aws`]); future releases will add Cloudflare Turnstile
//! and `DataDome` replay paths.
//!
//! # Flow
//!
//! ```text
//!     HTML + headers
//!         │
//!         ▼
//!   detect_challenge ──► Some(ChallengeKind::AwsWaf(ctx))
//!         │
//!         ▼
//!    solve_challenge ──► Ok(Cookie { name:"aws-waf-token", value:"..." })
//!                        Err(WafError::UnknownAlgorithm) → caller falls
//!                        back to js / browser tier
//! ```
//!
//! The public API is intentionally narrow so that callers (CLI, MCP,
//! library embedders) can route through one function instead of
//! branching on vendor.

pub mod aws;

pub use aws::{AwsWafError, ChallengeAlgorithmMap, GokuContext, SolvedChallenge};

/// Top-level errors returned by the WAF dispatcher.
#[derive(Debug, thiserror::Error)]
pub enum WafError {
    /// The response does not look like a known WAF challenge.
    #[error("waf: no challenge detected")]
    NoChallenge,
    /// AWS WAF replay path failed. See inner error.
    #[error("waf: aws replay failed: {0}")]
    Aws(#[from] AwsWafError),
    /// Non-AWS vendor not yet supported.
    #[error("waf: vendor not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Identified challenge kind after `detect_challenge` runs.
///
/// Each variant carries the parsed context the solver needs so the
/// caller does not re-parse the HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChallengeKind {
    /// AWS WAF `gokuProps` challenge.
    AwsWaf(Box<GokuContext>),
    /// Cloudflare / Turnstile — placeholder, not solved in replay yet.
    Cloudflare,
    /// `DataDome` — placeholder, not solved in replay yet.
    DataDome,
}

/// Detect which WAF challenge (if any) the response is emitting.
///
/// The function is cheap (string scans only) and never allocates more
/// than the response body.
///
/// # Arguments
/// * `html` — raw HTML body of the interstitial response.
/// * `headers` — iterable of `(name, value)` pairs. AWS WAF sets
///   `x-amzn-waf-action: challenge` in the interstitial response.
#[must_use]
pub fn detect_challenge<'a, I>(html: &str, headers: I) -> Option<ChallengeKind>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    // 1. AWS WAF — header-first path.
    let mut aws_by_header = false;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("x-amzn-waf-action")
            && (value.eq_ignore_ascii_case("challenge") || value.eq_ignore_ascii_case("captcha"))
        {
            aws_by_header = true;
            break;
        }
    }
    if (aws_by_header || html.contains("awswaf.com"))
        && let Some(ctx) = aws::extract_goku_props(html)
    {
        return Some(ChallengeKind::AwsWaf(Box::new(ctx)));
    }

    // 2. Cloudflare / Turnstile.
    if html.contains("challenges.cloudflare.com") || html.contains("cf-turnstile") {
        return Some(ChallengeKind::Cloudflare);
    }

    // 3. DataDome.
    if html.contains(".datadome.co") || html.contains("dd_cookie_test") {
        return Some(ChallengeKind::DataDome);
    }

    None
}

/// Minimal cookie produced by a successful challenge solve.
///
/// We emit this instead of `reqwest::cookie::Cookie` so the WAF module
/// stays HTTP-client-agnostic. Callers attach the cookie to whichever
/// jar they use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
}

/// Solve a detected challenge in replay mode and return the resulting
/// cookie.
///
/// This variant is synchronous and does **not** perform any network
/// I/O. It is useful for unit tests and for callers who want to issue
/// the verify POST themselves (the [`SolvedChallenge`] payload is what
/// they need to POST).
///
/// # Errors
/// Returns [`WafError::Aws`] when the AWS replay solver cannot handle
/// the algorithm hash, or [`WafError::NotImplemented`] for vendors that
/// are detected but not solvable in this release.
pub fn solve_replay(kind: &ChallengeKind) -> Result<SolvedChallenge, WafError> {
    match kind {
        ChallengeKind::AwsWaf(ctx) => Ok(aws::solve_replay(ctx)?),
        ChallengeKind::Cloudflare => Err(WafError::NotImplemented("cloudflare")),
        ChallengeKind::DataDome => Err(WafError::NotImplemented("datadome")),
    }
}

#[cfg(test)]
mod tests {
    use super::{ChallengeKind, detect_challenge, solve_replay};

    const AWS_FIXTURE: &str = r#"
        <html><head>
          <script src="https://abc123.awswaf.com/x/y/challenge.js"></script>
          <script>
            window.gokuProps = {
              "challenge": "deadbeef",
              "challengeType": "deadbeefcafebabe1234567890abcdef1234567890abcdefdeadbeefcafebabe"
            };
          </script>
        </head></html>
    "#;

    #[test]
    fn detects_aws_from_header() {
        let headers = [("x-amzn-waf-action", "challenge")];
        let kind = detect_challenge(AWS_FIXTURE, headers).expect("aws detected");
        assert!(matches!(kind, ChallengeKind::AwsWaf(_)));
    }

    #[test]
    fn detects_aws_from_body() {
        let kind = detect_challenge(AWS_FIXTURE, std::iter::empty::<(&str, &str)>())
            .expect("aws detected");
        assert!(matches!(kind, ChallengeKind::AwsWaf(_)));
    }

    #[test]
    fn detects_cloudflare_turnstile() {
        let html =
            r#"<script src="https://challenges.cloudflare.com/cdn-cgi/challenge/..."></script>"#;
        let kind = detect_challenge(html, std::iter::empty::<(&str, &str)>())
            .expect("cloudflare detected");
        assert!(matches!(kind, ChallengeKind::Cloudflare));
    }

    #[test]
    fn detects_datadome() {
        let html = r#"<script src="https://js.datadome.co/boot.js"></script>"#;
        let kind =
            detect_challenge(html, std::iter::empty::<(&str, &str)>()).expect("datadome detected");
        assert!(matches!(kind, ChallengeKind::DataDome));
    }

    #[test]
    fn ignores_clean_html() {
        let html = "<html><body><h1>hi</h1></body></html>";
        assert!(detect_challenge(html, std::iter::empty::<(&str, &str)>()).is_none());
    }

    #[test]
    fn solve_replay_for_aws_returns_solution() {
        let kind = detect_challenge(AWS_FIXTURE, std::iter::empty::<(&str, &str)>())
            .expect("aws detected");
        let solved = solve_replay(&kind).expect("solver succeeds for mp_verify");
        assert_eq!(solved.algo, "mp_verify_network_bandwidth");
    }
}
