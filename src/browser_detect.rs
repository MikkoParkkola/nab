//! Browser Detection
//!
//! Automatically detects the default web browser on the system.
//! Supports macOS, Linux, and Windows.

use anyhow::Result;
use std::process::Command;

/// Detected browser type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserType {
    Brave,
    Chrome,
    Firefox,
    Safari,
    Edge,
    Dia,
}

impl BrowserType {
    /// Convert to string name used by cookie extraction
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            BrowserType::Brave => "brave",
            BrowserType::Chrome => "chrome",
            BrowserType::Firefox => "firefox",
            BrowserType::Safari => "safari",
            BrowserType::Edge => "edge",
            BrowserType::Dia => "chrome", // Dia uses Chromium cookie format
        }
    }
}

/// Detect the default web browser on the current system
pub fn detect_default_browser() -> Result<BrowserType> {
    #[cfg(target_os = "macos")]
    {
        detect_macos_default_browser()
    }

    #[cfg(target_os = "linux")]
    {
        detect_linux_default_browser()
    }

    #[cfg(target_os = "windows")]
    {
        detect_windows_default_browser()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(anyhow!("Browser detection not supported on this platform"))
    }
}

#[cfg(target_os = "macos")]
fn detect_macos_default_browser() -> Result<BrowserType> {
    // Try to get default browser from LaunchServices
    // This reads from ~/Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist
    let output = Command::new("defaults")
        .args([
            "read",
            "com.apple.LaunchServices/com.apple.launchservices.secure",
            "LSHandlers",
        ])
        .output();

    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Look for HTTP/HTTPS handler
        // The output contains LSHandlerRoleAll entries with bundle identifiers
        if stdout.contains("company.thebrowser.dia") {
            return Ok(BrowserType::Dia);
        }
        if stdout.contains("com.brave.Browser") {
            return Ok(BrowserType::Brave);
        }
        if stdout.contains("com.google.Chrome") {
            return Ok(BrowserType::Chrome);
        }
        if stdout.contains("org.mozilla.firefox") {
            return Ok(BrowserType::Firefox);
        }
        if stdout.contains("com.apple.Safari") {
            return Ok(BrowserType::Safari);
        }
        if stdout.contains("com.microsoft.edgemac") {
            return Ok(BrowserType::Edge);
        }
    }

    // Fallback: Try to detect installed browsers by checking if they exist
    fallback_detect_browser()
}

#[cfg(target_os = "linux")]
fn detect_linux_default_browser() -> Result<BrowserType> {
    // Try xdg-settings first
    let output = Command::new("xdg-settings")
        .args(&["get", "default-web-browser"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let browser = String::from_utf8_lossy(&output.stdout).to_lowercase();

            if browser.contains("brave") {
                return Ok(BrowserType::Brave);
            }
            if browser.contains("chrome") || browser.contains("chromium") {
                return Ok(BrowserType::Chrome);
            }
            if browser.contains("firefox") {
                return Ok(BrowserType::Firefox);
            }
        }
    }

    // Fallback
    fallback_detect_browser()
}

#[cfg(target_os = "windows")]
fn detect_windows_default_browser() -> Result<BrowserType> {
    // Try to read from Windows Registry
    // HKEY_CURRENT_USER\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\http\UserChoice
    let output = Command::new("reg")
        .args(&[
            "query",
            r"HKEY_CURRENT_USER\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\http\UserChoice",
            "/v",
            "ProgId",
        ])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let prog_id = String::from_utf8_lossy(&output.stdout).to_lowercase();

            if prog_id.contains("brave") {
                return Ok(BrowserType::Brave);
            }
            if prog_id.contains("chrome") {
                return Ok(BrowserType::Chrome);
            }
            if prog_id.contains("firefox") {
                return Ok(BrowserType::Firefox);
            }
            if prog_id.contains("edge") {
                return Ok(BrowserType::Edge);
            }
        }
    }

    // Fallback
    fallback_detect_browser()
}

/// Fallback detection: check if browser applications exist
fn fallback_detect_browser() -> Result<BrowserType> {
    #[cfg(target_os = "macos")]
    {
        let browsers = [
            ("/Applications/Brave Browser.app", BrowserType::Brave),
            ("/Applications/Google Chrome.app", BrowserType::Chrome),
            ("/Applications/Firefox.app", BrowserType::Firefox),
            ("/Applications/Safari.app", BrowserType::Safari),
            ("/Applications/Microsoft Edge.app", BrowserType::Edge),
        ];

        for (path, browser_type) in browsers {
            if std::path::Path::new(path).exists() {
                return Ok(browser_type);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Check common binary locations
        let browsers = [
            ("brave", BrowserType::Brave),
            ("brave-browser", BrowserType::Brave),
            ("google-chrome", BrowserType::Chrome),
            ("chromium", BrowserType::Chrome),
            ("firefox", BrowserType::Firefox),
        ];

        for (name, browser_type) in browsers {
            if Command::new("which").arg(name).output().is_ok() {
                return Ok(browser_type);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Check common install locations
        let browsers = [
            (
                r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
                BrowserType::Brave,
            ),
            (
                r"C:\Program Files (x86)\BraveSoftware\Brave-Browser\Application\brave.exe",
                BrowserType::Brave,
            ),
            (
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                BrowserType::Chrome,
            ),
            (
                r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
                BrowserType::Chrome,
            ),
            (
                r"C:\Program Files\Mozilla Firefox\firefox.exe",
                BrowserType::Firefox,
            ),
            (
                r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
                BrowserType::Firefox,
            ),
            (
                r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
                BrowserType::Edge,
            ),
        ];

        for (path, browser_type) in browsers {
            if std::path::Path::new(path).exists() {
                return Ok(browser_type);
            }
        }
    }

    // Default to Chrome as most common
    Ok(BrowserType::Chrome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_type_as_str() {
        assert_eq!(BrowserType::Brave.as_str(), "brave");
        assert_eq!(BrowserType::Chrome.as_str(), "chrome");
        assert_eq!(BrowserType::Firefox.as_str(), "firefox");
        assert_eq!(BrowserType::Safari.as_str(), "safari");
        assert_eq!(BrowserType::Edge.as_str(), "edge");
        assert_eq!(BrowserType::Dia.as_str(), "chrome"); // Dia uses Chrome cookie format
    }

    #[test]
    fn test_detect_browser() {
        // This will detect the actual default browser on the system
        // Just verify it doesn't panic
        let result = detect_default_browser();
        assert!(result.is_ok());
    }
}
