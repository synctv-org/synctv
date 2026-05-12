//! Proxy tests around runtime behavior and explicit SSRF policies.
//!
//! Runtime proxy clients receive an explicit SSRF policy from the application.
//! These tests cover deterministic runtime failures plus strict-policy checks.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;

use synctv_proxy::{proxy_fetch_and_forward, NoopMetrics, ProxyConfig};

fn proxy_client() -> reqwest::Client {
    synctv_proxy::build_proxy_http_client(synctv_common::ssrf::SsrfGuard::disabled())
        .expect("proxy HTTP client should build for tests")
}

/// Verify that a loopback target still fails when nothing is listening.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_loopback_target_without_listener_returns_error() {
    let client = proxy_client();
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let cfg = ProxyConfig {
        ssrf_guard: &ssrf_guard,
        client: &client,
        url: "http://127.0.0.1:8080/admin",
        provider_headers: &HashMap::new(),
        range_header: None,
        request_control: None,
        upstream_header_timeout: None,
    };
    let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
    assert!(
        result.is_err(),
        "loopback target without a listener should fail when SSRF is explicitly disabled"
    );
}

#[test]
fn test_proxy_disabled_ssrf_policy_has_no_dns_resolver() {
    assert!(synctv_common::ssrf::SsrfGuard::disabled()
        .dns_resolver()
        .is_none());
}

#[test]
fn test_proxy_strict_ssrf_policy_still_blocks_private_and_metadata_ips() {
    for ip in ["127.0.0.1", "192.168.1.1", "169.254.169.254", "::1"] {
        let ip = ip.parse().unwrap();
        assert!(
            synctv_common::ssrf::SsrfGuard::strict_policy().is_ip_blocked(&ip),
            "strict SSRF policy should block {ip}"
        );
    }

    for ip in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
        let ip = ip.parse().unwrap();
        assert!(
            !synctv_common::ssrf::SsrfGuard::strict_policy().is_ip_blocked(&ip),
            "strict SSRF policy should allow public IP {ip}"
        );
    }
}
