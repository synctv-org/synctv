//! Tests for proxy client behavior and explicit SSRF ACL semantics.
//!
//! Runtime proxy clients receive an explicit SSRF policy from the application.
//! These tests distinguish disabled test behavior from strict-policy checks.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;
use synctv_proxy::{proxy_fetch_and_forward, NoopMetrics, ProxyConfig};

fn proxy_client() -> reqwest::Client {
    synctv_proxy::build_proxy_http_client(synctv_common::ssrf::SsrfGuard::disabled())
        .expect("proxy HTTP client should build for tests")
}

// DNS-level SSRF protection tests

/// Verify that an upstream connection failure is surfaced with the disabled policy.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_client_loopback_target_with_connection_close_fails() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind an ephemeral loopback port");
    let address = listener
        .local_addr()
        .expect("test listener should expose its address")
        .to_string();
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
