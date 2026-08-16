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

/// Verify that an upstream connection failure is surfaced with the disabled policy.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_loopback_target_with_connection_close_returns_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind an ephemeral loopback port");
    let address = listener
        .local_addr()
        .expect("test listener should expose local addr");
    let connection_close_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;

        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer).await;
            drop(stream);
        }
    });

    let client = proxy_client();
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let url = format!("http://{address}/admin");
    let cfg = ProxyConfig {
        ssrf_guard: &ssrf_guard,
        client: &client,
        url: &url,
        provider_headers: &HashMap::new(),
        range_header: None,
        request_control: None,
        upstream_header_timeout: None,
    };
    let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
    connection_close_task.abort();
    assert!(
        result.is_err(),
        "loopback target with a closing connection should fail when SSRF is explicitly disabled"
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
