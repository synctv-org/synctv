//! Tests for gRPC room_id validation in interceptors
//!
//! These tests verify that the gRPC interceptors properly validate room_id format
//! to match HTTP layer validation rules:
//! - Must not be empty
//! - Must be exactly 12 characters
//! - Must contain only alphanumeric characters, underscores, and hyphens

use synctv_api::grpc::interceptors::{AuthInterceptor, RoomContext, SecurityCheckPassed};
use synctv_core::models::UserId;
use synctv_core::service::auth::{JwtService, TokenType};
use synctv_core::service::{AuthenticatedToken, Claims};
use tonic::metadata::MetadataMap;
use tonic::Request;

/// Helper to create a valid JWT token for testing
fn create_test_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-long-enough-for-entropy-check-1234567890").unwrap()
}

/// Helper to create metadata with authorization and room-id headers
fn create_metadata(token: &str, room_id: &str) -> MetadataMap {
    let mut metadata = MetadataMap::new();
    metadata.insert("authorization", format!("Bearer {token}").parse().unwrap());
    metadata.insert("x-room-id", room_id.parse().unwrap());
    metadata
}

fn authenticated_token(user_id: &UserId) -> AuthenticatedToken {
    AuthenticatedToken {
        user_id: user_id.clone(),
        claims: Claims {
            sub: user_id.as_str().to_string(),
            typ: "access".to_string(),
            jti: "grpc-room-id-validation".to_string(),
            iat: 1_700_000_000,
            exp: 1_700_003_600,
            pv: 0,
            iss: None,
            aud: None,
        },
    }
}

// =============================================================================
// TDD Step 1: These tests should FAIL initially because room_id validation
// is not yet implemented in the gRPC interceptor.
// =============================================================================

#[tokio::test]
async fn test_valid_room_id_format_should_pass() {
    // Valid room IDs should pass validation
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();
    let interceptor = AuthInterceptor::new(jwt_service);

    // Test various valid room_id formats
    let exact_len_room_id = "a".repeat(12);
    let valid_room_ids = vec![
        "room1234_abx", // alphanumeric + underscore
        "ROOM1234-XYZ", // uppercase + hyphen
        "room_123-xyz", // underscore + hyphen
        "AbCdEf123456", // mixed case alphanumeric
        &exact_len_room_id,
        "123456789012", // numbers only
    ];

    for room_id in valid_room_ids {
        let mut request = Request::new(());
        // Inject SecurityCheckPassed marker to simulate BlacklistCheckLayer
        request.extensions_mut().insert(SecurityCheckPassed);
        request
            .extensions_mut()
            .insert(authenticated_token(&user_id));
        *request.metadata_mut() = create_metadata(&token, room_id);

        let result = interceptor.inject_room(request);
        assert!(
            result.is_ok(),
            "Valid room_id '{room_id}' should pass validation but got error: {:?}",
            result.err()
        );

        let room_context = result.unwrap().extensions().get::<RoomContext>().cloned();
        assert!(
            room_context.is_some(),
            "RoomContext should be present for valid room_id '{room_id}'"
        );
    }
}

#[tokio::test]
async fn test_empty_room_id_should_be_rejected() {
    // Empty room_id should be rejected with INVALID_ARGUMENT
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();
    let interceptor = AuthInterceptor::new(jwt_service);

    let mut request = Request::new(());
    // Inject SecurityCheckPassed marker to simulate BlacklistCheckLayer
    request.extensions_mut().insert(SecurityCheckPassed);
    request
        .extensions_mut()
        .insert(authenticated_token(&user_id));
    *request.metadata_mut() = create_metadata(&token, "");

    let result = interceptor.inject_room(request);
    assert!(result.is_err(), "Empty room_id should be rejected");

    let status = result.unwrap_err();
    assert_eq!(
        status.code(),
        tonic::Code::InvalidArgument,
        "Empty room_id should return INVALID_ARGUMENT, got: {:?}",
        status.message()
    );
    assert!(
        status.message().to_lowercase().contains("room_id")
            || status.message().to_lowercase().contains("room"),
        "Error message should mention room_id, got: {:?}",
        status.message()
    );
}

#[tokio::test]
async fn test_too_long_room_id_should_be_rejected() {
    // Room_id exceeding 12 characters should be rejected
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();
    let interceptor = AuthInterceptor::new(jwt_service);

    let too_long_room_id = "a".repeat(13);

    let mut request = Request::new(());
    // Inject SecurityCheckPassed marker to simulate BlacklistCheckLayer
    request.extensions_mut().insert(SecurityCheckPassed);
    request
        .extensions_mut()
        .insert(authenticated_token(&user_id));
    *request.metadata_mut() = create_metadata(&token, &too_long_room_id);

    let result = interceptor.inject_room(request);
    assert!(
        result.is_err(),
        "Room_id with {} chars should be rejected (max 12)",
        too_long_room_id.len()
    );

    let status = result.unwrap_err();
    assert_eq!(
        status.code(),
        tonic::Code::InvalidArgument,
        "Too long room_id should return INVALID_ARGUMENT, got: {:?}",
        status.message()
    );
    assert!(
        status.message().to_lowercase().contains("room_id")
            || status.message().to_lowercase().contains("long")
            || status.message().to_lowercase().contains("length"),
        "Error message should mention room_id or length, got: {:?}",
        status.message()
    );
}

#[tokio::test]
async fn test_invalid_characters_in_room_id_should_be_rejected() {
    // Room_id with invalid characters should be rejected
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();
    let interceptor = AuthInterceptor::new(jwt_service);

    // Test various invalid room_id formats
    let invalid_room_ids = vec![
        "room123",   // too short
        "room@123",  // @ symbol
        "room#123",  // # symbol
        "room$123",  // $ symbol
        "room%123",  // % symbol
        "room&123",  // & symbol
        "room*123",  // * symbol
        "room 123",  // space
        "room.123",  // dot
        "room/123",  // forward slash
        "room\\123", // backslash
        "room!123",  // exclamation mark
        "room+123",  // plus sign
        "room=123",  // equal sign
        "room[123]", // brackets
        "room{123}", // braces
        "room(123)", // parentheses
        "room'123",  // single quote
        "room\"123", // double quote
        "room<123>", // angle brackets
        "room,123",  // comma
        "room:123",  // colon
        "room;123",  // semicolon
        "room|123",  // pipe
        "room?123",  // question mark
        "room`123",  // backtick
        "room~123",  // tilde
        "room^123",  // caret
    ];

    for room_id in invalid_room_ids {
        let mut request = Request::new(());
        // Inject SecurityCheckPassed marker to simulate BlacklistCheckLayer
        request.extensions_mut().insert(SecurityCheckPassed);
        request
            .extensions_mut()
            .insert(authenticated_token(&user_id));
        *request.metadata_mut() = create_metadata(&token, room_id);

        let result = interceptor.inject_room(request);
        assert!(
            result.is_err(),
            "Room_id '{room_id}' with invalid characters should be rejected"
        );

        let status = result.unwrap_err();
        assert_eq!(
            status.code(),
            tonic::Code::InvalidArgument,
            "Invalid room_id '{room_id}' should return INVALID_ARGUMENT, got: {:?}",
            status.message()
        );
    }
}

#[tokio::test]
async fn test_unicode_characters_in_room_id_should_be_rejected() {
    // Room_id with Unicode/CJK characters should be rejected
    // The HTTP layer only allows ASCII alphanumeric, underscore, and hyphen
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();
    let interceptor = AuthInterceptor::new(jwt_service);

    let unicode_room_ids = vec![
        "roomééé",   // Accented characters (ASCII-compatible for gRPC metadata)
        "room_cafe", // still too short and should fail under fixed-length IDs
    ];

    for room_id in unicode_room_ids {
        let mut request = Request::new(());
        // Inject SecurityCheckPassed marker to simulate BlacklistCheckLayer
        request.extensions_mut().insert(SecurityCheckPassed);
        request
            .extensions_mut()
            .insert(authenticated_token(&user_id));
        *request.metadata_mut() = create_metadata(&token, room_id);

        let result = interceptor.inject_room(request);
        if room_id.contains("é") || room_id == "room_cafe" {
            assert!(result.is_err(), "Room_id '{room_id}' should be rejected");

            let status = result.unwrap_err();
            assert_eq!(
                status.code(),
                tonic::Code::InvalidArgument,
                "Invalid room_id should return INVALID_ARGUMENT"
            );
        }
    }

    // Note: Full Unicode/CJK characters often can't be encoded in gRPC ASCII metadata
    // so we skip testing strings like "房间123" here - they would fail at metadata parse time
}

#[tokio::test]
async fn test_exactly_12_char_room_id_should_pass() {
    // Room_id exactly at the fixed 12-char boundary should pass
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();
    let interceptor = AuthInterceptor::new(jwt_service);

    let boundary_room_id = "a".repeat(12);
    assert_eq!(boundary_room_id.len(), 12);

    let mut request = Request::new(());
    // Inject SecurityCheckPassed marker to simulate BlacklistCheckLayer
    request.extensions_mut().insert(SecurityCheckPassed);
    request
        .extensions_mut()
        .insert(authenticated_token(&user_id));
    *request.metadata_mut() = create_metadata(&token, &boundary_room_id);

    let result = interceptor.inject_room(request);
    assert!(
        result.is_ok(),
        "Room_id exactly 12 chars should pass validation, got error: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_consistency_with_http_validation() {
    // This test ensures gRPC validation matches HTTP validation rules
    // by using the same validation function
    use synctv_api::room_id_validation::parse_room_id;

    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();
    let interceptor = AuthInterceptor::new(jwt_service);

    // Test cases that should have consistent behavior between HTTP and gRPC
    let exact_len_room_id = "a".repeat(12);
    let too_long_room_id = "a".repeat(13);
    let test_cases: Vec<(&str, bool)> = vec![
        ("validroom_12", true),
        ("", false),
        ("room@invalid", false),
        (&exact_len_room_id, true),
        (&too_long_room_id, false),
        ("room with space", false),
    ];

    for (room_id, should_be_valid) in test_cases {
        // Check HTTP validation
        let http_result = parse_room_id(room_id).is_ok();

        // Check gRPC validation
        let mut request = Request::new(());
        // Inject SecurityCheckPassed marker to simulate BlacklistCheckLayer
        request.extensions_mut().insert(SecurityCheckPassed);
        request
            .extensions_mut()
            .insert(authenticated_token(&user_id));
        *request.metadata_mut() = create_metadata(&token, room_id);
        let grpc_result = interceptor.inject_room(request).is_ok();

        assert_eq!(
            http_result, grpc_result,
            "HTTP and gRPC validation should be consistent for room_id '{room_id}': HTTP={http_result}, gRPC={grpc_result}"
        );
        assert_eq!(
            grpc_result, should_be_valid,
            "Validation result for room_id '{room_id}' should be {should_be_valid}"
        );
    }
}
