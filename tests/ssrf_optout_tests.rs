//! Security tests for the SSRF private-IP opt-out (issue #107).
//!
//! These tests construct [`SsrfPolicy`] values explicitly and call the
//! policy-aware predicates with literal IP addresses. They never touch DNS or
//! process environment, so they are deterministic under cargo's parallel
//! in-process test runner.
//!
//! The security contract under test:
//! 1. The default path still blocks private IPs (no regression).
//! 2. With the opt-out enabled, a private IP is allowed.
//! 3. An allowlist scopes the opt-out to specific ranges.
//! 4. Loopback, link-local (cloud metadata), and other dangerous ranges remain
//!    blocked **regardless** of the opt-out — these are the critical asserts.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use nab::ssrf::{
    IpCidr, SsrfPolicy, is_denied_ipv4_with_policy, is_denied_ipv6_with_policy,
    validate_ip_with_policy,
};

// Re-usable literal addresses.
const CORP_HOST: Ipv4Addr = Ipv4Addr::new(10, 252, 24, 131); // genai.booking.com from the issue
const PRIVATE_192: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 50);
const PRIVATE_172: Ipv4Addr = Ipv4Addr::new(172, 16, 5, 5);
const CGN: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 1);
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;
const AWS_METADATA: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
const PUBLIC: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34); // example.com

fn allow_private() -> SsrfPolicy {
    SsrfPolicy::deny_all().with_allow_private(true)
}

// ─── (1) Default still blocks private IPs ──────────────────────────────────────

#[test]
fn default_policy_blocks_private_ipv4() {
    let policy = SsrfPolicy::deny_all();
    assert!(is_denied_ipv4_with_policy(CORP_HOST, &policy));
    assert!(is_denied_ipv4_with_policy(PRIVATE_192, &policy));
    assert!(is_denied_ipv4_with_policy(PRIVATE_172, &policy));
    assert!(is_denied_ipv4_with_policy(CGN, &policy));
}

#[test]
fn default_policy_blocks_ula_ipv6() {
    let policy = SsrfPolicy::deny_all();
    let ula: Ipv6Addr = "fd00::1".parse().unwrap();
    assert!(is_denied_ipv6_with_policy(ula, &policy));
}

#[test]
fn bare_default_helpers_unchanged_for_private() {
    // The single-arg public helpers must remain byte-identical (deny-all).
    assert!(nab::ssrf::is_denied_ipv4(CORP_HOST));
    assert!(nab::ssrf::is_denied_ipv4(PRIVATE_192));
    assert!(validate_ip_with_policy(IpAddr::V4(CORP_HOST), &SsrfPolicy::deny_all()).is_err());
}

// ─── (2) Opt-out enabled → private IP allowed ──────────────────────────────────

#[test]
fn allow_private_permits_rfc1918() {
    let policy = allow_private();
    assert!(!is_denied_ipv4_with_policy(CORP_HOST, &policy));
    assert!(!is_denied_ipv4_with_policy(PRIVATE_192, &policy));
    assert!(!is_denied_ipv4_with_policy(PRIVATE_172, &policy));
    assert!(validate_ip_with_policy(IpAddr::V4(CORP_HOST), &policy).is_ok());
}

#[test]
fn allow_private_permits_cgn() {
    let policy = allow_private();
    assert!(!is_denied_ipv4_with_policy(CGN, &policy));
}

#[test]
fn allow_private_permits_ula_ipv6() {
    let policy = allow_private();
    let ula: Ipv6Addr = "fd12:3456::1".parse().unwrap();
    assert!(!is_denied_ipv6_with_policy(ula, &policy));
}

// ─── (3) Allowlist scoping ─────────────────────────────────────────────────────

#[test]
fn allowlist_permits_only_listed_range() {
    let policy = SsrfPolicy::deny_all().with_allowlist_entries(["10.252.0.0/16"]);
    // In range → allowed.
    assert!(!is_denied_ipv4_with_policy(CORP_HOST, &policy));
    // Different private range, NOT in the allowlist → still blocked.
    assert!(is_denied_ipv4_with_policy(PRIVATE_192, &policy));
    assert!(is_denied_ipv4_with_policy(PRIVATE_172, &policy));
}

#[test]
fn allowlist_host_route_is_exact() {
    let policy = SsrfPolicy::deny_all().with_allowlist_entries(["192.168.1.50"]);
    assert!(!is_denied_ipv4_with_policy(PRIVATE_192, &policy));
    // Neighbour address not covered by the /32 host route.
    assert!(is_denied_ipv4_with_policy(
        Ipv4Addr::new(192, 168, 1, 51),
        &policy
    ));
}

#[test]
fn allowlist_ipv6_cidr_scopes() {
    let policy = SsrfPolicy::deny_all().with_allowlist_entries(["fd00::/16"]);
    let in_range: Ipv6Addr = "fd00:abcd::1".parse().unwrap();
    let out_of_range: Ipv6Addr = "fd01::1".parse().unwrap();
    assert!(!is_denied_ipv6_with_policy(in_range, &policy));
    assert!(is_denied_ipv6_with_policy(out_of_range, &policy));
}

#[test]
fn allowlist_skips_malformed_entries() {
    // Malformed entries are dropped; the valid one still applies.
    let policy = SsrfPolicy::deny_all().with_allowlist_entries([
        "not-an-ip",
        "10.0.0.0/99",
        "",
        "10.252.0.0/16",
    ]);
    assert!(!is_denied_ipv4_with_policy(CORP_HOST, &policy));
}

// ─── (4) CRITICAL: dangerous ranges never relaxable ────────────────────────────

#[test]
fn allow_private_never_unblocks_loopback() {
    let policy = allow_private();
    assert!(is_denied_ipv4_with_policy(LOOPBACK, &policy));
    let v6_loopback: Ipv6Addr = "::1".parse().unwrap();
    assert!(is_denied_ipv6_with_policy(v6_loopback, &policy));
}

#[test]
fn allow_private_never_unblocks_cloud_metadata() {
    // 169.254.169.254 — AWS/GCP/Azure IMDS. The single most dangerous SSRF
    // target. Must stay blocked even with the broadest opt-out.
    let policy = allow_private();
    assert!(is_denied_ipv4_with_policy(AWS_METADATA, &policy));
    // Whole link-local /16 stays blocked.
    assert!(is_denied_ipv4_with_policy(
        Ipv4Addr::new(169, 254, 0, 1),
        &policy
    ));
}

#[test]
fn allowlist_cannot_unblock_metadata_even_if_listed() {
    // An operator explicitly lists the metadata IP. It must STILL be blocked:
    // the allowlist only ever exempts the relaxable (private/ULA/CGN) subset.
    let policy = SsrfPolicy::deny_all().with_allowlist_entries(["169.254.169.254/32"]);
    assert!(is_denied_ipv4_with_policy(AWS_METADATA, &policy));
}

#[test]
fn allowlist_cannot_unblock_loopback_even_if_listed() {
    let policy = SsrfPolicy::deny_all().with_allowlist_entries(["127.0.0.1/8"]);
    assert!(is_denied_ipv4_with_policy(LOOPBACK, &policy));
}

#[test]
fn allow_private_never_unblocks_link_local_ipv6() {
    let policy = allow_private();
    let link_local: Ipv6Addr = "fe80::1".parse().unwrap();
    assert!(is_denied_ipv6_with_policy(link_local, &policy));
}

#[test]
fn allow_private_never_unblocks_mapped_metadata() {
    // ::ffff:169.254.169.254 — IPv4-mapped metadata bypass attempt.
    let policy = allow_private();
    let mapped: Ipv6Addr = "::ffff:169.254.169.254".parse().unwrap();
    assert!(is_denied_ipv6_with_policy(mapped, &policy));
}

#[test]
fn allow_private_does_not_affect_public_or_documentation() {
    let policy = allow_private();
    // Public stays allowed (it always was).
    assert!(!is_denied_ipv4_with_policy(PUBLIC, &policy));
    // Documentation range stays blocked (not relaxable).
    assert!(is_denied_ipv4_with_policy(
        Ipv4Addr::new(192, 0, 2, 1),
        &policy
    ));
    // Broadcast stays blocked.
    assert!(is_denied_ipv4_with_policy(Ipv4Addr::BROADCAST, &policy));
}

// ─── CIDR parsing / matching edge cases (boundaries) ───────────────────────────

#[test]
fn cidr_parse_rejects_bad_input() {
    assert!(IpCidr::parse("nonsense").is_err());
    assert!(IpCidr::parse("10.0.0.0/33").is_err());
    assert!(IpCidr::parse("::/129").is_err());
    assert!(IpCidr::parse("10.0.0.0/-1").is_err());
}

#[test]
fn cidr_zero_prefix_matches_whole_family_but_not_across_families() {
    let v4_any = IpCidr::parse("0.0.0.0/0").unwrap();
    assert!(v4_any.contains(IpAddr::V4(PUBLIC)));
    assert!(v4_any.contains(IpAddr::V4(LOOPBACK)));
    // Never matches across families.
    assert!(!v4_any.contains("::1".parse::<IpAddr>().unwrap()));
}

#[test]
fn cidr_full_prefix_is_host_route() {
    let host = IpCidr::parse("10.1.2.3/32").unwrap();
    assert!(host.contains(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))));
    assert!(!host.contains(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 4))));

    let v6_host = IpCidr::parse("fd00::1/128").unwrap();
    assert!(v6_host.contains("fd00::1".parse::<IpAddr>().unwrap()));
    assert!(!v6_host.contains("fd00::2".parse::<IpAddr>().unwrap()));
}

#[test]
fn cidr_masks_host_bits_at_parse_time() {
    // 10.1.2.3/8 and 10.0.0.0/8 must be equal after canonicalisation.
    assert_eq!(
        IpCidr::parse("10.1.2.3/8").unwrap(),
        IpCidr::parse("10.0.0.0/8").unwrap()
    );
}

#[test]
fn cidr_prefix_boundary_24() {
    let net = IpCidr::parse("192.168.1.0/24").unwrap();
    assert!(net.contains(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 0))));
    assert!(net.contains(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 255))));
    assert!(!net.contains(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 0))));
}

#[test]
fn bare_ip_parses_as_host_route() {
    let v4 = IpCidr::parse("10.0.0.5").unwrap();
    assert!(v4.contains(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
    assert!(!v4.contains(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 6))));
}

// ─── Policy helpers ────────────────────────────────────────────────────────────

#[test]
fn deny_all_is_not_relaxed() {
    assert!(!SsrfPolicy::deny_all().is_relaxed());
}

#[test]
fn allow_private_and_allowlist_are_relaxed() {
    assert!(allow_private().is_relaxed());
    assert!(
        SsrfPolicy::deny_all()
            .with_allowlist_entries(["10.0.0.0/8"])
            .is_relaxed()
    );
}
