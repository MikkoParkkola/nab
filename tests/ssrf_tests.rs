//! Integration tests for SSRF protection (`nab::ssrf`).
//!
//! Extracted from inline tests in `src/ssrf.rs`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use nab::ssrf::{
    DEFAULT_MAX_BODY_SIZE, DEFAULT_MAX_REDIRECTS, extract_mapped_ipv4, is_denied_ipv4,
    is_denied_ipv6, validate_ip, validate_redirect_target, validate_url,
};
use url::Url;

// ─── IPv4 deny list ──────────────────────────────────────────────────────────

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

// ─── IPv6 deny list ──────────────────────────────────────────────────────────

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

// ─── IPv4-mapped IPv6 bypass prevention ──────────────────────────────────────

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

// ─── validate_ip ─────────────────────────────────────────────────────────────

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

// ─── URL validation ──────────────────────────────────────────────────────────

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
#[ignore = "requires external DNS resolution"]
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

// ─── Redirect validation ────────────────────────────────────────────────────

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

// ─── Constants ──────────────────────────────────────────────────────────────

#[test]
fn default_max_redirects_is_reasonable() {
    // Documenting constant invariants
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(
            DEFAULT_MAX_REDIRECTS >= 3,
            "should allow at least 3 redirects"
        );
        assert!(
            DEFAULT_MAX_REDIRECTS <= 10,
            "should not allow more than 10 redirects"
        );
    }
}

#[test]
fn default_max_body_size_is_10mb() {
    assert_eq!(DEFAULT_MAX_BODY_SIZE, 10 * 1024 * 1024);
}
