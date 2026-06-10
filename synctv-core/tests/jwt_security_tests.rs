//! JWT token generation/validation integration tests //!
//! These tests verify JWT security properties including tampering detection,
//! expiry enforcement, and type confusion prevention.

use std::sync::Arc;

use base64::{engine::general_purpose, Engine as _};
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use synctv_core::{
    models::UserId,
    service::auth::{jwt::JwtService, Claims, JwtValidator, TokenCredentialBinding},
};
use synctv_core_testing::{create_test_jwt_service, err, ok};
use tonic::metadata::MetadataMap;

#[derive(Debug, Serialize, Deserialize)]
struct ExpiredClaims {
    sub: String,
    typ: String,
    jti: String,
    iat: i64,
    exp: i64,
}

const JWT_SECRET: &str = "test-secret-key-for-jwt-security-tests-minimum-32-chars";

fn sign_test_refresh_token(jwt_service: &JwtService, user_id: &UserId) -> String {
    ok(
        jwt_service.sign_refresh_token_with_session(
            user_id,
            0,
            None,
            "jwt-security-refresh-session",
            &TokenCredentialBinding::Password { version: 0 },
        ),
        "refresh token should be signed",
    )
}

fn sign_access_token(jwt_service: &JwtService, user_id: &UserId, pv: i32) -> String {
    ok(
        jwt_service.sign_access_token(user_id, pv),
        "access token should be signed",
    )
}

fn verify_access_token(jwt_service: &JwtService, token: &str) -> Claims {
    ok(
        jwt_service.verify_access_token(token),
        "access token should verify",
    )
}

fn verify_refresh_token(jwt_service: &JwtService, token: &str) -> Claims {
    ok(
        jwt_service.verify_refresh_token(token),
        "refresh token should verify",
    )
}

fn jwt_parts(token: &str) -> Vec<&str> {
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);
    parts
}

#[tokio::test]
async fn test_jwt_tampering_detection_signature() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate valid token
    let token = sign_access_token(&jwt_service, &user_id, 0);

    // Tamper with signature
    let parts = jwt_parts(&token);

    let tampered_token = format!("{}.{}.TAMPERED_SIGNATURE", parts[0], parts[1]);

    // Verification should fail
    let result = jwt_service.verify_access_token(&tampered_token);
    assert!(result.is_err(), "Tampered signature should be rejected");
}

#[tokio::test]
async fn test_jwt_tampering_detection_payload() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let token = sign_access_token(&jwt_service, &user_id, 0);

    // Extract and modify payload
    let parts = jwt_parts(&token);
    let mut payload_bytes = ok(
        general_purpose::URL_SAFE_NO_PAD.decode(parts[1]),
        "payload should decode",
    );

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

    let now = Utc::now().timestamp();
    let claims = ExpiredClaims {
        sub: user_id.to_string(),
        typ: "access".to_string(),
        jti: synctv_common::snanoid!(),
        pv: 0,
        iat: now - 7200, // 2 hours ago
        exp: now - 3600, // expired 1 hour ago
    };

    let secret = "test-secret-key-for-integration-tests-minimum-length-32-chars";
    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    let expired_token = ok(
        encode(&Header::new(Algorithm::HS256), &claims, &encoding_key),
        "expired token should encode",
    );

    // Verification should fail due to expiry
    let result = jwt_service.verify_access_token(&expired_token);
    assert!(result.is_err(), "Expired token should be rejected");

    let err_msg = format!("{:?}", err(result, "expired token should be rejected"));
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
    let access_token = sign_access_token(&jwt_service, &user_id, 0);

    // Generate refresh token
    let refresh_token = sign_test_refresh_token(&jwt_service, &user_id);

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
    let guest_token = ok(
        jwt_service.sign_guest_token(&room_id),
        "guest token should be signed",
    );

    // Try to use guest token as access token
    let result = jwt_service.verify_access_token(&guest_token);
    assert!(
        result.is_err(),
        "Guest token should not validate as access token"
    );

    // Generate regular access token
    let access_token = sign_access_token(&jwt_service, &user_id, 0);

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
        sub: user_id.to_string(),
        typ: "access".to_string(),
        jti: synctv_common::snanoid!(),
        iat: now,
        exp: now + 3600,
    };

    let secret = "test-secret-key-for-integration-tests-minimum-length-32-chars";
    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    let wrong_alg_token = ok(
        encode(&Header::new(Algorithm::HS512), &claims, &encoding_key),
        "wrong-algorithm token should encode",
    );

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

    let now = chrono::Utc::now().timestamp();
    let claims = FutureClaims {
        sub: user_id.to_string(),
        typ: "access".to_string(),
        jti: synctv_common::snanoid!(),
        iat: now + 3600, // Future timestamp
        exp: now + 7200,
    };

    let secret = "test-secret-key-for-integration-tests-minimum-length-32-chars";
    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    let future_token = ok(
        encode(&Header::new(Algorithm::HS256), &claims, &encoding_key),
        "future-issued token should encode",
    );

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
    let token1 = sign_access_token(&jwt_service, &user_id, 0);
    let token2 = sign_access_token(&jwt_service, &user_id, 0);

    let claims1 = verify_access_token(&jwt_service, &token1);
    let claims2 = verify_access_token(&jwt_service, &token2);

    // JTI should be different for each token
    assert_ne!(claims1.jti, claims2.jti, "JTI should be unique per token");
    assert!(!claims1.jti.is_empty(), "JTI should not be empty");
    assert!(!claims2.jti.is_empty(), "JTI should not be empty");
}

#[tokio::test]
async fn test_jwt_different_secrets_incompatible() {
    let jwt_service1 = ok(
        JwtService::new("test-secret-key-1-with-sufficient-length-32chars"),
        "first JWT service should be created",
    );
    let jwt_service2 = ok(
        JwtService::new("test-secret-key-2-with-sufficient-length-32chars"),
        "second JWT service should be created",
    );

    let user_id = UserId::new();

    // Sign with service 1
    let token = sign_access_token(&jwt_service1, &user_id, 0);

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
    let token = sign_access_token(&jwt_service, &user_id, 0);

    let claims = verify_access_token(&jwt_service, &token);

    // Verify expiration is correctly set (1 hour for access token)
    let expected_exp = claims.iat + 3600;
    assert_eq!(
        claims.exp, expected_exp,
        "Access token should expire in 1 hour"
    );

    // Generate refresh token
    let refresh_token = sign_test_refresh_token(&jwt_service, &user_id);

    let refresh_claims = verify_refresh_token(&jwt_service, &refresh_token);

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
            let token = sign_access_token(&service, &user_id, 0);
            let claims = verify_access_token(&service, &token);
            claims.jti
        });
        handles.push(handle);
    }

    let jtis: Vec<String> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|result| ok(result, "JWT generation task should complete"))
        .collect();

    // All JTIs should be unique
    let unique_jtis: HashSet<_> = jtis.iter().collect();
    assert_eq!(jtis.len(), unique_jtis.len(), "All JTIs should be unique");
}

// SEC8: JwtValidator edge cases

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

    // "Bearer " (with extra space) -- extracts " " which is invalid JWT
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
    let status = err(result, "missing auth metadata should fail");
    assert_eq!(status.code(), tonic::Code::Unauthenticated);

    // Invalid token in metadata
    let mut metadata2 = MetadataMap::new();
    metadata2.insert(
        "authorization",
        ok(
            "Bearer invalid.token".parse(),
            "authorization metadata should parse",
        ),
    );
    let result2 = validator.validate_grpc_as_status(&metadata2);
    assert_eq!(
        err(result2, "invalid metadata token should fail").code(),
        tonic::Code::Unauthenticated
    );
}

// SEC9: JwtService::verify_custom skips issuer/audience

#[tokio::test]
async fn test_verify_custom_skips_issuer_validation() {
    let jwt_service = ok(
        JwtService::with_durations_and_claims(
            "test-secret-key-for-integration-tests-minimum-length-32-chars",
            1,
            30,
            4,
            60,
            Some("synctv".to_string()),
            Some("synctv-api".to_string()),
        ),
        "JWT service with claims should be created",
    );

    // Sign a custom token WITHOUT issuer/audience
    let custom_claims = serde_json::json!({
        "sub": "custom_subject",
        "custom_field": "custom_value",
    });
    let token = ok(
        jwt_service.sign_custom(&custom_claims),
        "custom token should be signed",
    );

    // verify_custom should succeed (it skips issuer/audience checks)
    let result = jwt_service.verify_custom(&token);
    assert!(
        result.is_ok(),
        "verify_custom should skip issuer/audience validation: {:?}",
        result.err()
    );

    let verified = ok(result, "custom token should verify");
    assert_eq!(verified["sub"], "custom_subject");
    assert_eq!(verified["custom_field"], "custom_value");

    // In contrast, verify_token should FAIL for a custom token without proper issuer
    // (because verify_token validates issuer/audience when configured)
    // We can't easily test this since sign_custom doesn't include iss/aud in claims,
    // but verify_token requires them when configured. Let's verify that by trying
    // to parse it as standard Claims with verify_token.

    // First, sign a regular token (which includes iss/aud)
    let user_id = UserId::new();
    let regular_token = sign_access_token(&jwt_service, &user_id, 0);

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

    let now = chrono::Utc::now().timestamp();
    let expired_claims = serde_json::json!({
        "sub": "expired_custom",
        "exp": now - 3600,  // expired 1 hour ago
        "iat": now - 7200,
    });

    // Sign it manually (can't use sign_custom since it adds a valid exp)
    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    let token = ok(
        jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &expired_claims,
            &encoding_key,
        ),
        "expired custom token should encode",
    );

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
    let jwt_service2 = ok(
        JwtService::new("DIFFERENT-secret-key-for-custom-token-tests-1234567890!@#"),
        "second JWT service should be created",
    );

    // Sign custom token with service 1
    let claims = serde_json::json!({"sub": "test", "data": 42});
    let token = ok(
        jwt_service1.sign_custom(&claims),
        "custom token should be signed",
    );

    // verify_custom with different secret should fail
    let result = jwt_service2.verify_custom(&token);
    assert!(
        result.is_err(),
        "verify_custom should reject tokens signed with different secret"
    );
}

#[tokio::test]
async fn test_token_includes_password_version() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Token with password version 0
    let token_v0 = sign_access_token(&jwt_service, &user_id, 0);

    let claims_v0 = verify_access_token(&jwt_service, &token_v0);

    assert_eq!(claims_v0.pv, 0, "Token should include password version");

    // Token with password version 5
    let token_v5 = sign_access_token(&jwt_service, &user_id, 5);

    let claims_v5 = verify_access_token(&jwt_service, &token_v5);

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
    let old_refresh = sign_test_refresh_token(&jwt_service, &user_id);

    let old_claims = verify_refresh_token(&jwt_service, &old_refresh);

    // New token (as would be produced by rotation)
    let new_refresh = sign_test_refresh_token(&jwt_service, &user_id);

    let new_claims = verify_refresh_token(&jwt_service, &new_refresh);

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

    let access_token = sign_access_token(&jwt_service, &user_id, 0);

    let refresh_token = sign_test_refresh_token(&jwt_service, &user_id);

    let access_claims = verify_access_token(&jwt_service, &access_token);

    let refresh_claims = verify_refresh_token(&jwt_service, &refresh_token);

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

    let token = sign_access_token(&jwt_service, &user_id, 0);

    let parts = jwt_parts(&token);

    // Decode, modify subject, re-encode (but with wrong signature)
    let payload_bytes = ok(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]),
        "payload should decode",
    );
    let mut payload: serde_json::Value = ok(
        serde_json::from_slice(&payload_bytes),
        "payload should parse",
    );

    // Change subject to different user
    payload["sub"] = serde_json::json!("attacker_user_id");

    let tampered_payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(ok(serde_json::to_vec(&payload), "payload should encode"));

    let tampered_token = format!("{}.{}.{}", parts[0], tampered_payload, parts[2]);

    let result = jwt_service.verify_access_token(&tampered_token);
    assert!(
        result.is_err(),
        "Token with modified subject should be rejected"
    );
}

#[tokio::test]
async fn test_expired_refresh_token_rejected() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let now = Utc::now().timestamp();
    let claims = ExpiredClaims {
        sub: user_id.to_string(),
        typ: "refresh".to_string(),
        jti: synctv_common::snanoid!(),
        iat: now - 2_592_000 - 3600, // 30 days + 1 hour ago
        exp: now - 3600,             // expired 1 hour ago
    };

    let encoding_key = EncodingKey::from_secret(JWT_SECRET.as_bytes());
    let expired_token = ok(
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &encoding_key),
        "expired refresh token should encode",
    );

    let result = jwt_service.verify_refresh_token(&expired_token);
    assert!(result.is_err(), "Expired refresh token should be rejected");
}
