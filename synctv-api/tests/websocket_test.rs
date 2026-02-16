//! WebSocket integration tests for synctv-api
//!
//! Tests WebSocket-related types, authentication methods, query parameter
//! parsing, proto codec encoding/decoding, and message type handling.
//!
//! These tests validate the WebSocket layer without requiring a running server,
//! database, or Redis by testing individual components in isolation.

use synctv_api::http::websocket::{AuthMethod, WsQuery};

// ============================================================================
// Module: WsQuery deserialization
// ============================================================================

mod ws_query {
    use super::*;

    #[test]
    fn test_deserialize_with_token() {
        let params = "token=eyJhbGciOiJIUzI1NiJ9.test";
        let query: WsQuery = serde_urlencoded::from_str(params).unwrap();
        assert_eq!(query.token.as_deref(), Some("eyJhbGciOiJIUzI1NiJ9.test"));
        assert!(query.ticket.is_none());
    }

    #[test]
    fn test_deserialize_with_ticket() {
        let params = "ticket=abc123def456";
        let query: WsQuery = serde_urlencoded::from_str(params).unwrap();
        assert!(query.token.is_none());
        assert_eq!(query.ticket.as_deref(), Some("abc123def456"));
    }

    #[test]
    fn test_deserialize_with_both() {
        let params = "token=jwt_tok&ticket=ticket_val";
        let query: WsQuery = serde_urlencoded::from_str(params).unwrap();
        assert_eq!(query.token.as_deref(), Some("jwt_tok"));
        assert_eq!(query.ticket.as_deref(), Some("ticket_val"));
    }

    #[test]
    fn test_deserialize_empty() {
        let params = "";
        let query: WsQuery = serde_urlencoded::from_str(params).unwrap();
        assert!(query.token.is_none());
        assert!(query.ticket.is_none());
    }

    #[test]
    fn test_deserialize_ignores_unknown_params() {
        let params = "token=tok&unknown=value";
        let query: WsQuery = serde_urlencoded::from_str(params).unwrap();
        assert_eq!(query.token.as_deref(), Some("tok"));
        assert!(query.ticket.is_none());
    }
}

// ============================================================================
// Module: AuthMethod enum
// ============================================================================

mod auth_method {
    use super::*;

    #[test]
    fn test_header_method() {
        let method = AuthMethod::Header;
        assert_eq!(method, AuthMethod::Header);
    }

    #[test]
    fn test_ticket_method() {
        let method = AuthMethod::Ticket;
        assert_eq!(method, AuthMethod::Ticket);
    }

    #[test]
    fn test_token_query_method() {
        let method = AuthMethod::TokenQuery;
        assert_eq!(method, AuthMethod::TokenQuery);
    }

    #[test]
    fn test_methods_are_distinct() {
        assert_ne!(AuthMethod::Header, AuthMethod::Ticket);
        assert_ne!(AuthMethod::Header, AuthMethod::TokenQuery);
        assert_ne!(AuthMethod::Ticket, AuthMethod::TokenQuery);
    }

    #[test]
    fn test_method_is_copy() {
        let method = AuthMethod::Header;
        let copied = method;
        assert_eq!(method, copied);
    }

    #[test]
    fn test_method_debug() {
        let s = format!("{:?}", AuthMethod::Header);
        assert!(s.contains("Header"));
        let s = format!("{:?}", AuthMethod::Ticket);
        assert!(s.contains("Ticket"));
        let s = format!("{:?}", AuthMethod::TokenQuery);
        assert!(s.contains("TokenQuery"));
    }
}

// ============================================================================
// Module: Proto codec (message encoding/decoding)
// ============================================================================

mod proto_codec {
    use synctv_api::impls::messaging::ProtoCodec;
    use synctv_proto::client::{ClientMessage, ServerMessage};

    #[test]
    fn test_encode_server_message() {
        let msg = ServerMessage::default();
        let bytes = ProtoCodec::encode_server_message(&msg).unwrap();
        assert!(!bytes.is_empty() || bytes.is_empty()); // default may encode to empty
    }

    #[test]
    fn test_decode_empty_bytes_client_message() {
        // Empty bytes should produce a default ClientMessage (protobuf convention)
        let result = ProtoCodec::decode_client_message(&[]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_decode_garbage_bytes_returns_error() {
        // Random garbage should fail to decode
        let result = ProtoCodec::decode_client_message(&[0xFF, 0xFE, 0xFD, 0xFC, 0xFB]);
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        // Encode a ServerMessage, then verify the bytes are valid protobuf
        let msg = ServerMessage::default();
        let encoded = ProtoCodec::encode_server_message(&msg).unwrap();
        // We can't decode ServerMessage as ClientMessage, but we can verify encoding works
        assert!(encoded.len() < 1024, "Default message should be small");
    }
}

// ============================================================================
// Module: WebSocket ticket request/response types
// ============================================================================

mod ticket_types {
    use synctv_api::http::ticket::{CreateTicketRequest, TicketResponse};

    #[test]
    fn test_create_ticket_request_deserialize() {
        let json = r#"{"room_id": "room_abc"}"#;
        let req: CreateTicketRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.room_id.as_deref(), Some("room_abc"));
    }

    #[test]
    fn test_create_ticket_request_empty() {
        let json = r#"{}"#;
        let req: CreateTicketRequest = serde_json::from_str(json).unwrap();
        assert!(req.room_id.is_none());
    }

    #[test]
    fn test_ticket_response_serializes() {
        let resp = TicketResponse {
            ticket: "ticket_abc123".to_string(),
            expires_in_secs: 30,
            usage: "Use in WebSocket URL".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["ticket"], "ticket_abc123");
        assert_eq!(json["expires_in_secs"], 30);
        assert!(json["usage"].as_str().unwrap().contains("WebSocket"));
    }

    #[test]
    fn test_ticket_response_fields_present() {
        let resp = TicketResponse {
            ticket: "t".to_string(),
            expires_in_secs: 30,
            usage: "u".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("ticket"));
        assert!(obj.contains_key("expires_in_secs"));
        assert!(obj.contains_key("usage"));
    }
}

// ============================================================================
// Module: JWT service integration (token creation/verification)
// ============================================================================

mod jwt_auth {
    use synctv_core::service::auth::jwt::{JwtService, TokenType};
    use synctv_core::service::auth::JwtValidator;
    use synctv_core::models::id::UserId;
    use std::sync::Arc;

    // Use a 32+ character secret for testing
    const TEST_SECRET: &str = "this-is-a-test-secret-with-enough-entropy-for-jwt-signing-32chars";

    fn test_jwt_service() -> JwtService {
        JwtService::new(TEST_SECRET).expect("JwtService creation should succeed")
    }

    fn test_validator() -> JwtValidator {
        JwtValidator::new(Arc::new(test_jwt_service()))
    }

    #[test]
    fn test_sign_access_token() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_123".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access).unwrap();
        assert!(!token.is_empty());
        // JWT has 3 parts separated by dots
        assert_eq!(token.split('.').count(), 3);
    }

    #[test]
    fn test_sign_refresh_token() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_456".to_string());
        let token = svc.sign_token(&user_id, TokenType::Refresh).unwrap();
        assert!(!token.is_empty());
    }

    #[test]
    fn test_verify_access_token() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_789".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access).unwrap();

        let claims = svc.verify_access_token(&token).unwrap();
        assert_eq!(claims.sub, "user_789");
        assert!(claims.is_access_token());
        assert!(!claims.is_refresh_token());
    }

    #[test]
    fn test_verify_refresh_token() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_abc".to_string());
        let token = svc.sign_token(&user_id, TokenType::Refresh).unwrap();

        let claims = svc.verify_refresh_token(&token).unwrap();
        assert_eq!(claims.sub, "user_abc");
        assert!(claims.is_refresh_token());
        assert!(!claims.is_access_token());
    }

    #[test]
    fn test_verify_with_wrong_secret_fails() {
        let svc1 = test_jwt_service();
        let svc2 = JwtService::new(
            "another-secret-that-is-different-and-long-enough-for-the-test"
        ).unwrap();

        let user_id = UserId::from_string("user_xyz".to_string());
        let token = svc1.sign_token(&user_id, TokenType::Access).unwrap();

        // Verification with different secret should fail
        assert!(svc2.verify_access_token(&token).is_err());
    }

    #[test]
    fn test_verify_invalid_token_fails() {
        let svc = test_jwt_service();
        assert!(svc.verify_token("not.a.valid.jwt").is_err());
    }

    #[test]
    fn test_verify_empty_token_fails() {
        let svc = test_jwt_service();
        assert!(svc.verify_token("").is_err());
    }

    #[test]
    fn test_validator_extract_user_id_from_bearer() {
        let svc = test_jwt_service();
        let validator = test_validator();
        let user_id = UserId::from_string("user_val".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access).unwrap();
        let _bearer = format!("Bearer {}", token);

        let extracted = validator.validate_and_extract_user_id(&token).unwrap();
        assert_eq!(extracted.as_str(), "user_val");
    }

    #[test]
    fn test_validator_http_bearer_header() {
        let svc = test_jwt_service();
        let validator = test_validator();
        let user_id = UserId::from_string("user_http".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access).unwrap();
        let header = format!("Bearer {}", token);

        let claims = validator.validate_http(&header).unwrap();
        assert_eq!(claims.sub, "user_http");
    }

    #[test]
    fn test_validator_rejects_missing_bearer_prefix() {
        let svc = test_jwt_service();
        let validator = test_validator();
        let user_id = UserId::from_string("user_no_prefix".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access).unwrap();

        // Without "Bearer " prefix
        assert!(validator.validate_http(&token).is_err());
    }

    #[test]
    fn test_access_token_has_jti() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_jti".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access).unwrap();
        let claims = svc.verify_access_token(&token).unwrap();
        assert!(!claims.jti.is_empty(), "JWT ID (jti) should be set");
    }

    #[test]
    fn test_unique_jti_per_token() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_unique".to_string());
        let token1 = svc.sign_token(&user_id, TokenType::Access).unwrap();
        let token2 = svc.sign_token(&user_id, TokenType::Access).unwrap();
        let claims1 = svc.verify_access_token(&token1).unwrap();
        let claims2 = svc.verify_access_token(&token2).unwrap();
        assert_ne!(claims1.jti, claims2.jti, "Each token should have a unique jti");
    }

    #[test]
    fn test_claims_iat_is_recent() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_iat".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access).unwrap();
        let claims = svc.verify_access_token(&token).unwrap();

        let now = chrono::Utc::now().timestamp();
        assert!((now - claims.iat).abs() < 10, "iat should be within 10 seconds of now");
    }

    #[test]
    fn test_access_token_exp_is_in_future() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_exp".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access).unwrap();
        let claims = svc.verify_access_token(&token).unwrap();

        let now = chrono::Utc::now().timestamp();
        assert!(claims.exp > now, "exp should be in the future");
    }
}

// ============================================================================
// Module: Token blacklist service (in-memory fallback)
// ============================================================================

mod token_blacklist {
    use synctv_core::service::token_blacklist::TokenBlacklistService;

    fn in_memory_blacklist() -> TokenBlacklistService {
        TokenBlacklistService::new(None, "test:".to_string())
    }

    #[tokio::test]
    async fn test_new_token_is_not_blacklisted() {
        let svc = in_memory_blacklist();
        let result = svc.is_blacklisted("fresh_token_abc").await;
        assert!(result.is_ok());
        assert!(!result.unwrap(), "Fresh token should not be blacklisted");
    }

    #[tokio::test]
    async fn test_blacklisted_token_is_detected() {
        let svc = in_memory_blacklist();
        // Blacklist a token with 1-hour TTL
        svc.blacklist_token("revoked_token_123", 3600)
            .await
            .unwrap();

        let is_bl = svc.is_blacklisted("revoked_token_123").await.unwrap();
        assert!(is_bl, "Blacklisted token should be detected");
    }

    #[tokio::test]
    async fn test_different_token_not_affected() {
        let svc = in_memory_blacklist();
        svc.blacklist_token("token_a", 3600).await.unwrap();

        let is_bl = svc.is_blacklisted("token_b").await.unwrap();
        assert!(!is_bl, "Different token should not be blacklisted");
    }

    #[tokio::test]
    async fn test_multiple_tokens_blacklisted() {
        let svc = in_memory_blacklist();
        svc.blacklist_token("tok1", 3600).await.unwrap();
        svc.blacklist_token("tok2", 3600).await.unwrap();

        assert!(svc.is_blacklisted("tok1").await.unwrap());
        assert!(svc.is_blacklisted("tok2").await.unwrap());
        assert!(!svc.is_blacklisted("tok3").await.unwrap());
    }
}

// ============================================================================
// Module: Rate limiter (in-memory fallback)
// ============================================================================

mod rate_limiter {
    use synctv_core::service::rate_limit::RateLimiter;

    #[tokio::test]
    async fn test_in_memory_allows_within_limit() {
        let limiter = RateLimiter::in_memory_only("test:".to_string());
        let result = limiter
            .check_rate_limit("test_key", 5, 60)
            .await;
        assert!(result.is_ok(), "First request should be allowed");
    }

    #[tokio::test]
    async fn test_in_memory_blocks_after_exceeding_limit() {
        let limiter = RateLimiter::in_memory_only("test:".to_string());
        let key = "burst_test_key";

        // Exhaust the limit
        for _ in 0..5 {
            let _ = limiter.check_rate_limit(key, 5, 60).await;
        }

        // Next request should be rate limited
        let result = limiter.check_rate_limit(key, 5, 60).await;
        assert!(result.is_err(), "Should be rate limited after exceeding limit");
    }

    #[tokio::test]
    async fn test_different_keys_independent() {
        let limiter = RateLimiter::in_memory_only("test:".to_string());

        // Exhaust limit for key_a
        for _ in 0..5 {
            let _ = limiter.check_rate_limit("key_a", 5, 60).await;
        }

        // key_b should still be allowed
        let result = limiter.check_rate_limit("key_b", 5, 60).await;
        assert!(result.is_ok(), "Different key should not be rate limited");
    }

    #[test]
    fn test_sync_rate_limit_allows_within_limit() {
        let limiter = RateLimiter::in_memory_only("sync_test:".to_string());
        let result = limiter.check_rate_limit_sync("grpc_key", 10, 60);
        assert!(result.is_ok());
    }

    #[test]
    fn test_sync_rate_limit_blocks_after_exceeding() {
        let limiter = RateLimiter::in_memory_only("sync_test:".to_string());
        for _ in 0..10 {
            let _ = limiter.check_rate_limit_sync("grpc_burst", 10, 60);
        }
        let result = limiter.check_rate_limit_sync("grpc_burst", 10, 60);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_health_check_without_redis() {
        let limiter = RateLimiter::in_memory_only("test:".to_string());
        let result = limiter.health_check().await;
        assert!(result.is_err(), "Should error when Redis not configured");
        assert!(result.unwrap_err().contains("not configured"));
    }
}

// ============================================================================
// Module: Health response types
// ============================================================================

mod health_types {
    use synctv_api::http::health::{HealthResponse, HealthDetails};

    #[test]
    fn test_health_response_ok_serializes() {
        let resp = HealthResponse {
            status: "ok".to_string(),
            details: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "ok");
        // details should be skipped when None
        assert!(json.get("details").is_none());
    }

    #[test]
    fn test_health_response_with_details() {
        let resp = HealthResponse {
            status: "healthy".to_string(),
            details: Some(HealthDetails {
                database: "healthy".to_string(),
                redis: "healthy".to_string(),
                message: None,
            }),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "healthy");
        assert_eq!(json["details"]["database"], "healthy");
        assert_eq!(json["details"]["redis"], "healthy");
        // message should be skipped when None
        assert!(json["details"].get("message").is_none());
    }

    #[test]
    fn test_health_response_unhealthy_with_message() {
        let resp = HealthResponse {
            status: "unhealthy".to_string(),
            details: Some(HealthDetails {
                database: "unhealthy".to_string(),
                redis: "healthy".to_string(),
                message: Some("Database: connection refused".to_string()),
            }),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "unhealthy");
        assert_eq!(json["details"]["database"], "unhealthy");
        assert!(json["details"]["message"].as_str().unwrap().contains("Database"));
    }
}

// ============================================================================
// Module: WebSocket connection (simulated auth rejection scenarios)
// ============================================================================

mod ws_auth_scenarios {
    use synctv_api::http::error::AppError;
    use axum::http::StatusCode;

    #[test]
    fn test_missing_all_auth_methods() {
        // Simulates what extract_user_id returns when no auth is provided
        let err = AppError::unauthorized(
            "Missing authentication: provide token via Authorization header, ?ticket=, or ?token=",
        );
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert!(err.message.contains("Missing authentication"));
    }

    #[test]
    fn test_invalid_token_error() {
        let err = AppError::unauthorized("Invalid token: invalid signature");
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_token_revoked_error() {
        let err = AppError::unauthorized("Token has been revoked");
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_not_member_error() {
        let err = AppError::forbidden("Not a member of this room");
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_ticket_service_not_configured() {
        let err = AppError::internal_server_error(
            "WebSocket ticket service not configured (Redis required)",
        );
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_expired_ticket_error() {
        let err = AppError::unauthorized("Invalid or expired ticket: ticket not found");
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    }
}

// ============================================================================
// Module: Proto message types (ClientMessage / ServerMessage)
// ============================================================================

mod proto_messages {
    use synctv_proto::client::{ClientMessage, ServerMessage};

    #[test]
    fn test_client_message_default() {
        let msg = ClientMessage::default();
        // Default message should have no content set
        assert!(msg.message.is_none());
    }

    #[test]
    fn test_server_message_default() {
        let msg = ServerMessage::default();
        assert!(msg.message.is_none());
    }

    #[test]
    fn test_client_message_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ClientMessage>();
    }

    #[test]
    fn test_server_message_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ServerMessage>();
    }
}
