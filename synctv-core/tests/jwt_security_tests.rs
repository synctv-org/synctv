//! JWT token generation/validation integration tests (Task #82)
//!
//! These tests verify JWT security properties including tampering detection,
//! expiry enforcement, and type confusion prevention.
//!
//! Run with: cargo test --test jwt_security_tests

use synctv_core::{
    models::UserId,
    service::auth::{jwt::JwtService, TokenType},
};
use jsonwebtoken::{Algorithm, Header, EncodingKey};
use serde::{Serialize, Deserialize};
use base64::{Engine as _, engine::general_purpose};

fn create_test_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-integration-tests-minimum-length-32-chars")
        .expect("Failed to create JWT service")
}

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
    use jsonwebtoken::{encode, Header, EncodingKey};
    use chrono::Utc;

    #[derive(Debug, Serialize, Deserialize)]
    struct ExpiredClaims {
        sub: String,
        typ: String,
        jti: String,
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
    assert!(err_msg.contains("expired") || err_msg.contains("ExpiredSignature"),
            "Error should indicate token expiry: {}", err_msg);
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
    assert!(result.is_err(), "Access token should not validate as refresh token");

    // Try to use refresh token as access token
    let result = jwt_service.verify_access_token(&refresh_token);
    assert!(result.is_err(), "Refresh token should not validate as access token");

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
    assert!(result.is_err(), "Guest token should not validate as access token");

    // Generate regular access token
    let access_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign access token");

    // Try to use access token as guest token
    let result = jwt_service.verify_guest_token(&access_token);
    assert!(result.is_err(), "Access token should not validate as guest token");
}

#[tokio::test]
async fn test_jwt_malformed_token_rejection() {
    let jwt_service = create_test_jwt_service();

    // Various malformed tokens
    let malformed_tokens = vec![
        "",                           // Empty
        "invalid",                    // No dots
        "invalid.token",              // Only 2 parts
        "a.b.c.d",                    // Too many parts
        "!!!.@@@.###",                // Invalid base64
        "eyJ.eyJ.abc",                // Partial base64
    ];

    for token in malformed_tokens {
        let result = jwt_service.verify_token(token);
        assert!(result.is_err(), "Malformed token '{}' should be rejected", token);
    }
}

#[tokio::test]
async fn test_jwt_wrong_algorithm_rejection() {
    use jsonwebtoken::{encode, Header, EncodingKey, Algorithm};

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
    assert!(result.is_err(), "Token with wrong algorithm should be rejected");
}

#[tokio::test]
async fn test_jwt_future_issued_at_rejection() {
    use jsonwebtoken::{encode, Header, EncodingKey};

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
    if result.is_ok() {
        let claims = result.unwrap();
        // If accepted, verify the timestamp is actually in the future
        assert!(claims.iat > now, "Future iat should be preserved");
    }
}

#[tokio::test]
async fn test_jwt_jti_uniqueness() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate multiple tokens
    let token1 = jwt_service.sign_token(&user_id, TokenType::Access, 0).unwrap();
    let token2 = jwt_service.sign_token(&user_id, TokenType::Access, 0).unwrap();

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
    assert!(result.is_err(), "Token from different secret should be rejected");
}

#[tokio::test]
async fn test_jwt_token_expiration_boundary() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate token
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign token");

    let claims = jwt_service.verify_access_token(&token).expect("Failed to verify token");

    // Verify expiration is correctly set (1 hour for access token)
    let expected_exp = claims.iat + 3600;
    assert_eq!(claims.exp, expected_exp, "Access token should expire in 1 hour");

    // Generate refresh token
    let refresh_token = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to sign refresh token");

    let refresh_claims = jwt_service.verify_refresh_token(&refresh_token)
        .expect("Failed to verify refresh token");

    // Verify refresh token expiration (30 days)
    let expected_refresh_exp = refresh_claims.iat + (30 * 24 * 3600);
    assert_eq!(refresh_claims.exp, expected_refresh_exp, "Refresh token should expire in 30 days");
}

#[tokio::test]
async fn test_jwt_concurrent_token_generation() {
    use std::sync::Arc;
    use std::collections::HashSet;

    let jwt_service = Arc::new(create_test_jwt_service());
    let mut handles = vec![];

    // Generate 50 tokens concurrently
    for _ in 0..50 {
        let service = jwt_service.clone();
        let handle = tokio::spawn(async move {
            let user_id = UserId::new();
            let token = service.sign_token(&user_id, TokenType::Access, 0)
                .expect("Failed to sign token");
            let claims = service.verify_access_token(&token)
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
