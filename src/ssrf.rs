//! SSRF (Server-Side Request Forgery) protection.
//!
//! Validates resolved IP addresses against a comprehensive deny list of RFC
//! special-use address ranges. Also detects IPv4-mapped/embedded IPv6 addresses
//! that could bypass naive checks.
//!
//! # Covered RFC Special-Use Ranges
//!
//! ## IPv4
//! | CIDR | RFC | Description |
//! |------|-----|-------------|
//! | `0.0.0.0/32` | 1122 | Unspecified |
//! | `10.0.0.0/8` | 1918 | Private |
//! | `100.64.0.0/10` | 6598 | Carrier-Grade NAT (CGN) |
//! | `127.0.0.0/8` | 1122 | Loopback |
//! | `169.254.0.0/16` | 3927 | Link-local (includes AWS metadata) |
//! | `172.16.0.0/12` | 1918 | Private |
//! | `192.0.0.0/24` | 6890 | IETF Protocol Assignments |
//! | `192.0.2.0/24` | 5737 | Documentation (TEST-NET-1) |
//! | `192.88.99.0/24` | 7526 | 6to4 Relay Anycast (deprecated) |
//! | `192.168.0.0/16` | 1918 | Private |
//! | `198.18.0.0/15` | 2544 | Benchmarking |
//! | `198.51.100.0/24` | 5737 | Documentation (TEST-NET-2) |
//! | `203.0.113.0/24` | 5737 | Documentation (TEST-NET-3) |
//! | `224.0.0.0/4` | 5771 | Multicast |
//! | `240.0.0.0/4` | 1112 | Reserved (Class E) |
//! | `255.255.255.255/32` | 919 | Broadcast |
//!
//! ## IPv6
//! | CIDR | RFC | Description |
//! |------|-----|-------------|
//! | `::/128` | 4291 | Unspecified |
//! | `::1/128` | 4291 | Loopback |
//! | `::ffff:0:0/96` | 4291 | IPv4-mapped (delegated to IPv4 check) |
//! | `64:ff9b::/96` | 6052 | NAT64 well-known (embedded IPv4 checked) |
//! | `64:ff9b:1::/48` | 8215 | NAT64 local-use |
//! | `100::/64` | 6666 | Discard-Only |
//! | `2001::/32` | 4380 | Teredo tunneling |
//! | `2001:20::/28` | 7343 | ORCHID v2 |
//! | `2001:db8::/32` | 3849 | Documentation |
//! | `2002::/16` | 3056 | 6to4 (deprecated, embedded IPv4 checked) |
//! | `fc00::/7` | 4193 | Unique Local Address (ULA) |
//! | `fe80::/10` | 4291 | Link-local |
//! | `fec0::/10` | 3879 | Site-local (deprecated) |
//! | `ff00::/8` | 4291 | Multicast |
//!
//! # DNS Pinning
//!
//! [`resolve_and_validate`] resolves a hostname once, validates the resolved IP
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

// ─── IPv4 deny list ──────────────────────────────────────────────────────────

/// Returns `true` if the given IPv4 address is in a denied range.
///
/// See module-level docs for the full CIDR table.
fn is_denied_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_multicast()
        || is_ipv4_documentation(ip)
        || is_ipv4_benchmarking(ip)
        || is_ipv4_cgn(ip)
        || is_ipv4_protocol_assignments(ip)
        || is_ipv4_6to4_relay(ip)
        || is_ipv4_reserved(ip)
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

/// `100.64.0.0/10` -- Carrier-Grade NAT (RFC 6598).
///
/// Shared address space used by ISPs for large-scale NAT. Not routable on
/// the public Internet; often used in cloud VPC internal networking.
fn is_ipv4_cgn(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    // 100.64.0.0/10 = first octet 100, second octet 64..127 (bits: 01xxxxxx)
    octets[0] == 100 && (octets[1] & 0xC0) == 64
}

/// `192.0.0.0/24` -- IETF Protocol Assignments (RFC 6890).
///
/// Includes DS-Lite (`192.0.0.0/29`), NAT64 discovery, and other IETF uses.
fn is_ipv4_protocol_assignments(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 192 && octets[1] == 0 && octets[2] == 0
}

/// `192.88.99.0/24` -- 6to4 Relay Anycast (RFC 7526, deprecated).
fn is_ipv4_6to4_relay(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 192 && octets[1] == 88 && octets[2] == 99
}

/// `240.0.0.0/4` -- Reserved for future use (RFC 1112, Class E).
///
/// Also catches `255.0.0.0/8` through `255.255.255.254` (broadcast
/// `255.255.255.255` is already caught by `is_broadcast()`).
fn is_ipv4_reserved(ip: Ipv4Addr) -> bool {
    ip.octets()[0] >= 240
}

// ─── IPv6 deny list ──────────────────────────────────────────────────────────

/// Returns `true` if the given IPv6 address is in a denied range.
///
/// See module-level docs for the full CIDR table.
///
/// Also detects IPv4-mapped IPv6 addresses (`::ffff:x.x.x.x`) and validates
/// the embedded IPv4 address against the IPv4 deny list.
fn is_denied_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }

    // Check IPv4-mapped IPv6 addresses (::ffff:x.x.x.x)
    // This catches bypass attempts like ::ffff:127.0.0.1
    if let Some(ipv4) = extract_mapped_ipv4(&ip) {
        return is_denied_ipv4(ipv4);
    }

    let segments = ip.segments();

    // Link-local (fe80::/10)
    if segments[0] & 0xffc0 == 0xfe80 {
        return true;
    }

    // Site-local (fec0::/10, deprecated but still must be blocked) -- RFC 3879
    if segments[0] & 0xffc0 == 0xfec0 {
        return true;
    }

    // Unique local / ULA (fc00::/7)
    if segments[0] & 0xfe00 == 0xfc00 {
        return true;
    }

    // Documentation (2001:db8::/32)
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return true;
    }

    // Discard-Only (100::/64) -- RFC 6666
    if segments[0] == 0x0100 && segments[1..4] == [0, 0, 0] {
        return true;
    }

    // Teredo (2001::/32) -- RFC 4380
    // Teredo tunnels IPv4 inside IPv6; the embedded server/client IPs could be
    // private. Block the entire prefix since it is a tunneling mechanism.
    if segments[0] == 0x2001 && segments[1] == 0x0000 {
        return true;
    }

    // ORCHID v2 (2001:20::/28) -- RFC 7343
    // Overlay Routable Cryptographic Hash Identifiers (non-routable experiment).
    if segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0020 {
        return true;
    }

    // 6to4 (2002::/16) -- RFC 3056
    // Embeds an IPv4 address in bits 16..48. The entire prefix is deprecated
    // (RFC 7526) and should not be used for fetching content.
    if segments[0] == 0x2002 {
        return true;
    }

    // NAT64 well-known prefix (64:ff9b::/96) -- RFC 6052
    if segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2..6] == [0, 0, 0, 0] {
        // Embedded IPv4 in last 32 bits
        let embedded = Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            (segments[6] & 0xff) as u8,
            (segments[7] >> 8) as u8,
            (segments[7] & 0xff) as u8,
        );
        return is_denied_ipv4(embedded);
    }

    // NAT64 local-use prefix (64:ff9b:1::/48) -- RFC 8215
    // Entire prefix is for local NAT64 deployment; not globally routable.
    if segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2] == 0x0001 {
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

// ─── Public API ──────────────────────────────────────────────────────────────

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
        assert!(is_denied_ipv4(Ipv4Addr::LOCALHOST));
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
        assert!(is_denied_ipv4(Ipv4Addr::BROADCAST));
    }

    #[test]
    fn denies_ipv4_unspecified() {
        assert!(is_denied_ipv4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn denies_ipv4_cgn() {
        // 100.64.0.0/10 -- Carrier-Grade NAT (RFC 6598)
        assert!(is_denied_ipv4(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(100, 100, 100, 100)));
        assert!(is_denied_ipv4(Ipv4Addr::new(100, 127, 255, 255)));
        // Just outside the range
        assert!(!is_denied_ipv4(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!is_denied_ipv4(Ipv4Addr::new(100, 128, 0, 0)));
    }

    #[test]
    fn denies_ipv4_protocol_assignments() {
        // 192.0.0.0/24 -- IETF Protocol Assignments (RFC 6890)
        assert!(is_denied_ipv4(Ipv4Addr::new(192, 0, 0, 0)));
        assert!(is_denied_ipv4(Ipv4Addr::new(192, 0, 0, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(192, 0, 0, 255)));
    }

    #[test]
    fn denies_ipv4_6to4_relay() {
        // 192.88.99.0/24 -- 6to4 Relay Anycast (RFC 7526)
        assert!(is_denied_ipv4(Ipv4Addr::new(192, 88, 99, 0)));
        assert!(is_denied_ipv4(Ipv4Addr::new(192, 88, 99, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(192, 88, 99, 255)));
    }

    #[test]
    fn denies_ipv4_multicast() {
        // 224.0.0.0/4 -- Multicast (RFC 5771)
        assert!(is_denied_ipv4(Ipv4Addr::new(224, 0, 0, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(239, 255, 255, 255)));
    }

    #[test]
    fn denies_ipv4_reserved_class_e() {
        // 240.0.0.0/4 -- Reserved / Future use (RFC 1112)
        assert!(is_denied_ipv4(Ipv4Addr::new(240, 0, 0, 1)));
        assert!(is_denied_ipv4(Ipv4Addr::new(250, 1, 2, 3)));
        assert!(is_denied_ipv4(Ipv4Addr::new(255, 255, 255, 254)));
    }

    #[test]
    fn denies_ipv4_aws_metadata() {
        // 169.254.169.254 -- AWS/cloud metadata (covered by link-local)
        assert!(is_denied_ipv4(Ipv4Addr::new(169, 254, 169, 254)));
    }

    #[test]
    fn allows_ipv4_public() {
        assert!(!is_denied_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_denied_ipv4(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!is_denied_ipv4(Ipv4Addr::new(93, 184, 216, 34)));
        // Just outside CGN range
        assert!(!is_denied_ipv4(Ipv4Addr::new(100, 63, 255, 255)));
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
    fn denies_ipv6_site_local_deprecated() {
        // fec0::/10 -- deprecated site-local (RFC 3879)
        let ip: Ipv6Addr = "fec0::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
        let ip: Ipv6Addr = "feff::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv6_documentation() {
        let ip: Ipv6Addr = "2001:db8::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv6_multicast() {
        // ff00::/8 -- Multicast
        let ip: Ipv6Addr = "ff02::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
        let ip: Ipv6Addr = "ff0e::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv6_teredo() {
        // 2001::/32 -- Teredo tunneling (RFC 4380)
        let ip: Ipv6Addr = "2001:0000::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
        let ip: Ipv6Addr = "2001:0000:4136:e378:8000:63bf:3fff:fdd2".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv6_orchid_v2() {
        // 2001:20::/28 -- ORCHID v2 (RFC 7343)
        let ip: Ipv6Addr = "2001:20::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
        let ip: Ipv6Addr = "2001:2f::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv6_6to4() {
        // 2002::/16 -- 6to4 (RFC 3056), entirely blocked
        // 6to4 embedding public IPv4 8.8.8.8 -- still blocked (deprecated)
        let ip: Ipv6Addr = "2002:0808:0808::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
        // 6to4 embedding private 192.168.1.1
        let ip: Ipv6Addr = "2002:c0a8:0101::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv6_nat64_well_known_private() {
        // 64:ff9b::/96 with denied embedded IPv4
        // NAT64 embedding 127.0.0.1
        let ip: Ipv6Addr = "64:ff9b::127.0.0.1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
        // NAT64 embedding 10.0.0.1
        let ip: Ipv6Addr = "64:ff9b::10.0.0.1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn allows_ipv6_nat64_with_public_embedded() {
        // 64:ff9b::/96 with public embedded IPv4 should be allowed
        let ip: Ipv6Addr = "64:ff9b::8.8.8.8".parse().unwrap();
        assert!(!is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv6_nat64_local_use() {
        // 64:ff9b:1::/48 -- NAT64 local-use (RFC 8215)
        let ip: Ipv6Addr = "64:ff9b:1::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv6_discard() {
        // 100::/64 -- Discard-Only (RFC 6666)
        let ip: Ipv6Addr = "100::1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn allows_ipv6_public() {
        // Google's public DNS
        let ip: Ipv6Addr = "2001:4860:4860::8888".parse().unwrap();
        assert!(!is_denied_ipv6(ip));
        // Cloudflare
        let ip: Ipv6Addr = "2606:4700:4700::1111".parse().unwrap();
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
    fn denies_ipv4_mapped_cgn() {
        // ::ffff:100.64.0.1 -- CGN via IPv4-mapped bypass
        let ip: Ipv6Addr = "::ffff:100.64.0.1".parse().unwrap();
        assert!(is_denied_ipv6(ip));
    }

    #[test]
    fn denies_ipv4_mapped_aws_metadata() {
        // ::ffff:169.254.169.254 -- AWS metadata via IPv4-mapped bypass
        let ip: Ipv6Addr = "::ffff:169.254.169.254".parse().unwrap();
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
        assert_eq!(v4, Ipv4Addr::LOCALHOST);
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
        assert!(validate_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)).is_err());
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
    fn validate_url_blocks_cgn_literal() {
        let url = Url::parse("http://100.64.0.1/internal").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_blocks_aws_metadata() {
        let url = Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_blocks_multicast() {
        let url = Url::parse("http://224.0.0.1/").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_blocks_reserved_class_e() {
        let url = Url::parse("http://240.0.0.1/").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_blocks_6to4_ipv6() {
        let url = Url::parse("http://[2002:c0a8:0101::1]/").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_blocks_teredo_ipv6() {
        let url = Url::parse("http://[2001:0000:4136:e378:8000:63bf:3fff:fdd2]/").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[test]
    fn validate_url_blocks_nat64_private() {
        let url = Url::parse("http://[64:ff9b::10.0.0.1]/").unwrap();
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
        // Documenting constant invariants - values intentionally checked at test-time for visibility
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(DEFAULT_MAX_REDIRECTS >= 3, "should allow at least 3 redirects");
            assert!(DEFAULT_MAX_REDIRECTS <= 10, "should not allow more than 10 redirects");
        }
    }

    #[test]
    fn default_max_body_size_is_10mb() {
        assert_eq!(DEFAULT_MAX_BODY_SIZE, 10 * 1024 * 1024);
    }
}
