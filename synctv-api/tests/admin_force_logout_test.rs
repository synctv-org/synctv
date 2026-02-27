//! Test that UpdateUserPassword API does not include force_logout parameter.
//!
//! This test verifies that the force_logout parameter has been removed from
//! the UpdateUserPassword API, as the implementation does not support
//! token blacklisting for session invalidation.

use synctv_proto::admin::{UpdateUserPasswordRequest, UpdateUserPasswordResponse};

/// Verify that UpdateUserPasswordRequest does NOT have force_logout field.
/// This test ensures the API matches the actual implementation behavior.
#[test]
fn test_update_password_request_no_force_logout() {
    // Create a request with only the supported fields
    let request = UpdateUserPasswordRequest {
        user_id: "test-user-id".to_string(),
        new_password: "newpassword123".to_string(),
        reason: "Admin reset".to_string(),
    };

    // Verify the request can be serialized
    let json = serde_json::to_string(&request).expect("Failed to serialize request");

    // Verify that force_logout is NOT in the serialized JSON
    assert!(
        !json.contains("force_logout"),
        "force_logout field should not exist in UpdateUserPasswordRequest"
    );

    // Verify the expected fields are present
    assert!(json.contains("user_id"));
    assert!(json.contains("new_password"));
    assert!(json.contains("reason"));
}

/// Verify that UpdateUserPasswordResponse does NOT have sessions_invalidated field.
/// This test ensures the response matches the actual implementation behavior.
#[test]
fn test_update_password_response_no_sessions_invalidated() {
    // Create a response with only the success field
    let response = UpdateUserPasswordResponse {
        success: true,
    };

    // Verify the response can be serialized
    let json = serde_json::to_string(&response).expect("Failed to serialize response");

    // Verify that sessions_invalidated is NOT in the serialized JSON
    assert!(
        !json.contains("sessions_invalidated"),
        "sessions_invalidated field should not exist in UpdateUserPasswordResponse"
    );

    // Verify the expected field is present
    assert!(json.contains("success"));
}
