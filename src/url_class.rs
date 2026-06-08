//! Pure URL classification predicates.
//!
//! These helpers are plain string/regex matching with **no** browser, network,
//! or async dependency, so this module is always compiled regardless of the
//! `browser` / `browser-launcher` feature flags. Keeping them here lets the CLI
//! fetch ladder branch on URL shape (e.g. the authed DOM-render short-circuit)
//! without pulling in — or feature-gating against — the browser stack.

use regex::Regex;
use std::sync::LazyLock;

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
/// use nab::url_class::is_x_article_url;
/// assert!(is_x_article_url("https://x.com/i/article/123"));
/// assert!(is_x_article_url("https://www.x.com/i/article/9"));
/// assert!(!is_x_article_url("https://x.com/home"));
/// assert!(!is_x_article_url("https://fxtwitter.com/i/article/1"));
/// ```
#[must_use]
pub fn is_x_article_url(url: &str) -> bool {
    X_ARTICLE_RE.is_match(url)
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
}
