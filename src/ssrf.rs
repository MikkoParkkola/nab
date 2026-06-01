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

use tracing::warn;
use url::Url;

use crate::error::NabError;

/// Default maximum number of redirect hops allowed.
pub const DEFAULT_MAX_REDIRECTS: u32 = 5;

/// Default maximum response body size in bytes (10 MB).
pub const DEFAULT_MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

/// Environment variable that, when set to `1`/`true`, relaxes the SSRF guard
/// for **private/internal** ranges only (RFC 1918, IPv6 ULA, and CGN).
///
/// **OFF by default.** This is an explicit, audited opt-out for the documented
/// use case of reaching internal corporate dashboards. It never unblocks
/// loopback, link-local (cloud metadata `169.254.169.254`), unspecified,
/// multicast, broadcast, documentation, benchmarking, reserved ranges —
/// those stay denied regardless of this flag. See [`SsrfPolicy`].
pub const ALLOW_PRIVATE_ENV: &str = "NAB_SSRF_ALLOW_PRIVATE";

/// Environment variable holding a comma-separated allowlist of private
/// addresses / CIDR blocks (e.g. `10.252.0.0/16,192.168.1.5,fd00::/8`).
///
/// **OFF by default (empty).** This is the *preferred*, narrowly-scoped opt-out:
/// only addresses inside one of the listed ranges are exempted from the
/// private/ULA/CGN block, and only those ranges. Like [`ALLOW_PRIVATE_ENV`],
/// an allowlist entry can never unblock loopback, link-local/metadata, any
/// other always-denied range — those are filtered out before the allowlist is
/// consulted. See [`SsrfPolicy`].
pub const ALLOWLIST_ENV: &str = "NAB_SSRF_ALLOWLIST";

// ─── SSRF policy ───────────────────────────────────────────────────────────────

/// A single CIDR entry in an SSRF allowlist (IPv4 / IPv6).
///
/// Matching is exact bit-prefix comparison; host bits in the supplied network
/// address are ignored (canonicalised away at parse time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpCidr {
    /// IPv4 network: base address masked to `prefix_len` bits.
    V4 {
        /// Network address with host bits cleared.
        network: u32,
        /// Prefix length, `0..=32`.
        prefix_len: u8,
    },
    /// IPv6 network: base address masked to `prefix_len` bits.
    V6 {
        /// Network address with host bits cleared.
        network: u128,
        /// Prefix length, `0..=128`.
        prefix_len: u8,
    },
}

impl IpCidr {
    /// Parses a CIDR string (`10.0.0.0/8`) / a bare IP (`192.168.1.5`, treated
    /// as a host route — `/32` for IPv4, `/128` for IPv6).
    ///
    /// Host bits are masked off so `10.1.2.3/8` is stored as `10.0.0.0/8`.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a human-readable message if the address / prefix
    /// length cannot be parsed / the prefix exceeds the address width.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        let (addr_part, prefix_part) = match spec.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (spec, None),
        };

        match addr_part.parse::<IpAddr>() {
            Ok(IpAddr::V4(v4)) => {
                let prefix_len = Self::parse_prefix(prefix_part, 32)?;
                let bits = u32::from(v4);
                Ok(Self::V4 {
                    network: mask_u32(bits, prefix_len),
                    prefix_len,
                })
            }
            Ok(IpAddr::V6(v6)) => {
                let prefix_len = Self::parse_prefix(prefix_part, 128)?;
                let bits = u128::from(v6);
                Ok(Self::V6 {
                    network: mask_u128(bits, prefix_len),
                    prefix_len,
                })
            }
            Err(e) => Err(format!("invalid IP address '{addr_part}': {e}")),
        }
    }

    fn parse_prefix(prefix_part: Option<&str>, max: u8) -> Result<u8, String> {
        match prefix_part {
            None => Ok(max),
            Some(p) => {
                let value: u8 = p
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid prefix length '{p}'"))?;
                if value > max {
                    return Err(format!("prefix /{value} exceeds maximum /{max}"));
                }
                Ok(value)
            }
        }
    }

    /// Returns `true` if `ip` falls within this CIDR block. IPv4 / IPv6 never
    /// match across families.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self, ip) {
            (Self::V4 { network, prefix_len }, IpAddr::V4(v4)) => {
                mask_u32(u32::from(v4), *prefix_len) == *network
            }
            (Self::V6 { network, prefix_len }, IpAddr::V6(v6)) => {
                mask_u128(u128::from(v6), *prefix_len) == *network
            }
            _ => false,
        }
    }
}

/// Masks `bits` to the high `prefix_len` bits (IPv4). `prefix_len == 0` yields 0.
fn mask_u32(bits: u32, prefix_len: u8) -> u32 {
    if prefix_len == 0 {
        0
    } else if prefix_len >= 32 {
        bits
    } else {
        bits & (u32::MAX << (32 - prefix_len))
    }
}

/// Masks `bits` to the high `prefix_len` bits (IPv6). `prefix_len == 0` yields 0.
fn mask_u128(bits: u128, prefix_len: u8) -> u128 {
    if prefix_len == 0 {
        0
    } else if prefix_len >= 128 {
        bits
    } else {
        bits & (u128::MAX << (128 - prefix_len))
    }
}

/// Controls how far the SSRF guard relaxes the private/internal deny rules.
///
/// **The default ([`SsrfPolicy::deny_all`]) blocks every special-use range** —
/// identical to nab's behaviour before the opt-out existed. Relaxation is
/// strictly additive and only ever affects the *relaxable* subset:
///
/// | Range | Relaxable? |
/// |-------|-----------|
/// | RFC 1918 private (`10/8`, `172.16/12`, `192.168/16`) | yes |
/// | IPv6 ULA (`fc00::/7`) | yes |
/// | CGN (`100.64/10`) | yes |
/// | loopback (`127/8`, `::1`) | **never** |
/// | link-local incl. cloud metadata (`169.254/16`, `fe80::/10`) | **never** |
/// | unspecified, multicast, broadcast, documentation, benchmarking, reserved | **never** |
///
/// A policy can relax the relaxable set in two ways, OR-combined:
/// - `allow_private == true` — relax for *all* private/ULA/CGN addresses.
/// - `allowlist` — relax only for addresses inside an explicit CIDR/host entry.
///
/// Even with relaxation enabled, an address is only allowed through if it is in
/// the relaxable set. Loopback and metadata are filtered *before* the allowlist
/// is consulted, so an allowlist entry can never reach them.
#[derive(Debug, Clone, Default)]
pub struct SsrfPolicy {
    /// When `true`, every private/ULA/CGN address is allowed.
    allow_private: bool,
    /// Addresses inside any of these blocks are allowed even when
    /// `allow_private` is `false`. Only relaxable ranges are ever exempted.
    allowlist: Vec<IpCidr>,
}

impl SsrfPolicy {
    /// The default, fully-locked-down policy: every special-use range is denied.
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            allow_private: false,
            allowlist: Vec::new(),
        }
    }

    /// Builds a policy from the process environment
    /// ([`ALLOW_PRIVATE_ENV`] + [`ALLOWLIST_ENV`]).
    ///
    /// Unset / unparseable entries fall back to the locked-down default;
    /// malformed allowlist entries are skipped with a `tracing::warn`.
    #[must_use]
    pub fn from_env() -> Self {
        let allow_private = std::env::var(ALLOW_PRIVATE_ENV)
            .ok()
            .as_deref()
            .map(str::trim)
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

        let allowlist = std::env::var(ALLOWLIST_ENV)
            .ok()
            .map(|raw| parse_allowlist(&raw))
            .unwrap_or_default();

        Self {
            allow_private,
            allowlist,
        }
    }

    /// Overrides the `allow_private` flag (used to layer a CLI flag / MCP param
    /// over the env-derived default). Only ever turns the flag *on*.
    #[must_use]
    pub fn with_allow_private(mut self, allow: bool) -> Self {
        if allow {
            self.allow_private = true;
        }
        self
    }

    /// Appends parsed CIDR/host entries to the allowlist, skipping malformed
    /// ones with a warning. Used to layer CLI/MCP allowlist entries over env.
    #[must_use]
    pub fn with_allowlist_entries<I, S>(mut self, entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for entry in entries {
            let entry = entry.as_ref();
            if entry.trim().is_empty() {
                continue;
            }
            match IpCidr::parse(entry) {
                Ok(cidr) => self.allowlist.push(cidr),
                Err(e) => warn!("SSRF: ignoring malformed allowlist entry '{entry}': {e}"),
            }
        }
        self
    }

    /// Returns `true` if this policy relaxes anything at all (used to decide
    /// whether to emit an audit log on the fetch path).
    #[must_use]
    pub fn is_relaxed(&self) -> bool {
        self.allow_private || !self.allowlist.is_empty()
    }

    /// Returns `true` if `ip` is in the *relaxable* set (private/ULA/CGN) **and**
    /// this policy permits it (via `allow_private` / an allowlist match).
    ///
    /// This is the single gate through which any relaxation flows. It returns
    /// `false` for every always-denied range, so callers can safely OR it
    /// against the unconditional deny checks.
    fn permits_relaxable(&self, ip: IpAddr) -> bool {
        if !is_relaxable(ip) {
            return false;
        }
        if self.allow_private {
            return true;
        }
        self.allowlist.iter().any(|cidr| cidr.contains(ip))
    }
}

/// Parses a comma-separated allowlist string into CIDR entries, skipping blanks
/// and warning on malformed entries.
fn parse_allowlist(raw: &str) -> Vec<IpCidr> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|spec| match IpCidr::parse(spec) {
            Ok(cidr) => Some(cidr),
            Err(e) => {
                warn!("SSRF: ignoring malformed allowlist entry '{spec}': {e}");
                None
            }
        })
        .collect()
}

/// Returns `true` if `ip` is in the **relaxable** subset: RFC 1918 private,
/// IPv6 ULA, CGN. These — and only these — can be unblocked by a policy.
///
/// Loopback, link-local (cloud metadata), every other special-use range
/// return `false` here and therefore can never be relaxed.
fn is_relaxable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || is_ipv4_cgn(v4),
        // ULA fc00::/7 — segment[0] high 7 bits == fc00.
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

// ─── IPv4 deny list ──────────────────────────────────────────────────────────

/// Returns `true` if the given IPv4 address is in a denied range.
///
/// Uses the fully-locked-down default policy ([`SsrfPolicy::deny_all`]); every
/// special-use range is denied. See module-level docs for the full CIDR table.
/// For policy-aware validation (the opt-out path), use
/// [`is_denied_ipv4_with_policy`].
#[must_use]
pub fn is_denied_ipv4(ip: Ipv4Addr) -> bool {
    is_denied_ipv4_with_policy(ip, &SsrfPolicy::deny_all())
}

/// Returns `true` if the given IPv4 address is in a denied range, honouring an
/// explicit [`SsrfPolicy`].
///
/// The policy can only relax the *relaxable* subset (RFC 1918 private, CGN);
/// loopback, link-local (cloud metadata), and all other ranges stay denied.
#[must_use]
pub fn is_denied_ipv4_with_policy(ip: Ipv4Addr, policy: &SsrfPolicy) -> bool {
    // Unconditional denies — never relaxable.
    let always_denied = ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_multicast()
        || is_ipv4_documentation(ip)
        || is_ipv4_benchmarking(ip)
        || is_ipv4_protocol_assignments(ip)
        || is_ipv4_6to4_relay(ip)
        || is_ipv4_reserved(ip);
    if always_denied {
        return true;
    }

    // Relaxable subset (private + CGN): denied unless the policy permits it.
    if ip.is_private() || is_ipv4_cgn(ip) {
        return !policy.permits_relaxable(IpAddr::V4(ip));
    }

    false
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
/// Uses the fully-locked-down default policy ([`SsrfPolicy::deny_all`]). For the
/// policy-aware opt-out path, use [`is_denied_ipv6_with_policy`].
///
/// Also detects IPv4-mapped IPv6 addresses (`::ffff:x.x.x.x`) and validates
/// the embedded IPv4 address against the IPv4 deny list.
#[must_use]
pub fn is_denied_ipv6(ip: Ipv6Addr) -> bool {
    is_denied_ipv6_with_policy(ip, &SsrfPolicy::deny_all())
}

/// Returns `true` if the given IPv6 address is in a denied range, honouring an
/// explicit [`SsrfPolicy`].
///
/// Only the IPv6 ULA range (`fc00::/7`) and the embedded-IPv4 relaxable subset
/// can be relaxed; every other IPv6 special range stays denied.
#[must_use]
pub fn is_denied_ipv6_with_policy(ip: Ipv6Addr, policy: &SsrfPolicy) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }

    // Check IPv4-mapped IPv6 addresses (::ffff:x.x.x.x)
    // This catches bypass attempts like ::ffff:127.0.0.1
    if let Some(ipv4) = extract_mapped_ipv4(&ip) {
        return is_denied_ipv4_with_policy(ipv4, policy);
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

    // Unique local / ULA (fc00::/7) -- relaxable.
    if segments[0] & 0xfe00 == 0xfc00 {
        return !policy.permits_relaxable(IpAddr::V6(ip));
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
        return is_denied_ipv4_with_policy(embedded, policy);
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

/// Validates an IP address against the SSRF deny list (fully-locked-down
/// default policy).
///
/// Returns `Ok(())` if the address is allowed. For the policy-aware opt-out
/// path, use [`validate_ip_with_policy`].
pub fn validate_ip(ip: IpAddr) -> Result<(), NabError> {
    validate_ip_with_policy(ip, &SsrfPolicy::deny_all())
}

/// Validates an IP address against the SSRF deny list, honouring an explicit
/// [`SsrfPolicy`].
///
/// When a relaxed policy permits an otherwise-private address, an audit log is
/// emitted (`tracing::warn`) so the bypass is always visible in logs.
///
/// # Errors
///
/// Returns [`NabError::SsrfBlocked`] if the address is in a denied range that
/// the policy does not relax.
pub fn validate_ip_with_policy(ip: IpAddr, policy: &SsrfPolicy) -> Result<(), NabError> {
    let denied = match ip {
        IpAddr::V4(v4) => is_denied_ipv4_with_policy(v4, policy),
        IpAddr::V6(v6) => is_denied_ipv6_with_policy(v6, policy),
    };
    if denied {
        return Err(NabError::SsrfBlocked(format!(
            "IP address {ip} is in a denied range"
        )));
    }
    // Audit: a relaxed policy let a private/internal address through.
    if policy.is_relaxed() && is_relaxable(ip) {
        warn!(
            allowed_ip = %ip,
            "SSRF: allowing private/internal address via NAB_SSRF opt-out (loopback/metadata stay blocked)"
        );
    }
    Ok(())
}

/// Resolves a hostname to IP addresses and validates each against the SSRF
/// deny list (fully-locked-down default policy).
///
/// Returns the first allowed [`SocketAddr`]. For the policy-aware opt-out path,
/// use [`resolve_and_validate_with_policy`].
pub fn resolve_and_validate(host: &str, port: u16) -> Result<SocketAddr, NabError> {
    resolve_and_validate_with_policy(host, port, &SsrfPolicy::deny_all())
}

/// Policy-aware variant of [`resolve_and_validate`].
///
/// # Errors
///
/// Returns [`NabError::SsrfBlocked`] if DNS resolution fails / every resolved
/// address is denied under `policy`.
pub fn resolve_and_validate_with_policy(
    host: &str,
    port: u16,
    policy: &SsrfPolicy,
) -> Result<SocketAddr, NabError> {
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
        match validate_ip_with_policy(addr.ip(), policy) {
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

/// Validates a URL's host against the SSRF deny list by resolving DNS
/// (fully-locked-down default policy).
///
/// This is the main entry point for SSRF validation. It:
/// 1. Parses the URL to extract host and port
/// 2. Resolves the hostname via DNS
/// 3. Validates all resolved IPs against the deny list
/// 4. Returns the first allowed `SocketAddr` for DNS pinning
///
/// For the policy-aware opt-out path, use [`validate_url_with_policy`].
pub fn validate_url(url: &Url) -> Result<SocketAddr, NabError> {
    validate_url_with_policy(url, &SsrfPolicy::deny_all())
}

/// Policy-aware variant of [`validate_url`].
///
/// # Errors
///
/// Returns [`NabError::InvalidUrl`] when the URL has no host, [`NabError::SsrfBlocked`]
/// when every candidate address is denied under `policy`.
pub fn validate_url_with_policy(url: &Url, policy: &SsrfPolicy) -> Result<SocketAddr, NabError> {
    let host = url
        .host_str()
        .ok_or_else(|| NabError::InvalidUrl(format!("URL has no host: {url}")))?;

    let port = url.port_or_known_default().unwrap_or(443);

    // Check if host is a raw IP address first (no DNS needed)
    if let Ok(ip) = host.parse::<IpAddr>() {
        validate_ip_with_policy(ip, policy)?;
        return Ok(SocketAddr::new(ip, port));
    }

    // Also check bracket-stripped IPv6 literals like [::ffff:127.0.0.1]
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = stripped.parse::<IpAddr>() {
        validate_ip_with_policy(ip, policy)?;
        return Ok(SocketAddr::new(ip, port));
    }

    resolve_and_validate_with_policy(host, port, policy)
}

/// Validates a redirect target URL against the SSRF deny list (fully-locked-down
/// default policy).
///
/// Called before following each redirect hop to prevent redirect-based SSRF.
pub fn validate_redirect_target(url: &Url) -> Result<(), NabError> {
    validate_redirect_target_with_policy(url, &SsrfPolicy::deny_all())
}

/// Policy-aware variant of [`validate_redirect_target`].
///
/// The redirect target is validated under the *same* policy as the initial
/// request, so an opt-out does not silently widen across a redirect boundary
/// beyond what the user already permitted.
///
/// # Errors
///
/// Returns [`NabError::SsrfBlocked`] for a disallowed scheme / a denied target.
pub fn validate_redirect_target_with_policy(
    url: &Url,
    policy: &SsrfPolicy,
) -> Result<(), NabError> {
    // Only validate http/https schemes
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(NabError::SsrfBlocked(format!(
                "disallowed redirect scheme '{scheme}'"
            )));
        }
    }

    validate_url_with_policy(url, policy).map(|_| ())
}
