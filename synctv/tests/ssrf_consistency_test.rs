//! SSRF ACL tests.
//!
//! Verifies that the `synctv_common::ssrf` ACL correctly blocks private/internal IPs
//! and hostnames, and allows public IPs and hostnames.

#![allow(clippy::unwrap_used)]
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use synctv_common::ssrf::SsrfGuard;

// Test data

/// IPv4 addresses that must be blocked.
const BLOCKED_IPV4: &[(u8, u8, u8, u8)] = &[
    (127, 0, 0, 1),
    (10, 0, 0, 1),
    (172, 16, 0, 1),
    (192, 168, 1, 1),
    (169, 254, 169, 254),
    // CGNAT / Shared Address Space (100.64.0.0/10, RFC 6598)
    (100, 64, 0, 1),
    (100, 100, 100, 100),
    (100, 127, 255, 255),
    (0, 0, 0, 0),
    (224, 0, 0, 1),
    (240, 0, 0, 1),
    (255, 255, 255, 255),
];

/// IPv4 addresses that must be allowed.
const ALLOWED_IPV4: &[(u8, u8, u8, u8)] = &[
    (8, 8, 8, 8),
    (1, 1, 1, 1),
    (93, 184, 216, 34),
    (100, 128, 0, 0), // just outside CGNAT
];

// Tests

#[test]
fn ssrf_acl_builds_successfully() {
    let _acl = SsrfGuard::shared_default().acl().clone();
}

#[test]
fn blocked_ipv4_are_blocked() {
    for &(a, b, c, d) in BLOCKED_IPV4 {
        let ip = IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        assert!(
            SsrfGuard::shared_default().is_ip_blocked(&ip),
            "is_ip_blocked should block {ip}"
        );
    }
}

#[test]
fn allowed_ipv4_are_allowed() {
    for &(a, b, c, d) in ALLOWED_IPV4 {
        let ip = IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        assert!(
            !SsrfGuard::shared_default().is_ip_blocked(&ip),
            "is_ip_blocked should allow {ip}"
        );
    }
}

#[test]
fn blocked_ipv6_are_blocked() {
    let blocked: Vec<Ipv6Addr> = vec![
        Ipv6Addr::LOCALHOST,
        Ipv6Addr::UNSPECIFIED,
        "fe80::1".parse().unwrap(),
        "fc00::1".parse().unwrap(),
        "fd00::1".parse().unwrap(),
    ];

    for ipv6 in &blocked {
        let ip = IpAddr::V6(*ipv6);
        assert!(
            SsrfGuard::shared_default().is_ip_blocked(&ip),
            "is_ip_blocked should block {ip}"
        );
    }
}

#[test]
fn allowed_ipv6_are_allowed() {
    let allowed: Vec<Ipv6Addr> = vec![
        "2606:4700:4700::1111".parse().unwrap(), // Cloudflare DNS
        "2400:cb00::1".parse().unwrap(),
    ];

    for ipv6 in &allowed {
        let ip = IpAddr::V6(*ipv6);
        assert!(
            !SsrfGuard::shared_default().is_ip_blocked(&ip),
            "is_ip_blocked should allow {ip}"
        );
    }
}
