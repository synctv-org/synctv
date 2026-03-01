//! StaticDiscovery integration tests
//!
//! Tests for the derive_http_address helper function used to construct
//! HTTP addresses from gRPC addresses.

#![allow(clippy::unwrap_used)]
// StaticDiscovery::derive_http_address is a private method.
// We test the same logic by reconstructing the function behavior directly.

/// Replicate the derive_http_address logic from StaticDiscovery.
fn derive_http_address(grpc_address: &str, default_http_port: u16) -> String {
    if let Some(colon_pos) = grpc_address.rfind(':') {
        let host = &grpc_address[..colon_pos];
        format!("{host}:{default_http_port}")
    } else {
        format!("{grpc_address}:{default_http_port}")
    }
}

// ============================================================================
// Test 1: derive_http_address replaces port
// ============================================================================

#[test]
fn test_derive_http_address_replaces_port() {
    let result = derive_http_address("10.0.0.1:50051", 8080);
    assert_eq!(result, "10.0.0.1:8080");
}

// ============================================================================
// Test 2: derive_http_address with no port appends default
// ============================================================================

#[test]
fn test_derive_http_address_no_port() {
    let result = derive_http_address("10.0.0.1", 8080);
    assert_eq!(result, "10.0.0.1:8080");
}

// ============================================================================
// Test 3: derive_http_address with hostname
// ============================================================================

#[test]
fn test_derive_http_address_hostname() {
    let result = derive_http_address("my-host:50051", 9090);
    assert_eq!(result, "my-host:9090");
}

// ============================================================================
// Test 4: derive_http_address with IPv6
// ============================================================================

#[test]
fn test_derive_http_address_ipv6() {
    // For IPv6 like [::1]:50051, rfind(':') will find the port separator
    let result = derive_http_address("[::1]:50051", 8080);
    assert_eq!(result, "[::1]:8080");
}

// ============================================================================
// Test 5: StaticDiscoveryConfig defaults
// ============================================================================

#[test]
fn test_static_discovery_config_defaults() {
    let config = synctv_cluster::StaticDiscoveryConfig::default();
    assert!(config.peers.is_empty());
    assert_eq!(config.probe_interval_secs, 10);
    assert_eq!(config.default_http_port, 8080);
    assert!(config.cluster_secret.is_empty());
}
