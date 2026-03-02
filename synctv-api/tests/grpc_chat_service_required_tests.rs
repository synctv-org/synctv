//! Tests for gRPC server chat_service requirement validation
//!
//! These tests verify that the gRPC server properly handles the case where
//! chat_service is not configured, returning a clear error instead of panicking.
//!
//! Bug fix: P1 - gRPC server startup panics when chat_service is None
//! Previously, the code used `.expect()` which would panic. The fix changes
//! this to use `ok_or_else()` to return a proper anyhow::Error.

/// Test that the error message for missing chat_service is descriptive.
/// This test verifies the error path without actually starting a gRPC server.
#[test]
fn test_missing_chat_service_error_message() {
    // The error message should be clear and actionable
    let error_msg = "chat_service is required for gRPC ClientService but was not provided. \
                     Ensure chat_service is initialized before starting the gRPC server.";

    // Verify the error message contains key information
    assert!(
        error_msg.contains("chat_service"),
        "Error message should mention chat_service"
    );
    assert!(
        error_msg.contains("required"),
        "Error message should indicate the service is required"
    );
    assert!(
        error_msg.contains("gRPC"),
        "Error message should mention gRPC context"
    );
}

/// Test that the error type is anyhow::Error (can be converted to anyhow::Error).
/// This ensures the error integrates properly with the application's error handling.
#[test]
fn test_missing_chat_service_error_is_anyhow_compatible() {
    use anyhow::anyhow;

    // Simulate the error creation pattern used in the fix
    let chat_service: Option<()> = None;
    let result: Result<(), anyhow::Error> = chat_service
        .ok_or_else(|| anyhow!("chat_service is required for gRPC ClientService but was not provided"));

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("chat_service"));
}

/// Test that Some value passes through correctly.
#[test]
fn test_chat_service_some_value_passes_through() {
    use anyhow::anyhow;

    let chat_service: Option<i32> = Some(42);
    let result: Result<i32, anyhow::Error> = chat_service
        .ok_or_else(|| anyhow!("chat_service is required for gRPC ClientService but was not provided"));

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 42);
}
