//! Browser Fingerprint Spoofing
//!
//! Generates realistic browser fingerprints to avoid detection.
//! Based on real browser statistics and anti-fingerprinting research.

pub mod autoupdate;

use rand::seq::SliceRandom;
use rand::Rng;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE, USER_AGENT,
};

// Load versions once on first use (auto-updates if stale)
static BROWSER_VERSIONS: std::sync::LazyLock<autoupdate::BrowserVersions> =
    std::sync::LazyLock::new(autoupdate::BrowserVersions::load_or_update);

/// Browser profile with realistic fingerprint
#[derive(Debug, Clone)]
pub struct BrowserProfile {
    pub user_agent: String,
    pub accept: String,
    pub accept_language: String,
    pub accept_encoding: String,
    pub sec_ch_ua: String,
    pub sec_ch_ua_mobile: String,
    pub sec_ch_ua_platform: String,
    pub sec_fetch_dest: String,
    pub sec_fetch_mode: String,
    pub sec_fetch_site: String,
    pub sec_fetch_user: String,
}

// Browser versions now loaded dynamically via BROWSER_VERSIONS lazy static
// Auto-updates from official APIs when >30 days old

/// Platform configurations
#[derive(Debug, Clone, Copy)]
pub enum Platform {
    MacOS,
    Windows,
    Linux,
}

impl Platform {
    fn random() -> Self {
        let mut rng = rand::thread_rng();
        // Realistic distribution: Windows 65%, macOS 20%, Linux 15%
        let roll: f32 = rng.gen();
        if roll < 0.65 {
            Platform::Windows
        } else if roll < 0.85 {
            Platform::MacOS
        } else {
            Platform::Linux
        }
    }

    fn os_string(&self) -> &'static str {
        match self {
            Platform::MacOS => "Macintosh; Intel Mac OS X 10_15_7",
            Platform::Windows => "Windows NT 10.0; Win64; x64",
            Platform::Linux => "X11; Linux x86_64",
        }
    }

    fn sec_ch_platform(&self) -> &'static str {
        match self {
            Platform::MacOS => "\"macOS\"",
            Platform::Windows => "\"Windows\"",
            Platform::Linux => "\"Linux\"",
        }
    }
}

/// Generate a realistic Chrome browser profile
#[must_use]
pub fn chrome_profile() -> BrowserProfile {
    let mut rng = rand::thread_rng();
    let platform = Platform::random();
    let (major, full) = BROWSER_VERSIONS
        .chrome
        .choose(&mut rng)
        .expect("Chrome versions list should not be empty");

    let user_agent = format!(
        "Mozilla/5.0 ({}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{} Safari/537.36",
        platform.os_string(),
        full
    );

    // Realistic Sec-CH-UA with brand ordering variation
    let brands = [
        format!("\"Google Chrome\";v=\"{major}\""),
        format!("\"Chromium\";v=\"{major}\""),
        "\"Not_A Brand\";v=\"24\"".to_string(),
    ];

    BrowserProfile {
        user_agent,
        accept: "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7".to_string(),
        accept_language: random_accept_language(),
        accept_encoding: "gzip, deflate, br, zstd".to_string(),
        sec_ch_ua: brands.join(", "),
        sec_ch_ua_mobile: "?0".to_string(),
        sec_ch_ua_platform: platform.sec_ch_platform().to_string(),
        sec_fetch_dest: "document".to_string(),
        sec_fetch_mode: "navigate".to_string(),
        sec_fetch_site: "none".to_string(),
        sec_fetch_user: "?1".to_string(),
    }
}

/// Generate a realistic Firefox browser profile
#[must_use]
pub fn firefox_profile() -> BrowserProfile {
    let mut rng = rand::thread_rng();
    let platform = Platform::random();
    let version = BROWSER_VERSIONS
        .firefox
        .choose(&mut rng)
        .expect("Firefox versions list should not be empty");

    let user_agent = format!(
        "Mozilla/5.0 ({}; rv:{}) Gecko/20100101 Firefox/{}",
        platform.os_string(),
        version,
        version
    );

    BrowserProfile {
        user_agent,
        accept:
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"
                .to_string(),
        accept_language: random_accept_language(),
        accept_encoding: "gzip, deflate, br, zstd".to_string(),
        // Firefox doesn't send Sec-CH-UA headers
        sec_ch_ua: String::new(),
        sec_ch_ua_mobile: String::new(),
        sec_ch_ua_platform: String::new(),
        sec_fetch_dest: "document".to_string(),
        sec_fetch_mode: "navigate".to_string(),
        sec_fetch_site: "none".to_string(),
        sec_fetch_user: "?1".to_string(),
    }
}

/// Generate a realistic Safari browser profile
#[must_use]
pub fn safari_profile() -> BrowserProfile {
    let mut rng = rand::thread_rng();
    let (version, webkit) = BROWSER_VERSIONS
        .safari
        .choose(&mut rng)
        .expect("Safari versions list should not be empty");

    // Safari only runs on macOS/iOS - always use macOS for desktop
    let user_agent = format!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/{webkit} (KHTML, like Gecko) Version/{version} Safari/{webkit}"
    );

    BrowserProfile {
        user_agent,
        accept: "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string(),
        accept_language: random_accept_language(),
        accept_encoding: "gzip, deflate, br".to_string(), // Safari doesn't support zstd yet
        // Safari doesn't send Sec-CH-UA headers
        sec_ch_ua: String::new(),
        sec_ch_ua_mobile: String::new(),
        sec_ch_ua_platform: String::new(),
        sec_fetch_dest: "document".to_string(),
        sec_fetch_mode: "navigate".to_string(),
        sec_fetch_site: "none".to_string(),
        sec_fetch_user: "?1".to_string(),
    }
}

/// Generate a random browser profile (weighted by market share)
#[must_use]
pub fn random_profile() -> BrowserProfile {
    let mut rng = rand::thread_rng();
    // Realistic distribution: Chrome 65%, Safari 20%, Firefox 10%, Edge 5%
    let roll: f32 = rng.gen();
    if roll < 0.65 {
        chrome_profile()
    } else if roll < 0.85 {
        safari_profile()
    } else {
        firefox_profile()
    }
}

/// Generate random Accept-Language header
fn random_accept_language() -> String {
    let mut rng = rand::thread_rng();
    let languages = [
        "en-US,en;q=0.9",
        "en-GB,en;q=0.9",
        "en-US,en;q=0.9,de;q=0.8",
        "en-US,en;q=0.9,fr;q=0.8",
        "en-US,en;q=0.9,es;q=0.8",
        "en-US,en;q=0.9,ja;q=0.8",
        "fi-FI,fi;q=0.9,en;q=0.8",
    ];
    languages
        .choose(&mut rng)
        .expect("Languages list should not be empty")
        .to_string()
}

impl BrowserProfile {
    /// Convert profile to reqwest `HeaderMap`
    pub fn to_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();

        // These header values are generated from known-good static strings or controlled values,
        // so unwrap is safe here. In production code, these should never fail.
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&self.user_agent)
                .expect("User agent should be valid header value"),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_str(&self.accept).expect("Accept should be valid header value"),
        );
        headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_str(&self.accept_language)
                .expect("Accept-Language should be valid header value"),
        );
        headers.insert(
            ACCEPT_ENCODING,
            HeaderValue::from_str(&self.accept_encoding)
                .expect("Accept-Encoding should be valid header value"),
        );

        // Add Sec-CH-UA headers for Chrome
        if !self.sec_ch_ua.is_empty() {
            headers.insert(
                "Sec-CH-UA",
                HeaderValue::from_str(&self.sec_ch_ua)
                    .expect("Sec-CH-UA should be valid header value"),
            );
            headers.insert(
                "Sec-CH-UA-Mobile",
                HeaderValue::from_str(&self.sec_ch_ua_mobile)
                    .expect("Sec-CH-UA-Mobile should be valid header value"),
            );
            headers.insert(
                "Sec-CH-UA-Platform",
                HeaderValue::from_str(&self.sec_ch_ua_platform)
                    .expect("Sec-CH-UA-Platform should be valid header value"),
            );
        }

        // Sec-Fetch headers (all modern browsers)
        headers.insert(
            "Sec-Fetch-Dest",
            HeaderValue::from_str(&self.sec_fetch_dest)
                .expect("Sec-Fetch-Dest should be valid header value"),
        );
        headers.insert(
            "Sec-Fetch-Mode",
            HeaderValue::from_str(&self.sec_fetch_mode)
                .expect("Sec-Fetch-Mode should be valid header value"),
        );
        headers.insert(
            "Sec-Fetch-Site",
            HeaderValue::from_str(&self.sec_fetch_site)
                .expect("Sec-Fetch-Site should be valid header value"),
        );
        headers.insert(
            "Sec-Fetch-User",
            HeaderValue::from_str(&self.sec_fetch_user)
                .expect("Sec-Fetch-User should be valid header value"),
        );

        // Additional headers that real browsers send
        headers.insert("Upgrade-Insecure-Requests", HeaderValue::from_static("1"));
        headers.insert("Cache-Control", HeaderValue::from_static("max-age=0"));

        headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chrome_profile() {
        let profile = chrome_profile();
        assert!(profile.user_agent.contains("Chrome"));
        assert!(!profile.sec_ch_ua.is_empty());
    }

    #[test]
    fn test_firefox_profile() {
        let profile = firefox_profile();
        assert!(profile.user_agent.contains("Firefox"));
        assert!(profile.sec_ch_ua.is_empty()); // Firefox doesn't send these
    }

    #[test]
    fn test_safari_profile() {
        let profile = safari_profile();
        assert!(profile.user_agent.contains("Safari"));
        assert!(profile.user_agent.contains("Macintosh"));
    }

    #[test]
    fn test_headers_conversion() {
        let profile = random_profile();
        let headers = profile.to_headers();
        assert!(headers.contains_key(USER_AGENT));
        assert!(headers.contains_key(ACCEPT));
    }

    #[test]
    fn test_browser_versions_not_empty() {
        let versions = &*BROWSER_VERSIONS;
        assert!(!versions.chrome.is_empty(), "Chrome versions should not be empty");
        assert!(!versions.firefox.is_empty(), "Firefox versions should not be empty");
        assert!(!versions.safari.is_empty(), "Safari versions should not be empty");
    }

    #[test]
    fn test_chrome_versions_format() {
        let versions = &*BROWSER_VERSIONS;
        for (major, full) in &versions.chrome {
            assert!(!major.is_empty(), "Major version should not be empty");
            assert!(!full.is_empty(), "Full version should not be empty");
            assert!(full.starts_with(major), "Full version should start with major");
        }
    }

    #[test]
    fn test_random_profile_deterministic_structure() {
        let profile = random_profile();
        assert!(!profile.user_agent.is_empty());
        assert!(!profile.accept.is_empty());
        assert!(!profile.accept_language.is_empty());
        assert!(!profile.accept_encoding.is_empty());
    }

    #[test]
    fn test_profile_to_headers_includes_required() {
        let profile = chrome_profile();
        let headers = profile.to_headers();

        assert!(headers.contains_key("user-agent"));
        assert!(headers.contains_key("accept"));
        assert!(headers.contains_key("accept-language"));
        assert!(headers.contains_key("accept-encoding"));
    }

    #[test]
    fn test_firefox_no_sec_ch_ua() {
        let profile = firefox_profile();
        assert!(profile.sec_ch_ua.is_empty());
        assert!(profile.sec_ch_ua_mobile.is_empty());
        assert!(profile.sec_ch_ua_platform.is_empty());
    }

    #[test]
    fn test_safari_only_macos() {
        let profile = safari_profile();
        assert!(profile.user_agent.contains("Macintosh"));
        assert!(profile.user_agent.contains("Safari"));
    }

    #[test]
    fn test_platform_os_string_not_empty() {
        assert!(!Platform::MacOS.os_string().is_empty());
        assert!(!Platform::Windows.os_string().is_empty());
        assert!(!Platform::Linux.os_string().is_empty());
    }
}
