//! gRPC server handler tests
//!
//! Tests for gRPC service validation layers (input validation before dispatching to HTTP clients).

#![allow(clippy::unwrap_used)]
use synctv_media_providers::grpc::validation::{validate_host, validate_required};

// === Bilibili gRPC validation ===

#[test]
fn test_bilibili_grpc_match_empty_url_invalid_argument() {
    // validate_required is used in the gRPC layer to check the URL field
    let result = validate_required("url", "");
    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(
        status.message().contains("url"),
        "Error should mention the field name: {}",
        status.message()
    );
}

// === Alist gRPC validation ===

#[test]
fn test_alist_grpc_private_ip_host_rejected() {
    // Private IPs should be rejected by the SSRF validation layer
    let result = validate_host("http://192.168.1.100:5244");
    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    let result = validate_host("http://10.0.0.1:5244");
    assert!(result.is_err());

    let result = validate_host("http://127.0.0.1:5244");
    assert!(result.is_err());

    // Loopback IPv6
    let result = validate_host("http://[::1]:5244");
    assert!(result.is_err());

    // Cloud metadata endpoints
    let result = validate_host("http://169.254.169.254");
    assert!(result.is_err());

    // Public IPs should be allowed
    let result = validate_host("https://alist.example.com:5244");
    assert!(result.is_ok());
}
