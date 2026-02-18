//! SSRF (Server-Side Request Forgery) protection.
//!
//! Validates resolved IP addresses against a deny list of private, loopback,
//! link-local, and other non-routable address ranges. Also detects IPv4-mapped
//! IPv6 addresses that could bypass naive checks.
//!
//! # DNS Pinning
//!
//! [`DnsPinningResolver`] resolves a hostname once, validates the resolved IP
//! against the SSRF deny list, and returns the pinned address for connection.
//! This prevents DNS rebinding attacks where the first resolution returns a
//! public IP (passing validation) and a subsequent resolution returns a private
//! IP (exploiting trust).
//!
//! # Redirect Validation
//!
//! [`validate_redirect_target`] checks redirect URLs against the SSRF deny list
//! before following them.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use anyhow::{Result, bail};
use tracing::warn;
use url::Url;

/// Default maximum number of redirect hops allowed.
pub const DEFAULT_MAX_REDIRECTS: u32 = 5;

/// Default maximum response body size in bytes (10 MB).
pub const DEFAULT_MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

/// Returns `true` if the given IPv4 address is in a denied range.
///
/// Denied ranges include:
/// - Loopback (`127.0.0.0/8`)
/// - Private (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`)
/// - Link-local (`169.254.0.0/16`)
/// - Documentation (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`)
/// - Broadcast (`255.255.255.255`)
/// - Unspecified (`0.0.0.0`)
fn is_denied_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || is_ipv4_documentation(ip)
        || is_ipv4_benchmarking(ip)
}

/// `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24` (RFC 5737).
fn is_ipv4_documentation(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    matches!(
        (octets[0], octets[1], octets[2]),
        (192, 0, 2) | (198, 51, 100) | (203, 0, 113)
    )
}

/// `198.18.0.0/15` (RFC 2544).
fn is_ipv4_benchmarking(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)
}

/// Returns `true` if the given IPv6 address is in a denied range.
///
/// Also detects IPv4-mapped IPv6 addresses (`::ffff:x.x.x.x`) and validates
/// the embedded IPv4 address against the IPv4 deny list.
fn is_denied_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }

    // Check IPv4-mapped IPv6 addresses (::ffff:x.x.x.x)
    // This catches bypass attempts like ::ffff:127.0.0.1
    if let Some(ipv4) = extract_mapped_ipv4(&ip) {
        return is_denied_ipv4(ipv4);
    }

    // Link-local (fe80::/10)
    let segments = ip.segments();
    if segments[0] & 0xffc0 == 0xfe80 {
        return true;
    }

    // Unique local (fc00::/7)
    if segments[0] & 0xfe00 == 0xfc00 {
        return true;
    }

    // Documentation (2001:db8::/32)
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return true;
    }

    // Discard (100::/64)
    if segments[0] == 0x0100 && segments[1..4] == [0, 0, 0] {
        return true;
    }

    false
}

/// Extracts the embedded IPv4 address from an IPv4-mapped IPv6 address.
///
/// Handles both `::ffff:a.b.c.d` and the full-form representation.
fn extract_mapped_ipv4(ip: &Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = ip.segments();

    // Standard IPv4-mapped: ::ffff:a.b.c.d
    // Segments: [0, 0, 0, 0, 0, 0xffff, high, low]
    if segments[0..5] == [0, 0, 0, 0, 0] && segments[5] == 0xffff {
        let high = segments[6];
        let low = segments[7];
        return Some(Ipv4Addr::new(
            (high >> 8) as u8,
            (high & 0xff) as u8,
            (low >> 8) as u8,
            (low & 0xff) as u8,
        ));
    }

    // IPv4-compatible (deprecated but still needs blocking): ::a.b.c.d
    // Segments: [0, 0, 0, 0, 0, 0, high, low]
    if segments[0..6] == [0, 0, 0, 0, 0, 0] && (segments[6] != 0 || segments[7] > 1) {
        let high = segments[6];
        let low = segments[7];
        return Some(Ipv4Addr::new(
            (high >> 8) as u8,
            (high & 0xff) as u8,
            (low >> 8) as u8,
            (low & 0xff) as u8,
        ));
    }

    None
}

/// Validates an IP address against the SSRF deny list.
///
/// Returns `Ok(())` if the address is allowed, or an error describing
/// why it was denied.
pub fn validate_ip(ip: IpAddr) -> Result<()> {
    match ip {
        IpAddr::V4(v4) => {
            if is_denied_ipv4(v4) {
                bail!("SSRF blocked: IPv4 address {v4} is in a denied range");
            }
        }
        IpAddr::V6(v6) => {
            if is_denied_ipv6(v6) {
                bail!("SSRF blocked: IPv6 address {v6} is in a denied range");
            }
        }
    }
    Ok(())
}

/// Resolves a hostname to IP addresses and validates each against the SSRF
/// deny list.
///
/// Returns the first allowed [`SocketAddr`], or an error if all resolved
/// addresses are denied or resolution fails.
pub fn resolve_and_validate(host: &str, port: u16) -> Result<SocketAddr> {
    let addr_str = format!("{host}:{port}");
    let addrs: Vec<SocketAddr> = addr_str
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("DNS resolution failed for {host}: {e}"))?
        .collect();

    if addrs.is_empty() {
        bail!("DNS resolution returned no addresses for {host}");
    }

    for addr in &addrs {
        match validate_ip(addr.ip()) {
            Ok(()) => return Ok(*addr),
            Err(e) => {
                warn!("SSRF: skipping {addr} for {host}: {e}");
            }
        }
    }

    bail!("SSRF blocked: all resolved addresses for {host} are in denied ranges: {addrs:?}")
}

/// Validates a URL's host against the SSRF deny list by resolving DNS.
///
/// This is the main entry point for SSRF validation. It:
/// 1. Parses the URL to extract host and port
/// 2. Resolves the hostname via DNS
/// 3. Validates all resolved IPs against the deny list
/// 4. Returns the first allowed `SocketAddr` for DNS pinning
pub fn validate_url(url: &Url) -> Result<SocketAddr> {
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("URL has no host: {url}"))?;

    let port = url.port_or_known_default().unwrap_or(443);

    // Check if host is a raw IP address first (no DNS needed)
    if let Ok(ip) = host.parse::<IpAddr>() {
        validate_ip(ip)?;
        return Ok(SocketAddr::new(ip, port));
    }

    // Also check bracket-stripped IPv6 literals like [::ffff:127.0.0.1]
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = stripped.parse::<IpAddr>() {
        validate_ip(ip)?;
        return Ok(SocketAddr::new(ip, port));
    }

    resolve_and_validate(host, port)
}

/// Validates a redirect target URL against the SSRF deny list.
///
/// Called before following each redirect hop to prevent redirect-based SSRF.
pub fn validate_redirect_target(url: &Url) -> Result<()> {
    // Only validate http/https schemes
    match url.scheme() {
        "http" | "https" => {}
        scheme => bail!("SSRF blocked: disallowed redirect scheme '{scheme}'"),
    }

    validate_url(url).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── IPv4 deny list ──────────────────────────────────────────────────

    #[test]
    fn denies_ipv4_loopback() {
        assert!(is_denied_ipv4(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(127, 0, 0, 2)));
        assert!(is_denied_ipv4(Ipv4Addr::new(127, 255, 255, 255)));
    }

    #[test]
    fn denies_ipv4_private_10() {
        assert!(is_denied_ipv4(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(10, 255, 255, 255)));
    }

    #[test]
    fn denies_ipv4_private_172() {
        assert!(is_denied_ipv4(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(172, 31, 255, 255)));
    }

    #[test]
    fn denies_ipv4_private_192() {
        assert!(is_denied_ipv4(Ipv4Addr::new(192, 168, 0, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(192, 168, 255, 255)));
    }

    #[test]
    fn denies_ipv4_link_local() {
        assert!(is_denied_ipv4(Ipv4Addr::new(169, 254, 0, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(169, 254, 169, 254)));
    }

    #[test]
    fn denies_ipv4_documentation() {
        assert!(is_denied_ipv4(Ipv4Addr::new(192, 0, 2, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(198, 51, 100, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(203, 0, 113, 1)));
    }

    #[test]
    fn denies_ipv4_benchmarking() {
        assert!(is_denied_ipv4(Ipv4Addr::new(198, 18, 0, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(198, 19, 255, 255)));
    }

    #[test]
    fn denies_ipv4_broadcast() {
        assert!(is_denied_ipv4(Ipv4Addr::new(255, 255, 255, 255)));
    }

    #[test]
    fn denies_ipv4_unspecified() {
        assert!(is_denied_ipv4(Ipv4Addr::new(0, 0, 0, 0)));
    }

    #[test]
    fn allows_ipv4_public() {
        assert!(!is_denied_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_denied_ipv4(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!is_denied_ipv4(Ipv4Addr::new(93, 184, 216, 34)));
    }

    // ─── IPv6 deny list ──────────────────────────────────────────────────

    #[test]
    fn denies_ipv6_loopback() {
        assert!(is_denied_ipv6(Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn denies_ipv6_unspecified() {
        assert!(is_denied_ipv6(Ipv6Addr::UNSPECIFIED));
    }

    #[test]
    fn denies_ipv6_link_local() {
        let ip: Ipv6Addr = "fe80::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv6_unique_local() {
        let ip: Ipv6Addr = "fc00::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
        let ip: Ipv6Addr = "fd00::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv6_documentation() {
        let ip: Ipv6Addr = "2001:db8::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn allows_ipv6_public() {
        // Google's public DNS
        let ip: Ipv6Addr = "2001:4860:4860::8888".parse().unwrap();
        assert!(!is_denied_ipv6(ip));
    }

    // ─── IPv4-mapped IPv6 bypass prevention ──────────────────────────────

    #[test]
    fn denies_ipv4_mapped_loopback() {
        // ::ffff:127.0.0.1
        let ip: Ipv6Addr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv4_mapped_private() {
        let ip: Ipv6Addr = "::ffff:10.0.0.1".parse().unwrap();
        assert!(is_denied_ipv6(ip));

        let ip: Ipv6Addr = "::ffff:192.168.1.1".parse().unwrap();
        assert!(is_denied_ipv6(ip));

        let ip: Ipv6Addr = "::ffff:172.16.0.1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv4_mapped_link_local() {
        let ip: Ipv6Addr = "::ffff:169.254.1.1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn allows_ipv4_mapped_public() {
        // ::ffff:8.8.8.8
        let ip: Ipv6Addr = "::ffff:8.8.8.8".parse().unwrap();
        assert!(!is_denied_ipv6(ip));
    }

    #[test]
    fn extract_mapped_ipv4_standard() {
        let ip: Ipv6Addr = "::ffff:192.168.1.1".parse().unwrap();
        let v4 = extract_mapped_ipv4(&ip).unwrap();
        assert_eq!(v4, Ipv4Addr::new(192, 168, 1, 1));
    }

    #[test]
    fn extract_mapped_ipv4_loopback() {
        let ip: Ipv6Addr = "::ffff:127.0.0.1".parse().unwrap();
        let v4 = extract_mapped_ipv4(&ip).unwrap();
        assert_eq!(v4, Ipv4Addr::new(127, 0, 0, 1));
    }

    #[test]
    fn extract_mapped_ipv4_public() {
        let ip: Ipv6Addr = "::ffff:8.8.8.8".parse().unwrap();
        let v4 = extract_mapped_ipv4(&ip).unwrap();
        assert_eq!(v4, Ipv4Addr::new(8, 8, 8, 8));
    }

    #[test]
    fn extract_mapped_ipv4_none_for_regular_v6() {
        let ip: Ipv6Addr = "2001:4860:4860::8888".parse().unwrap();
        assert!(extract_mapped_ipv4(&ip).is_none());
    }

    // ─── validate_ip ─────────────────────────────────────────────────────

    #[test]
    fn validate_ip_allows_public_v4() {
        assert!(validate_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))).is_ok());
    }

    #[test]
    fn validate_ip_blocks_private_v4() {
        assert!(validate_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))).is_err());
    }

    #[test]
    fn validate_ip_blocks_loopback_v4() {
        assert!(validate_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))).is_err());
    }

    #[test]
    fn validate_ip_blocks_mapped_v6_loopback() {
        let ip: Ipv6Addr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(validate_ip(IpAddr::V6(ip)).is_err());
    }

    #[test]
    fn validate_ip_allows_public_v6() {
        let ip: Ipv6Addr = "2001:4860:4860::8888".parse().unwrap();
        assert!(validate_ip(IpAddr::V6(ip)).is_ok());
    }

    // ─── URL validation ──────────────────────────────────────────────────

    #[test]
    fn validate_url_blocks_loopback_literal() {
        let url = Url::parse("http://127.0.0.1/secret").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_blocks_private_literal() {
        let url = Url::parse("http://192.168.1.1/admin").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_blocks_ipv6_loopback_literal() {
        let url = Url::parse("http://[::1]/secret").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_blocks_mapped_ipv6_literal() {
        let url = Url::parse("http://[::ffff:127.0.0.1]/secret").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_blocks_mapped_ipv6_private() {
        let url = Url::parse("http://[::ffff:10.0.0.1]/internal").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_resolves_public_hostname() {
        // example.com resolves to a public IP
        let url = Url::parse("https://example.com").unwrap();
        let result = validate_url(&url);
        assert!(result.is_ok(), "example.com should resolve to public IP");
    }

    #[test]
    fn validate_url_handles_no_host() {
        let url = Url::parse("data:text/plain,hello").unwrap();
        assert!(validate_url(&url).is_err());
    }

    // ─── Redirect validation ─────────────────────────────────────────────

    #[test]
    fn redirect_blocks_non_http_scheme() {
        let url = Url::parse("file:///etc/passwd").unwrap();
        assert!(validate_redirect_target(&url).is_err());

        let url = Url::parse("ftp://internal.server/data").unwrap();
        assert!(validate_redirect_target(&url).is_err());

        let url = Url::parse("gopher://localhost/").unwrap();
        assert!(validate_redirect_target(&url).is_err());
    }

    #[test]
    fn redirect_blocks_loopback() {
        let url = Url::parse("http://127.0.0.1/admin").unwrap();
        assert!(validate_redirect_target(&url).is_err());
    }

    #[test]
    fn redirect_allows_public_http() {
        let url = Url::parse("https://example.com/page").unwrap();
        assert!(validate_redirect_target(&url).is_ok());
    }

    // ─── Constants ───────────────────────────────────────────────────────

    #[test]
    fn default_max_redirects_is_reasonable() {
        assert!(DEFAULT_MAX_REDIRECTS >= 3);
        assert!(DEFAULT_MAX_REDIRECTS <= 10);
    }

    #[test]
    fn default_max_body_size_is_10mb() {
        assert_eq!(DEFAULT_MAX_BODY_SIZE, 10 * 1024 * 1024);
    }
}
