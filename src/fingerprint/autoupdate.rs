// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

// Browser version auto-updater
// Fetches latest versions from official APIs and caches them locally

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const UPDATE_THRESHOLD_DAYS: i64 = 14; // Chrome releases every 4 weeks, check every 2 weeks
const SAFARI_STALE_THRESHOLD_DAYS: i64 = 180; // Safari updates quarterly

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BrowserVersions {
    pub last_updated: DateTime<Utc>,
    pub safari_last_checked: DateTime<Utc>,
    pub chrome: Vec<(String, String)>,
    pub firefox: Vec<String>,
    pub safari: Vec<(String, String)>,
}

impl BrowserVersions {
    /// Load versions from cache or fetch updates if stale
    #[must_use]
    pub fn load_or_update() -> Self {
        let config_path = Self::config_path();

        // Try to load existing config
        if let Ok(config) = Self::load_from_file(&config_path) {
            // Check if stale (>14 days old to match Chrome release cycle)
            if config.is_stale() {
                eprintln!(
                    "🔄 Browser versions outdated ({} days old), updating...",
                    config.cache_age_days()
                );

                match config.fetch_and_update() {
                    Ok(updated) => {
                        if let Err(e) = updated.save_to_file(&config_path) {
                            eprintln!("⚠️  Failed to save updates: {e}");
                        }
                        updated.check_safari_staleness();
                        return updated;
                    }
                    Err(e) => {
                        eprintln!("⚠️  Update failed ({e}), using cached versions");
                        config.check_safari_staleness();
                    }
                }
            }
            config.check_safari_staleness();
            return config;
        }

        // No config exists, create from defaults and try to update
        eprintln!("🔄 Initializing browser versions...");
        let config = Self::default();

        match config.fetch_and_update() {
            Ok(updated) => {
                if let Err(e) = updated.save_to_file(&config_path) {
                    eprintln!("⚠️  Failed to save initial config: {e}");
                    config.check_safari_staleness();
                    return config;
                }
                eprintln!("✅ Browser versions initialized");
                updated.check_safari_staleness();
                updated
            }
            Err(e) => {
                eprintln!("⚠️  Failed to fetch initial versions ({e}), using defaults");
                config.check_safari_staleness();
                config
            }
        }
    }

    fn cache_age_days(&self) -> i64 {
        Utc::now()
            .signed_duration_since(self.last_updated)
            .num_days()
    }

    fn safari_age_days(&self) -> i64 {
        Utc::now()
            .signed_duration_since(self.safari_last_checked)
            .num_days()
    }

    fn is_stale(&self) -> bool {
        self.cache_age_days() > UPDATE_THRESHOLD_DAYS
    }

    fn is_safari_critically_stale(&self) -> bool {
        self.safari_age_days() > SAFARI_STALE_THRESHOLD_DAYS
    }

    fn safari_staleness_notice(&self) -> Option<String> {
        self.is_safari_critically_stale().then(|| {
            format!(
                "⚠️  Safari versions are {} days old (>6 months)",
                self.safari_age_days()
            )
        })
    }

    fn check_safari_staleness(&self) {
        if let Some(notice) = self.safari_staleness_notice() {
            eprintln!("{notice}");
            eprintln!("   Check: https://developer.apple.com/documentation/safari-release-notes");
            eprintln!("   Or edit: {}", Self::config_path().display());
        }
    }

    #[allow(clippy::unnecessary_wraps)] // Result used for ? operator on inner calls
    fn fetch_and_update(&self) -> Result<Self, Box<dyn std::error::Error>> {
        // Determine cache severity level for better observability
        let cache_age_days = self.cache_age_days();
        let severity = if cache_age_days > 60 {
            ("🔴 ERROR", "CRITICAL") // >2 months = critical
        } else if cache_age_days > 14 {
            ("⚠️  WARN", "Degraded") // >2 weeks = degraded
        } else {
            ("ℹ️  INFO", "Normal")
        };

        // Fetch Chrome and Firefox (auto-update)
        let chrome = Self::fetch_chrome_versions().unwrap_or_else(|e| {
            eprintln!(
                "{} Chrome update failed ({e}), using {}-day-old cache",
                severity.0, cache_age_days
            );
            self.chrome.clone()
        });

        let firefox = Self::fetch_firefox_versions().unwrap_or_else(|e| {
            eprintln!(
                "{} Firefox update failed ({e}), using {}-day-old cache",
                severity.0, cache_age_days
            );
            self.firefox.clone()
        });

        // Safari: Try community list, fall back to cached
        let (safari, safari_updated) = match Self::fetch_safari_from_community() {
            Ok(versions) => {
                eprintln!("✅ Safari: Updated from community list");
                (versions, Utc::now())
            }
            Err(e) => {
                if self.is_safari_critically_stale() {
                    eprintln!(
                        "{} Safari update failed ({e}), using {}-day-old cache",
                        severity.0,
                        self.safari_age_days()
                    );
                }
                // Keep existing Safari versions and timestamp
                (self.safari.clone(), self.safari_last_checked)
            }
        };

        Ok(BrowserVersions {
            last_updated: Utc::now(),
            safari_last_checked: safari_updated,
            chrome,
            firefox,
            safari,
        })
    }

    fn fetch_chrome_versions() -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
        // Google's official Chrome version API - use "all" platforms for better coverage
        // macOS-only endpoint returns only 2 versions; all-platforms gives 8-10
        let url = "https://versionhistory.googleapis.com/v1/chrome/platforms/all/channels/stable/versions";

        let resp: serde_json::Value = Self::fetch_with_retry(url, 3)?;
        Self::parse_chrome_versions_response(&resp)
    }

    fn parse_chrome_versions_response(
        resp: &serde_json::Value,
    ) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
        let mut versions = Vec::new();
        if let Some(versions_array) = resp["versions"].as_array() {
            for ver in versions_array {
                if let Some(full) = ver["version"].as_str() {
                    let major = full.split('.').next().unwrap_or("0");
                    // Store full patch version for better authenticity
                    versions.push((major.to_string(), full.to_string()));
                }
            }
        } else {
            return Err("No 'versions' array in API response".into());
        }

        // Deduplicate by major version and keep latest 8 for better rotation diversity
        versions.sort_by(|a, b| {
            b.0.parse::<u32>()
                .unwrap_or(0)
                .cmp(&a.0.parse::<u32>().unwrap_or(0))
        });
        versions.dedup_by(|a, b| a.0 == b.0);
        versions.truncate(8);

        if versions.is_empty() {
            return Err("No Chrome versions found".into());
        }

        // SAFETY: versions.is_empty() was checked above; last() is always Some.
        eprintln!(
            "✅ Chrome: {} versions ({} to {})",
            versions.len(),
            versions[0].0,
            versions
                .last()
                .expect("non-empty versions list has a last element")
                .0
        );
        Ok(versions)
    }

    /// Fetch URL with retry logic (exponential backoff: 50ms, 200ms, 800ms)
    fn fetch_with_retry(
        url: &str,
        max_retries: u32,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let mut last_error = None;

        for attempt in 0..max_retries {
            if attempt > 0 {
                let delay_ms = 50 * (4_u64.pow(attempt - 1)); // 50, 200, 800ms
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }

            // `reqwest::blocking` owns an internal Tokio runtime. Browser
            // profiles are lazily initialized from async request paths, and
            // dropping that internal runtime on a Tokio worker panics. Keep
            // the complete blocking request lifecycle on a plain OS thread.
            let url = url.to_owned();
            match std::thread::spawn(move || Self::fetch_json_once(&url)).join() {
                Ok(Ok(json)) => return Ok(json),
                Ok(Err(error)) => last_error = Some(error),
                Err(_) => last_error = Some("Version-fetch worker panicked".to_string()),
            }
        }

        Err(last_error
            .unwrap_or_else(|| "Unknown error".to_string())
            .into())
    }

    fn fetch_json_once(url: &str) -> Result<serde_json::Value, String> {
        let response = reqwest::blocking::get(url).map_err(|e| format!("Network error: {e}"))?;
        let response = response
            .error_for_status()
            .map_err(|e| format!("HTTP error: {e}"))?;
        response
            .json::<serde_json::Value>()
            .map_err(|e| format!("JSON parse error: {e}"))
    }

    fn fetch_firefox_versions() -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let url = "https://product-details.mozilla.org/1.0/firefox_versions.json";
        let resp = Self::fetch_with_retry(url, 3)?;
        Self::parse_firefox_versions_response(&resp)
    }

    fn parse_firefox_versions_response(
        resp: &serde_json::Value,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let latest = resp["LATEST_FIREFOX_VERSION"]
            .as_str()
            .ok_or("Missing LATEST_FIREFOX_VERSION")?
            .split('.')
            .next()
            .ok_or("Invalid version format")?
            .parse::<u32>()?;

        // Generate last 6 versions for better rotation diversity
        let versions: Vec<String> = (0..6)
            .map(|i| format!("{}.0", latest.saturating_sub(i)))
            .collect();

        // SAFETY: versions always has exactly 6 elements (range 0..6 is never empty).
        eprintln!(
            "✅ Firefox: {} versions ({} to {})",
            versions.len(),
            versions[0],
            versions
                .last()
                .expect("6-element versions list has a last element")
        );
        Ok(versions)
    }

    fn fetch_safari_from_community() -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
        // Future: Implement community-maintained list
        // For now, return error to use cached versions
        Err("Community list not yet implemented".into())
    }

    fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("nab")
            .join("versions.json")
    }

    fn load_from_file(path: &PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: BrowserVersions = serde_json::from_str(&content)?;
        Ok(config)
    }

    fn save_to_file(&self, path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

impl Default for BrowserVersions {
    fn default() -> Self {
        let now = Utc::now();
        BrowserVersions {
            last_updated: now,
            safari_last_checked: now,
            // Bundled fallback used only when offline / before the first
            // successful API refresh. Kept fresh automatically by the
            // `bump-fingerprint` CI workflow (see .github/workflows/) so the
            // defaults never drift far from the live stable channel between
            // releases. Last refreshed: 2026-06-02.
            chrome: vec![
                ("153".into(), "153.0.8010.18".into()),
                ("152".into(), "152.0.7977.64".into()),
                ("151".into(), "151.0.7922.175".into()),
                ("150".into(), "150.0.7871.189".into()),
                ("149".into(), "149.0.7827.201".into()),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Floor for the bundled-default Chrome major version.
    ///
    /// The runtime auto-updater refreshes versions from Google's API, but when
    /// nab runs fully offline (or before the first successful fetch) the
    /// `Default` impl is the only fingerprint source. A bundled major that lags
    /// the real stable channel by many releases is a detection signal, so this
    /// floor guards against the defaults silently rotting between manual bumps.
    ///
    /// Keep this in lockstep with the `bump-fingerprint` CI workflow, which
    /// refreshes the defaults and is the mechanism that keeps this test passing
    /// without hand edits.
    const MIN_BUNDLED_CHROME_MAJOR: u32 = 140;
    const MIN_BUNDLED_FIREFOX_MAJOR: u32 = 140;

    fn highest_major<'a>(versions: impl IntoIterator<Item = &'a str>) -> u32 {
        versions
            .into_iter()
            .filter_map(|v| v.split('.').next())
            .filter_map(|m| m.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn bundled_chrome_default_is_not_stale() {
        let defaults = BrowserVersions::default();
        let newest = highest_major(defaults.chrome.iter().map(|(major, _)| major.as_str()));
        assert!(
            newest >= MIN_BUNDLED_CHROME_MAJOR,
            "bundled Chrome default major {newest} is below floor {MIN_BUNDLED_CHROME_MAJOR}; \
             refresh src/fingerprint/autoupdate.rs Default impl (the bump-fingerprint workflow does this automatically)"
        );
    }

    #[test]
    fn bundled_firefox_default_is_not_stale() {
        let defaults = BrowserVersions::default();
        let newest = highest_major(defaults.firefox.iter().map(String::as_str));
        assert!(
            newest >= MIN_BUNDLED_FIREFOX_MAJOR,
            "bundled Firefox default major {newest} is below floor {MIN_BUNDLED_FIREFOX_MAJOR}; \
             refresh src/fingerprint/autoupdate.rs Default impl"
        );
    }

    #[test]
    fn bundled_defaults_are_internally_consistent() {
        let defaults = BrowserVersions::default();
        // Chrome: every full version string starts with its declared major.
        for (major, full) in &defaults.chrome {
            assert!(
                full.starts_with(major),
                "Chrome full version {full} should start with major {major}"
            );
        }
        // Chrome majors are strictly descending (newest first, no duplicates).
        let majors: Vec<u32> = defaults
            .chrome
            .iter()
            .filter_map(|(m, _)| m.parse::<u32>().ok())
            .collect();
        assert_eq!(majors.len(), defaults.chrome.len(), "all majors parse");
        assert!(
            majors.windows(2).all(|w| w[0] > w[1]),
            "Chrome majors must be strictly descending and unique: {majors:?}"
        );
    }

    #[test]
    fn test_staleness() {
        let old = BrowserVersions {
            last_updated: Utc::now() - Duration::days(31),
            safari_last_checked: Utc::now(),
            ..Default::default()
        };
        assert!(old.is_stale());

        let fresh = BrowserVersions::default();
        assert!(!fresh.is_stale());
    }

    #[test]
    fn test_safari_staleness() {
        let old_safari = BrowserVersions {
            last_updated: Utc::now(),
            safari_last_checked: Utc::now() - Duration::days(185),
            ..Default::default()
        };
        assert!(old_safari.is_safari_critically_stale());
        assert_eq!(
            old_safari.safari_staleness_notice(),
            Some("⚠️  Safari versions are 185 days old (>6 months)".to_string())
        );

        let fresh_safari = BrowserVersions::default();
        assert_eq!(fresh_safari.safari_staleness_notice(), None);
    }

    #[test]
    fn test_fetch_chrome_versions() {
        let response = serde_json::json!({
            "versions": [
                {"version": "129.0.6668.59"},
                {"version": "131.0.6778.70"},
                {"version": "130.0.6723.58"},
                {"version": "131.0.6778.69"},
                {"version": "128.0.6613.84"},
                {"version": "127.0.6533.100"},
                {"version": "126.0.6478.126"},
                {"version": "125.0.6422.141"},
                {"version": "124.0.6367.207"},
                {"version": "123.0.6312.122"}
            ]
        });

        let versions = BrowserVersions::parse_chrome_versions_response(&response).unwrap();
        assert_eq!(versions.len(), 8, "Should keep latest 8 distinct majors");
        assert_eq!(versions[0], ("131".into(), "131.0.6778.70".into()));
        assert_eq!(versions[1], ("130".into(), "130.0.6723.58".into()));
        assert_eq!(versions[2], ("129".into(), "129.0.6668.59".into()));
        assert_eq!(
            versions.last().unwrap(),
            &("124".into(), "124.0.6367.207".into())
        );
    }

    #[test]
    fn test_fetch_firefox_versions() {
        let response = serde_json::json!({
            "LATEST_FIREFOX_VERSION": "136.0.1"
        });

        let versions = BrowserVersions::parse_firefox_versions_response(&response).unwrap();
        assert_eq!(
            versions,
            vec!["136.0", "135.0", "134.0", "133.0", "132.0", "131.0"]
        );
    }

    #[test]
    fn blocking_version_fetch_is_safe_inside_tokio_runtime() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP server");
        let address = listener.local_addr().expect("read local HTTP address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept version request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read version request");
            let body = r#"{"versions":[]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("write version response");
        });

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build Tokio runtime");
        let response = runtime
            .block_on(async { BrowserVersions::fetch_with_retry(&format!("http://{address}"), 1) });

        assert_eq!(
            response.expect("fetch JSON")["versions"],
            serde_json::json!([])
        );
        server.join().expect("local HTTP server should finish");
    }

    #[test]
    #[ignore = "requires external network access"]
    fn test_fetch_chrome_versions_live() {
        let versions = BrowserVersions::fetch_chrome_versions().unwrap();
        assert!(!versions.is_empty());
        let major: u32 = versions[0].0.parse().unwrap();
        assert!(major >= 100, "Chrome version too old: {major}");
    }

    #[test]
    #[ignore = "requires external network access"]
    fn test_fetch_firefox_versions_live() {
        let versions = BrowserVersions::fetch_firefox_versions().unwrap();
        assert!(!versions.is_empty());
        let major: u32 = versions[0].split('.').next().unwrap().parse().unwrap();
        assert!(major >= 100, "Firefox version too old: {major}");
    }
}
