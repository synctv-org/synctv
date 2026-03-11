//! Error conversion tests (no Docker needed)
//!
//! Tests Error -> `tonic::Status` conversions, `InternalExt` trait, and
//! the error Display formatting that is exposed to gRPC clients.
//!
//! Run with: cargo test --test `error_conversion_tests`
#![allow(clippy::unwrap_used)]

use synctv_core::Error;

// ============================================================================
// Error -> tonic::Status mapping
// ============================================================================

#[test]
fn test_not_found_maps_to_tonic_not_found() {
    let status: tonic::Status = Error::NotFound("room 42".to_string()).into();
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert!(status.message().contains("room 42"));
}

#[test]
fn test_authentication_maps_to_tonic_unauthenticated() {
    let status: tonic::Status = Error::Authentication("invalid token".to_string()).into();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    assert!(status.message().contains("invalid token"));
}

#[test]
fn test_authorization_maps_to_tonic_permission_denied() {
    let status: tonic::Status = Error::Authorization("not an admin".to_string()).into();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    assert!(status.message().contains("not an admin"));
}

#[test]
fn test_invalid_input_maps_to_tonic_invalid_argument() {
    let status: tonic::Status = Error::InvalidInput("bad field".to_string()).into();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("bad field"));
}

#[test]
fn test_already_exists_maps_to_tonic_already_exists() {
    let status: tonic::Status = Error::AlreadyExists("duplicate user".to_string()).into();
    assert_eq!(status.code(), tonic::Code::AlreadyExists);
    assert!(status.message().contains("duplicate user"));
}

#[test]
fn test_rate_limited_maps_to_tonic_resource_exhausted() {
    let status: tonic::Status = Error::RateLimited("too many requests".to_string()).into();
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    assert!(status.message().contains("too many requests"));
}

#[test]
fn test_service_unavailable_maps_to_tonic_unavailable() {
    let status: tonic::Status = Error::ServiceUnavailable("redis unavailable".to_string()).into();
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert!(status.message().contains("redis unavailable"));
}

#[test]
fn test_optimistic_lock_maps_to_tonic_aborted() {
    let status: tonic::Status = Error::OptimisticLockConflict.into();
    assert_eq!(status.code(), tonic::Code::Aborted);
}

#[test]
fn test_internal_error_does_not_leak_details() {
    let status: tonic::Status = Error::Internal("secret db connection string".to_string()).into();
    assert_eq!(status.code(), tonic::Code::Internal);
    // Internal errors should NOT leak details to clients
    assert!(
        !status.message().contains("secret"),
        "Internal error details should not be exposed to clients"
    );
    assert_eq!(status.message(), "Internal error");
}

#[test]
fn test_serialization_error_maps_to_internal() {
    let err = serde_json::from_str::<serde_json::Value>("not valid json").unwrap_err();
    let status: tonic::Status = Error::Serialization(err).into();
    assert_eq!(status.code(), tonic::Code::Internal);
}

// ============================================================================
// Error Display formatting
// ============================================================================

#[test]
fn test_error_display_format() {
    assert_eq!(
        Error::NotFound("user".to_string()).to_string(),
        "Not found: user"
    );
    assert_eq!(
        Error::AlreadyExists("room".to_string()).to_string(),
        "Already exists: room"
    );
    assert_eq!(
        Error::Authentication("expired".to_string()).to_string(),
        "Authentication error: expired"
    );
    assert_eq!(
        Error::Authorization("forbidden".to_string()).to_string(),
        "Authorization error: forbidden"
    );
    assert_eq!(
        Error::InvalidInput("missing field".to_string()).to_string(),
        "Invalid input: missing field"
    );
    assert_eq!(
        Error::RateLimited("slow down".to_string()).to_string(),
        "Rate limited: slow down"
    );
    assert_eq!(
        Error::Internal("panic".to_string()).to_string(),
        "Internal error: panic"
    );
    assert_eq!(
        Error::OptimisticLockConflict.to_string(),
        "Optimistic lock conflict"
    );
}

// ============================================================================
// InternalExt trait
// ============================================================================

#[test]
fn test_internal_ext_maps_error() {
    use synctv_core::error::InternalExt;

    let result: Result<(), std::io::Error> = Err(std::io::Error::other("disk full"));

    let mapped = result.internal("Failed to write file");
    assert!(mapped.is_err());
    match mapped.unwrap_err() {
        Error::Internal(msg) => assert_eq!(msg, "Failed to write file"),
        other => panic!("Expected Internal, got: {other:?}"),
    }
}

#[test]
fn test_internal_ext_with_err_includes_cause() {
    use synctv_core::error::InternalExt;

    let result: Result<(), std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "access denied",
    ));

    let mapped = result.internal_with_err("Failed to read config");
    assert!(mapped.is_err());
    match mapped.unwrap_err() {
        Error::Internal(msg) => {
            assert!(msg.contains("Failed to read config"));
            assert!(msg.contains("access denied"));
        }
        other => panic!("Expected Internal, got: {other:?}"),
    }
}

#[test]
fn test_internal_ext_preserves_ok() {
    use synctv_core::error::InternalExt;

    let result: Result<i32, std::io::Error> = Ok(42);
    let mapped = result.internal("should not happen");
    assert_eq!(mapped.unwrap(), 42);
}

// ============================================================================
// From<anyhow::Error> preserves error chain
// ============================================================================

#[test]
fn test_anyhow_error_preserves_chain() {
    let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
    let anyhow_err = anyhow::anyhow!(inner).context("loading config");

    let core_err: Error = anyhow_err.into();
    match core_err {
        Error::Internal(msg) => {
            assert!(
                msg.contains("loading config"),
                "Should contain context: {msg}"
            );
            assert!(
                msg.contains("file missing"),
                "Should contain root cause: {msg}"
            );
        }
        other => panic!("Expected Internal, got: {other:?}"),
    }
}
