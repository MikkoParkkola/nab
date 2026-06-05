// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Browser-profile fallback for `--cookies auto`.
//!
//! When `auto` picks a single browser profile and the resulting response is a
//! bot / Cloudflare challenge (the chosen profile lacks valid clearance cookies
//! for the target domain), nab retries with the remaining available browser
//! profiles in a deterministic order and returns the first non-challenge
//! response.
//!
//! The core loop ([`fallback_over_profiles`]) is generic over an injected
//! "attempt" closure so it can be unit-tested with canned responses — no
//! network and no real browser cookie store required. Production wires the
//! closure to the live HTTP path; tests wire it to a lookup table.
//!
//! Challenge detection reuses the shared
//! [`crate::content::response_classifier::classify_http_response`] classifier
//! (the same one the CLI fetch diagnostics use), so the fallback fires on
//! exactly the markers the issue describes (`cf-chl-`, `"just a moment..."`,
//! `cf-browser-verification`, …) without a second bespoke detector.

use std::future::Future;

use super::CookieSource;
use crate::content::response_classifier::{ResponseDiagnosticKind, classify_http_response};

/// Deterministic order in which `auto` fallback considers browser profiles.
///
/// Edge and Dia collapse onto [`CookieSource::Chrome`] (they share the Chromium
/// cookie store), so the list is intentionally de-duplicated by *cookie source*
/// rather than by browser name.
pub const FALLBACK_ORDER: [CookieSource; 4] = [
    CookieSource::Brave,
    CookieSource::Chrome,
    CookieSource::Firefox,
    CookieSource::Safari,
];

/// Build the ordered list of fallback candidates, excluding the profile that
/// `auto` already tried.
///
/// The result preserves [`FALLBACK_ORDER`] and drops `already_tried` so each
/// remaining profile is attempted at most once.
#[must_use]
pub fn fallback_candidates(already_tried: CookieSource) -> Vec<CookieSource> {
    FALLBACK_ORDER
        .iter()
        .copied()
        .filter(|source| *source != already_tried)
        .collect()
}

/// Whether a `(status, body)` pair is a bot / browser challenge that should
/// trigger profile fallback.
///
/// Returns `true` only for [`ResponseDiagnosticKind::BrowserChallenge`] classes
/// (Cloudflare, Turnstile, CAPTCHA interstitial, AWS WAF, …). Auth walls, rate
/// limits, and clean responses do **not** trigger fallback — a different
/// cookie profile cannot fix those.
#[must_use]
pub fn is_challenge(status: u16, body: &str) -> bool {
    matches!(
        classify_http_response(status, body).map(|d| d.kind),
        Some(ResponseDiagnosticKind::BrowserChallenge(_))
    )
}

/// Outcome of the per-profile attempt closure.
///
/// `Available` carries the `(status, body)` the profile produced. `Unavailable`
/// means the profile had no usable cookies for the domain (empty cookie store
/// or no cookies for the target) and therefore does not count as an attempt.
pub enum AttemptOutcome {
    /// The profile produced a response.
    Available { status: u16, body: String },
    /// The profile had no cookies for the domain; skip without counting it.
    Unavailable,
}

/// Result of running the fallback loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackResult {
    /// A profile produced a non-challenge response. Carries the winning
    /// profile so the caller can re-resolve cookies / log it.
    Resolved {
        source: CookieSource,
        status: u16,
        body: String,
    },
    /// Every available profile still returned a challenge (or none were
    /// available). The caller should keep its original response (AC NAB.COOKIE.3).
    Exhausted { tried: Vec<CookieSource> },
}

/// Iterate `candidates`, calling `attempt` for each, and return the first
/// profile whose response is **not** a challenge.
///
/// * Profiles reported [`AttemptOutcome::Unavailable`] are skipped and not
///   recorded as "tried" (they were never actually fetched with).
/// * The loop is bounded: each candidate is attempted at most once.
/// * On exhaustion, returns [`FallbackResult::Exhausted`] listing the profiles
///   that were actually attempted, so the caller can warn and keep the original
///   response instead of erroring.
///
/// The closure is async and fallible-free by construction: it maps a
/// [`CookieSource`] to an [`AttemptOutcome`]. Production resolves cookies and
/// performs the HTTP request inside it; tests return canned outcomes.
pub async fn fallback_over_profiles<F, Fut>(
    candidates: Vec<CookieSource>,
    mut attempt: F,
) -> FallbackResult
where
    F: FnMut(CookieSource) -> Fut,
    Fut: Future<Output = AttemptOutcome>,
{
    let mut tried = Vec::new();
    for source in candidates {
        match attempt(source).await {
            AttemptOutcome::Unavailable => {}
            AttemptOutcome::Available { status, body } => {
                tried.push(source);
                if !is_challenge(status, &body) {
                    return FallbackResult::Resolved {
                        source,
                        status,
                        body,
                    };
                }
            }
        }
    }
    FallbackResult::Exhausted { tried }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        AttemptOutcome, CookieSource, FallbackResult, fallback_candidates, fallback_over_profiles,
        is_challenge,
    };

    const CHALLENGE_BODY: &str = "<html><head><title>Just a moment...</title></head><body>\
         <div id='challenge-error-text'>Enable JavaScript and cookies to continue</div>\
         <div id='cf-chl-widget'></div></body></html>";
    const CLEAN_BODY: &str =
        "<html><body><article><h1>Real Article</h1><p>Lots of words.</p></article></body></html>";

    #[test]
    fn challenge_body_with_cloudflare_markers_is_detected() {
        // GIVEN a 403 Cloudflare interstitial — WHEN classified — THEN it is a challenge.
        assert!(is_challenge(403, CHALLENGE_BODY));
    }

    #[test]
    fn clean_article_is_not_a_challenge() {
        // GIVEN a normal 200 article — WHEN classified — THEN no fallback triggers.
        assert!(!is_challenge(200, CLEAN_BODY));
    }

    #[test]
    fn fallback_candidates_excludes_already_tried_and_keeps_order() {
        // GIVEN auto picked Brave — WHEN building candidates — THEN Brave is dropped, order preserved.
        let candidates = fallback_candidates(CookieSource::Brave);
        assert_eq!(
            candidates,
            vec![
                CookieSource::Chrome,
                CookieSource::Firefox,
                CookieSource::Safari
            ]
        );
    }

    /// AC NAB.COOKIE.4 — mock a 403-challenge from profile A and 200 from
    /// profile B; assert auto returns B's body.
    #[tokio::test]
    async fn fallback_returns_first_clean_profile_body() {
        // GIVEN Chrome challenges and Firefox returns clean content.
        let mut table: HashMap<CookieSource, (u16, &str)> = HashMap::new();
        table.insert(CookieSource::Chrome, (403, CHALLENGE_BODY));
        table.insert(CookieSource::Firefox, (200, CLEAN_BODY));
        table.insert(CookieSource::Safari, (200, CLEAN_BODY));

        // WHEN we fall back over Brave's siblings.
        let result = fallback_over_profiles(fallback_candidates(CookieSource::Brave), |source| {
            let entry = table.get(&source).copied();
            async move {
                match entry {
                    Some((status, body)) => AttemptOutcome::Available {
                        status,
                        body: body.to_string(),
                    },
                    None => AttemptOutcome::Unavailable,
                }
            }
        })
        .await;

        // THEN Firefox (first clean profile in order) wins.
        match result {
            FallbackResult::Resolved { source, body, .. } => {
                assert_eq!(source, CookieSource::Firefox);
                assert_eq!(body, CLEAN_BODY);
            }
            FallbackResult::Exhausted { .. } => panic!("expected a clean profile to resolve"),
        }
    }

    #[tokio::test]
    async fn unavailable_profiles_are_skipped_not_counted() {
        // GIVEN Chrome/Safari have no cookies and Firefox is clean.
        let result = fallback_over_profiles(fallback_candidates(CookieSource::Brave), |source| {
            let outcome = match source {
                CookieSource::Firefox => AttemptOutcome::Available {
                    status: 200,
                    body: CLEAN_BODY.to_string(),
                },
                _ => AttemptOutcome::Unavailable,
            };
            async move { outcome }
        })
        .await;

        // THEN Firefox resolves and the empty profiles were never tried.
        assert_eq!(
            result,
            FallbackResult::Resolved {
                source: CookieSource::Firefox,
                status: 200,
                body: CLEAN_BODY.to_string(),
            }
        );
    }

    #[tokio::test]
    async fn all_profiles_challenged_returns_exhausted_with_tried_list() {
        // GIVEN every available profile still returns a challenge.
        let result = fallback_over_profiles(fallback_candidates(CookieSource::Brave), |source| {
            let outcome = match source {
                CookieSource::Safari => AttemptOutcome::Unavailable,
                _ => AttemptOutcome::Available {
                    status: 403,
                    body: CHALLENGE_BODY.to_string(),
                },
            };
            async move { outcome }
        })
        .await;

        // THEN the loop reports exhaustion naming exactly the attempted profiles.
        assert_eq!(
            result,
            FallbackResult::Exhausted {
                tried: vec![CookieSource::Chrome, CookieSource::Firefox],
            }
        );
    }
}
