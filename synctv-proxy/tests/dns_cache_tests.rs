//! Explicit SSRF policy tests.

#![allow(clippy::unwrap_used)]
/// Verify that the explicit disabled policy has no ACL resolver.
#[test]
fn test_disabled_ssrf_policy_has_no_acl_resolver() {
    let guard = synctv_common::ssrf::SsrfGuard::disabled();
    assert!(guard.acl().is_none());
    assert!(guard.dns_resolver().is_none());
}

/// Verify that `strict_policy()` correctly identifies blocked IPs.
#[test]
fn test_ssrf_acl_blocks_private_ranges() {
    use std::net::IpAddr;

    let blocked: Vec<IpAddr> = vec![
        "127.0.0.1".parse().unwrap(),
        "10.0.0.1".parse().unwrap(),
        "192.168.1.1".parse().unwrap(),
        "172.16.0.1".parse().unwrap(),
        "169.254.169.254".parse().unwrap(),
        "::1".parse().unwrap(),
    ];
    for ip in &blocked {
        assert!(
            synctv_common::ssrf::SsrfGuard::strict_policy().is_ip_blocked(ip),
            "strict SSRF policy should block {ip}"
        );
    }

    let allowed: Vec<IpAddr> = vec![
        "1.1.1.1".parse().unwrap(),
        "8.8.8.8".parse().unwrap(),
        "93.184.216.34".parse().unwrap(),
    ];
    for ip in &allowed {
        assert!(
            !synctv_common::ssrf::SsrfGuard::strict_policy().is_ip_blocked(ip),
            "IP {ip} should be allowed"
        );
    }
}
