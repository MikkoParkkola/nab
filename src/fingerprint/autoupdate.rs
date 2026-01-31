// Browser version auto-updater
// Fetches latest versions from official APIs and caches them locally

use chrono::{DateTime, Duration, Utc};
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
                    (Utc::now() - config.last_updated).num_days()
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
            return config;
        }

        // No config exists, create from defaults and try to update
        eprintln!("🔄 Initializing browser versions...");
        let config = Self::default();

        match config.fetch_and_update() {
            Ok(updated) => {
                if let Err(e) = updated.save_to_file(&config_path) {
                    eprintln!("⚠️  Failed to save initial config: {e}");
                    return config;
                }
                eprintln!("✅ Browser versions initialized");
                updated
            }
            Err(e) => {
                eprintln!("⚠️  Failed to fetch initial versions ({e}), using defaults");
                config
            }
        }
    }

    fn is_stale(&self) -> bool {
        let now = Utc::now();
        let age = now.signed_duration_since(self.last_updated);
        age > Duration::days(UPDATE_THRESHOLD_DAYS)
    }

    fn is_safari_critically_stale(&self) -> bool {
        let safari_age = Utc::now().signed_duration_since(self.safari_last_checked);
        safari_age > Duration::days(SAFARI_STALE_THRESHOLD_DAYS)
    }

    fn check_safari_staleness(&self) {
        if self.is_safari_critically_stale() {
            let days = (Utc::now() - self.safari_last_checked).num_days();
            eprintln!("⚠️  Safari versions are {days} days old (>6 months)");
            eprintln!("   Check: https://developer.apple.com/documentation/safari-release-notes");
            eprintln!("   Or edit: {:?}", Self::config_path());
        }
    }

    fn fetch_and_update(&self) -> Result<Self, Box<dyn std::error::Error>> {
        // Determine cache severity level for better observability
        let cache_age_days = (Utc::now() - self.last_updated).num_days();
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
            Err(_) => {
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

        eprintln!(
            "✅ Chrome: {} versions ({} to {})",
            versions.len(),
            versions[0].0,
            versions.last().unwrap().0
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

            match reqwest::blocking::get(url) {
                Ok(resp) => match resp.error_for_status() {
                    Ok(resp) => match resp.json::<serde_json::Value>() {
                        Ok(json) => return Ok(json),
                        Err(e) => last_error = Some(format!("JSON parse error: {e}")),
                    },
                    Err(e) => last_error = Some(format!("HTTP error: {e}")),
                },
                Err(e) => last_error = Some(format!("Network error: {e}")),
            }
        }

        Err(last_error
            .unwrap_or_else(|| "Unknown error".to_string())
            .into())
    }

    fn fetch_firefox_versions() -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let url = "https://product-details.mozilla.org/1.0/firefox_versions.json";
        let resp = Self::fetch_with_retry(url, 3)?;

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

        eprintln!(
            "✅ Firefox: {} versions ({} to {})",
            versions.len(),
            versions[0],
            versions.last().unwrap()
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
            chrome: vec![
                ("131".into(), "131.0.0.0".into()),
                ("130".into(), "130.0.0.0".into()),
                ("129".into(), "129.0.0.0".into()),
                ("128".into(), "128.0.0.0".into()),
                ("127".into(), "127.0.0.0".into()),
            ],
            firefox: vec![
                "134.0".into(),
                "133.0".into(),
                "132.0".into(),
                "131.0".into(),
            ],
            safari: vec![
                ("18.2".into(), "619.1.15".into()),
                ("18.1".into(), "619.1.15".into()),
                ("18.0".into(), "618.1.15".into()),
                ("17.6".into(), "605.1.15".into()),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn test_fetch_chrome_versions() {
        // Network test - may fail if offline
        if let Ok(versions) = BrowserVersions::fetch_chrome_versions() {
            assert!(!versions.is_empty());
            // API may return varying counts - just verify we got some valid versions
            assert!(
                versions.len() <= 20,
                "Unexpectedly many versions: {}",
                versions.len()
            );
            // Major version should be reasonably recent
            let major: u32 = versions[0].0.parse().unwrap();
            assert!(major >= 100, "Chrome version too old: {}", major);
        }
    }

    #[test]
    fn test_fetch_firefox_versions() {
        // Network test - may fail if offline
        if let Ok(versions) = BrowserVersions::fetch_firefox_versions() {
            // API may return varying counts - just verify we got some valid versions
            assert!(!versions.is_empty());
            assert!(
                versions.len() <= 20,
                "Unexpectedly many versions: {}",
                versions.len()
            );
            // Version should be reasonably recent
            let major: u32 = versions[0].split('.').next().unwrap().parse().unwrap();
            assert!(major >= 100, "Firefox version too old: {}", major);
        }
    }
}
