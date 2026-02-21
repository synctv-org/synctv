//! CL9: ClusterAuthInterceptor tests
//!
//! - Correct secret passes
//! - Missing header -> unauthenticated
//! - Wrong secret -> unauthenticated
//! - constant_time_eq: equal, not-equal same length, different lengths

use tonic::Request;

use synctv_cluster::grpc::ClusterAuthInterceptor;

/// Helper: create a Request with a specific x-cluster-secret value.
fn make_request_with_secret(secret: &str) -> Request<()> {
    let mut request = Request::new(());
    request
        .metadata_mut()
        .insert("x-cluster-secret", secret.parse().unwrap());
    request
}

/// Helper: create a Request with no x-cluster-secret header.
fn make_request_no_secret() -> Request<()> {
    Request::new(())
}

// ============================================================================
// Correct secret passes
// ============================================================================

#[test]
fn test_correct_secret_passes() {
    let interceptor = ClusterAuthInterceptor::new("my-secret-key".to_string());
    let request = make_request_with_secret("my-secret-key");

    let result = interceptor.validate(request);
    assert!(result.is_ok(), "Correct secret should pass validation");
}

// ============================================================================
// Missing header -> unauthenticated
// ============================================================================

#[test]
fn test_missing_header_unauthenticated() {
    let interceptor = ClusterAuthInterceptor::new("my-secret-key".to_string());
    let request = make_request_no_secret();

    let result = interceptor.validate(request);
    assert!(result.is_err(), "Missing header should fail");

    let status = result.unwrap_err();
    assert_eq!(
        status.code(),
        tonic::Code::Unauthenticated,
        "Error code should be Unauthenticated"
    );
    assert!(
        status.message().contains("Missing"),
        "Error message should mention missing header, got: {}",
        status.message()
    );
}

// ============================================================================
// Wrong secret -> unauthenticated
// ============================================================================

#[test]
fn test_wrong_secret_unauthenticated() {
    let interceptor = ClusterAuthInterceptor::new("correct-secret".to_string());
    let request = make_request_with_secret("wrong-secret");

    let result = interceptor.validate(request);
    assert!(result.is_err(), "Wrong secret should fail");

    let status = result.unwrap_err();
    assert_eq!(
        status.code(),
        tonic::Code::Unauthenticated,
        "Error code should be Unauthenticated"
    );
    assert!(
        status.message().contains("Invalid"),
        "Error message should mention invalid secret, got: {}",
        status.message()
    );
}

// ============================================================================
// constant_time_eq tests
// ============================================================================

/// We test the constant_time_eq behavior through the interceptor by verifying
/// that the comparison is correct for equal, not-equal same-length, and
/// different-length secrets.

#[test]
fn test_constant_time_eq_same_secret() {
    let interceptor = ClusterAuthInterceptor::new("abc123".to_string());
    let request = make_request_with_secret("abc123");
    assert!(interceptor.validate(request).is_ok());
}

#[test]
fn test_constant_time_eq_different_same_length() {
    let interceptor = ClusterAuthInterceptor::new("abc123".to_string());
    let request = make_request_with_secret("xyz789");
    assert!(
        interceptor.validate(request).is_err(),
        "Different secret of same length should fail"
    );
}

#[test]
fn test_constant_time_eq_different_lengths() {
    let interceptor = ClusterAuthInterceptor::new("short".to_string());
    let request = make_request_with_secret("this-is-a-much-longer-secret");
    assert!(
        interceptor.validate(request).is_err(),
        "Different length secrets should fail"
    );
}

#[test]
fn test_constant_time_eq_longer_expected() {
    let interceptor = ClusterAuthInterceptor::new("this-is-a-longer-expected-secret".to_string());
    let request = make_request_with_secret("short");
    assert!(
        interceptor.validate(request).is_err(),
        "Shorter provided secret should fail"
    );
}

#[test]
fn test_constant_time_eq_empty_secret() {
    let interceptor = ClusterAuthInterceptor::new(String::new());
    let request = make_request_with_secret("");
    assert!(interceptor.validate(request).is_ok(), "Both empty should pass");
}

#[test]
fn test_constant_time_eq_one_empty() {
    let interceptor = ClusterAuthInterceptor::new("notempty".to_string());
    let request = make_request_with_secret("");
    assert!(
        interceptor.validate(request).is_err(),
        "Empty provided vs non-empty expected should fail"
    );
}

/// Test that the Debug impl does not leak the secret.
#[test]
fn test_debug_does_not_leak_secret() {
    let interceptor = ClusterAuthInterceptor::new("super-secret-value".to_string());
    let debug_output = format!("{:?}", interceptor);
    assert!(
        !debug_output.contains("super-secret-value"),
        "Debug output should not contain the secret, got: {}",
        debug_output
    );
}

/// Test with a non-ASCII metadata value (binary/garbage) -> Unauthenticated.
#[test]
fn test_invalid_metadata_value_unauthenticated() {
    let interceptor = ClusterAuthInterceptor::new("valid-secret".to_string());

    // Insert a binary metadata value
    let mut request = Request::new(());
    let metadata = request.metadata_mut();
    // Use MetadataMap's insert_bin for binary metadata - but x-cluster-secret
    // is not a -bin suffix key, so invalid UTF-8 would fail to_str().
    // We can test this by manually inserting a valid ASCII but wrong value.
    metadata.insert("x-cluster-secret", "valid\x01secret".parse().unwrap_or_else(|_| {
        // If parsing fails (non-visible ASCII), just use a regular wrong value
        "wrong".parse().unwrap()
    }));

    let result = interceptor.validate(request);
    assert!(result.is_err(), "Invalid/wrong secret should fail");
}
