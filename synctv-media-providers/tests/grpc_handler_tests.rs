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

#[test]
fn test_validate_required_rejects_whitespace_only() {
    let result = validate_required("token", "   ");
    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("token"));
}

// === Alist gRPC validation ===

#[test]
fn test_alist_grpc_host_url_format_validation() {
    for host in [
        "http://192.168.1.100:5244",
        "http://10.0.0.1:5244",
        "http://127.0.0.1:5244",
        "http://[::1]:5244",
        "http://169.254.169.254",
    ] {
        let result = validate_host(host);
        assert!(
            result.is_ok(),
            "validation layer should allow private and local hosts: {host}"
        );
    }

    let result = validate_host("https://alist.example.com:5244");
    assert!(result.is_ok());
}
