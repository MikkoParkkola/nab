//! Scan raw HTML for WAF/bot-challenge sub-resource references.
//!
//! Many modern bot-mitigation systems ship their challenge JavaScript from
//! a well-known CDN host and embed the script via a regular `<script src>`
//! tag. Rather than parsing the full DOM and running heuristics, this
//! module does a cheap regex scan for known challenge-vendor hostnames so
//! callers can decide up-front whether a page looks like a CAPTCHA wall.
//!
//! The scan is intentionally conservative and string-based so it never
//! allocates more than the input HTML and runs in microseconds on a cold
//! cache.
//!
//! # Example
//!
//! ```
//! use nab::detect::challenge_scanner::{scan_for_challenges, ChallengeVendor};
//!
//! let html = r#"<html><body><script src="https://abc.awswaf.com/foo/challenge.js"></script></body></html>"#;
//! let hits = scan_for_challenges(html);
//! assert_eq!(hits.len(), 1);
//! assert_eq!(hits[0].vendor, ChallengeVendor::AwsWaf);
//! ```

/// Which challenge vendor a detected sub-resource belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeVendor {
    /// AWS WAF (`*.awswaf.com`)
    AwsWaf,
    /// `DataDome` (`*.datadome.co`)
    DataDome,
    /// Cloudflare Turnstile / challenge platform (`challenges.cloudflare.com`)
    Cloudflare,
    /// `PerimeterX` / HUMAN (`*.perimeterx.net`, `*.px-cdn.net`, `*.px-cloud.net`)
    PerimeterX,
    /// Akamai Bot Manager (`*.akam.net`, `*.akamaihd.net` challenge paths)
    Akamai,
}

impl ChallengeVendor {
    /// Stable machine-readable identifier for structured output.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::AwsWaf => "aws_waf",
            Self::DataDome => "datadome",
            Self::Cloudflare => "cloudflare",
            Self::PerimeterX => "perimeterx",
            Self::Akamai => "akamai",
        }
    }
}

/// A single detected challenge sub-resource reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengeReference {
    pub vendor: ChallengeVendor,
    /// The matched substring (e.g. `abc.awswaf.com/xyz/challenge.js`).
    pub snippet: String,
}

/// Scan HTML for known challenge-vendor sub-resource hostnames.
///
/// Returns every distinct `(vendor, snippet)` pair found. The function is
/// case-insensitive on hostnames and never allocates more than the output
/// vector.
#[must_use]
pub fn scan_for_challenges(html: &str) -> Vec<ChallengeReference> {
    let mut hits: Vec<ChallengeReference> = Vec::new();
    let lower = html.to_ascii_lowercase();

    // (needle, vendor) pairs. Needles are lowercase and match typical
    // CDN hostnames embedded inside `<script src=...>` tags.
    // Allow: declaring this `const` inside the function keeps the data local;
    // hoisting to module scope would pollute the namespace.
    #[allow(clippy::items_after_statements)]
    const NEEDLES: &[(&str, ChallengeVendor)] = &[
        (".awswaf.com", ChallengeVendor::AwsWaf),
        (".datadome.co", ChallengeVendor::DataDome),
        ("challenges.cloudflare.com", ChallengeVendor::Cloudflare),
        (".perimeterx.net", ChallengeVendor::PerimeterX),
        (".px-cdn.net", ChallengeVendor::PerimeterX),
        (".px-cloud.net", ChallengeVendor::PerimeterX),
        ("/akam/", ChallengeVendor::Akamai),
    ];

    for (needle, vendor) in NEEDLES {
        let mut start = 0;
        while let Some(idx) = lower[start..].find(needle) {
            let abs = start + idx;
            // Extract a small window around the match for diagnostics.
            let window_start = lower[..abs]
                .rmatch_indices(['"', '\'', '<', ' '])
                .next()
                .map_or(abs.saturating_sub(16), |(i, _)| i + 1);
            let window_end = lower[abs..]
                .find(['"', '\'', '>', ' '])
                .map_or(lower.len().min(abs + needle.len() + 32), |i| abs + i);
            let snippet = html.get(window_start..window_end).unwrap_or("").to_string();

            let reference = ChallengeReference {
                vendor: *vendor,
                snippet,
            };
            // De-duplicate: keep only the first hit per (vendor, snippet).
            if !hits.iter().any(|existing| existing == &reference) {
                hits.push(reference);
            }

            start = abs + needle.len();
            if start >= lower.len() {
                break;
            }
        }
    }

    hits
}

/// Convenience: return the first detected vendor, if any.
#[must_use]
pub fn first_vendor(html: &str) -> Option<ChallengeVendor> {
    scan_for_challenges(html)
        .into_iter()
        .next()
        .map(|r| r.vendor)
}

#[cfg(test)]
mod tests {
    use super::{ChallengeVendor, first_vendor, scan_for_challenges};

    #[test]
    fn detects_aws_waf_script() {
        let html = r#"<script src="https://abc123.awswaf.com/xyz/challenge.js"></script>"#;
        let hits = scan_for_challenges(html);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].vendor, ChallengeVendor::AwsWaf);
        assert!(hits[0].snippet.contains("awswaf.com"));
    }

    #[test]
    fn detects_datadome_script() {
        let html = r"<script src='https://js.datadome.co/boot.js'></script>";
        assert_eq!(first_vendor(html), Some(ChallengeVendor::DataDome));
    }

    #[test]
    fn detects_cloudflare_turnstile_iframe() {
        let html = r#"<iframe src="https://challenges.cloudflare.com/cdn-cgi/challenge-platform/..."></iframe>"#;
        assert_eq!(first_vendor(html), Some(ChallengeVendor::Cloudflare));
    }

    #[test]
    fn detects_perimeterx_cdn() {
        let html = r#"<script src="https://client.perimeterx.net/PX12345/main.min.js"></script>"#;
        assert_eq!(first_vendor(html), Some(ChallengeVendor::PerimeterX));
    }

    #[test]
    fn ignores_clean_html() {
        let html = "<html><body><h1>Welcome</h1><p>No challenge here.</p></body></html>";
        assert!(scan_for_challenges(html).is_empty());
        assert_eq!(first_vendor(html), None);
    }

    #[test]
    fn is_case_insensitive() {
        let html = r#"<SCRIPT SRC="HTTPS://ABC.AWSWAF.COM/x.js"></SCRIPT>"#;
        assert_eq!(first_vendor(html), Some(ChallengeVendor::AwsWaf));
    }

    #[test]
    fn deduplicates_identical_snippets() {
        let html = r#"
            <script src="https://a.awswaf.com/x.js"></script>
            <script src="https://a.awswaf.com/x.js"></script>
        "#;
        let hits = scan_for_challenges(html);
        assert_eq!(hits.len(), 1, "expected dedup on identical snippets");
    }

    #[test]
    fn reports_multiple_vendors() {
        let html = r#"
            <script src="https://abc.awswaf.com/c.js"></script>
            <script src="https://js.datadome.co/b.js"></script>
        "#;
        let hits = scan_for_challenges(html);
        assert_eq!(hits.len(), 2);
        let vendors: Vec<_> = hits.iter().map(|h| h.vendor).collect();
        assert!(vendors.contains(&ChallengeVendor::AwsWaf));
        assert!(vendors.contains(&ChallengeVendor::DataDome));
    }
}
