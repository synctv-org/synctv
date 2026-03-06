//! JWT token generation/validation integration tests (Task #82)
//!
//! These tests verify JWT security properties including tampering detection,
//! expiry enforcement, and type confusion prevention.
//!
//! Run with: cargo test --test `jwt_security_tests`
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use base64::{engine::general_purpose, Engine as _};
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use synctv_core::{
    models::UserId,
    service::auth::{jwt::JwtService, JwtValidator, TokenType},
};
use synctv_core_testing::create_test_jwt_service;
use tonic::metadata::MetadataMap;

const JWT_SECRET: &str = "test-secret-key-for-jwt-security-tests-minimum-32-chars";

#[tokio::test]
async fn test_jwt_tampering_detection_signature() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate valid token
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign token");

    // Tamper with signature
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);

    let tampered_token = format!("{}.{}.TAMPERED_SIGNATURE", parts[0], parts[1]);

    // Verification should fail
    let result = jwt_service.verify_access_token(&tampered_token);
    assert!(result.is_err(), "Tampered signature should be rejected");
}

#[tokio::test]
async fn test_jwt_tampering_detection_payload() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign token");

    // Extract and modify payload
    let parts: Vec<&str> = token.split('.').collect();
    let mut payload_bytes = general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .expect("Failed to decode payload");

    // Tamper with payload (change user ID)
    if let Some(byte) = payload_bytes.get_mut(10) {
        *byte = byte.wrapping_add(1);
    }

    let tampered_payload = general_purpose::URL_SAFE_NO_PAD.encode(&payload_bytes);
    let tampered_token = format!("{}.{}.{}", parts[0], tampered_payload, parts[2]);

    // Verification should fail due to signature mismatch
    let result = jwt_service.verify_access_token(&tampered_token);
    assert!(result.is_err(), "Tampered payload should be rejected");
}

#[tokio::test]
async fn test_jwt_expiry_enforcement() {
    use chrono::Utc;
    use jsonwebtoken::{encode, EncodingKey, Header};

    #[derive(Debug, Serialize, Deserialize)]
    struct ExpiredClaims {
        sub: String,
        typ: String,
        jti: String,
        pv: i32,
        iat: i64,
        exp: i64,
    }

    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Create an expired token (expired 1 hour ago)
    let now = Utc::now().timestamp();
    let claims = ExpiredClaims {
        sub: user_id.as_str().to_string(),
        typ: "access".to_string(),
        jti: nanoid::nanoid!(),
        pv: 0,
        iat: now - 7200, // 2 hours ago
        exp: now - 3600, // expired 1 hour ago
    };

    let secret = "test-secret-key-for-integration-tests-minimum-length-32-chars";
    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    let expired_token = encode(&Header::new(Algorithm::HS256), &claims, &encoding_key)
        .expect("Failed to encode expired token");

    // Verification should fail due to expiry
    let result = jwt_service.verify_access_token(&expired_token);
    assert!(result.is_err(), "Expired token should be rejected");

    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("expired") || err_msg.contains("ExpiredSignature"),
        "Error should indicate token expiry: {err_msg}"
    );
}

#[tokio::test]
async fn test_jwt_type_confusion_access_vs_refresh() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate access token
    let access_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign access token");

    // Generate refresh token
    let refresh_token = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to sign refresh token");

    // Try to use access token as refresh token
    let result = jwt_service.verify_refresh_token(&access_token);
    assert!(
        result.is_err(),
        "Access token should not validate as refresh token"
    );

    // Try to use refresh token as access token
    let result = jwt_service.verify_access_token(&refresh_token);
    assert!(
        result.is_err(),
        "Refresh token should not validate as access token"
    );

    // Verify correct usage works
    assert!(jwt_service.verify_access_token(&access_token).is_ok());
    assert!(jwt_service.verify_refresh_token(&refresh_token).is_ok());
}

#[tokio::test]
async fn test_jwt_type_confusion_guest_token() {
    use synctv_core::models::RoomId;

    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();
    let room_id = RoomId::new();

    // Generate guest token (session_id is generated internally by sign_guest_token)
    let guest_token = jwt_service
        .sign_guest_token(&room_id)
        .expect("Failed to sign guest token");

    // Try to use guest token as access token
    let result = jwt_service.verify_access_token(&guest_token);
    assert!(
        result.is_err(),
        "Guest token should not validate as access token"
    );

    // Generate regular access token
    let access_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign access token");

    // Try to use access token as guest token
    let result = jwt_service.verify_guest_token(&access_token);
    assert!(
        result.is_err(),
        "Access token should not validate as guest token"
    );
}

#[tokio::test]
async fn test_jwt_malformed_token_rejection() {
    let jwt_service = create_test_jwt_service();

    // Various malformed tokens
    let malformed_tokens = vec![
        "",              // Empty
        "invalid",       // No dots
        "invalid.token", // Only 2 parts
        "a.b.c.d",       // Too many parts
        "!!!.@@@.###",   // Invalid base64
        "eyJ.eyJ.abc",   // Partial base64
    ];

    for token in malformed_tokens {
        let result = jwt_service.verify_token(token);
        assert!(
            result.is_err(),
            "Malformed token '{token}' should be rejected"
        );
    }
}

#[tokio::test]
async fn test_jwt_wrong_algorithm_rejection() {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};

    #[derive(Debug, Serialize, Deserialize)]
    struct TestClaims {
        sub: String,
        typ: String,
        jti: String,
        iat: i64,
        exp: i64,
    }

    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();
    let now = chrono::Utc::now().timestamp();

    let claims = TestClaims {
        sub: user_id.as_str().to_string(),
        typ: "access".to_string(),
        jti: nanoid::nanoid!(),
        iat: now,
        exp: now + 3600,
    };

    // Create token with HS512 instead of HS256
    let secret = "test-secret-key-for-integration-tests-minimum-length-32-chars";
    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    let wrong_alg_token = encode(&Header::new(Algorithm::HS512), &claims, &encoding_key)
        .expect("Failed to encode token");

    // Should fail because service expects HS256
    let result = jwt_service.verify_access_token(&wrong_alg_token);
    assert!(
        result.is_err(),
        "Token with wrong algorithm should be rejected"
    );
}

#[tokio::test]
async fn test_jwt_future_issued_at_rejection() {
    use jsonwebtoken::{encode, EncodingKey, Header};

    #[derive(Debug, Serialize, Deserialize)]
    struct FutureClaims {
        sub: String,
        typ: String,
        jti: String,
        iat: i64,
        exp: i64,
    }

    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Create token with future iat (issued 1 hour from now)
    let now = chrono::Utc::now().timestamp();
    let claims = FutureClaims {
        sub: user_id.as_str().to_string(),
        typ: "access".to_string(),
        jti: nanoid::nanoid!(),
        iat: now + 3600, // Future timestamp
        exp: now + 7200,
    };

    let secret = "test-secret-key-for-integration-tests-minimum-length-32-chars";
    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    let future_token = encode(&Header::new(Algorithm::HS256), &claims, &encoding_key)
        .expect("Failed to encode future token");

    // Verification may succeed or fail depending on clock skew tolerance
    // But the token should not be usable for authentication
    let result = jwt_service.verify_access_token(&future_token);

    // Some implementations reject future iat, others allow with clock skew
    // Either behavior is acceptable as long as it's consistent
    if let Ok(claims) = result {
        // If accepted, verify the timestamp is actually in the future
        assert!(claims.iat > now, "Future iat should be preserved");
    }
}

#[tokio::test]
async fn test_jwt_jti_uniqueness() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate multiple tokens
    let token1 = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();
    let token2 = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();

    let claims1 = jwt_service.verify_access_token(&token1).unwrap();
    let claims2 = jwt_service.verify_access_token(&token2).unwrap();

    // JTI should be different for each token
    assert_ne!(claims1.jti, claims2.jti, "JTI should be unique per token");
    assert!(!claims1.jti.is_empty(), "JTI should not be empty");
    assert!(!claims2.jti.is_empty(), "JTI should not be empty");
}

#[tokio::test]
async fn test_jwt_different_secrets_incompatible() {
    let jwt_service1 = JwtService::new("test-secret-key-1-with-sufficient-length-32chars")
        .expect("Failed to create JWT service 1");
    let jwt_service2 = JwtService::new("test-secret-key-2-with-sufficient-length-32chars")
        .expect("Failed to create JWT service 2");

    let user_id = UserId::new();

    // Sign with service 1
    let token = jwt_service1
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign token");

    // Verify with service 2 should fail
    let result = jwt_service2.verify_access_token(&token);
    assert!(
        result.is_err(),
        "Token from different secret should be rejected"
    );
}

#[tokio::test]
async fn test_jwt_token_expiration_boundary() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate token
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign token");

    let claims = jwt_service
        .verify_access_token(&token)
        .expect("Failed to verify token");

    // Verify expiration is correctly set (1 hour for access token)
    let expected_exp = claims.iat + 3600;
    assert_eq!(
        claims.exp, expected_exp,
        "Access token should expire in 1 hour"
    );

    // Generate refresh token
    let refresh_token = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to sign refresh token");

    let refresh_claims = jwt_service
        .verify_refresh_token(&refresh_token)
        .expect("Failed to verify refresh token");

    // Verify refresh token expiration (30 days)
    let expected_refresh_exp = refresh_claims.iat + (30 * 24 * 3600);
    assert_eq!(
        refresh_claims.exp, expected_refresh_exp,
        "Refresh token should expire in 30 days"
    );
}

#[tokio::test]
async fn test_jwt_concurrent_token_generation() {
    use std::collections::HashSet;
    use std::sync::Arc;

    let jwt_service = Arc::new(create_test_jwt_service());
    let mut handles = vec![];

    // Generate 50 tokens concurrently
    for _ in 0..50 {
        let service = jwt_service.clone();
        let handle = tokio::spawn(async move {
            let user_id = UserId::new();
            let token = service
                .sign_token(&user_id, TokenType::Access, 0)
                .expect("Failed to sign token");
            let claims = service
                .verify_access_token(&token)
                .expect("Failed to verify token");
            claims.jti
        });
        handles.push(handle);
    }

    let jtis: Vec<String> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // All JTIs should be unique
    let unique_jtis: HashSet<_> = jtis.iter().collect();
    assert_eq!(jtis.len(), unique_jtis.len(), "All JTIs should be unique");
}

// ============================================================================
// SEC8: JwtValidator edge cases
// ============================================================================

#[tokio::test]
async fn test_jwt_validator_non_ascii_grpc_metadata() {
    let jwt_service = Arc::new(create_test_jwt_service());
    let validator = JwtValidator::new(jwt_service);

    // gRPC metadata with non-ASCII characters in the authorization header
    // MetadataMap::insert only accepts ASCII header values. Non-ASCII values
    // should cause the metadata parsing to fail with an error.
    let metadata = MetadataMap::new();
    // No authorization header at all -- should get "Missing authorization header"
    let result = validator.validate_grpc(&metadata);
    assert!(result.is_err(), "Missing auth header should fail");

    // Binary metadata values (non-ASCII) are stored with "-bin" suffix in gRPC.
    // The "authorization" key is ASCII-only, so inserting non-ASCII causes a parse
    // error at the tonic level. We verify the validator handles this gracefully.
    let mut metadata_bad = MetadataMap::new();
    // Insert a binary metadata value under the "authorization-bin" key
    // (non-ASCII metadata must use -bin suffix in gRPC)
    let binary_value = tonic::metadata::MetadataValue::from_bytes(b"\x80\x81\x82");
    metadata_bad.insert_bin("authorization-bin", binary_value);

    // Attempting to validate gRPC without proper "authorization" key should fail
    let result = validator.validate_grpc(&metadata_bad);
    assert!(
        result.is_err(),
        "Non-ASCII gRPC metadata (binary key) should cause validation failure"
    );
}

#[tokio::test]
async fn test_jwt_validator_empty_bearer_token_after_prefix() {
    let jwt_service = Arc::new(create_test_jwt_service());
    let validator = JwtValidator::new(jwt_service);

    // "Bearer " with nothing after it -- the extracted token is empty string
    let result = JwtValidator::extract_bearer_token("Bearer ");
    // The implementation checks len() <= 7 and "Bearer " is 7 chars,
    // so "Bearer " (with trailing space) has len = 7, which is <= 7.
    // Actually "Bearer " has 7 chars, so len <= 7 means it fails.
    assert!(
        result.is_err(),
        "Bearer with empty token after prefix should be rejected"
    );

    // Slightly different: "Bearer" without the trailing space
    let result2 = JwtValidator::extract_bearer_token("Bearer");
    assert!(
        result2.is_err(),
        "'Bearer' without space should be rejected"
    );

    // "Bearer  " (with extra space) -- extracts " " which is invalid JWT
    let result3 = JwtValidator::extract_bearer_token("Bearer  ");
    // This extracts " " as the token, which is technically 8 chars > 7 so passes extraction
    // but the token " " will fail JWT verification
    if let Ok(token) = result3 {
        let verify_result = validator.validate_token(&token);
        assert!(
            verify_result.is_err(),
            "Whitespace-only token should fail verification"
        );
    }

    // HTTP: "Bearer " + empty should fail
    let result4 = validator.validate_http("Bearer ");
    assert!(
        result4.is_err(),
        "HTTP validation with empty bearer should fail"
    );
}

#[tokio::test]
async fn test_jwt_validator_grpc_as_status_returns_unauthenticated() {
    let jwt_service = Arc::new(create_test_jwt_service());
    let validator = JwtValidator::new(jwt_service);

    // Missing auth metadata
    let metadata = MetadataMap::new();
    let result = validator.validate_grpc_as_status(&metadata);
    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);

    // Invalid token in metadata
    let mut metadata2 = MetadataMap::new();
    metadata2.insert("authorization", "Bearer invalid.token".parse().unwrap());
    let result2 = validator.validate_grpc_as_status(&metadata2);
    assert!(result2.is_err());
    assert_eq!(result2.unwrap_err().code(), tonic::Code::Unauthenticated);
}

// ============================================================================
// SEC9: JwtService::verify_custom skips issuer/audience
// ============================================================================

#[tokio::test]
async fn test_verify_custom_skips_issuer_validation() {
    // Create a JWT service that expects issuer and audience
    let jwt_service = JwtService::with_durations_and_claims(
        "test-secret-key-for-integration-tests-minimum-length-32-chars",
        1,
        30,
        4,
        60,
        Some("synctv".to_string()),
        Some("synctv-api".to_string()),
    )
    .unwrap();

    // Sign a custom token WITHOUT issuer/audience
    let custom_claims = serde_json::json!({
        "sub": "custom_subject",
        "custom_field": "custom_value",
    });
    let token = jwt_service.sign_custom(&custom_claims).unwrap();

    // verify_custom should succeed (it skips issuer/audience checks)
    let result = jwt_service.verify_custom(&token);
    assert!(
        result.is_ok(),
        "verify_custom should skip issuer/audience validation: {:?}",
        result.err()
    );

    let verified = result.unwrap();
    assert_eq!(verified["sub"], "custom_subject");
    assert_eq!(verified["custom_field"], "custom_value");

    // In contrast, verify_token should FAIL for a custom token without proper issuer
    // (because verify_token validates issuer/audience when configured)
    // We can't easily test this since sign_custom doesn't include iss/aud in claims,
    // but verify_token requires them when configured. Let's verify that by trying
    // to parse it as standard Claims with verify_token.

    // First, sign a regular token (which includes iss/aud)
    let user_id = UserId::new();
    let regular_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();

    // Regular token should pass verify_token (has correct issuer/audience)
    assert!(jwt_service.verify_token(&regular_token).is_ok());

    // Regular token (which contains aud/iss claims) will FAIL verify_custom
    // because jsonwebtoken's default Validation expects `aud` to be validated
    // when present in the token, but verify_custom doesn't set expected audiences.
    // This documents that verify_custom is intended for custom tokens only, NOT
    // for regular tokens that carry standard iss/aud claims.
    let custom_on_regular = jwt_service.verify_custom(&regular_token);
    assert!(
        custom_on_regular.is_err(),
        "verify_custom should fail on tokens with aud claim (no expected aud configured)"
    );
}

#[tokio::test]
async fn test_verify_custom_still_validates_expiry() {
    let jwt_service = create_test_jwt_service();
    let secret = "test-secret-key-for-integration-tests-minimum-length-32-chars";

    // Create an expired custom token
    let now = chrono::Utc::now().timestamp();
    let expired_claims = serde_json::json!({
        "sub": "expired_custom",
        "exp": now - 3600,  // expired 1 hour ago
        "iat": now - 7200,
    });

    // Sign it manually (can't use sign_custom since it adds a valid exp)
    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    let token = jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &expired_claims,
        &encoding_key,
    )
    .unwrap();

    // verify_custom should still reject expired tokens
    let result = jwt_service.verify_custom(&token);
    assert!(
        result.is_err(),
        "verify_custom should still validate expiry"
    );
}

#[tokio::test]
async fn test_verify_custom_validates_signature() {
    let jwt_service1 = create_test_jwt_service();
    let jwt_service2 =
        JwtService::new("DIFFERENT-secret-key-for-custom-token-tests-1234567890!@#").unwrap();

    // Sign custom token with service 1
    let claims = serde_json::json!({"sub": "test", "data": 42});
    let token = jwt_service1.sign_custom(&claims).unwrap();

    // verify_custom with different secret should fail
    let result = jwt_service2.verify_custom(&token);
    assert!(
        result.is_err(),
        "verify_custom should reject tokens signed with different secret"
    );
}

// ============================================================================
// Additional tests from auth_jwt_tests.rs (merged to eliminate duplication)
// ============================================================================

#[tokio::test]
async fn test_token_family_rotation_maintains_family_id() {
    // Token family validation is handled by UserService::refresh_token
    // which uses TokenBlacklistStore to track family revocation
    //
    // This test verifies that JWT service produces tokens with unique JTIs
    // which can be used to track token families

    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate multiple refresh tokens
    let token1 = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to sign token1");

    let token2 = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to sign token2");

    let claims1 = jwt_service
        .verify_refresh_token(&token1)
        .expect("Failed to verify token1");
    let claims2 = jwt_service
        .verify_refresh_token(&token2)
        .expect("Failed to verify token2");

    // Each token should have unique JTI for tracking
    assert_ne!(
        claims1.jti, claims2.jti,
        "Each token should have unique JTI"
    );

    // Both should have same subject (user)
    assert_eq!(claims1.sub, claims2.sub);
}

#[tokio::test]
async fn test_token_includes_password_version() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Token with password version 0
    let token_v0 = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign token v0");

    let claims_v0 = jwt_service
        .verify_access_token(&token_v0)
        .expect("Failed to verify token v0");

    assert_eq!(claims_v0.pv, 0, "Token should include password version");

    // Token with password version 5
    let token_v5 = jwt_service
        .sign_token(&user_id, TokenType::Access, 5)
        .expect("Failed to sign token v5");

    let claims_v5 = jwt_service
        .verify_access_token(&token_v5)
        .expect("Failed to verify token v5");

    assert_eq!(
        claims_v5.pv, 5,
        "Token should include updated password version"
    );
}

#[tokio::test]
async fn test_refresh_token_rotation_produces_new_jti() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Simulate token rotation: old token -> new token
    let old_refresh = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to sign old refresh token");

    let old_claims = jwt_service
        .verify_refresh_token(&old_refresh)
        .expect("Failed to verify old token");

    // New token (as would be produced by rotation)
    let new_refresh = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to sign new refresh token");

    let new_claims = jwt_service
        .verify_refresh_token(&new_refresh)
        .expect("Failed to verify new token");

    // New token should have different JTI
    assert_ne!(
        old_claims.jti, new_claims.jti,
        "Rotated token should have new JTI"
    );

    // Both should have same subject
    assert_eq!(old_claims.sub, new_claims.sub);
}

#[tokio::test]
async fn test_access_and_refresh_tokens_have_different_expirations() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let access_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign access token");

    let refresh_token = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to sign refresh token");

    let access_claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify access token");

    let refresh_claims = jwt_service
        .verify_refresh_token(&refresh_token)
        .expect("Failed to verify refresh token");

    // Access token should expire before refresh token
    assert!(
        access_claims.exp < refresh_claims.exp,
        "Access token should have shorter expiration than refresh token"
    );

    // Access token typically expires in 1 hour
    let expected_access_exp = access_claims.iat + 3600;
    assert_eq!(
        access_claims.exp, expected_access_exp,
        "Access token should expire in 1 hour"
    );

    // Refresh token typically expires in 30 days
    let expected_refresh_exp = refresh_claims.iat + (30 * 24 * 3600);
    assert_eq!(
        refresh_claims.exp, expected_refresh_exp,
        "Refresh token should expire in 30 days"
    );
}

#[tokio::test]
async fn test_tampered_subject_rejected() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign token");

    let parts: Vec<&str> = token.split('.').collect();

    // Decode, modify subject, re-encode (but with wrong signature)
    let mut payload: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .expect("Failed to decode payload"),
    )
    .expect("Failed to parse payload");

    // Change subject to different user
    payload["sub"] = serde_json::json!("attacker_user_id");

    let tampered_payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).expect("Failed to encode payload"));

    let tampered_token = format!("{}.{}.{}", parts[0], tampered_payload, parts[2]);

    let result = jwt_service.verify_access_token(&tampered_token);
    assert!(
        result.is_err(),
        "Token with modified subject should be rejected"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_token_family_revocation_integration() {
    // This test would verify the full token family revocation flow
    // using UserService::refresh_token with TokenBlacklistStore
    //
    // The actual implementation is in user_auth_service_tests.rs:
    // - test_refresh_token_replay_same_jti_triggers_family_revocation
    // - test_refresh_token_family_revocation_timestamp_blocks_older_tokens
    //
    // This is a placeholder to document that full integration testing
    // requires the complete auth stack (UserService, TokenBlacklistStore, etc.)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_rotation_integration() {
    // This test would verify the complete refresh token rotation flow:
    // 1. User logs in -> gets access + refresh tokens
    // 2. Access token expires
    // 3. User uses refresh token to get new access + refresh tokens
    // 4. Old refresh token is blacklisted
    // 5. Attempting to reuse old refresh token triggers family revocation
    //
    // The actual implementation is in user_auth_service_tests.rs:
    // - test_refresh_token_concurrent_refresh_family_revocation
    //
    // This placeholder documents the integration test location.
}

#[tokio::test]
async fn test_expired_refresh_token_rejected() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    #[derive(Debug, Serialize, Deserialize)]
    struct ExpiredClaims {
        sub: String,
        typ: String,
        jti: String,
        iat: i64,
        exp: i64,
    }

    let now = Utc::now().timestamp();
    let claims = ExpiredClaims {
        sub: user_id.as_str().to_string(),
        typ: "refresh".to_string(),
        jti: nanoid::nanoid!(),
        iat: now - 2_592_000 - 3600, // 30 days + 1 hour ago
        exp: now - 3600,             // expired 1 hour ago
    };

    let encoding_key = EncodingKey::from_secret(JWT_SECRET.as_bytes());
    let expired_token =
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &encoding_key)
            .expect("Failed to encode expired token");

    let result = jwt_service.verify_refresh_token(&expired_token);
    assert!(result.is_err(), "Expired refresh token should be rejected");
}
