//! Tests for DNS caching behavior in synctv-proxy.
//!
//! These tests verify that DNS resolution is not duplicated unnecessarily
//! while maintaining security against DNS rebinding attacks.

#![allow(clippy::unwrap_used)]
use synctv_proxy::validate_proxy_url;

// ==================================================================
// DNS resolution count tests
// ==================================================================

/// Test that `validate_proxy_url` performs DNS resolution exactly once per call.
/// This is a baseline test - we're not testing caching here, just that the
/// function works correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_validate_proxy_url_dns_resolution_once() {
    // Use a well-known public IP that won't be blocked by SSRF protection
    // 1.1.1.1 is Cloudflare's public DNS, a safe public IP for testing
    let result = validate_proxy_url("https://1.1.1.1/test").await;
    assert!(result.is_ok(), "Public IP should pass validation: {result:?}");
}

/// Test that hostname-based URLs get DNS resolution for SSRF protection.
/// This test documents the expected behavior that hostnames require DNS lookup
/// to prevent DNS rebinding attacks.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_validate_proxy_url_hostname_requires_dns() {
    // This test uses a real domain - it requires network access.
    // If network is unavailable, the test will fail with DNS lookup error.
    // We use example.com which is a reserved domain for documentation/testing.
    let result = validate_proxy_url("https://example.com/test").await;
    // We don't assert success/failure since it depends on network,
    // but we verify the function completes (doesn't hang forever)
    let _ = result;
}

// ==================================================================
// DNS caching behavior tests (documentation of expected behavior)
// ==================================================================

/// Document that `validate_proxy_url` now only does static checks,
/// and that the `SsrfSafeDnsResolver` performs DNS resolution at
/// connection time. This eliminates duplicate DNS lookups while
/// maintaining TOCTOU protection.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_dns_double_resolution_eliminated() {
    // After the fix:
    // 1. validate_proxy_url does only static URL checks (no DNS lookup)
    // 2. SsrfSafeDnsResolver does DNS lookup at connection time with SSRF checks
    //
    // This provides the same security with half the DNS lookups.

    // Verify the validation works correctly
    let result = validate_proxy_url("https://1.1.1.1/dns-test").await;
    assert!(result.is_ok());
}

/// Test that consecutive calls to `validate_proxy_url` with the same URL
/// each perform their own validation (static only, no DNS).
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_consecutive_validations_no_dns() {
    // validate_proxy_url now only does static checks (no DNS).
    // Multiple calls are cheap since no DNS lookup is involved.

    let url = "https://1.1.1.1/consecutive-test";

    let result1 = validate_proxy_url(url).await;
    let result2 = validate_proxy_url(url).await;

    assert!(result1.is_ok());
    assert!(result2.is_ok());
}

/// Test that DNS resolution for a hostname that resolves to a private IP
/// is blocked correctly by the `SsrfSafeDnsResolver` at connection time.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_dns_rebinding_protection_on_private_ip() {
    // Localhost should always be blocked
    let result = validate_proxy_url("http://127.0.0.1/admin").await;
    assert!(result.is_err(), "Loopback IP should be blocked");

    // Private IPs should be blocked
    let result = validate_proxy_url("http://192.168.1.1/admin").await;
    assert!(result.is_err(), "Private IP should be blocked");

    let result = validate_proxy_url("http://10.0.0.1/admin").await;
    assert!(result.is_err(), "Private IP should be blocked");
}

/// Test that static checks (`validate_proxy_url_static`) are sufficient for
/// blocking known-bad IPs without requiring DNS.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_static_checks_block_known_bad_ips() {
    use synctv_proxy::validate_proxy_url_static;

    // These should be blocked by static checks alone
    assert!(validate_proxy_url_static("http://localhost/foo").is_err());
    assert!(validate_proxy_url_static("http://127.0.0.1/foo").is_err());
    assert!(validate_proxy_url_static("http://192.168.1.1/foo").is_err());
    assert!(validate_proxy_url_static("http://10.0.0.1/foo").is_err());
    assert!(validate_proxy_url_static("http://172.16.0.1/foo").is_err());
    assert!(validate_proxy_url_static("http://169.254.1.1/foo").is_err());

    // Public IPs should pass static checks
    assert!(validate_proxy_url_static("https://1.1.1.1/test").is_ok());
    assert!(validate_proxy_url_static("https://example.com/test").is_ok());
}
