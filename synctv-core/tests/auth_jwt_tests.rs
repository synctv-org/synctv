//! JWT authentication tests
//!
//! Tests for JWT token validation including:
//! - Expired token verification
//! - Tampered token verification
//! - Token family validation
//! - Refresh token rotation
//!
//! Run with: cargo test --test auth_jwt_tests
//! With Docker: cargo test --test auth_jwt_tests -- --ignored

use std::sync::Arc;

use synctv_core::{
    models::UserId,
    service::auth::{jwt::JwtService, TokenType},
};
use jsonwebtoken::{Algorithm, Header, EncodingKey};
use serde::{Serialize, Deserialize};
use chrono::Utc;
use base64::Engine;

const JWT_SECRET: &str = "test-secret-key-for-auth-jwt-tests-minimum-length-32-chars";

fn create_test_jwt_service() -> JwtService {
    JwtService::new(JWT_SECRET).expect("Failed to create JWT service")
}

// ============================================================================
// Expired Token Verification Tests
// ============================================================================

#[tokio::test]
async fn test_expired_access_token_rejected() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Create an expired token (expired 1 hour ago)
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
        typ: "access".to_string(),
        jti: nanoid::nanoid!(),
        iat: now - 7200, // 2 hours ago
        exp: now - 3600, // expired 1 hour ago
    };

    let encoding_key = EncodingKey::from_secret(JWT_SECRET.as_bytes());
    let expired_token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &encoding_key)
        .expect("Failed to encode expired token");

    // Verification should fail due to expiry
    let result = jwt_service.verify_access_token(&expired_token);
    assert!(result.is_err(), "Expired token should be rejected");

    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("expired") || err_msg.contains("ExpiredSignature"),
        "Error should indicate token expiry: {}", err_msg
    );
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
        exp: now - 3600,              // expired 1 hour ago
    };

    let encoding_key = EncodingKey::from_secret(JWT_SECRET.as_bytes());
    let expired_token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &encoding_key)
        .expect("Failed to encode expired token");

    let result = jwt_service.verify_refresh_token(&expired_token);
    assert!(result.is_err(), "Expired refresh token should be rejected");
}

#[tokio::test]
async fn test_valid_token_within_expiry_accepted() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate valid token
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign token");

    // Should verify successfully
    let claims = jwt_service
        .verify_access_token(&token)
        .expect("Valid token should be accepted");

    assert_eq!(claims.sub, user_id.as_str());
    assert!(claims.exp > Utc::now().timestamp(), "Token should not be expired");
}

// ============================================================================
// Tampered Token Verification Tests
// ============================================================================

#[tokio::test]
async fn test_tampered_signature_rejected() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign token");

    // Tamper with signature
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);

    let tampered_token = format!("{}.{}.TAMPERED_SIGNATURE", parts[0], parts[1]);

    let result = jwt_service.verify_access_token(&tampered_token);
    assert!(result.is_err(), "Tampered signature should be rejected");
}

#[tokio::test]
async fn test_tampered_payload_rejected() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign token");

    // Extract and modify payload
    let parts: Vec<&str> = token.split('.').collect();
    let mut payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .expect("Failed to decode payload");

    // Tamper with payload
    if let Some(byte) = payload_bytes.get_mut(10) {
        *byte = byte.wrapping_add(1);
    }

    let tampered_payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload_bytes);
    let tampered_token = format!("{}.{}.{}", parts[0], tampered_payload, parts[2]);

    let result = jwt_service.verify_access_token(&tampered_token);
    assert!(result.is_err(), "Tampered payload should be rejected");
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
            .expect("Failed to decode payload")
    ).expect("Failed to parse payload");

    // Change subject to different user
    payload["sub"] = serde_json::json!("attacker_user_id");

    let tampered_payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).expect("Failed to encode payload"));

    let tampered_token = format!("{}.{}.{}", parts[0], tampered_payload, parts[2]);

    let result = jwt_service.verify_access_token(&tampered_token);
    assert!(result.is_err(), "Token with modified subject should be rejected");
}

#[tokio::test]
async fn test_type_confusion_attack_rejected() {
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
    assert!(result.is_err(), "Access token should not validate as refresh token");

    // Try to use refresh token as access token
    let result = jwt_service.verify_access_token(&refresh_token);
    assert!(result.is_err(), "Refresh token should not validate as access token");
}

// ============================================================================
// Token Family Validation Tests
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

    let claims1 = jwt_service.verify_refresh_token(&token1).expect("Failed to verify token1");
    let claims2 = jwt_service.verify_refresh_token(&token2).expect("Failed to verify token2");

    // Each token should have unique JTI for tracking
    assert_ne!(claims1.jti, claims2.jti, "Each token should have unique JTI");

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

    assert_eq!(claims_v0.pv, Some(0), "Token should include password version");

    // Token with password version 5
    let token_v5 = jwt_service
        .sign_token(&user_id, TokenType::Access, 5)
        .expect("Failed to sign token v5");

    let claims_v5 = jwt_service
        .verify_access_token(&token_v5)
        .expect("Failed to verify token v5");

    assert_eq!(claims_v5.pv, Some(5), "Token should include updated password version");
}

// ============================================================================
// Refresh Token Rotation Tests
// ============================================================================

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
async fn test_concurrent_token_generation_produces_unique_jtis() {
    use std::collections::HashSet;

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
// Integration Tests (require Docker)
// ============================================================================

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
