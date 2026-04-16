//! gRPC server handler tests
//!
//! Tests for gRPC service validation layers (input validation before dispatching to HTTP clients).

#![allow(clippy::unwrap_used)]
use synctv_media_providers::grpc::validation::{
    validate_host, validate_provider_grpc_host, validate_required,
};

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

#[test]
fn test_alist_grpc_host_url_format_validation() {
    for host in [
        "https://alist.example.com:5244",
        "http://93.184.216.34:5244",
    ] {
        let result = validate_provider_grpc_host(host);
        assert!(result.is_ok(), "public provider host should pass: {host}");
    }
}

#[test]
fn test_generic_host_validation_still_allows_private_targets_for_storage_layer() {
    let result = validate_host("http://192.168.1.100:5244");
    assert!(
        result.is_ok(),
        "generic host validation should stay compatible with credential storage"
    );
}

#[test]
fn test_provider_grpc_host_validation_stays_compatible_with_self_hosted_targets() {
    for host in [
        "http://192.168.1.100:5244",
        "http://10.0.0.1:5244",
        "http://127.0.0.1:5244",
        "http://[::1]:5244",
        "http://169.254.169.254",
        "http://localhost:8096",
    ] {
        let result = validate_provider_grpc_host(host);
        assert!(
            result.is_ok(),
            "gRPC host validation should remain compatible with self-hosted endpoints: {host}"
        );
    }
}
