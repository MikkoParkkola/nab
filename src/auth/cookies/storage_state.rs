// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Playwright / CDP `storage_state` export (`nab cookies export --format playwright`).
//!
//! Implements MIK-5359 acceptance criterion `export.1`: convert the user's
//! browser cookies into a schema-valid Playwright [`StorageState`] JSON document
//! that can seed a logged-in browser context later (rung 3 of the task engine —
//! see `docs/design/2026-06-01-nab-task-engine-browser-modes.md`).
//!
//! The export is intentionally **faithful** for Chromium-family browsers: real
//! `domain`, `path`, `expires`, `httpOnly`, `secure`, and `sameSite` are pulled
//! from the cookie database so the seeded context authenticates correctly. A
//! storage-state with synthesized defaults would pass a shape check yet silently
//! fail to authenticate (wrong `domain` → the browser never sends the cookie).
//!
//! Firefox / Safari / Python-fallback extraction currently surface only
//! `name`/`value` (see [`super::CookieSource::get_cookies`]); for those sources
//! the metadata is best-effort with safe defaults, documented in the ADR.
//!
//! `origins[]` (localStorage) is always emitted empty: per `export.1` that is
//! acceptable, and nab never reads page-local web storage from disk.

use serde::{Deserialize, Serialize};

/// Chromium stores timestamps as microseconds since 1601-01-01 (the Windows
/// FILETIME epoch). Playwright/Unix expect seconds since 1970-01-01. This is the
/// offset in seconds between the two epochs.
const WINDOWS_TO_UNIX_EPOCH_SECS: i64 = 11_644_473_600;

/// A single cookie in Playwright `storage_state` form.
///
/// Field names match the Playwright [`storageState`] schema exactly (camelCase),
/// so the serialized JSON is consumed without a translation layer.
///
/// [`storageState`]: https://playwright.dev/docs/api/class-browsercontext#browser-context-storage-state
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaywrightCookie {
    pub name: String,
    pub value: String,
    /// Cookie host key, e.g. `.github.com` or `api.github.com`. Load-bearing for
    /// authentication — must be the real `host_key`, never synthesized.
    pub domain: String,
    pub path: String,
    /// Unix seconds; `-1` denotes a session cookie (Playwright convention).
    pub expires: f64,
    #[serde(rename = "httpOnly")]
    pub http_only: bool,
    pub secure: bool,
    /// One of `"Strict"`, `"Lax"`, `"None"` — the only values Playwright accepts.
    #[serde(rename = "sameSite")]
    pub same_site: SameSite,
}

/// Playwright-accepted `sameSite` enum. Chromium persists an integer; Playwright
/// rejects anything outside these three string variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

/// An origin's local storage. Always emitted empty for `export.1` (`origins: []`),
/// but typed so a future phase can populate it without a wire-format change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginState {
    pub origin: String,
    #[serde(rename = "localStorage")]
    pub local_storage: Vec<LocalStorageEntry>,
}

/// A single `localStorage` key/value pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalStorageEntry {
    pub name: String,
    pub value: String,
}

/// A complete Playwright / CDP `storage_state` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageState {
    pub cookies: Vec<PlaywrightCookie>,
    pub origins: Vec<OriginState>,
}

impl StorageState {
    /// Build a `storage_state` from cookies, with an empty `origins` list.
    #[must_use]
    pub fn from_cookies(cookies: Vec<PlaywrightCookie>) -> Self {
        Self {
            cookies,
            origins: Vec::new(),
        }
    }

    /// Serialize to pretty JSON (the on-disk / stdout form).
    ///
    /// # Errors
    /// Returns an error only if serde fails to serialize, which cannot happen for
    /// this fully-owned, string-keyed structure.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

/// Convert a Chromium `expires_utc` value to a Playwright `expires` field.
///
/// Chromium stores microseconds since 1601-01-01; `0` means a session cookie.
/// Playwright wants Unix seconds, with `-1` for session cookies.
///
/// # Examples
/// ```
/// use nab::auth::cookies::storage_state::chromium_expiry_to_unix;
/// // Session cookie.
/// assert_eq!(chromium_expiry_to_unix(0), -1.0);
/// // 13_437_022_686_718_487 µs since 1601 → 2026-10-21T... Unix seconds.
/// assert_eq!(chromium_expiry_to_unix(13_437_022_686_718_487), 1_792_549_086.0);
/// ```
#[must_use]
pub fn chromium_expiry_to_unix(expires_utc: i64) -> f64 {
    if expires_utc <= 0 {
        return -1.0;
    }
    // Integer-divide microseconds → seconds before the epoch shift to avoid
    // float precision loss on the ~13-quadrillion magnitude.
    let unix_secs = expires_utc / 1_000_000 - WINDOWS_TO_UNIX_EPOCH_SECS;
    if unix_secs <= 0 { -1.0 } else { unix_secs as f64 }
}

/// Map a Chromium `samesite` integer to the Playwright [`SameSite`] enum.
///
/// Chromium values: `-1` unspecified, `0` None, `1` Lax, `2` Strict. Playwright
/// accepts only `Strict`/`Lax`/`None`, so "unspecified" maps to the browser
/// default (`Lax`) — a valid enum string, never an empty/invalid one.
#[must_use]
pub fn chromium_samesite_to_playwright(samesite: i64) -> SameSite {
    match samesite {
        0 => SameSite::None,
        2 => SameSite::Strict,
        // 1 (Lax) and -1 (unspecified → browser default Lax) and any unknown.
        _ => SameSite::Lax,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_session_expiry_maps_to_minus_one() {
        // GIVEN a Chromium session cookie (expires_utc == 0)
        // WHEN converted
        // THEN Playwright sees the session sentinel -1.
        assert_eq!(chromium_expiry_to_unix(0), -1.0);
    }

    #[test]
    fn chromium_negative_expiry_maps_to_minus_one() {
        assert_eq!(chromium_expiry_to_unix(-5), -1.0);
    }

    #[test]
    fn chromium_real_expiry_uses_exact_epoch_formula() {
        // GIVEN a real expires_utc read from a live Brave cookie DB
        // WHEN converted with the FILETIME→Unix formula
        // THEN the result is the exact Unix-seconds value, not an approximation.
        // 13_437_022_686_718_487 / 1_000_000 = 13_437_022_686
        // 13_437_022_686 - 11_644_473_600 = 1_792_549_086
        assert_eq!(chromium_expiry_to_unix(13_437_022_686_718_487), 1_792_549_086.0);
    }

    #[test]
    fn chromium_samesite_integers_map_to_exact_playwright_strings() {
        // GIVEN each distinct samesite int observed in a real cookie DB (-1,0,1,2)
        // WHEN mapped
        // THEN each lands on a Playwright-valid enum variant.
        assert_eq!(chromium_samesite_to_playwright(-1), SameSite::Lax); // unspecified
        assert_eq!(chromium_samesite_to_playwright(0), SameSite::None);
        assert_eq!(chromium_samesite_to_playwright(1), SameSite::Lax);
        assert_eq!(chromium_samesite_to_playwright(2), SameSite::Strict);
    }

    #[test]
    fn samesite_serializes_to_playwright_enum_strings() {
        // Playwright rejects anything other than Strict/Lax/None — assert the
        // exact JSON spellings.
        assert_eq!(serde_json::to_string(&SameSite::Strict).unwrap(), "\"Strict\"");
        assert_eq!(serde_json::to_string(&SameSite::Lax).unwrap(), "\"Lax\"");
        assert_eq!(serde_json::to_string(&SameSite::None).unwrap(), "\"None\"");
    }

    #[test]
    fn storage_state_serializes_with_camelcase_playwright_fields() {
        // GIVEN a storage_state with one cookie
        // WHEN serialized
        // THEN field names match Playwright's camelCase schema (httpOnly, sameSite)
        // and origins is an empty array.
        let state = StorageState::from_cookies(vec![PlaywrightCookie {
            name: "session".into(),
            value: "abc".into(),
            domain: ".example.com".into(),
            path: "/".into(),
            expires: -1.0,
            http_only: true,
            secure: true,
            same_site: SameSite::Lax,
        }]);
        let json = state.to_json().unwrap();
        assert!(json.contains("\"httpOnly\": true"), "json: {json}");
        assert!(json.contains("\"sameSite\": \"Lax\""), "json: {json}");
        assert!(json.contains("\"origins\": []"), "json: {json}");
    }

    #[test]
    fn storage_state_round_trips_through_json() {
        // GIVEN a storage_state
        // WHEN serialized and parsed back
        // THEN the parsed value equals the original (schema-valid round trip).
        let original = StorageState::from_cookies(vec![
            PlaywrightCookie {
                name: "a".into(),
                value: "1".into(),
                domain: ".github.com".into(),
                path: "/".into(),
                expires: 1_792_549_086.0,
                http_only: false,
                secure: true,
                same_site: SameSite::Strict,
            },
            PlaywrightCookie {
                name: "a".into(), // same name, different host — must NOT collapse.
                value: "2".into(),
                domain: "api.github.com".into(),
                path: "/v3".into(),
                expires: -1.0,
                http_only: true,
                secure: false,
                same_site: SameSite::None,
            },
        ]);
        let json = original.to_json().unwrap();
        let parsed: StorageState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
        // Same-name cookies on different hosts are preserved as distinct entries.
        assert_eq!(parsed.cookies.len(), 2);
    }
}
