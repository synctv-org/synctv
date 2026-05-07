//! Tests for proxy client behavior and explicit SSRF ACL semantics.
//!
//! Runtime proxy clients now use the shared default SSRF policy, which is
//! intentionally disabled unless callers opt into a strict policy.
//! These tests therefore distinguish:
//! - runtime behavior of the default proxy client
//! - explicit blocking behavior of `SsrfGuard::strict_policy()`

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;
use std::net::TcpListener;
use synctv_proxy::{proxy_fetch_and_forward, NoopMetrics, ProxyConfig};

fn proxy_client() -> reqwest::Client {
    synctv_proxy::build_proxy_http_client().expect("proxy HTTP client should build for tests")
}

// DNS-level SSRF protection tests

/// Verify that a loopback target still fails when no local server is listening.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_client_loopback_target_fails_without_listener() {
    let listener =
        TcpListener::bind("127.0.0.1:0").expect("test should reserve an unused loopback port");
    let unused_port = listener
        .local_addr()
        .expect("test listener should expose its address")
        .port();
    drop(listener);

    let client = proxy_client();
    let url = format!("http://127.0.0.1:{unused_port}/admin");
    let cfg = ProxyConfig {
        client: &client,
        url: &url,
        provider_headers: &HashMap::new(),
        range_header: None,
        request_control: None,
        upstream_header_timeout: None,
    };
    let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
    assert!(
        result.is_err(),
        "loopback target without a listener should fail even when default SSRF is disabled"
    );
}

/// Verify that the shared default policy is disabled for proxy clients.
#[test]
fn test_shared_default_ssrf_policy_is_disabled() {
    assert!(synctv_common::ssrf::SsrfGuard::shared_default()
        .acl()
        .is_none());
    assert!(synctv_common::ssrf::SsrfGuard::shared_default()
        .dns_resolver()
        .is_none());
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
