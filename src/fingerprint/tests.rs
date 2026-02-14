//! Additional unit tests for fingerprint module

#[cfg(test)]
mod tests {
    use super::super::*;

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
