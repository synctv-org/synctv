//! SSRF protection tests for the proxy module.
//!
//! SSRF protection is now enforced at the DNS resolver level via `synctv-common`.
//! The proxy HTTP client uses `SsrfGuard::shared_default().dns_resolver()`
//! which blocks connections
//! to private/internal IP addresses at DNS resolution time.
//!
//! These tests verify that `proxy_fetch_and_forward` blocks SSRF attempts
//! through the DNS-level protection.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;

use synctv_proxy::{proxy_fetch_and_forward, NoopMetrics, ProxyConfig};

fn proxy_client() -> reqwest::Client {
    synctv_proxy::build_proxy_http_client().expect("proxy HTTP client should build for tests")
}

/// Verify that `proxy_fetch_and_forward` blocks private IP targets
/// through the SSRF-safe DNS resolver.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_blocks_private_ip_via_dns() {
    let headers = axum::http::HeaderMap::new();
    let client = proxy_client();
    let cfg = ProxyConfig {
        client: &client,
        url: "http://192.168.1.1/secret",
        provider_headers: &HashMap::new(),
        client_headers: &headers,
    };
    let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
    assert!(
        result.is_err(),
        "Should block private IP URL via DNS resolver"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_blocks_loopback_via_dns() {
    let headers = axum::http::HeaderMap::new();
    let client = proxy_client();
    let cfg = ProxyConfig {
        client: &client,
        url: "http://127.0.0.1:8080/admin",
        provider_headers: &HashMap::new(),
        client_headers: &headers,
    };
    let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
    assert!(
        result.is_err(),
        "Should block loopback URL via DNS resolver"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_blocks_cloud_metadata_via_dns() {
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
