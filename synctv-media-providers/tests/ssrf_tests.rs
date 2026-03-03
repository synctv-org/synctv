//! SSRF protection unit tests
//!
//! Tests for the `synctv_common::ssrf` ACL with Teredo (`2001::/32`), 6to4 (`2002::/16`),
//! IPv4-mapped IPv6, and additional edge cases.

#![allow(clippy::unwrap_used)]
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use synctv_common::ssrf::is_ip_blocked;

// ============================================================================
// Teredo IPv6 (2001:0000::/32) blocking
// ============================================================================

#[test]
fn test_teredo_ipv6_blocked() {
    let teredo = Ipv6Addr::new(
        0x2001, 0x0000, 0x1234, 0x5678, 0x9abc, 0xdef0, 0x1111, 0x2222,
    );
    assert!(
        is_ip_blocked(&IpAddr::V6(teredo)),
        "Teredo addresses (2001:0000::/32) must be blocked"
    );
}

#[test]
fn test_teredo_ipv6_various_payloads() {
    let addrs = [
        Ipv6Addr::new(
            0x2001, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0001,
        ),
        Ipv6Addr::new(
            0x2001, 0x0000, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
        ),
        Ipv6Addr::new(
            0x2001, 0x0000, 0x4136, 0xe378, 0x8000, 0x63bf, 0x3fff, 0xfdd2,
        ),
    ];
    for addr in &addrs {
        assert!(
            is_ip_blocked(&IpAddr::V6(*addr)),
            "Teredo address {addr} must be blocked"
        );
    }
}

// ============================================================================
// 6to4 IPv6 (2002::/16) blocking
// ============================================================================

#[test]
fn test_6to4_ipv6_blocked() {
    let six_to_four = Ipv6Addr::new(
        0x2002, 0xc0a8, 0x0101, 0x0000, 0x0000, 0x0000, 0x0000, 0x0001,
    );
    assert!(
        is_ip_blocked(&IpAddr::V6(six_to_four)),
        "6to4 addresses (2002::/16) must be blocked"
    );
}

#[test]
fn test_6to4_ipv6_encapsulating_public() {
    // 6to4 encapsulating 8.8.8.8 -> 2002:0808:0808::1
    // Still blocked because 6to4 tunnel is inherently dangerous
    let addr = Ipv6Addr::new(
        0x2002, 0x0808, 0x0808, 0x0000, 0x0000, 0x0000, 0x0000, 0x0001,
    );
    assert!(
        is_ip_blocked(&IpAddr::V6(addr)),
        "6to4 even with public IPv4 payload must be blocked"
    );
}

// ============================================================================
// IPv4-mapped IPv6 addresses
// ============================================================================

#[test]
fn test_ipv4_mapped_ipv6_private_blocked() {
    // ::ffff:127.0.0.1
    let mapped_loopback = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001);
    assert!(
        is_ip_blocked(&IpAddr::V6(mapped_loopback)),
        "IPv4-mapped loopback must be blocked"
    );

    // ::ffff:192.168.1.1
    let mapped_private = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0101);
    assert!(
        is_ip_blocked(&IpAddr::V6(mapped_private)),
        "IPv4-mapped private must be blocked"
    );

    // ::ffff:10.0.0.1
    let mapped_10 = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x0001);
    assert!(
        is_ip_blocked(&IpAddr::V6(mapped_10)),
        "IPv4-mapped 10.x must be blocked"
    );
}

#[test]
fn test_ipv4_mapped_ipv6_public_allowed() {
    // ::ffff:8.8.8.8
    let mapped_public = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0808, 0x0808);
    assert!(
        !is_ip_blocked(&IpAddr::V6(mapped_public)),
        "IPv4-mapped public IP should be allowed"
    );
}

// ============================================================================
// IPv6 unique-local and link-local
// ============================================================================

#[test]
fn test_ipv6_unique_local_blocked() {
    let fc00 = Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1);
    assert!(is_ip_blocked(&IpAddr::V6(fc00)));

    let fd00 = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1);
    assert!(is_ip_blocked(&IpAddr::V6(fd00)));

    let fdff = Ipv6Addr::new(
        0xfdff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
    );
    assert!(is_ip_blocked(&IpAddr::V6(fdff)));
}

#[test]
fn test_ipv6_link_local_blocked() {
    let link_local = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1);
    assert!(is_ip_blocked(&IpAddr::V6(link_local)));
}

#[test]
fn test_ipv6_multicast_blocked() {
    let multicast = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);
    assert!(is_ip_blocked(&IpAddr::V6(multicast)));
}

// ============================================================================
// IPv6 global unicast (allowed)
// ============================================================================

#[test]
fn test_ipv6_global_unicast_allowed() {
    let cloudflare = Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111);
    assert!(
        !is_ip_blocked(&IpAddr::V6(cloudflare)),
        "Global unicast IPv6 should be allowed"
    );

    let public = Ipv6Addr::new(0x2400, 0xcb00, 0, 0, 0, 0, 0, 1);
    assert!(!is_ip_blocked(&IpAddr::V6(public)));
}

// ============================================================================
// is_ip_blocked dispatch
// ============================================================================

#[test]
fn test_is_ip_blocked_v4() {
    assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
}

#[test]
fn test_is_ip_blocked_v6() {
    assert!(is_ip_blocked(&IpAddr::V6(Ipv6Addr::LOCALHOST)));
    assert!(is_ip_blocked(&IpAddr::V6(Ipv6Addr::new(
        0x2001, 0, 0, 0, 0, 0, 0, 1
    ))));
}

// ============================================================================
// IPv4 boundary tests
// ============================================================================

#[test]
fn test_ipv4_172_range_boundary() {
    // 172.15.x.x should be allowed (below the 172.16-31 range)
    assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(
        172, 15, 255, 255
    ))));
    // 172.16.0.0 should be blocked
    assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 0))));
    // 172.31.255.255 should be blocked
    assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
    // 172.32.0.0 should be allowed (above the range)
    assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(172, 32, 0, 0))));
}

#[test]
fn test_ipv4_cgnat_boundary() {
    // Just below CGNAT range should be allowed
    assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(
        100, 63, 255, 255
    ))));
    // CGNAT range should be blocked
    assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 0))));
    assert!(is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(
        100, 127, 255, 255
    ))));
    // Just above CGNAT range should be allowed
    assert!(!is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(100, 128, 0, 0))));
}
