//! `StaticDiscovery` integration tests
//!
//! Tests for the `derive_http_address` helper function used to construct
//! HTTP addresses from gRPC addresses.

#![allow(clippy::unwrap_used)]
// StaticDiscovery::derive_http_address is a private method.
// We test the same logic by reconstructing the function behavior directly.

/// Replicate the `derive_http_address` logic from `StaticDiscovery`.
fn derive_http_address(grpc_address: &str, default_http_port: u16) -> String {
    grpc_address.rfind(':').map_or_else(
        || format!("{grpc_address}:{default_http_port}"),
        |colon_pos| {
            let host = &grpc_address[..colon_pos];
            format!("{host}:{default_http_port}")
        },
    )
}

#[test]
fn test_derive_http_address_replaces_port() {
    let result = derive_http_address("10.0.0.1:50051", 8080);
    assert_eq!(result, "10.0.0.1:8080");
}

#[test]
fn test_derive_http_address_no_port() {
    let result = derive_http_address("10.0.0.1", 8080);
    assert_eq!(result, "10.0.0.1:8080");
}

#[test]
fn test_derive_http_address_hostname() {
    let result = derive_http_address("my-host:50051", 9090);
    assert_eq!(result, "my-host:9090");
}

#[test]
fn test_derive_http_address_ipv6() {
    // For IPv6 like [::1]:50051, rfind(':') will find the port separator
    let result = derive_http_address("[::1]:50051", 8080);
    assert_eq!(result, "[::1]:8080");
}

#[test]
fn test_static_discovery_config_defaults() {
    let config = synctv_cluster::StaticDiscoveryConfig::default();
    assert!(config.peers.is_empty());
    assert_eq!(config.probe_interval_secs, 10);
    assert_eq!(config.default_http_port, 8080);
    assert!(config.cluster_secret.is_empty());
}

/// Verify that NodeInfo::new() creates nodes with epoch=1 and that
/// with_epoch(0) correctly sets epoch=0 for static discovery peers.
#[test]
fn test_node_info_epoch_defaults_and_override() {
    use synctv_cluster::discovery::NodeInfo;

    // Default epoch is 1
    let info = NodeInfo::new(
        "node1".to_string(),
        "localhost:50051".to_string(),
        "localhost:8080".to_string(),
    );
    assert_eq!(info.epoch, 1, "NodeInfo::new should default to epoch=1");

    // Static discovery should use epoch=0 so it never overwrites self-registered nodes
    let static_info = NodeInfo::new(
        "static_node".to_string(),
        "peer:50051".to_string(),
        "peer:8080".to_string(),
    )
    .with_epoch(0);
    assert_eq!(
        static_info.epoch, 0,
        "Static discovery peers should use epoch=0"
    );
}
