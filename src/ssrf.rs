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
//! | `0.0.0.0/8` | 1122 | "This" network / unspecified |
//! | `10.0.0.0/8` | 1918 | Private |
//! | `100.64.0.0/10` | 6598 | Carrier-Grade NAT (CGN) |
//! | `127.0.0.0/8` | 1122 | Loopback |
//! | `168.63.129.16/32` | Microsoft | Azure Wireserver |
//! | `169.254.0.0/16` | 3927 | Link-local (includes AWS metadata) |
//! | `172.16.0.0/12` | 1918 | Private |
//! | `192.0.0.0/24` | 6890 | IETF Protocol Assignments |
//! | `192.0.2.0/24` | 5737 | Documentation (TEST-NET-1) |
//! | `192.31.196.0/24` | 7535 | AS112 |
//! | `192.52.193.0/24` | 7450 | Automatic Multicast Tunneling |
//! | `192.88.99.0/24` | 7526 | 6to4 Relay Anycast (deprecated) |
//! | `192.168.0.0/16` | 1918 | Private |
//! | `192.175.48.0/24` | 7535 | AS112 |
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
//! | `64:ff9b::/96` | 6052 | NAT64 well-known |
//! | `64:ff9b:1::/48` | 8215 | NAT64 local-use |
//! | `100::/64` | 6666 | Discard-Only |
//! | `100:0:0:1::/64` | 6666 | Dummy |
//! | `2001::/23` | 6890 | IETF Protocol Assignments |
//! | `2001:20::/28` | 7343 | ORCHID v2 |
//! | `2001:db8::/32`, `3fff::/20` | 3849/9637 | Documentation |
//! | `2002::/16` | 3056 | 6to4 (deprecated translation prefix) |
//! | `2620:4f:8000::/48` | 7535 | AS112 |
//! | `5f00::/16` | 9602 | SRv6 SIDs |
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

use tracing::warn;
use url::Url;

use crate::error::NabError;

/// Default maximum number of redirect hops allowed.
pub const DEFAULT_MAX_REDIRECTS: u32 = 5;

/// Default maximum response body size in bytes (10 MB).
pub const DEFAULT_MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

// ─── IPv4 deny list ──────────────────────────────────────────────────────────

/// Returns `true` if the given IPv4 address is in a denied range.
///
/// See module-level docs for the full CIDR table.
pub fn is_denied_ipv4(ip: Ipv4Addr) -> bool {
    is_ipv4_this_network(ip)
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || is_ipv4_documentation(ip)
        || is_ipv4_benchmarking(ip)
        || is_ipv4_cgn(ip)
        || is_ipv4_protocol_assignments(ip)
        || is_ipv4_6to4_relay(ip)
        || is_ipv4_wireserver(ip)
        || is_ipv4_amt(ip)
        || is_ipv4_as112(ip)
        || is_ipv4_reserved(ip)
}

/// `0.0.0.0/8` -- "this" network (RFC 1122 / IANA special-purpose).
fn is_ipv4_this_network(ip: Ipv4Addr) -> bool {
    ip.octets()[0] == 0
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

/// `168.63.129.16/32` -- Azure Wireserver.
fn is_ipv4_wireserver(ip: Ipv4Addr) -> bool {
    ip.octets() == [168, 63, 129, 16]
}

/// `192.52.193.0/24` -- Automatic Multicast Tunneling (RFC 7450).
fn is_ipv4_amt(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 192 && octets[1] == 52 && octets[2] == 193
}

/// AS112 anycast service ranges (RFC 7535).
fn is_ipv4_as112(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    (octets[0] == 192 && octets[1] == 31 && octets[2] == 196)
        || (octets[0] == 192 && octets[1] == 175 && octets[2] == 48)
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
pub fn is_denied_ipv6(ip: Ipv6Addr) -> bool {
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

    // Documentation (2001:db8::/32 and 3fff::/20)
    if (segments[0] == 0x2001 && segments[1] == 0x0db8) || (segments[0] & 0xfff0) == 0x3ff0 {
        return true;
    }

    // Discard-Only (100::/64) -- RFC 6666
    if segments[0] == 0x0100 && segments[1..4] == [0, 0, 0] {
        return true;
    }

    // Dummy (100:0:0:1::/64) -- RFC 6666
    if segments[0] == 0x0100 && segments[1..4] == [0, 0, 1] {
        return true;
    }

    // IETF Protocol Assignments (2001::/23), including Teredo, benchmarking,
    // AMT, AS112, deprecated ORCHID, ORCHIDv2, and DET prefixes.
    if segments[0] == 0x2001 && (segments[1] & 0xfe00) == 0x0000 {
        return true;
    }

    // 6to4 (2002::/16) -- RFC 3056
    // Embeds an IPv4 address in bits 16..48. The entire prefix is deprecated
    // (RFC 7526) and should not be used for fetching content.
    if segments[0] == 0x2002 {
        return true;
    }

    // NAT64 translation prefixes (64:ff9b::/96, 64:ff9b:1::/48)
    if segments[0] == 0x0064
        && segments[1] == 0xff9b
        && (segments[2..6] == [0, 0, 0, 0] || segments[2] == 0x0001)
    {
        return true;
    }

    // AS112 (2620:4f:8000::/48) -- RFC 7535
    if segments[0] == 0x2620 && segments[1] == 0x004f && segments[2] == 0x8000 {
        return true;
    }

    // Segment Routing SIDs (5f00::/16) -- RFC 9602
    if segments[0] == 0x5f00 {
        return true;
    }

    false
}

/// Extracts the embedded IPv4 address from an IPv4-mapped IPv6 address.
///
/// Handles both `::ffff:a.b.c.d` and the full-form representation.
pub fn extract_mapped_ipv4(ip: &Ipv6Addr) -> Option<Ipv4Addr> {
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
/// Returns `Ok(())` if the address is allowed, or [`NabError::SsrfBlocked`]
/// describing why it was denied.
pub fn validate_ip(ip: IpAddr) -> Result<(), NabError> {
    match ip {
        IpAddr::V4(v4) => {
            if is_denied_ipv4(v4) {
                return Err(NabError::SsrfBlocked(format!(
                    "IPv4 address {v4} is in a denied range"
                )));
            }
        }
        IpAddr::V6(v6) => {
            if is_denied_ipv6(v6) {
                return Err(NabError::SsrfBlocked(format!(
                    "IPv6 address {v6} is in a denied range"
                )));
            }
        }
    }
    Ok(())
}

/// Resolves a hostname to IP addresses and validates each against the SSRF
/// deny list.
///
/// Returns the first allowed [`SocketAddr`], or [`NabError::SsrfBlocked`] if
/// all resolved addresses are denied or DNS resolution fails.
pub fn resolve_and_validate(host: &str, port: u16) -> Result<SocketAddr, NabError> {
    let addr_str = format!("{host}:{port}");
    let addrs: Vec<SocketAddr> = addr_str
        .to_socket_addrs()
        .map_err(|e| NabError::SsrfBlocked(format!("DNS resolution failed for {host}: {e}")))?
        .collect();

    if addrs.is_empty() {
        return Err(NabError::SsrfBlocked(format!(
            "DNS resolution returned no addresses for {host}"
        )));
    }

    for addr in &addrs {
        match validate_ip(addr.ip()) {
            Ok(()) => return Ok(*addr),
            Err(e) => {
                warn!("SSRF: skipping {addr} for {host}: {e}");
            }
        }
    }

    Err(NabError::SsrfBlocked(format!(
        "all resolved addresses for {host} are in denied ranges: {addrs:?}"
    )))
}

/// Validates a URL's host against the SSRF deny list by resolving DNS.
///
/// This is the main entry point for SSRF validation. It:
/// 1. Parses the URL to extract host and port
/// 2. Resolves the hostname via DNS
/// 3. Validates all resolved IPs against the deny list
/// 4. Returns the first allowed `SocketAddr` for DNS pinning
pub fn validate_url(url: &Url) -> Result<SocketAddr, NabError> {
    let host = url
        .host_str()
        .ok_or_else(|| NabError::InvalidUrl(format!("URL has no host: {url}")))?;

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
pub fn validate_redirect_target(url: &Url) -> Result<(), NabError> {
    // Only validate http/https schemes
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(NabError::SsrfBlocked(format!(
                "disallowed redirect scheme '{scheme}'"
            )));
        }
    }

    validate_url(url).map(|_| ())
}
