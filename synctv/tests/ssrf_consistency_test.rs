//! SSRF consistency tests.
//!
//! Verifies that `synctv_core::validation::SSRFValidator` (the high-level
//! validator) and `synctv_media_providers::ssrf` (the shared primitives) agree
//! on which IPs and hostnames should be blocked or allowed.
//!
//! These two modules live in separate crates. This integration test catches
//! drift between them.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use synctv_core::validation::SSRFValidator;
use synctv_media_providers::ssrf as primitives;

// ---------------------------------------------------------------------------
// Test data
// ---------------------------------------------------------------------------

/// URLs / IPs that must be blocked by both implementations.
const BLOCKED_URLS: &[&str] = &[
    // RFC1918 private ranges
    "http://10.0.0.1/path",
    "http://10.255.255.255/path",
    "http://172.16.0.1/path",
    "http://172.31.255.255/path",
    "http://192.168.0.1/path",
    "http://192.168.255.255/path",
    // Loopback
    "http://127.0.0.1/path",
    "http://127.255.255.255/path",
    // Link-local
    "http://169.254.0.1/path",
    // Cloud metadata endpoints
    "http://169.254.169.254/latest/meta-data",
    // Current network
    "http://0.0.0.0/path",
    // CGNAT / Shared Address Space (100.64.0.0/10, RFC 6598)
    "http://100.64.0.1/path",
    "http://100.127.255.255/path",
    // Multicast
    "http://224.0.0.1/path",
    // Reserved
    "http://240.0.0.1/path",
    "http://255.255.255.255/path",
    // IPv6 loopback
    "http://[::1]/path",
    // IPv6 link-local
    "http://[fe80::1]/path",
    // IPv6 unique-local
    "http://[fc00::1]/path",
    "http://[fd00::1]/path",
    // IPv4-mapped IPv6 private
    "http://[::ffff:192.168.0.1]/path",
    "http://[::ffff:127.0.0.1]/path",
    "http://[::ffff:10.0.0.1]/path",
];

/// URLs / IPs that must be allowed by both implementations.
const ALLOWED_URLS: &[&str] = &[
    "http://8.8.8.8/path",
    "http://1.1.1.1/path",
    "http://93.184.216.34/path",
    "https://example.com/path",
    "https://google.com/path",
    "https://github.com/path",
    "http://[::ffff:8.8.8.8]/path",
    "http://[::ffff:1.1.1.1]/path",
];

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

/// Hostnames that must be blocked.
const BLOCKED_HOSTNAMES: &[&str] = &[
    "localhost",
    "localhost.localdomain",
    "metadata.google.internal",
    "myserver.local",
    "myserver.internal",
    "kubernetes.default",
    "k8s.api",
    "docker.local",
    "container.internal",
    "instance-data",
    "metadata.azure",
];

/// Hostnames that must be allowed.
const ALLOWED_HOSTNAMES: &[&str] = &[
    "example.com",
    "google.com",
    "api.bilibili.com",
    "github.com",
];

// ---------------------------------------------------------------------------
// Tests: SSRFValidator (synctv-core) agrees with primitives (media-providers)
// ---------------------------------------------------------------------------

#[test]
fn blocked_urls_are_blocked_by_core_validator() {
    let validator = SSRFValidator::new();
    for url in BLOCKED_URLS {
        assert!(
            validator.validate_url(url).is_err(),
            "SSRFValidator should block URL: {url}"
        );
    }
}

#[test]
fn allowed_urls_are_allowed_by_core_validator() {
    let validator = SSRFValidator::new();
    for url in ALLOWED_URLS {
        assert!(
            validator.validate_url(url).is_ok(),
            "SSRFValidator should allow URL: {url}"
        );
    }
}

#[test]
fn blocked_urls_are_blocked_by_primitives() {
    for url in BLOCKED_URLS {
        let result = primitives::check_url(url);
        assert!(
            !result.is_ok(),
            "primitives::check_url should block URL: {url}"
        );
    }
}

#[test]
fn allowed_urls_are_allowed_by_primitives() {
    for url in ALLOWED_URLS {
        let result = primitives::check_url(url);
        assert!(
            result.is_ok(),
            "primitives::check_url should allow URL: {url}"
        );
    }
}

#[test]
fn blocked_ipv4_agree() {
    let validator = SSRFValidator::new();
    for &(a, b, c, d) in BLOCKED_IPV4 {
        let ipv4 = Ipv4Addr::new(a, b, c, d);
        let ip = IpAddr::V4(ipv4);

        assert!(
            primitives::is_blocked_ipv4(&ipv4),
            "primitives::is_blocked_ipv4 should block {ipv4}"
        );
        assert!(
            validator.validate_ip(&ip).is_err(),
            "SSRFValidator::validate_ip should block {ip}"
        );
    }
}

#[test]
fn allowed_ipv4_agree() {
    let validator = SSRFValidator::new();
    for &(a, b, c, d) in ALLOWED_IPV4 {
        let ipv4 = Ipv4Addr::new(a, b, c, d);
        let ip = IpAddr::V4(ipv4);

        assert!(
            !primitives::is_blocked_ipv4(&ipv4),
            "primitives::is_blocked_ipv4 should allow {ipv4}"
        );
        assert!(
            validator.validate_ip(&ip).is_ok(),
            "SSRFValidator::validate_ip should allow {ip}"
        );
    }
}

#[test]
fn blocked_ipv6_agree() {
    let validator = SSRFValidator::new();

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
            primitives::is_blocked_ipv6(ipv6),
            "primitives::is_blocked_ipv6 should block {ipv6}"
        );
        assert!(
            validator.validate_ip(&ip).is_err(),
            "SSRFValidator::validate_ip should block {ip}"
        );
    }
}

#[test]
fn blocked_hostnames_agree() {
    let validator = SSRFValidator::new();
    for hostname in BLOCKED_HOSTNAMES {
        let url = format!("http://{hostname}/path");

        let prim_result = primitives::check_hostname(hostname);
        assert!(
            !prim_result.is_ok(),
            "primitives::check_hostname should block hostname: {hostname}"
        );

        assert!(
            validator.validate_url(&url).is_err(),
            "SSRFValidator should block hostname URL: {url}"
        );
    }
}

#[test]
fn allowed_hostnames_agree() {
    let validator = SSRFValidator::new();
    for hostname in ALLOWED_HOSTNAMES {
        let url = format!("https://{hostname}/path");

        let prim_result = primitives::check_hostname(hostname);
        assert!(
            prim_result.is_ok(),
            "primitives::check_hostname should allow hostname: {hostname}"
        );

        assert!(
            validator.validate_url(&url).is_ok(),
            "SSRFValidator should allow hostname URL: {url}"
        );
    }
}
