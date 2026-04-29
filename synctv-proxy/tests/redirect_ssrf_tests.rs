//! Proxy tests around runtime behavior and explicit SSRF policies.
//!
//! The runtime proxy client uses the shared default SSRF policy, which is
//! intentionally disabled unless callers opt into a strict policy.
//! These tests therefore avoid assuming default runtime blocking and instead
//! cover deterministic runtime failures plus explicit strict-policy checks.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;

use synctv_proxy::{proxy_fetch_and_forward, NoopMetrics, ProxyConfig};

fn proxy_client() -> reqwest::Client {
    synctv_proxy::build_proxy_http_client().expect("proxy HTTP client should build for tests")
}

/// Verify that a loopback target still fails when nothing is listening.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_loopback_target_without_listener_returns_error() {
    let client = proxy_client();
    let cfg = ProxyConfig {
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
        "loopback target without a listener should fail even when default SSRF is disabled"
    );
}

#[test]
fn test_proxy_shared_default_ssrf_policy_is_disabled() {
    assert!(
        synctv_common::ssrf::SsrfGuard::shared_default()
            .dns_resolver()
            .is_none(),
        "default proxy runtime should not inject an SSRF DNS resolver"
    );
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
