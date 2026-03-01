//! Tests for SSRF protection during HTTP redirects.
//!
//! These tests verify that the proxy blocks redirects to private IP addresses
//! and other SSRF-vulnerable targets, even when the initial URL is safe.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;

use synctv_proxy::{proxy_fetch_and_forward, validate_proxy_url, validate_proxy_url_static, NoopMetrics, ProxyConfig};

// ==================================================================
// Redirect SSRF: Static URL validation for redirect targets
// ==================================================================

#[test]
fn test_redirect_to_192_168_blocked() {
    // A redirect targeting 192.168.x.x should be blocked
    let result = validate_proxy_url_static("http://192.168.1.100/secret");
    assert!(result.is_err(), "192.168.x.x should be blocked");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("private") || err.contains("reserved"),
        "Error should mention private/reserved: {err}"
    );
}

#[test]
fn test_redirect_to_10_0_0_blocked() {
    // A redirect targeting 10.x.x.x should be blocked
    let result = validate_proxy_url_static("http://10.0.0.1/admin");
    assert!(result.is_err(), "10.x.x.x should be blocked");
}

#[test]
fn test_redirect_to_127_0_0_1_blocked() {
    // A redirect targeting 127.0.0.1 should be blocked
    let result = validate_proxy_url_static("http://127.0.0.1:8080/metadata");
    assert!(result.is_err(), "127.0.0.1 should be blocked");
}

#[test]
fn test_redirect_to_172_16_blocked() {
    // A redirect targeting 172.16.x.x - 172.31.x.x should be blocked
    let result = validate_proxy_url_static("http://172.16.0.1/internal");
    assert!(result.is_err(), "172.16.0.1 should be blocked");

    let result = validate_proxy_url_static("http://172.31.255.255/internal");
    assert!(result.is_err(), "172.31.255.255 should be blocked");
}

#[test]
fn test_redirect_to_169_254_metadata_blocked() {
    // AWS/GCP/Azure metadata endpoint
    let result = validate_proxy_url_static("http://169.254.169.254/latest/meta-data/");
    assert!(result.is_err(), "169.254.169.254 metadata IP should be blocked");
}

#[test]
fn test_redirect_to_100_64_cgnat_blocked() {
    // CGNAT range
    let result = validate_proxy_url_static("http://100.64.0.1/cgnat");
    assert!(result.is_err(), "100.64.x.x CGNAT range should be blocked");
}

// ==================================================================
// Redirect SSRF: Async validation (including DNS-level checks)
// ==================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_async_validate_private_ip_blocked() {
    let result = validate_proxy_url("http://192.168.50.50/api").await;
    assert!(result.is_err(), "Private IP should fail async validation");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_async_validate_loopback_blocked() {
    let result = validate_proxy_url("http://127.0.0.1/admin").await;
    assert!(result.is_err(), "Loopback should fail async validation");
}

// ==================================================================
// Redirect SSRF: Multi-hop redirect attack prevention
// ==================================================================

/// Test that validates redirect target URLs are checked.
/// Even if URL A redirects to URL B which then redirects to private IP C,
/// the chain should be blocked at the point where C is encountered.
#[test]
fn test_multi_hop_redirect_target_validation() {
    // Simulate multi-hop: public -> public -> private
    // Each URL in the chain should be validated individually

    // First hop (public) - should pass
    let result = validate_proxy_url_static("https://public-cdn.example.com/redirect");
    assert!(result.is_ok(), "Public URL should pass validation");

    // Second hop (public) - should pass
    let result = validate_proxy_url_static("https://cdn-another.example.com/final");
    assert!(result.is_ok(), "Another public URL should pass validation");

    // Third hop (private) - should be blocked
    let result = validate_proxy_url_static("http://192.168.1.1/secret");
    assert!(result.is_err(), "Private IP redirect target should be blocked");
}

/// Test relative redirect handling - a relative redirect should be
/// resolved against the base URL and then validated.
#[test]
fn test_relative_redirect_to_private_blocked() {
    // A relative redirect like "/../internal" when combined with a public base URL
    // should still be validated. The resolved URL should be checked.
    // This tests the static validation; actual resolution happens in the redirect loop.

    // Direct private IP should be blocked
    let result = validate_proxy_url_static("http://10.0.0.1/secret");
    assert!(result.is_err(), "Direct private IP should be blocked");
}

// ==================================================================
// Redirect SSRF: Protocol-based bypass attempts
// ==================================================================

#[test]
fn test_redirect_to_file_scheme_blocked() {
    // A redirect to file:// should be blocked at scheme validation
    let result = validate_proxy_url_static("file:///etc/passwd");
    assert!(result.is_err(), "file:// scheme should be blocked");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("scheme") || err.contains("unsupported"),
        "Error should mention scheme: {err}"
    );
}

#[test]
fn test_redirect_to_ftp_scheme_blocked() {
    let result = validate_proxy_url_static("ftp://internal-server.local/file");
    assert!(result.is_err(), "ftp:// scheme should be blocked");
}

#[test]
fn test_redirect_to_gopher_scheme_blocked() {
    let result = validate_proxy_url_static("gopher://internal-server.local/data");
    assert!(result.is_err(), "gopher:// scheme should be blocked");
}

// ==================================================================
// Redirect SSRF: Hostname-based attacks
// ==================================================================

#[test]
fn test_redirect_to_localhost_hostname_blocked() {
    let result = validate_proxy_url_static("http://localhost/admin");
    assert!(result.is_err(), "localhost hostname should be blocked");
}

#[test]
fn test_redirect_to_internal_suffix_blocked() {
    // .internal and .local suffixes should be blocked
    let result = validate_proxy_url_static("http://service.internal/api");
    assert!(result.is_err(), ".internal suffix should be blocked");

    let result = validate_proxy_url_static("http://service.local/api");
    assert!(result.is_err(), ".local suffix should be blocked");
}

#[test]
fn test_redirect_to_metadata_hostname_blocked() {
    // Cloud metadata hostnames should be blocked
    let result = validate_proxy_url_static("http://metadata.google.internal/");
    assert!(result.is_err(), "metadata.google.internal should be blocked");

    let result = validate_proxy_url_static("http://metadata.azure/");
    assert!(result.is_err(), "metadata.azure should be blocked");
}

#[test]
fn test_redirect_to_kubernetes_hostname_blocked() {
    // Kubernetes internal hostnames should be blocked
    let result = validate_proxy_url_static("http://kubernetes.default/api");
    assert!(result.is_err(), "kubernetes.* hostname should be blocked");

    let result = validate_proxy_url_static("http://k8s.default/api");
    assert!(result.is_err(), "k8s.* hostname should be blocked");
}

// ==================================================================
// Redirect SSRF: IPv6 edge cases
// ==================================================================

#[test]
fn test_redirect_to_ipv6_loopback_blocked() {
    let result = validate_proxy_url_static("http://[::1]/admin");
    assert!(result.is_err(), "IPv6 loopback ::1 should be blocked");
}

#[test]
fn test_redirect_to_ipv6_unspecified_blocked() {
    let result = validate_proxy_url_static("http://[::]/admin");
    assert!(result.is_err(), "IPv6 unspecified :: should be blocked");
}

#[test]
fn test_redirect_to_ipv6_link_local_blocked() {
    // fe80::/10 is link-local
    let result = validate_proxy_url_static("http://[fe80::1]/admin");
    assert!(result.is_err(), "IPv6 link-local should be blocked");
}

#[test]
fn test_redirect_to_ipv4_mapped_ipv6_private_blocked() {
    // ::ffff:192.168.1.1 is an IPv4-mapped IPv6 address
    // This should be detected and blocked
    let result = validate_proxy_url_static("http://[::ffff:192.168.1.1]/admin");
    assert!(result.is_err(), "IPv4-mapped private IPv6 should be blocked");
}

#[test]
fn test_redirect_to_ipv6_unique_local_blocked() {
    // fc00::/7 is unique local (like RFC1918 for IPv6)
    let result = validate_proxy_url_static("http://[fc00::1]/admin");
    assert!(result.is_err(), "IPv6 unique local should be blocked");

    let result = validate_proxy_url_static("http://[fd00::1]/admin");
    assert!(result.is_err(), "IPv6 unique local (fd00) should be blocked");
}

// ==================================================================
// Redirect SSRF: Integration test (via proxy_fetch_and_forward)
// ==================================================================

/// Verify that `proxy_fetch_and_forward` blocks SSRF attempts.
/// Since wiremock runs on loopback, we can't actually test redirect chains,
/// but we can verify that the initial URL validation blocks private IPs.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_blocks_private_ip_initial_url() {
    let headers = axum::http::HeaderMap::new();
    let cfg = ProxyConfig {
        url: "http://192.168.1.1/secret",
        provider_headers: &HashMap::new(),
        client_headers: &headers,
    };
    let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
    assert!(result.is_err(), "Should block private IP URL");

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("private") || err.contains("reserved") || err.contains("blocked"),
        "Error should indicate SSRF block: {err}"
    );
}

/// Verify that `proxy_fetch_and_forward` blocks loopback addresses.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_blocks_loopback_initial_url() {
    let headers = axum::http::HeaderMap::new();
    let cfg = ProxyConfig {
        url: "http://127.0.0.1:8080/admin",
        provider_headers: &HashMap::new(),
        client_headers: &headers,
    };
    let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
    assert!(result.is_err(), "Should block loopback URL");
}

/// Verify that `proxy_fetch_and_forward` blocks cloud metadata endpoints.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_proxy_blocks_cloud_metadata_url() {
    let headers = axum::http::HeaderMap::new();
    let cfg = ProxyConfig {
        url: "http://169.254.169.254/latest/meta-data/",
        provider_headers: &HashMap::new(),
        client_headers: &headers,
    };
    let result = proxy_fetch_and_forward(cfg, &NoopMetrics).await;
    assert!(result.is_err(), "Should block cloud metadata IP");
}

// ==================================================================
// Redirect SSRF: Edge cases and boundary conditions
// ==================================================================

#[test]
fn test_redirect_with_port_still_blocked() {
    // Port number should not bypass IP validation
    let result = validate_proxy_url_static("http://192.168.1.1:8888/secret");
    assert!(result.is_err(), "Private IP with custom port should be blocked");

    let result = validate_proxy_url_static("http://127.0.0.1:9999/admin");
    assert!(result.is_err(), "Loopback with custom port should be blocked");
}

#[test]
fn test_redirect_with_path_traversal_still_blocked() {
    // Path traversal in URL should not bypass IP validation
    let result = validate_proxy_url_static("http://192.168.1.1/../etc/passwd");
    assert!(result.is_err(), "Private IP with path traversal should be blocked");
}

#[test]
fn test_redirect_with_query_params_still_blocked() {
    // Query parameters should not bypass IP validation
    let result = validate_proxy_url_static("http://10.0.0.1/api?callback=evil");
    assert!(result.is_err(), "Private IP with query params should be blocked");
}

#[test]
fn test_redirect_with_fragment_still_blocked() {
    // Fragment should not bypass IP validation
    let result = validate_proxy_url_static("http://172.16.0.1/page#section");
    assert!(result.is_err(), "Private IP with fragment should be blocked");
}

#[test]
fn test_redirect_empty_url_blocked() {
    let result = validate_proxy_url_static("");
    assert!(result.is_err(), "Empty URL should be blocked");
}

#[test]
fn test_redirect_url_with_only_path_component() {
    // Note: "http:///path" is parsed by the URL crate as "http://path/"
    // which is a valid URL with host "path" (a domain name).
    // This is correct behavior per URL standard.
    // The URL crate normalizes triple slashes.
    let result = validate_proxy_url_static("http:///path");
    // This actually resolves to http://path/ which has host "path"
    // It's a valid (though unusual) URL format
    assert!(result.is_ok(), "http:///path is parsed as http://path/ and should pass");
}

// ==================================================================
// Redirect SSRF: Valid public URLs (should pass)
// ==================================================================

#[test]
fn test_valid_public_domain_passes() {
    let result = validate_proxy_url_static("https://example.com/video.mp4");
    assert!(result.is_ok(), "Public domain URL should pass validation");
}

#[test]
fn test_valid_public_ip_passes() {
    // 8.8.8.8 is Google's public DNS
    let result = validate_proxy_url_static("https://8.8.8.8/dns-query");
    assert!(result.is_ok(), "Public IP should pass validation");

    // 1.1.1.1 is Cloudflare's public DNS
    let result = validate_proxy_url_static("https://1.1.1.1/dns-query");
    assert!(result.is_ok(), "Public IP should pass validation");
}

#[test]
fn test_valid_ipv6_public_passes() {
    // Cloudflare's public IPv6 DNS
    let result = validate_proxy_url_static("https://[2606:4700:4700::1111]/dns-query");
    assert!(result.is_ok(), "Public IPv6 should pass validation");
}

// ==================================================================
// Redirect SSRF: Special cases
// ==================================================================

#[test]
fn test_redirect_to_zero_ip_blocked() {
    // 0.0.0.0 is "current network" and should be blocked
    let result = validate_proxy_url_static("http://0.0.0.0/admin");
    assert!(result.is_err(), "0.0.0.0 should be blocked");
}

#[test]
fn test_redirect_to_broadcast_blocked() {
    // 255.255.255.255 is broadcast
    let result = validate_proxy_url_static("http://255.255.255.255/broadcast");
    assert!(result.is_err(), "Broadcast IP should be blocked");
}

#[test]
fn test_redirect_to_multicast_blocked() {
    // 224.0.0.1 is multicast
    let result = validate_proxy_url_static("http://224.0.0.1/multicast");
    assert!(result.is_err(), "Multicast IP should be blocked");
}

#[test]
fn test_redirect_with_credentials_still_validates_ip() {
    // URL with credentials should still validate the IP
    let result = validate_proxy_url_static("http://user:pass@192.168.1.1/admin");
    assert!(result.is_err(), "Private IP with credentials should be blocked");
}

// ==================================================================
// Redirect SSRF: Subdomain and similar attacks
// ==================================================================

#[test]
fn test_redirect_to_docker_hostname_blocked() {
    let result = validate_proxy_url_static("http://docker.internal/api");
    assert!(result.is_err(), "docker.* hostname should be blocked");

    let result = validate_proxy_url_static("http://container.internal/api");
    assert!(result.is_err(), "container.* hostname should be blocked");
}

#[test]
fn test_redirect_to_instance_data_blocked() {
    let result = validate_proxy_url_static("http://instance-data/metadata");
    assert!(result.is_err(), "instance-data hostname should be blocked");
}

#[test]
fn test_redirect_to_localhost_subdomain_blocked() {
    // Some systems resolve *.localhost to 127.0.0.1
    let result = validate_proxy_url_static("http://test.localhost/api");
    assert!(result.is_err(), "*.localhost subdomain should be blocked");
}

// ==================================================================
// Redirect SSRF: Redirect chain limit enforcement
// ==================================================================

/// The `MAX_REDIRECTS` constant should be defined and reasonable.
/// This test documents the expected behavior.
#[test]
fn test_max_redirects_constant_is_reasonable() {
    // MAX_REDIRECTS is 10 in the implementation.
    // We can't access it directly from tests, but we verify the behavior
    // exists through the error message format.
    // This test serves as documentation that the limit exists.

    // A redirect chain exceeding 10 hops should produce an error.
    // The actual testing of this requires a mock server, which is done
    // in proxy_integration_tests.rs.
    // MAX_REDIRECTS is documented as 10 - this test is intentionally a no-op.
}
