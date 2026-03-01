//! SSRF protection tests for `RtmpProvider`
//!
//! These tests verify that the `RtmpProvider` validates its `base_url` configuration
//! and `source_config` fields to prevent Server-Side Request Forgery attacks.
#![allow(clippy::unwrap_used)]

use synctv_core::provider::{MediaProvider, ProviderContext, ProviderError, RtmpProvider};
use serde_json::json;

const fn create_context() -> ProviderContext<'static> {
    ProviderContext::new("synctv")
        .with_user_id("test_user")
        .with_room_id("test_room")
}

// ============================================================================
// SSRF Protection: base_url validation at provider creation time
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_rejects_private_ip_base_url() {
    // RtmpProvider should reject base_url pointing to private IP addresses
    let private_ips = vec![
        "http://192.168.1.1:8080",
        "http://10.0.0.1:8080",
        "http://172.16.0.1:8080",
        "http://127.0.0.1:8080",
    ];

    for base_url in private_ips {
        let result = RtmpProvider::new_validated(base_url);
        assert!(
            result.is_err(),
            "RtmpProvider should reject private IP base_url: {base_url}"
        );
        // Check error message content
        if let Err(err) = result {
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("SSRF") || err_msg.contains("private"),
                "Error should mention SSRF protection or private address for: {base_url}, got: {err_msg}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_rejects_localhost_base_url() {
    // RtmpProvider should reject base_url with localhost
    let localhost_urls = vec![
        "http://localhost:8080",
        "https://localhost:8080",
        "http://localhost",
    ];

    for base_url in localhost_urls {
        let result = RtmpProvider::new_validated(base_url);
        assert!(
            result.is_err(),
            "RtmpProvider should reject localhost base_url: {base_url}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_rejects_link_local_base_url() {
    // RtmpProvider should reject link-local addresses (169.254.0.0/16)
    let link_local_urls = vec![
        "http://169.254.1.1:8080",
        "http://169.254.169.254:8080", // AWS metadata address
    ];

    for base_url in link_local_urls {
        let result = RtmpProvider::new_validated(base_url);
        assert!(
            result.is_err(),
            "RtmpProvider should reject link-local base_url: {base_url}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_rejects_cloud_metadata_hostnames() {
    // RtmpProvider should reject hostnames that could resolve to cloud metadata endpoints
    let metadata_hostnames = vec![
        "http://metadata.google.internal:8080",
        "http://instance-data:8080",
        "http://169.254.169.254:8080",
    ];

    for base_url in metadata_hostnames {
        let result = RtmpProvider::new_validated(base_url);
        assert!(
            result.is_err(),
            "RtmpProvider should reject cloud metadata hostname: {base_url}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_accepts_public_base_url() {
    // RtmpProvider should accept base_url pointing to public addresses
    let public_urls = vec![
        "https://example.com",
        "https://api.example.com:8080",
        "http://93.184.216.34:8080", // example.com IP
        "https://synctv.example.com",
    ];

    for base_url in public_urls {
        let result = RtmpProvider::new_validated(base_url);
        assert!(
            result.is_ok(),
            "RtmpProvider should accept public base_url: {base_url}, error: {:?}",
            result.err()
        );
    }
}

// ============================================================================
// SSRF Protection: validate_source_config should not accept external URLs
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_validate_source_config_rejects_url_field() {
    // RtmpProvider's source_config should NOT accept external URLs
    // If a URL field is present, it should be rejected to prevent SSRF
    let provider = RtmpProvider::new("https://example.com");
    let ctx = create_context();

    let malicious_configs = vec![
        // Direct URL field
        json!({
            "room_id": "room123",
            "media_id": "media456",
            "url": "http://192.168.1.1/internal"
        }),
        // RTMP URL field
        json!({
            "room_id": "room123",
            "media_id": "media456",
            "rtmp_url": "rtmp://localhost/live/stream"
        }),
        // Source URL field
        json!({
            "room_id": "room123",
            "media_id": "media456",
            "source_url": "http://169.254.169.254/metadata"
        }),
    ];

    for config in malicious_configs {
        let result = provider.validate_source_config(&ctx, &config).await;
        assert!(
            result.is_err(),
            "RtmpProvider should reject source_config with URL fields: {config}"
        );
        // Check error type
        if let Err(ProviderError::InvalidConfig(msg)) = result {
            assert!(
                msg.contains("URL") || msg.contains("not supported") || msg.contains("invalid"),
                "Error message should mention URL or not supported: {msg}"
            );
        } else if let Err(err) = result {
            panic!("Expected InvalidConfig error, got: {err:?}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_validate_source_config_accepts_valid_fields() {
    // RtmpProvider should accept source_config with only room_id and media_id
    let provider = RtmpProvider::new("https://example.com");
    let ctx = create_context();

    let valid_config = json!({
        "room_id": "room123",
        "media_id": "media456"
    });

    let result = provider.validate_source_config(&ctx, &valid_config).await;
    assert!(
        result.is_ok(),
        "RtmpProvider should accept valid source_config: {:?}",
        result.err()
    );
}

// ============================================================================
// SSRF Protection: IPv6 addresses
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_rejects_ipv6_loopback_base_url() {
    // RtmpProvider should reject IPv6 loopback
    let ipv6_loopback_urls = vec![
        "http://[::1]:8080",
        "http://[0:0:0:0:0:0:0:1]:8080",
    ];

    for base_url in ipv6_loopback_urls {
        let result = RtmpProvider::new_validated(base_url);
        assert!(
            result.is_err(),
            "RtmpProvider should reject IPv6 loopback base_url: {base_url}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_rejects_ipv6_unique_local_base_url() {
    // RtmpProvider should reject IPv6 unique local addresses (fc00::/7)
    let ipv6_ula_urls = vec![
        "http://[fc00::1]:8080",
        "http://[fd00::1]:8080",
    ];

    for base_url in ipv6_ula_urls {
        let result = RtmpProvider::new_validated(base_url);
        assert!(
            result.is_err(),
            "RtmpProvider should reject IPv6 unique local base_url: {base_url}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_accepts_ipv6_public_base_url() {
    // RtmpProvider should accept public IPv6 addresses
    let ipv6_public_urls = vec![
        "http://[2001:4860:4860::8888]:8080", // Google DNS
        "https://[2606:4700:4700::1111]",      // Cloudflare DNS
    ];

    for base_url in ipv6_public_urls {
        let result = RtmpProvider::new_validated(base_url);
        assert!(
            result.is_ok(),
            "RtmpProvider should accept public IPv6 base_url: {base_url}, error: {:?}",
            result.err()
        );
    }
}

// ============================================================================
// SSRF Protection: IPv4-mapped IPv6 addresses
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_rejects_ipv4_mapped_private_base_url() {
    // RtmpProvider should detect and reject IPv4-mapped IPv6 private addresses
    let ipv4_mapped_urls = vec![
        "http://[::ffff:192.168.1.1]:8080",
        "http://[::ffff:10.0.0.1]:8080",
        "http://[::ffff:127.0.0.1]:8080",
    ];

    for base_url in ipv4_mapped_urls {
        let result = RtmpProvider::new_validated(base_url);
        assert!(
            result.is_err(),
            "RtmpProvider should reject IPv4-mapped private base_url: {base_url}"
        );
    }
}

// ============================================================================
// SSRF Protection: CGNAT range (100.64.0.0/10)
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_blocks_cgnat_base_url() {
    // CGNAT / Shared Address Space (100.64.0.0/10, RFC 6598) is blocked for SSRF protection.
    let cgnat_urls = vec![
        "http://100.64.0.1:8080",
        "http://100.100.100.100:8080",
        "http://100.127.255.255:8080",
    ];

    for base_url in cgnat_urls {
        let result = RtmpProvider::new_validated(base_url);
        assert!(
            result.is_err(),
            "RtmpProvider should block CGNAT base_url: {base_url}"
        );
    }
}
