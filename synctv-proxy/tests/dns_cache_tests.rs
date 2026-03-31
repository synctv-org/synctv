//! Tests for DNS-level SSRF protection in synctv-proxy.
//!
//! SSRF protection is enforced at the DNS resolver level via `synctv-common`.
//! The proxy HTTP client uses `SsrfGuard::shared_default().dns_resolver()`
//! which blocks connections
//! to private/internal IP addresses at DNS resolution time.
//!
//! These tests verify that the DNS resolver correctly blocks private IPs
//! and allows public IPs.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;
use synctv_proxy::{proxy_fetch_and_forward, NoopMetrics, ProxyConfig};

fn proxy_client() -> reqwest::Client {
    synctv_proxy::build_proxy_http_client().expect("proxy HTTP client should build for tests")
}

// ==================================================================
// DNS-level SSRF protection tests
// ==================================================================

/// Verify that the DNS resolver blocks loopback addresses.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_dns_resolver_blocks_loopback() {
    let headers = axum::http::HeaderMap::new();
    let client = proxy_client();
    let cfg = ProxyConfig {
        client: &client,
        url: "http://127.0.0.1/admin",
        provider_headers: &HashMap::new(),
        client_headers: &headers,
    };
    let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
    assert!(result.is_err(), "Should block loopback IP via DNS resolver");
}

/// Verify that the DNS resolver blocks private IP ranges.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_dns_resolver_blocks_private_ips() {
    let private_ips = [
        "http://192.168.1.1/secret",
        "http://10.0.0.1/internal",
        "http://172.16.0.1/admin",
    ];

    for url in &private_ips {
        let headers = axum::http::HeaderMap::new();
        let client = proxy_client();
        let cfg = ProxyConfig {
            client: &client,
            url,
            provider_headers: &HashMap::new(),
            client_headers: &headers,
        };
        let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
        assert!(
            result.is_err(),
            "Should block private IP {url} via DNS resolver"
        );
    }
}

/// Verify that the DNS resolver blocks cloud metadata endpoints.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_dns_resolver_blocks_cloud_metadata() {
    let headers = axum::http::HeaderMap::new();
    let client = proxy_client();
    let cfg = ProxyConfig {
        client: &client,
        url: "http://169.254.169.254/latest/meta-data/",
        provider_headers: &HashMap::new(),
        client_headers: &headers,
    };
    let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
    assert!(
        result.is_err(),
        "Should block cloud metadata IP via DNS resolver"
    );
}

/// Verify that `synctv_common::ssrf::is_ip_blocked` correctly identifies blocked IPs.
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
            synctv_common::ssrf::SsrfGuard::shared_default().is_ip_blocked(ip),
            "IP {ip} should be blocked"
        );
    }

    let allowed: Vec<IpAddr> = vec![
        "1.1.1.1".parse().unwrap(),
        "8.8.8.8".parse().unwrap(),
        "93.184.216.34".parse().unwrap(),
    ];
    for ip in &allowed {
        assert!(
            !synctv_common::ssrf::SsrfGuard::shared_default().is_ip_blocked(ip),
            "IP {ip} should be allowed"
        );
    }
}
