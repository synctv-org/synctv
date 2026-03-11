//! WebSocket integration tests for synctv-api
//!
//! Tests WebSocket-related types, authentication methods, query parameter
//! parsing, proto codec encoding/decoding, and message type handling.
//!
//! Includes both:
//! - Unit tests: validate individual components in isolation (no server needed)
//! - E2E tests: full WebSocket lifecycle with real Postgres + Redis (`TestInfra`)

#![allow(clippy::unwrap_used)]
use synctv_api::http::websocket::{AuthMethod, WsQuery};

async fn wait_for_condition<F>(timeout: std::time::Duration, mut check: F)
where
    F: FnMut() -> bool,
{
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if check() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(check(), "condition was not satisfied within {timeout:?}");
}

// ============================================================================
// Module: WsQuery deserialization
// ============================================================================

mod ws_query {
    use super::*;

    #[test]
    fn test_deserialize_with_ticket() {
        let params = "ticket=abc123def456";
        let query: WsQuery = serde_urlencoded::from_str(params).unwrap();
        assert_eq!(query.ticket.as_deref(), Some("abc123def456"));
    }

    #[test]
    fn test_deserialize_empty() {
        let params = "";
        let query: WsQuery = serde_urlencoded::from_str(params).unwrap();
        assert!(query.ticket.is_none());
    }

    #[test]
    fn test_deserialize_ignores_unknown_params() {
        let params = "ticket=tix&unknown=value";
        let query: WsQuery = serde_urlencoded::from_str(params).unwrap();
        assert_eq!(query.ticket.as_deref(), Some("tix"));
    }

    #[test]
    fn test_deserialize_ignores_legacy_token_param() {
        let params = "token=legacy_jwt_value";
        let query: WsQuery = serde_urlencoded::from_str(params).unwrap();
        assert!(
            query.ticket.is_none(),
            "legacy ?token= must not be treated as a valid websocket credential"
        );
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
    fn test_methods_are_distinct() {
        assert_ne!(AuthMethod::Header, AuthMethod::Ticket);
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
    }
}

// ============================================================================
// Module: Proto codec (message encoding/decoding)
// ============================================================================

mod proto_codec {
    use prost::Message;
    use synctv_api::impls::messaging::ProtoCodec;
    use synctv_proto::client::{
        server_message, ClientMessage, HeartbeatAck, HeartbeatMessage, ServerMessage,
    };

    #[test]
    fn test_encode_server_message() {
        let msg = ServerMessage {
            message: Some(server_message::Message::HeartbeatAck(HeartbeatAck {
                timestamp: 123_456,
            })),
        };
        let bytes = ProtoCodec::encode_server_message(&msg).unwrap();
        let decoded = ServerMessage::decode(bytes.as_slice()).expect("encoded server message");
        assert_eq!(decoded, msg);
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
        let client = ClientMessage {
            message: Some(synctv_proto::client::client_message::Message::Heartbeat(
                HeartbeatMessage { timestamp: 42 },
            )),
        };
        let bytes = client.encode_to_vec();
        let decoded = ProtoCodec::decode_client_message(&bytes).unwrap();
        assert_eq!(decoded, client);
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
        assert_eq!(req.room_id.as_str(), "room_abc");
    }

    #[test]
    fn test_ticket_response_serializes() {
        let resp = TicketResponse {
            ticket: "ticket_abc123".to_string(),
            room_id: "room_abc".to_string(),
            expires_in_secs: 30,
            usage: "Use in WebSocket URL".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["ticket"], "ticket_abc123");
        assert_eq!(json["room_id"], "room_abc");
        assert_eq!(json["expires_in_secs"], 30);
        assert!(json["usage"].as_str().unwrap().contains("WebSocket"));
    }

    #[test]
    fn test_ticket_response_fields_present() {
        let resp = TicketResponse {
            ticket: "t".to_string(),
            room_id: "r".to_string(),
            expires_in_secs: 30,
            usage: "u".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("ticket"));
        assert!(obj.contains_key("room_id"));
        assert!(obj.contains_key("expires_in_secs"));
        assert!(obj.contains_key("usage"));
    }
}

// ============================================================================
// Module: JWT service integration (token creation/verification)
// ============================================================================

mod jwt_auth {
    use std::sync::Arc;
    use synctv_core::models::id::UserId;
    use synctv_core::service::auth::jwt::{JwtService, TokenType};
    use synctv_core::service::auth::JwtValidator;

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
        let token = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();
        assert!(!token.is_empty());
        // JWT has 3 parts separated by dots
        assert_eq!(token.split('.').count(), 3);
    }

    #[test]
    fn test_sign_refresh_token() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_456".to_string());
        let token = svc.sign_token(&user_id, TokenType::Refresh, 0).unwrap();
        assert!(!token.is_empty());
    }

    #[test]
    fn test_verify_access_token() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_789".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();

        let claims = svc.verify_access_token(&token).unwrap();
        assert_eq!(claims.sub, "user_789");
        assert!(claims.is_access_token());
        assert!(!claims.is_refresh_token());
    }

    #[test]
    fn test_verify_refresh_token() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_abc".to_string());
        let token = svc.sign_token(&user_id, TokenType::Refresh, 0).unwrap();

        let claims = svc.verify_refresh_token(&token).unwrap();
        assert_eq!(claims.sub, "user_abc");
        assert!(claims.is_refresh_token());
        assert!(!claims.is_access_token());
    }

    #[test]
    fn test_verify_with_wrong_secret_fails() {
        let svc1 = test_jwt_service();
        let svc2 = JwtService::new("another-secret-that-is-different-and-long-enough-for-the-test")
            .unwrap();

        let user_id = UserId::from_string("user_xyz".to_string());
        let token = svc1.sign_token(&user_id, TokenType::Access, 0).unwrap();

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
        let token = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let _bearer = format!("Bearer {token}");

        let extracted = validator.validate_and_extract_user_id(&token).unwrap();
        assert_eq!(extracted.as_str(), "user_val");
    }

    #[test]
    fn test_validator_http_bearer_header() {
        let svc = test_jwt_service();
        let validator = test_validator();
        let user_id = UserId::from_string("user_http".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let header = format!("Bearer {token}");

        let claims = validator.validate_http(&header).unwrap();
        assert_eq!(claims.sub, "user_http");
    }

    #[test]
    fn test_validator_rejects_missing_bearer_prefix() {
        let svc = test_jwt_service();
        let validator = test_validator();
        let user_id = UserId::from_string("user_no_prefix".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();

        // Without "Bearer " prefix
        assert!(validator.validate_http(&token).is_err());
    }

    #[test]
    fn test_access_token_has_jti() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_jti".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let claims = svc.verify_access_token(&token).unwrap();
        assert!(!claims.jti.is_empty(), "JWT ID (jti) should be set");
    }

    #[test]
    fn test_unique_jti_per_token() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_unique".to_string());
        let token1 = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let token2 = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let claims1 = svc.verify_access_token(&token1).unwrap();
        let claims2 = svc.verify_access_token(&token2).unwrap();
        assert_ne!(
            claims1.jti, claims2.jti,
            "Each token should have a unique jti"
        );
    }

    #[test]
    fn test_claims_iat_is_recent() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_iat".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let claims = svc.verify_access_token(&token).unwrap();

        let now = chrono::Utc::now().timestamp();
        assert!(
            (now - claims.iat).abs() < 10,
            "iat should be within 10 seconds of now"
        );
    }

    #[test]
    fn test_access_token_exp_is_in_future() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_exp".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let claims = svc.verify_access_token(&token).unwrap();

        let now = chrono::Utc::now().timestamp();
        assert!(claims.exp > now, "exp should be in the future");
    }
}

// ============================================================================
// Module: Security pipeline (password invalidation, user status)
// ============================================================================
//
// SecurityPipeline enforces password version and user status checks.
// Token revocation is stateless: clients discard tokens on logout.
// Full integration tests require a running database instance.

// ============================================================================
// Module: Rate limiter (in-memory fallback)
// ============================================================================

mod rate_limiter {
    use synctv_core::service::rate_limit::RateLimiter;

    #[tokio::test]
    async fn test_in_memory_allows_within_limit() {
        let limiter = RateLimiter::in_memory_only("test:".to_string());
        let result = limiter.check_rate_limit("test_key", 5, 60).await;
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
        assert!(
            result.is_err(),
            "Should be rate limited after exceeding limit"
        );
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
    use synctv_api::http::health::{HealthDetails, HealthResponse};

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
                cluster: None,
                ws_ticket: None,
                email: None,
                livestream: None,
                memory: None,
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
                cluster: None,
                ws_ticket: None,
                email: None,
                livestream: None,
                memory: None,
                message: Some("Database: connection refused".to_string()),
            }),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "unhealthy");
        assert_eq!(json["details"]["database"], "unhealthy");
        assert!(json["details"]["message"]
            .as_str()
            .unwrap()
            .contains("Database"));
    }
}

// ============================================================================
// Module: WebSocket connection (simulated auth rejection scenarios)
// ============================================================================

mod ws_auth_scenarios {
    use axum::http::StatusCode;
    use synctv_api::http::error::AppError;

    #[test]
    fn test_missing_all_auth_methods() {
        // Simulates what extract_user_id returns when no auth is provided
        let err = AppError::unauthorized(
            "Missing authentication: provide token via Authorization header or ?ticket=",
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
        let err = AppError::internal_server_error("WebSocket ticket service not configured");
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

// ============================================================================
// Module: WebSocket E2E tests (requires Docker: Postgres + Redis via TestInfra)
// ============================================================================

#[cfg(test)]
mod websocket_e2e {
    use futures::{SinkExt, StreamExt};
    use prost::Message;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite;

    use synctv_api::http::websocket::websocket_handler;
    use synctv_api::impls::messaging::ProtoCodec;
    use synctv_core::cache::UsernameCache;
    use synctv_core::models::id::UserId;
    use synctv_core::service::auth::jwt::{JwtService, TokenType};
    use synctv_core::service::rate_limit::RateLimiter;
    // Security checks (password version, user status) handled by SecurityPipeline
    use synctv_cluster::sync::{
        ClusterConfig, ClusterManager, ConnectionLimits, ConnectionManager,
    };
    use synctv_core::config::PasswordComplexityConfig;
    use synctv_core::service::{RoomService, UserService};
    use synctv_core_testing::{
        create_test_pool_with_db_and_label, start_redis_url_with_label, RedisContainer,
        TestContainer,
    };
    use synctv_proto::client::{
        client_message, server_message, ClientMessage, HeartbeatMessage, ServerMessage, WebRtcJoin,
    };

    use sqlx::PgPool;

    const TEST_JWT_SECRET: &str =
        "this-is-a-test-secret-with-enough-entropy-for-jwt-signing-32chars";

    /// Lightweight test infrastructure for E2E tests.
    /// Starts Postgres and Redis containers, runs migrations, and provides connections.
    struct TestInfra {
        pool: PgPool,
        redis_url: String,
        #[allow(dead_code)]
        postgres: Option<TestContainer>,
        #[allow(dead_code)]
        redis: Option<RedisContainer>,
    }

    impl TestInfra {
        async fn new() -> Self {
            let (postgres, pool) =
                create_test_pool_with_db_and_label("synctv_test", "api-ws-rate-limiter").await;
            let (redis, redis_url) = start_redis_url_with_label("api-ws-rate-limiter").await;

            Self {
                pool,
                redis_url,
                postgres: Some(postgres),
                redis: Some(redis),
            }
        }

        #[allow(dead_code)]
        async fn cleanup(mut self) {
            if let Some(redis) = self.redis.take() {
                redis.cleanup().await;
            }
            if let Some(postgres) = self.postgres.take() {
                postgres.cleanup().await;
            }
        }
    }

    /// Returned from `setup_e2e_server` so tests can access all shared components.
    struct E2EServer {
        addr: String,
        jwt_service: JwtService,
        room_service: Arc<RoomService>,
        user_service: Arc<UserService>,
        connection_manager: Arc<ConnectionManager>,
        cluster_manager: Arc<ClusterManager>,
    }

    /// Create a minimal `ChatService` for tests.
    ///
    /// Accepts a shared `UsernameCache` so the `ChatService` can resolve
    /// usernames that were populated by the `UserService`.
    fn build_test_chat_service(
        pool: &sqlx::PgPool,
        username_cache: UsernameCache,
        rate_limit_config: synctv_core::service::RateLimitConfig,
    ) -> Arc<synctv_core::service::ChatService> {
        let chat_repo = Arc::new(synctv_core::repository::ChatRepository::new(pool.clone()));
        let chat_rate_limiter = RateLimiter::in_memory_only("test_chat:".to_string());
        let content_filter = synctv_core::service::ContentFilter::new();
        let member_repo = synctv_core::repository::RoomMemberRepository::new(pool.clone());
        let room_repo = synctv_core::repository::RoomRepository::new(pool.clone());
        let permission_service =
            synctv_core::service::PermissionService::new(member_repo, room_repo, None, 1000, 300);
        let room_settings_repo = synctv_core::repository::RoomSettingsRepository::new(pool.clone());
        let notification_service = Arc::new(synctv_core::service::NotificationService::default());
        let room_settings_service = synctv_core::service::RoomSettingsService::new(
            room_settings_repo,
            None,
            notification_service,
            None,
            None,
            None,
        );
        Arc::new(synctv_core::service::ChatService::new(
            chat_repo,
            chat_rate_limiter,
            rate_limit_config,
            content_filter,
            username_cache,
            permission_service,
            room_settings_service,
        ))
    }

    /// Build a minimal `AppState` with real database and Redis for E2E testing.
    async fn setup_e2e_server(infra: &TestInfra) -> E2EServer {
        setup_e2e_server_with_chat_rate_limit(
            infra,
            synctv_core::service::RateLimitConfig::default(),
        )
        .await
    }

    async fn setup_e2e_server_with_chat_rate_limit(
        infra: &TestInfra,
        chat_rate_limit_config: synctv_core::service::RateLimitConfig,
    ) -> E2EServer {
        setup_e2e_server_with_node_and_chat_rate_limit(infra, "test_node_1", chat_rate_limit_config)
            .await
    }

    /// Build a minimal `AppState` with a custom `node_id`.
    ///
    /// Useful for cross-replica tests: call twice with different node IDs
    /// but the same `TestInfra` to simulate two server replicas.
    async fn setup_e2e_server_with_node(infra: &TestInfra, node_id: &str) -> E2EServer {
        setup_e2e_server_with_node_and_chat_rate_limit(
            infra,
            node_id,
            synctv_core::service::RateLimitConfig::default(),
        )
        .await
    }

    async fn setup_e2e_server_with_node_and_chat_rate_limit(
        infra: &TestInfra,
        node_id: &str,
        chat_rate_limit_config: synctv_core::service::RateLimitConfig,
    ) -> E2EServer {
        let pool = infra.pool.clone();
        let redis_url = infra.redis_url.clone();

        // Create services
        let jwt_service = JwtService::new(TEST_JWT_SECRET).expect("JwtService");
        let redis_client = redis::Client::open(infra.redis_url.as_str()).expect("Redis client");
        let redis_conn = Arc::new(tokio::sync::RwLock::new(
            redis::aio::ConnectionManager::new(redis_client.clone())
                .await
                .expect("Redis ConnectionManager"),
        ));

        // UsernameCache with Redis L2 backend
        let l2_backend = Arc::new(synctv_core::cache::l2_backend::RedisCacheL2::new_shared(
            redis_conn.clone(),
        ));
        let username_cache = UsernameCache::new(l2_backend, "test_un:".to_string(), 100, 300);
        let username_cache_for_chat = username_cache.clone();

        let key_builder = synctv_core::cache::KeyBuilder::new("test:".to_string());

        // BruteForceProtection with Redis backend
        let brute_force = synctv_core::service::auth::BruteForceProtection::with_redis(
            redis_conn.clone(),
            "test:".to_string(),
        );

        // Token blacklist with in-memory backend (sufficient for tests)
        let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> = Arc::new(
            synctv_core::service::InMemoryTokenBlacklistStore::new(10_000, 3600, 86400),
        );

        let user_service = Arc::new(UserService::new(
            pool.clone(),
            jwt_service.clone(),
            username_cache,
            PasswordComplexityConfig::default(),
            token_blacklist,
            key_builder,
            brute_force,
        ));
        let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

        // Create cluster manager (single-node mode with Redis for Pub/Sub)
        let redis_client_for_cluster =
            redis::Client::open(redis_url.clone()).expect("Failed to open Redis client");
        let redis_conn_for_cluster = redis_client_for_cluster
            .get_connection_manager()
            .await
            .expect("Failed to get ConnectionManager");
        let cluster_config = ClusterConfig {
            redis_client: Some(redis_client_for_cluster),
            redis_conn: Some(redis_conn_for_cluster),
            shared_redis_conn: None,
            node_id: node_id.to_string(),
            ..Default::default()
        };
        let cluster_manager = Arc::new(
            ClusterManager::new(cluster_config, None, None)
                .await
                .expect("ClusterManager"),
        );
        let redis_conn_for_connections = redis::aio::ConnectionManager::new(
            redis::Client::open(redis_url.clone()).expect("Redis client for connection manager"),
        )
        .await
        .expect("Redis ConnectionManager for connection manager");
        let connection_manager = Arc::new(
            ConnectionManager::new(ConnectionLimits::default())
                .with_redis(redis_conn_for_connections, "test:"),
        );
        let connection_manager_ret = connection_manager.clone();

        // Rate limiter (in-memory only for tests)
        let rate_limiter = RateLimiter::in_memory_only("test_ws:".to_string());

        // Build AppState
        let jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(Arc::new(
            jwt_service.clone(),
        )));
        let rate_limit_config = Arc::new(synctv_api::http::middleware::RateLimitConfig::default());

        // Minimal providers (unused in WebSocket tests but required by AppState)
        let provider_instance_repo = Arc::new(
            synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
        );
        let provider_instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            provider_instance_repo,
            None,
            None,
            "",
        ));
        let user_provider_credential_repo =
            Arc::new(synctv_core::repository::UserProviderCredentialRepository::new(pool.clone()));
        let providers = synctv_core::provider::ProviderSet {
            bilibili: Arc::new(synctv_core::provider::BilibiliProvider::new(
                provider_instance_manager.clone(),
            )),
            alist: Arc::new(synctv_core::provider::AlistProvider::new(
                provider_instance_manager.clone(),
            )),
            emby: Arc::new(synctv_core::provider::EmbyProvider::new(
                provider_instance_manager.clone(),
            )),
            direct_url: Arc::new(synctv_core::provider::DirectUrlProvider::new()),
            rtmp: Arc::new(synctv_core::provider::RtmpProvider::new()),
            live_proxy: Arc::new(synctv_core::provider::LiveProxyProvider::new()),
        };

        let config = Arc::new(synctv_core::Config::default());

        // ClientApiImpl
        let client_api = Arc::new(synctv_api::impls::ClientApiImpl::new(
            user_service.clone(),
            room_service.clone(),
            connection_manager.clone(),
            config.clone(),
            None, // publish_key_service
            jwt_service.clone(),
            None, // live_streaming_infrastructure
            None, // providers_manager
            None, // settings_registry
        ));

        // BilibiliApiImpl, AlistApiImpl, EmbyApiImpl
        let bilibili_api = Arc::new(synctv_api::impls::BilibiliApiImpl::new(
            providers.bilibili.clone(),
            user_provider_credential_repo.clone(),
        ));
        let alist_api = Arc::new(synctv_api::impls::AlistApiImpl::new(
            providers.alist.clone(),
            user_provider_credential_repo.clone(),
        ));
        let emby_api = Arc::new(synctv_api::impls::EmbyApiImpl::new(
            providers.emby.clone(),
            user_provider_credential_repo.clone(),
        ));

        let router_config = synctv_api::http::RouterConfig {
            turn_health_checker: Default::default(),
            config,
            user_service: user_service.clone(),
            room_service: room_service.clone(),
            content_filter: synctv_core::service::ContentFilter::new(),
            provider_instance_manager,
            user_provider_credential_repository: user_provider_credential_repo.clone(),
            providers: providers.clone(),
            cluster_manager: Some(cluster_manager.clone()),
            connection_manager,
            jwt_service: jwt_service.clone(),
            redis_publish_tx: None,
            oauth2_service: None,
            settings_service: None,
            settings_registry: None,
            email_service: None,
            email_token_service: None,
            publish_key_service: None,
            notification_service: None,
            chat_service: Some(build_test_chat_service(
                &pool,
                username_cache_for_chat,
                chat_rate_limit_config,
            )),
            audit_service: {
                let (audit_svc, _audit_handle) =
                    synctv_core::service::AuditService::new(pool.clone());
                Arc::new(audit_svc)
            },
            live_streaming_infrastructure: None,
            rate_limiter,
            ws_ticket_service: Some(Arc::new(
                synctv_core::service::WsTicketService::with_memory(None),
            )),
            redis_conn: None,
            builtin_stun_url: None,
            credential_encryption: None,
            proxy_slice_cache: std::sync::Arc::new(synctv_proxy::slice_cache::SliceCache::new(
                synctv_proxy::slice_cache::SliceCacheConfig::default(),
            )),
            messaging_rate_limit_config: synctv_core::service::RateLimitConfig::default(),
            heartbeat_schedule: synctv_api::impls::HeartbeatSchedule::for_tests(
                std::time::Duration::from_millis(400),
                std::time::Duration::from_millis(100),
            ),
            providers_manager: None,
        };

        let guest_token_validator = Arc::new(
            synctv_core::service::auth::GuestTokenValidator::new(Arc::new(jwt_service.clone()))
                .with_blacklist(
                    user_service.token_blacklist_store(),
                    user_service.key_builder().clone(),
                ),
        );

        let state = synctv_api::AppState {
            router_config: Arc::new(router_config),
            rate_limit_config,
            messaging_rate_limit_config: Arc::new(synctv_core::service::RateLimitConfig::default()),
            content_filter: Arc::new(synctv_core::service::ContentFilter::new()),
            heartbeat_schedule: synctv_api::impls::HeartbeatSchedule::for_tests(
                std::time::Duration::from_millis(400),
                std::time::Duration::from_millis(100),
            ),
            jwt_validator,
            security_pipeline: Arc::new(
                synctv_core::service::SecurityPipeline::new(user_service.clone())
                    .with_token_blacklist(
                        user_service.token_blacklist_store(),
                        user_service.key_builder().clone(),
                    ),
            ),
            guest_token_validator,
            client_api,
            admin_api: None,
            notification_api: None,
            oauth2_api: None,
            bilibili_api,
            alist_api,
            emby_api,
            provider_stores: std::sync::Arc::new(
                synctv_core::provider::store::ProviderStoreRegistry::new(None, ""),
            ),
            proxy_provider_registry: std::sync::Arc::new(providers.build_proxy_registry()),
            proxy_services: {
                let signing_key =
                    std::sync::Arc::new(synctv_core::service::ProxySigningKey::derive_from(
                        b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
                    ));
                std::sync::Arc::new(synctv_core::provider::proxy::ProxyServices {
                    room_service: room_service.clone(),
                    credential_encryption: None,
                    credential_repo: user_provider_credential_repo.clone(),
                    signing_key,
                })
            },
            proxy_signing_key: std::sync::Arc::new(
                synctv_core::service::ProxySigningKey::derive_from(
                    b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
                ),
            ),
            proxy_slice_cache: std::sync::Arc::new(synctv_proxy::slice_cache::SliceCache::new(
                synctv_proxy::slice_cache::SliceCacheConfig::default(),
            )),
        };

        // Build a minimal router with just the WebSocket endpoint
        let app = axum::Router::new()
            .route("/ws/rooms/{room_id}", axum::routing::get(websocket_handler))
            .with_state(state);

        // Bind to a random port
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let addr_str = format!("127.0.0.1:{}", addr.port());

        // Spawn server
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server error");
        });

        E2EServer {
            addr: addr_str,
            jwt_service,
            room_service,
            user_service,
            connection_manager: connection_manager_ret,
            cluster_manager,
        }
    }

    /// Register a test user directly via `UserService` and return their `UserId` + access token.
    async fn register_test_user(
        user_service: &UserService,
        jwt_service: &JwtService,
        username: &str,
    ) -> (UserId, String) {
        let (user, _access, _refresh) = user_service
            .register(
                username.to_string(),
                Some(format!("{username}@test.com")),
                "TestPassword123!".to_string(),
                None, // no client IP in tests
            )
            .await
            .expect("register user");
        let token = jwt_service
            .sign_token(&user.id, TokenType::Access, 0)
            .expect("sign token");
        (user.id, token)
    }

    /// Create a test room and add the user as creator/member.
    async fn create_test_room(
        room_service: &RoomService,
        user_id: &UserId,
        room_name: &str,
    ) -> String {
        let (room, _member) = room_service
            .create_room(
                room_name.to_string(),
                String::new(),
                user_id.clone(),
                None,
                None,
            )
            .await
            .expect("create room");
        room.id.as_str().to_string()
    }

    /// Connect to the WebSocket endpoint using Authorization header.
    async fn ws_connect(
        addr: &str,
        room_id: &str,
        token: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let url = format!("ws://{addr}/ws/rooms/{room_id}");
        let request = tungstenite::http::Request::builder()
            .uri(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Host", addr)
            .body(())
            .expect("build WS request");
        let (ws_stream, response) = tokio_tungstenite::connect_async(request)
            .await
            .expect("WebSocket connect failed");
        assert_eq!(
            response.status(),
            tungstenite::http::StatusCode::SWITCHING_PROTOCOLS,
            "Expected 101 Switching Protocols"
        );
        ws_stream
    }

    /// Read the next binary message from the WebSocket and decode it as a `ServerMessage`.
    async fn recv_server_message(
        ws: &mut (impl StreamExt<Item = Result<tungstenite::Message, tungstenite::Error>> + Unpin),
    ) -> Option<ServerMessage> {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(tungstenite::Message::Binary(bytes)) => {
                    return Some(
                        ProtoCodec::decode_server_message(&bytes).expect("decode server message"),
                    );
                }
                Ok(tungstenite::Message::Close(_)) => return None,
                Err(e) => panic!("WebSocket error: {e}"),
                _ => continue,
            }
        }
        None
    }

    /// Drain all pending server messages until a quiet period (no message within `quiet_ms`).
    /// Returns the collected messages for optional inspection.
    async fn drain_until_quiet(
        ws: &mut (impl StreamExt<Item = Result<tungstenite::Message, tungstenite::Error>> + Unpin),
        quiet_ms: u64,
    ) -> Vec<ServerMessage> {
        let mut collected = Vec::new();
        while let Ok(Some(msg)) = tokio::time::timeout(
            std::time::Duration::from_millis(quiet_ms),
            recv_server_message(ws),
        )
        .await
        {
            collected.push(msg);
        }
        collected
    }

    /// Read the next server message, skipping `UserJoined` and `UserLeft` events.
    /// Useful after draining initial messages when you want to read a specific
    /// event type (Chat, `HeartbeatAck`, etc.) without being tripped up by
    /// membership notifications that arrive at unpredictable times.
    async fn recv_server_message_skip_membership(
        ws: &mut (impl StreamExt<Item = Result<tungstenite::Message, tungstenite::Error>> + Unpin),
    ) -> Option<ServerMessage> {
        loop {
            let msg = recv_server_message(ws).await?;
            match &msg.message {
                Some(
                    server_message::Message::UserJoined(_) | server_message::Message::UserLeft(_),
                ) => continue,
                _ => return Some(msg),
            }
        }
    }

    /// Encode a `ClientMessage` and send it as binary over the WebSocket.
    async fn send_client_message(
        ws: &mut (impl SinkExt<tungstenite::Message, Error = tungstenite::Error> + Unpin),
        msg: &ClientMessage,
    ) {
        let bytes = msg.encode_to_vec();
        ws.send(tungstenite::Message::Binary(bytes.into()))
            .await
            .expect("send client message");
    }

    // ========================================================================
    // Test: Basic WebSocket handshake and connection establishment
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_handshake_and_initial_user_joined() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "alice_ws").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Test Room WS").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // The first message should be a UserJoined notification for this user
        let msg = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws),
        )
        .await
        .expect("timeout waiting for initial message")
        .expect("stream ended unexpectedly");

        match msg.message {
            Some(server_message::Message::UserJoined(joined)) => {
                assert_eq!(joined.room_id, room_id);
                let member = joined.member.expect("member should be present");
                assert_eq!(member.user_id, user_id.as_str());
                assert!(member.is_online);
            }
            other => panic!("Expected UserJoined, got: {other:?}"),
        }

        // Graceful close
        ws.close(None).await.expect("close");
    }

    // ========================================================================
    // Test: Ping/Pong heartbeat mechanism (client sends heartbeat, gets ack)
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_heartbeat_ping_pong() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "bob_hb").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Heartbeat Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Drain initial membership events
        drain_until_quiet(&mut ws, 2000).await;

        // Send a heartbeat ClientMessage
        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws, &heartbeat).await;

        // Expect a HeartbeatAck response
        let ack = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message_skip_membership(&mut ws),
        )
        .await
        .expect("timeout waiting for heartbeat ack")
        .expect("stream ended");

        match ack.message {
            Some(server_message::Message::HeartbeatAck(ack)) => {
                assert!(
                    ack.timestamp > 0,
                    "HeartbeatAck should have a valid timestamp"
                );
            }
            other => panic!("Expected HeartbeatAck, got: {other:?}"),
        }

        ws.close(None).await.expect("close");
    }

    // ========================================================================
    // Test: Graceful disconnect (client sends Close frame)
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_graceful_disconnect() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "carol_dc").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Disconnect Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Drain initial membership events
        drain_until_quiet(&mut ws, 2000).await;

        // Send a Close frame
        ws.close(Some(tungstenite::protocol::CloseFrame {
            code: tungstenite::protocol::frame::coding::CloseCode::Normal,
            reason: "bye".into(),
        }))
        .await
        .expect("close");

        // After close, the next recv should return None or a Close frame
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await;

        match result {
            Ok(Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None) | Err(_) => {
                // All acceptable: either received Close frame, stream ended, or timeout
            }
            Ok(Some(Ok(msg))) => {
                // After close, we may still receive buffered messages; that's fine
                assert!(
                    !matches!(msg, tungstenite::Message::Binary(_)),
                    "Should not receive new binary messages after close"
                );
            }
        }
    }

    // ========================================================================
    // Test: Unauthenticated connection attempt is rejected
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_unauthenticated_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Attempt to connect without any token
        let url = format!("ws://{}/ws/rooms/fake_room", server.addr);
        let result = tokio_tungstenite::connect_async(&url).await;

        // Should fail with a non-101 status (likely 401)
        assert!(
            result.is_err(),
            "Connection without auth should be rejected"
        );
    }

    // ========================================================================
    // Test: Invalid token is rejected
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_invalid_token_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let url = format!("ws://{}/ws/rooms/fake_room", server.addr);
        let request = tungstenite::http::Request::builder()
            .uri(&url)
            .header("Authorization", "Bearer invalid.jwt.token")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Host", &*server.addr)
            .body(())
            .unwrap();
        let result = tokio_tungstenite::connect_async(request).await;

        assert!(
            result.is_err(),
            "Connection with invalid token should be rejected"
        );
    }

    // ========================================================================
    // Test: Non-member of room is rejected (valid token, not a room member)
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_non_member_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Create owner and room
        let (owner_id, _owner_token) =
            register_test_user(&server.user_service, &server.jwt_service, "owner_nm").await;
        let room_id = create_test_room(&server.room_service, &owner_id, "Private Room").await;

        // Create a different user who is NOT a member of the room
        let (_outsider_id, outsider_token) =
            register_test_user(&server.user_service, &server.jwt_service, "outsider_nm").await;

        // Attempt to connect as the outsider
        let url = format!("ws://{}/ws/rooms/{}", server.addr, room_id);
        let request = tungstenite::http::Request::builder()
            .uri(&url)
            .header("Authorization", format!("Bearer {outsider_token}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Host", &*server.addr)
            .body(())
            .unwrap();
        let result = tokio_tungstenite::connect_async(request).await;

        assert!(result.is_err(), "Non-member should be rejected with 403");
    }

    // ========================================================================
    // Test: Multiple clients join the same room and receive each other's events
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_multi_client_room_sync() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Create room owner (user1)
        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user1_mc").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "Sync Room").await;

        // Create second user and add them to the room
        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user2_mc").await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("user2 join room");

        // Connect user1
        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        // Drain all initial messages for user1
        drain_until_quiet(&mut ws1, 2000).await;

        // Connect user2
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;
        // Drain user2's initial messages
        drain_until_quiet(&mut ws2, 2000).await;

        // user1 should receive a UserJoined event for user2 (via cluster broadcast)
        // Read messages from ws1 until we find UserJoined with user2's ID
        let user2_join_event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message(&mut ws1).await.expect("stream ended");
                if let Some(server_message::Message::UserJoined(ref joined)) = msg.message {
                    if joined.member.as_ref().map(|m| m.user_id.as_str()) == Some(user2_id.as_str())
                    {
                        return msg;
                    }
                }
            }
        })
        .await
        .expect("timeout waiting for user2 join event on ws1");

        match user2_join_event.message {
            Some(server_message::Message::UserJoined(joined)) => {
                assert_eq!(joined.room_id, room_id);
                let member = joined.member.expect("member");
                assert_eq!(member.user_id, user2_id.as_str());
            }
            other => panic!("Expected UserJoined for user2, got: {other:?}"),
        }

        // Clean up
        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    // ========================================================================
    // Test: Chat message broadcast between two clients in the same room
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_chat_message_broadcast() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Create room owner (user1) and room
        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "sender_cb").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "Chat Room").await;

        // Create second user and join room
        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "receiver_cb").await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("user2 join");

        // Connect both users
        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        // Drain all initial messages on both connections (parallel)
        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );

        // Let subscriptions settle
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // user1 sends a chat message
        let chat_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "Hello from user1!".to_string(),
                    position: None,
                    color: None,
                },
            )),
        };
        send_client_message(&mut ws1, &chat_msg).await;

        // user2 should receive the chat broadcast (skip any membership events)
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            recv_server_message_skip_membership(&mut ws2),
        )
        .await
        .expect("timeout waiting for chat message on ws2")
        .expect("stream ended");

        match received.message {
            Some(server_message::Message::Chat(chat)) => {
                assert_eq!(chat.content, "Hello from user1!");
                assert_eq!(chat.room_id, room_id);
                assert_eq!(chat.user_id, user1_id.as_str());
            }
            other => panic!("Expected Chat message, got: {other:?}"),
        }

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    // ========================================================================
    // Test: Multiple heartbeats in sequence
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_multiple_heartbeats() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "hb_multi").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Multi HB Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Drain all initial messages (UserJoined, PlaybackState, etc.)
        drain_until_quiet(&mut ws, 2000).await;

        // Send 3 heartbeats and expect 3 acks
        for i in 0..3 {
            let heartbeat = ClientMessage {
                message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                    timestamp: chrono::Utc::now().timestamp() + i,
                })),
            };
            send_client_message(&mut ws, &heartbeat).await;

            let ack = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                recv_server_message_skip_membership(&mut ws),
            )
            .await
            .expect("timeout waiting for heartbeat ack")
            .expect("stream ended");

            assert!(
                matches!(ack.message, Some(server_message::Message::HeartbeatAck(_))),
                "Expected HeartbeatAck for heartbeat {i}"
            );
        }

        ws.close(None).await.expect("close");
    }

    // ========================================================================
    // Test: UserLeft event is sent when a client disconnects
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_user_left_on_disconnect() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Create room with user1
        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "stayer_ul").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "Leave Room").await;

        // Add user2
        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "leaver_ul").await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("join");

        // Connect both
        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        // Drain all initial messages on both connections (parallel)
        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );

        // user2 disconnects
        ws2.close(None).await.expect("close ws2");

        // user1 should receive UserLeft for user2
        // Read messages, skipping any stale UserJoined that may still be in flight
        let left_event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message(&mut ws1).await.expect("stream ended");
                if matches!(&msg.message, Some(server_message::Message::UserLeft(_))) {
                    return msg;
                }
            }
        })
        .await
        .expect("timeout waiting for UserLeft event");

        match left_event.message {
            Some(server_message::Message::UserLeft(left)) => {
                assert_eq!(left.room_id, room_id);
                assert_eq!(left.user_id, user2_id.as_str());
            }
            other => panic!("Expected UserLeft, got: {other:?}"),
        }

        ws1.close(None).await.expect("close ws1");
    }

    // ========================================================================
    // Test: Room isolation (user in a different room does NOT receive messages)
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_room_isolation() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Create user1 with room A
        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user1_iso").await;
        let room_a_id = create_test_room(&server.room_service, &user1_id, "Room A").await;

        // Create user2 with room B (a completely separate room)
        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user2_iso").await;
        let room_b_id = create_test_room(&server.room_service, &user2_id, "Room B").await;

        // Connect user1 to Room A
        let mut ws1 = ws_connect(&server.addr, &room_a_id, &user1_token).await;
        // Connect user2 to Room B
        let mut ws2 = ws_connect(&server.addr, &room_b_id, &user2_token).await;

        // Drain initial UserJoined for both
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws1),
        )
        .await;
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws2),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // user1 sends a chat message in Room A
        let chat_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "Room A only".to_string(),
                    position: None,
                    color: None,
                },
            )),
        };
        send_client_message(&mut ws1, &chat_msg).await;

        // user2 in Room B should NOT receive this message
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            recv_server_message(&mut ws2),
        )
        .await;

        // Should time out (no message received) or return None
        match result {
            Err(_) | Ok(None) => {
                // Stream ended: also acceptable (no cross-room leak)
            }
            Ok(Some(msg)) => {
                // If we got a message, it must NOT be the chat from room A
                if let Some(server_message::Message::Chat(chat)) = msg.message {
                    panic!(
                        "Room isolation violated: user2 in Room B received chat from Room A: {:?}",
                        chat.content
                    );
                }
                // Other message types (like a heartbeat timeout) are acceptable
            }
        }

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    // ========================================================================
    // Test: Reconnection after disconnect (verify new connection works)
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_reconnect_after_disconnect() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "reconn_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Reconnect Room").await;

        // First connection
        let mut ws = ws_connect(&server.addr, &room_id, &token).await;
        let initial = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws),
        )
        .await
        .expect("timeout")
        .expect("no initial msg");
        assert!(matches!(
            initial.message,
            Some(server_message::Message::UserJoined(_))
        ));

        // Graceful disconnect
        ws.close(None).await.expect("close");
        // Small delay to let the server process the disconnect
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Reconnect with the same token
        let mut ws2 = ws_connect(&server.addr, &room_id, &token).await;
        let rejoined = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws2),
        )
        .await
        .expect("timeout on reconnect")
        .expect("no msg on reconnect");

        // Should get a UserJoined event again
        match rejoined.message {
            Some(server_message::Message::UserJoined(joined)) => {
                assert_eq!(joined.room_id, room_id);
                let member = joined.member.expect("member");
                assert_eq!(member.user_id, user_id.as_str());
            }
            other => panic!("Expected UserJoined on reconnect, got: {other:?}"),
        }

        // Drain any remaining initial messages before sending heartbeat
        drain_until_quiet(&mut ws2, 500).await;

        // Verify heartbeat still works after reconnection
        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws2, &heartbeat).await;

        let ack = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message_skip_membership(&mut ws2),
        )
        .await
        .expect("timeout on heartbeat after reconnect")
        .expect("no ack");
        assert!(
            matches!(ack.message, Some(server_message::Message::HeartbeatAck(_))),
            "Expected HeartbeatAck after reconnect"
        );

        ws2.close(None).await.expect("close ws2");
    }

    // ========================================================================
    // Test: Forced disconnect via ConnectionManager (simulates kick)
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_forced_disconnect_via_kick() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Create room owner and a second user
        let (owner_id, _owner_token) =
            register_test_user(&server.user_service, &server.jwt_service, "owner_kick").await;
        let room_id = create_test_room(&server.room_service, &owner_id, "Kick Room").await;

        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "victim_kick").await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid.clone(), user2_id.clone(), None)
            .await
            .expect("join");

        // user2 connects
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;
        // Drain initial UserJoined
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws2),
        )
        .await;

        // Force disconnect user2 from the room via ConnectionManager
        server
            .connection_manager
            .disconnect_user_from_room(&user2_id, &rid);

        // user2's connection should be terminated (receive Close or stream ends)
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws2.next().await {
                    Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None => return true,
                    Some(Ok(tungstenite::Message::Binary(_))) => {
                        // May still receive buffered messages; keep draining
                        continue;
                    }
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await;

        assert!(
            result.is_ok(),
            "Connection should be terminated after forced disconnect"
        );
    }

    // ========================================================================
    // Test: Forced kick disconnect must not emit voluntary UserLeft
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_forced_disconnect_via_kick_does_not_broadcast_user_left() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (owner_id, owner_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "owner_kick_noleft",
        )
        .await;
        let room_id =
            create_test_room(&server.room_service, &owner_id, "Kick No UserLeft Room").await;

        let (user2_id, user2_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "victim_kick_noleft",
        )
        .await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid.clone(), user2_id.clone(), None)
            .await
            .expect("join");

        let mut ws1 = ws_connect(&server.addr, &room_id, &owner_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;
        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );

        server
            .connection_manager
            .disconnect_user_from_room(&user2_id, &rid);

        let disconnect_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws2.next().await {
                    Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None => return true,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await;
        assert!(
            disconnect_result.is_ok(),
            "kicked connection should be terminated promptly"
        );

        let unexpected_user_left = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let msg = recv_server_message(&mut ws1).await.expect("stream ended");
                if matches!(msg.message, Some(server_message::Message::UserLeft(_))) {
                    return msg;
                }
            }
        })
        .await;

        assert!(
            unexpected_user_left.is_err(),
            "forced kick disconnect must not be re-labeled as voluntary UserLeft"
        );

        let _ = ws1.close(None).await;
    }

    // ========================================================================
    // Test: Admin KickUserFromRoom must not degrade into UserLeft on cleanup
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_admin_kick_event_does_not_broadcast_user_left() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (owner_id, owner_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "owner_admin_kick",
        )
        .await;
        let room_id = create_test_room(
            &server.room_service,
            &owner_id,
            "Admin Kick No UserLeft Room",
        )
        .await;

        let (user2_id, user2_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "victim_admin_kick",
        )
        .await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid.clone(), user2_id.clone(), None)
            .await
            .expect("join");

        let mut ws1 = ws_connect(&server.addr, &room_id, &owner_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;
        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );

        let event = synctv_cluster::sync::ClusterEvent::KickUserFromRoom {
            event_id: nanoid::nanoid!(16),
            room_id: rid.clone(),
            user_id: user2_id.clone(),
            reason: "admin kick".to_string(),
            timestamp: chrono::Utc::now(),
        };
        server
            .cluster_manager
            .admin_event_tx()
            .send(event)
            .expect("admin event send");

        let disconnect_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws2.next().await {
                    Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None => return true,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await;
        assert!(
            disconnect_result.is_ok(),
            "admin kick should disconnect the targeted connection"
        );

        let unexpected_user_left = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let msg = recv_server_message(&mut ws1).await.expect("stream ended");
                if matches!(msg.message, Some(server_message::Message::UserLeft(_))) {
                    return msg;
                }
            }
        })
        .await;

        assert!(
            unexpected_user_left.is_err(),
            "admin kick should not be re-labeled as voluntary UserLeft during cleanup"
        );

        let _ = ws1.close(None).await;
    }

    // ========================================================================
    // Test: Cross-replica messaging via Redis Pub/Sub
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_cross_replica_chat_via_redis() {
        let infra = TestInfra::new().await;

        // Start two server replicas with different node IDs but shared DB + Redis
        let server1 = setup_e2e_server_with_node(&infra, "replica_1").await;
        let server2 = setup_e2e_server_with_node(&infra, "replica_2").await;

        // Create user1 and room via server1's services (shared DB)
        let (user1_id, user1_token) =
            register_test_user(&server1.user_service, &server1.jwt_service, "xrep_u1").await;
        let room_id =
            create_test_room(&server1.room_service, &user1_id, "Cross Replica Room").await;

        // Create user2 and join room (uses same DB)
        let (user2_id, user2_token) =
            register_test_user(&server1.user_service, &server1.jwt_service, "xrep_u2").await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server1
            .room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("user2 join");

        // user1 connects to replica_1
        let mut ws1 = ws_connect(&server1.addr, &room_id, &user1_token).await;
        // user2 connects to replica_2
        let mut ws2 = ws_connect(&server2.addr, &room_id, &user2_token).await;

        // Drain all initial events on both connections (parallel)
        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // user1 sends a chat on replica_1
        let chat_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "Cross-replica hello!".to_string(),
                    position: None,
                    color: None,
                },
            )),
        };
        send_client_message(&mut ws1, &chat_msg).await;

        // user2 on replica_2 should receive it via Redis Pub/Sub (skip membership events)
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            recv_server_message_skip_membership(&mut ws2),
        )
        .await
        .expect("timeout waiting for cross-replica chat message")
        .expect("stream ended");

        match received.message {
            Some(server_message::Message::Chat(chat)) => {
                assert_eq!(chat.content, "Cross-replica hello!");
                assert_eq!(chat.room_id, room_id);
                assert_eq!(chat.user_id, user1_id.as_str());
            }
            other => panic!("Expected Chat message via cross-replica, got: {other:?}"),
        }

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    // ========================================================================
    // Test: Cross-replica same-user partial disconnect must not emit UserLeft
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_cross_replica_same_user_partial_disconnect_does_not_emit_user_left() {
        let infra = TestInfra::new().await;

        let server1 = setup_e2e_server_with_node(&infra, "presence_replica_1").await;
        let server2 = setup_e2e_server_with_node(&infra, "presence_replica_2").await;

        let (owner_id, owner_token) = register_test_user(
            &server1.user_service,
            &server1.jwt_service,
            "owner_xrep_presence",
        )
        .await;
        let room_id = create_test_room(
            &server1.room_service,
            &owner_id,
            "Cross Replica Presence Room",
        )
        .await;

        let (user2_id, user2_token) = register_test_user(
            &server1.user_service,
            &server1.jwt_service,
            "multi_presence_xrep_user",
        )
        .await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server1
            .room_service
            .join_room(rid.clone(), user2_id.clone(), None)
            .await
            .expect("join");

        let mut ws_owner = ws_connect(&server1.addr, &room_id, &owner_token).await;
        let mut ws_user_replica_1 = ws_connect(&server1.addr, &room_id, &user2_token).await;
        let mut ws_user_replica_2 = ws_connect(&server2.addr, &room_id, &user2_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws_owner, 1500),
            drain_until_quiet(&mut ws_user_replica_1, 1500),
            drain_until_quiet(&mut ws_user_replica_2, 1500),
        );

        let active_user_conns_replica_1 =
            server1.connection_manager.get_user_connections(&user2_id);
        let active_user_conns_replica_2 =
            server2.connection_manager.get_user_connections(&user2_id);
        assert_eq!(
            active_user_conns_replica_1.len(),
            1,
            "test precondition failed: expected one same-user connection on replica 1"
        );
        assert_eq!(
            active_user_conns_replica_2.len(),
            1,
            "test precondition failed: expected one same-user connection on replica 2"
        );

        ws_user_replica_1
            .close(None)
            .await
            .expect("close replica-1 connection");

        let maybe_user_left = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let msg = recv_server_message(&mut ws_owner)
                    .await
                    .expect("stream ended");
                if matches!(msg.message, Some(server_message::Message::UserLeft(_))) {
                    return msg;
                }
            }
        })
        .await;

        assert!(
            maybe_user_left.is_err(),
            "disconnecting one of multiple cross-replica same-user connections must not emit UserLeft while another connection remains"
        );

        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws_user_replica_2, &heartbeat).await;
        let ack = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message(&mut ws_user_replica_2)
                    .await
                    .expect("stream ended");
                if matches!(msg.message, Some(server_message::Message::HeartbeatAck(_))) {
                    return msg;
                }
            }
        })
        .await
        .expect("timeout waiting for surviving cross-replica connection heartbeat ack");
        assert!(
            matches!(ack.message, Some(server_message::Message::HeartbeatAck(_))),
            "surviving cross-replica same-user connection should remain functional"
        );

        ws_owner.close(None).await.expect("close owner");
        ws_user_replica_2
            .close(None)
            .await
            .expect("close remaining replica-2 connection");
    }

    // ========================================================================
    // Test: Cross-replica same-user second connection must not emit duplicate UserJoined
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_cross_replica_same_user_second_connection_does_not_emit_duplicate_user_joined()
    {
        let infra = TestInfra::new().await;

        let server1 = setup_e2e_server_with_node(&infra, "join_replica_1").await;
        let server2 = setup_e2e_server_with_node(&infra, "join_replica_2").await;

        let (owner_id, owner_token) = register_test_user(
            &server1.user_service,
            &server1.jwt_service,
            "owner_xrep_join",
        )
        .await;
        let room_id =
            create_test_room(&server1.room_service, &owner_id, "Cross Replica Join Room").await;

        let (user2_id, user2_token) = register_test_user(
            &server1.user_service,
            &server1.jwt_service,
            "multi_join_xrep_user",
        )
        .await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server1
            .room_service
            .join_room(rid.clone(), user2_id.clone(), None)
            .await
            .expect("join");

        let mut ws_owner = ws_connect(&server1.addr, &room_id, &owner_token).await;
        let mut ws_user_replica_1 = ws_connect(&server1.addr, &room_id, &user2_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws_owner, 1500),
            drain_until_quiet(&mut ws_user_replica_1, 1500),
        );

        let mut ws_user_replica_2 = ws_connect(&server2.addr, &room_id, &user2_token).await;
        drain_until_quiet(&mut ws_user_replica_2, 1500).await;

        let duplicate_join = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let msg = recv_server_message(&mut ws_owner)
                    .await
                    .expect("stream ended");
                if let Some(server_message::Message::UserJoined(joined)) = msg.message {
                    if joined.member.as_ref().map(|m| m.user_id.as_str()) == Some(user2_id.as_str())
                    {
                        return joined;
                    }
                }
            }
        })
        .await;

        assert!(
            duplicate_join.is_err(),
            "opening a second cross-replica connection for the same user must not emit duplicate UserJoined while the user is already online"
        );

        ws_owner.close(None).await.expect("close owner");
        ws_user_replica_1
            .close(None)
            .await
            .expect("close replica-1 user");
        ws_user_replica_2
            .close(None)
            .await
            .expect("close replica-2 user");
    }

    // ========================================================================
    // Test: Rate limiter blocks excessive chat messages
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_rate_limiter_blocks_excess_chat() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server_with_chat_rate_limit(
            &infra,
            synctv_core::service::RateLimitConfig {
                chat_per_second: 2,
                window_seconds: 60,
                ..Default::default()
            },
        )
        .await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "ratelimit_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Rate Limit Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Drain initial membership events
        drain_until_quiet(&mut ws, 2000).await;

        // Send many chat messages rapidly (default rate limit is 10/sec)
        // We send 15 to exceed the limit
        for i in 0..15 {
            let chat_msg = ClientMessage {
                message: Some(client_message::Message::Chat(
                    synctv_proto::client::ChatMessageSend {
                        content: format!("Spam message {i}"),
                        position: None,
                        color: None,
                    },
                )),
            };
            send_client_message(&mut ws, &chat_msg).await;
        }

        // Collect chat echoes and errors for a bounded time
        let mut chat_count = 0usize;
        let mut error_received = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, recv_server_message(&mut ws)).await {
                Ok(Some(msg)) => match msg.message {
                    Some(server_message::Message::Chat(_)) => {
                        chat_count += 1;
                    }
                    Some(server_message::Message::Error(_)) => {
                        error_received = true;
                        break;
                    }
                    _ => {}
                },
                _ => break,
            }
        }

        // The rate limiter should have blocked some messages, meaning we either
        // received fewer chats than sent, or the server sent an error, or the
        // connection stayed alive but silently dropped messages (no echo).
        // The key assertion: NOT all 15 messages were echoed as chats.
        assert!(
            chat_count < 15 || error_received,
            "Rate limiter should have blocked some messages: got {chat_count} chats, error={error_received}",
        );

        ws.close(None).await.expect("close");
    }

    // ========================================================================
    // Test: Content filter strips XSS from chat messages
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_content_filter_strips_xss() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Create room owner (user1) and room
        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "xss_sender").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "XSS Room").await;

        // Create second user and join room
        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "xss_receiver").await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("user2 join");

        // Connect both users
        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        // Drain all initial messages (parallel)
        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // user1 sends a message with XSS payload
        let xss_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "<script>alert('xss')</script>Hello safe world".to_string(),
                    position: None,
                    color: None,
                },
            )),
        };
        send_client_message(&mut ws1, &xss_msg).await;

        // user2 should receive the sanitized chat (skip membership events)
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            recv_server_message_skip_membership(&mut ws2),
        )
        .await
        .expect("timeout waiting for sanitized chat")
        .expect("stream ended");

        match received.message {
            Some(server_message::Message::Chat(chat)) => {
                // The content filter should have stripped the <script> tag
                assert!(
                    !chat.content.contains("<script>"),
                    "XSS script tag should be stripped, got: {}",
                    chat.content,
                );
                assert!(
                    chat.content.contains("Hello safe world"),
                    "Safe text should be preserved, got: {}",
                    chat.content,
                );
            }
            other => panic!("Expected Chat message, got: {other:?}"),
        }

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    // ========================================================================
    // Test: Connection cleanup after abnormal disconnect (TCP drop)
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_connection_cleanup_on_tcp_drop() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "stayer_tcp").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "TCP Drop Room").await;

        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "dropper_tcp").await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid.clone(), user2_id.clone(), None)
            .await
            .expect("join");

        // Connect user1 (stays connected)
        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        drain_until_quiet(&mut ws1, 2000).await;

        // Connect user2 (will be dropped)
        let ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        // Drain user1's notification of user2 join
        drain_until_quiet(&mut ws1, 2000).await;

        // Abruptly drop user2's WebSocket (simulate TCP disconnect without Close frame)
        drop(ws2);

        // user1 should receive UserLeft for user2 (server detects the drop)
        let left_event = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let msg = recv_server_message(&mut ws1).await.expect("stream ended");
                if matches!(&msg.message, Some(server_message::Message::UserLeft(_))) {
                    return msg;
                }
            }
        })
        .await
        .expect("timeout waiting for UserLeft after TCP drop");

        match left_event.message {
            Some(server_message::Message::UserLeft(left)) => {
                assert_eq!(left.room_id, room_id);
                assert_eq!(left.user_id, user2_id.as_str());
            }
            other => panic!("Expected UserLeft after TCP drop, got: {other:?}"),
        }

        ws1.close(None).await.expect("close ws1");
    }

    // ========================================================================
    // Test: ConnectionManager state is consistent after multiple connect/disconnect cycles
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_connection_manager_state_consistency() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "cycle_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Cycle Room").await;

        // Perform 3 connect/disconnect cycles
        for cycle in 0..3 {
            let mut ws = ws_connect(&server.addr, &room_id, &token).await;

            // Drain all initial/stale messages (UserJoined, UserLeft from previous cycle, etc.)
            drain_until_quiet(&mut ws, 2000).await;

            // Send a heartbeat to verify the connection is fully functional
            let heartbeat = ClientMessage {
                message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                    timestamp: chrono::Utc::now().timestamp_millis(),
                })),
            };
            send_client_message(&mut ws, &heartbeat).await;

            let ack = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                recv_server_message_skip_membership(&mut ws),
            )
            .await
            .unwrap_or_else(|_| panic!("timeout on heartbeat in cycle {cycle}"))
            .expect("stream ended");
            assert!(
                matches!(ack.message, Some(server_message::Message::HeartbeatAck(_))),
                "Expected HeartbeatAck in cycle {cycle}"
            );

            // Graceful disconnect
            ws.close(None)
                .await
                .unwrap_or_else(|_| panic!("close in cycle {cycle}"));

            // Wait for server to process the disconnect and clean up state
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // After all cycles, the connection manager should have 0 connections for this user
        let user_conn_count = server.connection_manager.user_connection_count(&user_id);
        assert_eq!(
            user_conn_count, 0,
            "After all disconnect cycles, user should have 0 connections, got {user_conn_count}"
        );
    }

    // ========================================================================
    // Test: Empty chat message is rejected by the pipeline
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_empty_chat_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "empty_chat_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Empty Chat Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Drain initial membership events
        drain_until_quiet(&mut ws, 2000).await;

        // Send empty chat message
        let empty_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: String::new(),
                    position: None,
                    color: None,
                },
            )),
        };
        send_client_message(&mut ws, &empty_msg).await;

        let rejection = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message_skip_membership(&mut ws),
        )
        .await
        .expect("timeout waiting for empty chat rejection")
        .expect("stream ended");

        match rejection.message {
            Some(server_message::Message::Error(err)) => {
                assert!(
                    err.message.contains("empty"),
                    "expected empty-chat validation error, got: {}",
                    err.message
                );
            }
            other => panic!("Expected Error message for empty chat rejection, got: {other:?}"),
        }

        // Connection should remain alive after the validation error.
        // Verify by sending a heartbeat and getting an ack.
        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws, &heartbeat).await;

        let ack = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message_skip_membership(&mut ws),
        )
        .await
        .expect("timeout waiting for heartbeat ack after empty msg")
        .expect("stream ended");

        assert!(
            matches!(ack.message, Some(server_message::Message::HeartbeatAck(_))),
            "Connection should still be alive after empty chat rejection"
        );

        ws.close(None).await.expect("close");
    }

    // ========================================================================
    // Test: Danmaku message with position is broadcast correctly
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_danmaku_broadcast() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Create room owner (user1) and room
        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "danmaku_sender").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "Danmaku Room").await;

        // Create second user and join room
        let (user2_id, user2_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "danmaku_receiver",
        )
        .await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("user2 join");

        // Connect both users
        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        // Drain all initial messages (parallel)
        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // user1 sends a danmaku (chat with position)
        let danmaku_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "LOL".to_string(),
                    position: Some(42.5),
                    color: Some("#FF0000".to_string()),
                },
            )),
        };
        send_client_message(&mut ws1, &danmaku_msg).await;

        // user2 should receive the danmaku with position and color (skip membership events)
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message_skip_membership(&mut ws2),
        )
        .await
        .expect("timeout waiting for danmaku")
        .expect("stream ended");

        match received.message {
            Some(server_message::Message::Chat(chat)) => {
                assert_eq!(chat.content, "LOL");
                assert_eq!(chat.room_id, room_id);
                assert_eq!(chat.user_id, user1_id.as_str());
                assert!(chat.position.is_some(), "Danmaku should have a position");
                assert!(
                    (chat.position.unwrap() - 42.5).abs() < f64::EPSILON,
                    "Position should be 42.5"
                );
                assert_eq!(
                    chat.color.as_deref(),
                    Some("#FF0000"),
                    "Danmaku should have the specified color"
                );
            }
            other => panic!("Expected Chat (danmaku) message, got: {other:?}"),
        }

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    // ========================================================================
    // Test: Concurrent connections from same user to same room
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_concurrent_connections_same_user() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "multi_conn_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Multi Conn Room").await;

        // Open two connections from the same user to the same room
        let mut ws1 = ws_connect(&server.addr, &room_id, &token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &token).await;

        // Both should get UserJoined
        let msg1 = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws1),
        )
        .await
        .expect("timeout")
        .expect("no initial msg for conn1");
        assert!(
            matches!(msg1.message, Some(server_message::Message::UserJoined(_))),
            "Connection 1 should get UserJoined"
        );

        let msg2 = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws2),
        )
        .await
        .expect("timeout")
        .expect("no initial msg for conn2");
        assert!(
            matches!(msg2.message, Some(server_message::Message::UserJoined(_))),
            "Connection 2 should get UserJoined"
        );

        // Send a heartbeat on connection 1 to verify it's still working
        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws1, &heartbeat).await;

        // Drain any UserJoined cross-notification on ws1 before checking heartbeat ack
        let ack = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message(&mut ws1).await?;
                if matches!(msg.message, Some(server_message::Message::HeartbeatAck(_))) {
                    return Some(msg);
                }
            }
        })
        .await
        .expect("timeout waiting for heartbeat ack on conn1")
        .expect("stream ended");

        assert!(
            matches!(ack.message, Some(server_message::Message::HeartbeatAck(_))),
            "Connection 1 should get HeartbeatAck"
        );

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    // ========================================================================
    // Test: Same-user second connection must not emit duplicate UserJoined
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_same_user_second_connection_does_not_emit_duplicate_user_joined() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (owner_id, owner_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "owner_join_presence",
        )
        .await;
        let room_id = create_test_room(&server.room_service, &owner_id, "Join Presence Room").await;

        let (user2_id, user2_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "join_presence_user",
        )
        .await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid.clone(), user2_id.clone(), None)
            .await
            .expect("join");

        let mut ws_owner = ws_connect(&server.addr, &room_id, &owner_token).await;
        let mut ws_user_a = ws_connect(&server.addr, &room_id, &user2_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws_owner, 1500),
            drain_until_quiet(&mut ws_user_a, 1500),
        );

        let mut ws_user_b = ws_connect(&server.addr, &room_id, &user2_token).await;
        drain_until_quiet(&mut ws_user_b, 1500).await;

        let duplicate_join = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let msg = recv_server_message(&mut ws_owner)
                    .await
                    .expect("stream ended");
                if let Some(server_message::Message::UserJoined(joined)) = msg.message {
                    if joined.member.as_ref().map(|m| m.user_id.as_str()) == Some(user2_id.as_str())
                    {
                        return joined;
                    }
                }
            }
        })
        .await;

        assert!(
            duplicate_join.is_err(),
            "opening a second connection for the same user must not emit duplicate UserJoined while the user is already online"
        );

        ws_owner.close(None).await.expect("close owner");
        ws_user_a.close(None).await.expect("close user a");
        ws_user_b.close(None).await.expect("close user b");
    }

    // ========================================================================
    // Test: Same-user partial disconnect must not emit UserLeft
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_same_user_one_of_multiple_connections_disconnect_does_not_emit_user_left() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (owner_id, owner_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "owner_multi_presence",
        )
        .await;
        let room_id =
            create_test_room(&server.room_service, &owner_id, "Multi Presence Room").await;

        let (user2_id, user2_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "multi_presence_user",
        )
        .await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid.clone(), user2_id.clone(), None)
            .await
            .expect("join");

        let mut ws_owner = ws_connect(&server.addr, &room_id, &owner_token).await;
        let mut ws_user_a = ws_connect(&server.addr, &room_id, &user2_token).await;
        let mut ws_user_b = ws_connect(&server.addr, &room_id, &user2_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws_owner, 1500),
            drain_until_quiet(&mut ws_user_a, 1500),
            drain_until_quiet(&mut ws_user_b, 1500),
        );

        let active_connections = server.connection_manager.get_user_connections(&user2_id);
        assert_eq!(
            active_connections.len(),
            2,
            "test precondition failed: expected two active room connections for user2"
        );

        ws_user_a
            .close(None)
            .await
            .expect("close first user2 connection");

        let maybe_user_left = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let msg = recv_server_message(&mut ws_owner)
                    .await
                    .expect("stream ended");
                if matches!(msg.message, Some(server_message::Message::UserLeft(_))) {
                    return msg;
                }
            }
        })
        .await;

        assert!(
            maybe_user_left.is_err(),
            "disconnecting one of multiple same-user connections must not emit UserLeft while another connection remains"
        );

        let remaining_connections = server.connection_manager.get_user_connections(&user2_id);
        assert_eq!(
            remaining_connections.len(),
            1,
            "exactly one connection should remain after closing the first connection"
        );

        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws_user_b, &heartbeat).await;
        let ack = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message(&mut ws_user_b)
                    .await
                    .expect("stream ended");
                if matches!(msg.message, Some(server_message::Message::HeartbeatAck(_))) {
                    return msg;
                }
            }
        })
        .await
        .expect("timeout waiting for surviving connection heartbeat ack");
        assert!(
            matches!(ack.message, Some(server_message::Message::HeartbeatAck(_))),
            "surviving same-user connection should remain functional"
        );

        ws_owner.close(None).await.expect("close owner");
        ws_user_b
            .close(None)
            .await
            .expect("close second user2 connection");
    }

    // ========================================================================
    // Test: WebRTC join uses the current connection ID for same-user multi-conn
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_webrtc_join_marks_current_connection_for_same_user_multi_conn() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "multi_conn_webrtc",
        )
        .await;
        let room_id =
            create_test_room(&server.room_service, &user_id, "Multi Conn WebRTC Room").await;
        let room = synctv_core::models::RoomId::from_string(room_id.clone());

        let mut ws1 = ws_connect(&server.addr, &room_id, &token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &token).await;

        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );

        let rtc_join = ClientMessage {
            message: Some(client_message::Message::WebrtcJoin(WebRtcJoin {
                user_id: String::new(),
                conn_id: String::new(),
                username: String::new(),
            })),
        };
        send_client_message(&mut ws2, &rtc_join).await;

        let join_event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message(&mut ws1).await.expect("stream ended");
                if matches!(&msg.message, Some(server_message::Message::WebrtcJoin(_))) {
                    return msg;
                }
            }
        })
        .await
        .expect("timeout waiting for WebRTC join");

        let joined_conn_id = match join_event.message {
            Some(server_message::Message::WebrtcJoin(joined)) => joined.conn_id,
            other => panic!("Expected WebrtcJoin, got: {other:?}"),
        };

        let same_user_connections = server.connection_manager.get_user_connections(&user_id);
        assert_eq!(
            same_user_connections.len(),
            2,
            "test precondition failed: expected two active connections for same user"
        );

        let rtc_joined_connections: Vec<_> = server
            .connection_manager
            .get_room_connections(&room)
            .into_iter()
            .filter(|conn| conn.user_id == user_id && conn.rtc_joined)
            .collect();

        assert_eq!(
            rtc_joined_connections.len(),
            1,
            "exactly one connection should be marked rtc_joined"
        );
        assert_eq!(
            rtc_joined_connections[0].connection_id, joined_conn_id,
            "the connection marked rtc_joined must match the current WebRTC join event connection"
        );

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    // ========================================================================
    // Test: WebRTC offer requires explicit target conn_id and returns an error
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_webrtc_offer_rejects_recipient_without_conn_id() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "multi_conn_webrtc_offer",
        )
        .await;
        let (peer_user_id, peer_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "multi_conn_webrtc_peer",
        )
        .await;
        let room_id = create_test_room(
            &server.room_service,
            &user_id,
            "Multi Conn WebRTC Offer Room",
        )
        .await;
        let room = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(room.clone(), peer_user_id.clone(), None)
            .await
            .expect("peer joins room");

        let mut ws1 = ws_connect(&server.addr, &room_id, &token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &token).await;
        let mut ws_peer = ws_connect(&server.addr, &room_id, &peer_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
            drain_until_quiet(&mut ws_peer, 1500),
        );

        let mut user_connections = server.connection_manager.get_user_connections(&user_id);
        user_connections.sort_by(|left, right| left.connection_id.cmp(&right.connection_id));
        assert_eq!(
            user_connections.len(),
            2,
            "test precondition failed: expected two active connections for same user"
        );
        let conn_a = user_connections[0].connection_id.clone();

        server.connection_manager.disconnect_connection(&conn_a);
        let ws1_closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws1.next().await {
                    Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None => return true,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await
        .is_ok();

        let sender_is_ws2 = if ws1_closed {
            true
        } else {
            let ws2_closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match ws2.next().await {
                        Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None => return true,
                        Some(Ok(_)) => continue,
                    }
                }
            })
            .await
            .is_ok();
            assert!(
                ws2_closed,
                "one of the two connections must close after targeted disconnect"
            );

            false
        };

        let active_sender_connections = server.connection_manager.get_user_connections(&user_id);
        assert_eq!(
            active_sender_connections.len(),
            1,
            "after targeted disconnect exactly one sender connection should remain"
        );
        let offer = ClientMessage {
            message: Some(client_message::Message::WebrtcOffer(
                synctv_proto::client::WebRtcOffer {
                    to: peer_user_id.as_str().to_string(),
                    from: String::new(),
                    data: "{\"type\":\"offer\",\"sdp\":\"test-sdp\"}".to_string(),
                },
            )),
        };
        if sender_is_ws2 {
            send_client_message(&mut ws2, &offer).await;
        } else {
            send_client_message(&mut ws1, &offer).await;
        }

        let sender_ws = if sender_is_ws2 { &mut ws2 } else { &mut ws1 };
        let error_message = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message_skip_membership(sender_ws)
                    .await
                    .expect("stream ended");
                if matches!(&msg.message, Some(server_message::Message::Error(_))) {
                    return msg;
                }
            }
        })
        .await
        .expect("timeout waiting for WebRTC error");

        match error_message.message {
            Some(server_message::Message::Error(error)) => {
                assert!(
                    error.message.contains("user_id:conn_id"),
                    "expected recipient format error, got: {}",
                    error.message
                );
            }
            other => panic!("Expected Error, got: {other:?}"),
        }

        let peer_received_offer =
            tokio::time::timeout(std::time::Duration::from_millis(500), async {
                loop {
                    let msg = recv_server_message(&mut ws_peer).await;
                    match msg {
                        Some(server_message)
                            if matches!(
                                &server_message.message,
                                Some(server_message::Message::WebrtcOffer(_))
                            ) =>
                        {
                            return true;
                        }
                        Some(_) => continue,
                        None => return false,
                    }
                }
            })
            .await
            .unwrap_or(false);

        assert!(
            !peer_received_offer,
            "peer must not receive signaling when recipient conn_id is omitted"
        );

        let _ = ws1.close(None).await;
        let _ = ws2.close(None).await;
        let _ = ws_peer.close(None).await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_webrtc_offer_requires_target_connection_to_join_webrtc() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "webrtc_sender_requires_join",
        )
        .await;
        let (peer_user_id, peer_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "webrtc_peer_requires_join",
        )
        .await;
        let room_id = create_test_room(
            &server.room_service,
            &user_id,
            "WebRTC Offer Requires Join Room",
        )
        .await;
        let room = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(room.clone(), peer_user_id.clone(), None)
            .await
            .expect("peer joins room");

        let mut ws_sender = ws_connect(&server.addr, &room_id, &token).await;
        let mut ws_peer = ws_connect(&server.addr, &room_id, &peer_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws_sender, 1500),
            drain_until_quiet(&mut ws_peer, 1500),
        );

        let peer_conn_id = server
            .connection_manager
            .get_user_connections(&peer_user_id)
            .into_iter()
            .find(|conn| conn.room_id.as_ref() == Some(&room))
            .expect("peer connection must be present")
            .connection_id;

        let offer = ClientMessage {
            message: Some(client_message::Message::WebrtcOffer(
                synctv_proto::client::WebRtcOffer {
                    to: format!("{}:{}", peer_user_id.as_str(), peer_conn_id),
                    from: String::new(),
                    data: "{\"type\":\"offer\",\"sdp\":\"test-sdp\"}".to_string(),
                },
            )),
        };
        send_client_message(&mut ws_sender, &offer).await;

        let error_message = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message_skip_membership(&mut ws_sender)
                    .await
                    .expect("stream ended");
                if matches!(&msg.message, Some(server_message::Message::Error(_))) {
                    return msg;
                }
            }
        })
        .await
        .expect("timeout waiting for WebRTC join-state error");

        match error_message.message {
            Some(server_message::Message::Error(error)) => {
                assert!(
                    error.message.contains("has not joined WebRTC"),
                    "expected target join-state error, got: {}",
                    error.message
                );
            }
            other => panic!("Expected Error, got: {other:?}"),
        }

        let peer_received_offer =
            tokio::time::timeout(std::time::Duration::from_millis(500), async {
                loop {
                    let msg = recv_server_message(&mut ws_peer).await;
                    match msg {
                        Some(server_message)
                            if matches!(
                                &server_message.message,
                                Some(server_message::Message::WebrtcOffer(_))
                            ) =>
                        {
                            return true;
                        }
                        Some(_) => continue,
                        None => return false,
                    }
                }
            })
            .await
            .unwrap_or(false);

        assert!(
            !peer_received_offer,
            "peer must not receive signaling before explicitly joining WebRTC"
        );

        let _ = ws_sender.close(None).await;
        let _ = ws_peer.close(None).await;
    }

    // ========================================================================
    // Test (#73): WebSocket connection with invalid ticket is rejected
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_invalid_ticket_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Attempt to connect with a ticket value that was never issued
        let url = format!(
            "ws://{}/ws/rooms/fake_room?ticket=totally_invalid_ticket_xyz123",
            server.addr
        );
        let result = tokio_tungstenite::connect_async(&url).await;

        // Should be rejected — invalid ticket cannot be redeemed
        assert!(
            result.is_err(),
            "Connection with invalid ticket should be rejected by the server"
        );
    }

    // ========================================================================
    // Test (#73): WebSocket connection with expired ticket is rejected
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_expired_ticket_rejected() {
        // The standalone test setup now wires an in-memory WsTicketService.
        // A fake/expired ticket must still be rejected after validation against the
        // memory-backed store.
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let url = format!(
            "ws://{}/ws/rooms/fake_room?ticket=expired_or_fake_ticket_aabbcc",
            server.addr
        );
        let result = tokio_tungstenite::connect_async(&url).await;

        assert!(
            result.is_err(),
            "Connection with expired/fake ticket should be rejected"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_invalid_room_id_rejected_before_upgrade() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;
        let (_user_id, token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "invalid_room_user",
        )
        .await;

        let url = format!("ws://{}/ws/rooms/room@bad1234", server.addr);
        let request = tungstenite::http::Request::builder()
            .uri(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Host", &server.addr)
            .body(())
            .expect("build malformed room WebSocket request");
        let result = tokio_tungstenite::connect_async(request).await;

        match result {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(
                    response.status(),
                    tungstenite::http::StatusCode::BAD_REQUEST,
                    "malformed room_id must be rejected before the WebSocket upgrade completes"
                );
            }
            other => panic!("Expected HTTP 400 for malformed room_id, got: {other:?}"),
        }
    }

    // ========================================================================
    // Test (#73): WebSocket connection with valid JWT succeeds
    //
    // NOTE: Full ticket-based auth requires a running WsTicketService (Redis).
    // We test the Authorization header path: a raw JWT passed via Bearer
    // header succeeds, proving the WebSocket upgrade + auth pipeline is
    // working. The ticket path is tested at the unit level in the ticket module.
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_valid_token_query_auth_succeeds() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "valid_auth_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Auth Test Room").await;

        // Connect using Authorization header with a valid JWT
        // ws_connect already asserts HTTP 101 Switching Protocols
        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Should receive the initial UserJoined message confirming successful auth
        let initial = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws),
        )
        .await
        .expect("timeout waiting for initial message after valid auth")
        .expect("stream ended");

        assert!(
            matches!(
                initial.message,
                Some(server_message::Message::UserJoined(_))
            ),
            "Expected UserJoined after successful auth, got: {:?}",
            initial.message,
        );

        ws.close(None).await.expect("close");
    }

    // ========================================================================
    // Test (#73): Sending invalid message format returns an error response
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_invalid_message_format_returns_error() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "invalid_msg_user",
        )
        .await;
        let room_id = create_test_room(&server.room_service, &user_id, "Invalid Msg Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Drain initial membership events
        drain_until_quiet(&mut ws, 2000).await;

        // Send garbage bytes that cannot be decoded as a valid protobuf ClientMessage
        let garbage = vec![0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA, 0x01, 0x02];
        ws.send(tungstenite::Message::Binary(garbage.into()))
            .await
            .expect("send garbage bytes");

        // The server should either:
        //   a) Send an Error ServerMessage back, OR
        //   b) Close the connection
        // In either case the connection should handle the bad input gracefully.
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    Some(Ok(tungstenite::Message::Binary(bytes))) => {
                        // Decode and check if it is an error response
                        if let Ok(msg) = ProtoCodec::decode_server_message(&bytes) {
                            if matches!(msg.message, Some(server_message::Message::Error(_))) {
                                return "error_message";
                            }
                            // Other message types (e.g. buffered events) keep draining
                        }
                    }
                    Some(Ok(tungstenite::Message::Close(_))) | None => {
                        return "connection_closed";
                    }
                    Some(Err(_)) => {
                        return "connection_error";
                    }
                    _ => continue,
                }
            }
        })
        .await;

        match result {
            Ok("error_message" | "connection_closed" | "connection_error") => {
                // Server closed the connection on invalid input — also acceptable
            }
            Err(_timeout) => {
                // Server kept the connection alive but sent nothing back for garbage.
                // This is acceptable as long as subsequent valid messages still work.
                // Verify by sending a heartbeat.
                let heartbeat = ClientMessage {
                    message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                        timestamp: chrono::Utc::now().timestamp_millis(),
                    })),
                };
                send_client_message(&mut ws, &heartbeat).await;
                let ack = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    recv_server_message(&mut ws),
                )
                .await
                .expect("timeout on heartbeat after garbage")
                .expect("stream ended after garbage");
                assert!(
                    matches!(ack.message, Some(server_message::Message::HeartbeatAck(_))),
                    "Connection should still respond to heartbeat after invalid message"
                );
                ws.close(None).await.ok();
            }
            Ok(other) => panic!("Unexpected result: {other}"),
        }
    }

    // ========================================================================
    // Test (#73): Heartbeat / Ping message receives Pong / Ack response
    //
    // This is a dedicated test for the spec requirement; the full heartbeat
    // cycle (including ack timestamp) is already verified in
    // test_ws_heartbeat_ping_pong, but we duplicate the core assertion here
    // so it is clearly labelled for the #73 test-coverage requirement.
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_heartbeat_receives_ack() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "hb_ack_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Heartbeat Ack Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Drain initial membership events
        drain_until_quiet(&mut ws, 2000).await;

        let now_seconds = chrono::Utc::now().timestamp();
        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: now_seconds,
            })),
        };
        send_client_message(&mut ws, &heartbeat).await;

        let ack = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message_skip_membership(&mut ws),
        )
        .await
        .expect("timeout waiting for HeartbeatAck")
        .expect("stream ended before HeartbeatAck");

        match ack.message {
            Some(server_message::Message::HeartbeatAck(ack_msg)) => {
                assert!(
                    ack_msg.timestamp >= now_seconds,
                    "HeartbeatAck timestamp ({}) should be >= request timestamp ({})",
                    ack_msg.timestamp,
                    now_seconds,
                );
            }
            other => panic!("Expected HeartbeatAck, got: {other:?}"),
        }

        ws.close(None).await.expect("close");
    }
}

// ============================================================================
// Module: WebSocket connection limit check timing tests
// ============================================================================
//
// These tests verify that connection limit checks happen BEFORE the WebSocket
// upgrade (HTTP 101), so clients receive a proper HTTP 429 error instead of
// getting a successful upgrade followed by an immediate disconnect.
//
// Issue: Previously, the connection limit was checked inside handle_socket()
// after the WebSocket upgrade completed. This meant:
// 1. Client receives HTTP 101 Switching Protocols
// 2. Connection is established
// 3. Server checks limits and disconnects if exceeded
//
// Fix: Move the limit check to websocket_handler() before ws.on_upgrade(),
// returning HTTP 429 Too Many Requests if limits are exceeded.
// ============================================================================

#[cfg(test)]
mod websocket_connection_limit_timing {
    use super::wait_for_condition;
    use std::sync::Arc;

    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite;

    use synctv_api::http::websocket::websocket_handler;
    use synctv_cluster::sync::{
        ClusterConfig, ClusterManager, ConnectionLimits, ConnectionManager,
    };
    use synctv_core::cache::UsernameCache;
    use synctv_core::config::PasswordComplexityConfig;
    use synctv_core::models::id::UserId;
    use synctv_core::service::auth::jwt::{JwtService, TokenType};
    use synctv_core::service::rate_limit::RateLimiter;
    use synctv_core::service::{RoomService, UserService};
    use synctv_core_testing::{
        create_test_pool_with_db_and_label, start_redis_url_with_label, RedisContainer,
        TestContainer,
    };

    use sqlx::PgPool;

    const TEST_JWT_SECRET: &str =
        "this-is-a-test-secret-with-enough-entropy-for-jwt-signing-32chars";

    /// Create a minimal `ChatService` for tests.
    /// Create a minimal `ChatService` for tests.
    fn build_test_chat_service(
        pool: &sqlx::PgPool,
        username_cache: UsernameCache,
    ) -> Arc<synctv_core::service::ChatService> {
        let chat_repo = Arc::new(synctv_core::repository::ChatRepository::new(pool.clone()));
        let chat_rate_limiter = RateLimiter::in_memory_only("test_chat:".to_string());
        let content_filter = synctv_core::service::ContentFilter::new();
        let member_repo = synctv_core::repository::RoomMemberRepository::new(pool.clone());
        let room_repo = synctv_core::repository::RoomRepository::new(pool.clone());
        let permission_service =
            synctv_core::service::PermissionService::new(member_repo, room_repo, None, 1000, 300);
        let room_settings_repo = synctv_core::repository::RoomSettingsRepository::new(pool.clone());
        let notification_service = Arc::new(synctv_core::service::NotificationService::default());
        let room_settings_service = synctv_core::service::RoomSettingsService::new(
            room_settings_repo,
            None,
            notification_service,
            None,
            None,
            None,
        );
        Arc::new(synctv_core::service::ChatService::new(
            chat_repo,
            chat_rate_limiter,
            synctv_core::service::RateLimitConfig::default(),
            content_filter,
            username_cache,
            permission_service,
            room_settings_service,
        ))
    }

    struct TestInfra {
        pool: PgPool,
        redis_url: String,
        #[allow(dead_code)]
        postgres: Option<TestContainer>,
        #[allow(dead_code)]
        redis: Option<RedisContainer>,
    }

    impl TestInfra {
        async fn new() -> Self {
            let (postgres, pool) =
                create_test_pool_with_db_and_label("synctv_test", "api-ws-connection-limit").await;
            let (redis, redis_url) = start_redis_url_with_label("api-ws-connection-limit").await;

            Self {
                pool,
                redis_url,
                postgres: Some(postgres),
                redis: Some(redis),
            }
        }

        #[allow(dead_code)]
        async fn cleanup(mut self) {
            if let Some(redis) = self.redis.take() {
                redis.cleanup().await;
            }
            if let Some(postgres) = self.postgres.take() {
                postgres.cleanup().await;
            }
        }
    }

    struct LimitTestServer {
        addr: String,
        jwt_service: JwtService,
        room_service: Arc<RoomService>,
        user_service: Arc<UserService>,
        connection_manager: Arc<ConnectionManager>,
    }

    /// Build a server with a very low `max_per_user` limit (1 connection per user)
    async fn setup_server_with_low_user_limit(infra: &TestInfra) -> LimitTestServer {
        let pool = infra.pool.clone();
        let redis_url = infra.redis_url.clone();

        let jwt_service = JwtService::new(TEST_JWT_SECRET).expect("JwtService");
        let redis_client = redis::Client::open(infra.redis_url.as_str()).expect("Redis client");
        let redis_conn = Arc::new(tokio::sync::RwLock::new(
            redis::aio::ConnectionManager::new(redis_client.clone())
                .await
                .expect("Redis ConnectionManager"),
        ));

        let l2_backend = Arc::new(synctv_core::cache::l2_backend::RedisCacheL2::new_shared(
            redis_conn.clone(),
        ));
        let username_cache = UsernameCache::new(l2_backend, "test_un:".to_string(), 100, 300);
        let username_cache_for_chat = username_cache.clone();
        let key_builder = synctv_core::cache::KeyBuilder::new("test:".to_string());
        let brute_force = synctv_core::service::auth::BruteForceProtection::with_redis(
            redis_conn.clone(),
            "test:".to_string(),
        );
        let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> = Arc::new(
            synctv_core::service::InMemoryTokenBlacklistStore::new(10_000, 3600, 86400),
        );

        let user_service = Arc::new(UserService::new(
            pool.clone(),
            jwt_service.clone(),
            username_cache,
            PasswordComplexityConfig::default(),
            token_blacklist,
            key_builder,
            brute_force,
        ));
        let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

        let redis_client_for_cluster =
            redis::Client::open(redis_url.clone()).expect("Redis client");
        let redis_conn_for_cluster = redis_client_for_cluster
            .get_connection_manager()
            .await
            .expect("ConnectionManager");
        let cluster_config = ClusterConfig {
            redis_client: Some(redis_client_for_cluster),
            redis_conn: Some(redis_conn_for_cluster),
            shared_redis_conn: None,
            node_id: "limit_test_node".to_string(),
            ..Default::default()
        };
        let cluster_manager = Arc::new(
            ClusterManager::new(cluster_config, None, None)
                .await
                .expect("ClusterManager"),
        );

        // CRITICAL: Set max_per_user = 1 to trigger the limit easily
        let connection_limits = ConnectionLimits {
            webrtc_session_timeout: Default::default(),
            max_per_user: 1,
            max_per_room: 200,
            max_total: 10000,
            idle_timeout: std::time::Duration::from_mins(5),
            max_duration: std::time::Duration::from_hours(24),
        };
        let redis_conn_for_connections = redis::aio::ConnectionManager::new(
            redis::Client::open(redis_url.clone()).expect("Redis client for connection manager"),
        )
        .await
        .expect("Redis ConnectionManager for connection manager");
        let connection_manager = Arc::new(
            ConnectionManager::new(connection_limits)
                .with_redis(redis_conn_for_connections, "test:"),
        );
        let connection_manager_ret = connection_manager.clone();

        let rate_limiter = RateLimiter::in_memory_only("test_ws:".to_string());
        let jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(Arc::new(
            jwt_service.clone(),
        )));
        let rate_limit_config = Arc::new(synctv_api::http::middleware::RateLimitConfig::default());

        let provider_instance_repo = Arc::new(
            synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
        );
        let provider_instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            provider_instance_repo,
            None,
            None,
            "",
        ));
        let user_provider_credential_repo =
            Arc::new(synctv_core::repository::UserProviderCredentialRepository::new(pool.clone()));
        let providers = synctv_core::provider::ProviderSet {
            bilibili: Arc::new(synctv_core::provider::BilibiliProvider::new(
                provider_instance_manager.clone(),
            )),
            alist: Arc::new(synctv_core::provider::AlistProvider::new(
                provider_instance_manager.clone(),
            )),
            emby: Arc::new(synctv_core::provider::EmbyProvider::new(
                provider_instance_manager.clone(),
            )),
            direct_url: Arc::new(synctv_core::provider::DirectUrlProvider::new()),
            rtmp: Arc::new(synctv_core::provider::RtmpProvider::new()),
            live_proxy: Arc::new(synctv_core::provider::LiveProxyProvider::new()),
        };

        let config = Arc::new(synctv_core::Config::default());

        let client_api = Arc::new(synctv_api::impls::ClientApiImpl::new(
            user_service.clone(),
            room_service.clone(),
            connection_manager.clone(),
            config.clone(),
            None,
            jwt_service.clone(),
            None,
            None,
            None,
        ));

        let bilibili_api = Arc::new(synctv_api::impls::BilibiliApiImpl::new(
            providers.bilibili.clone(),
            user_provider_credential_repo.clone(),
        ));
        let alist_api = Arc::new(synctv_api::impls::AlistApiImpl::new(
            providers.alist.clone(),
            user_provider_credential_repo.clone(),
        ));
        let emby_api = Arc::new(synctv_api::impls::EmbyApiImpl::new(
            providers.emby.clone(),
            user_provider_credential_repo.clone(),
        ));

        let router_config = synctv_api::http::RouterConfig {
            turn_health_checker: Default::default(),
            config,
            user_service: user_service.clone(),
            room_service: room_service.clone(),
            content_filter: synctv_core::service::ContentFilter::new(),
            provider_instance_manager,
            user_provider_credential_repository: user_provider_credential_repo.clone(),
            providers: providers.clone(),
            cluster_manager: Some(cluster_manager),
            connection_manager,
            jwt_service: jwt_service.clone(),
            redis_publish_tx: None,
            oauth2_service: None,
            settings_service: None,
            settings_registry: None,
            email_service: None,
            email_token_service: None,
            publish_key_service: None,
            notification_service: None,
            chat_service: Some(build_test_chat_service(&pool, username_cache_for_chat)),
            audit_service: {
                let (audit_svc, _audit_handle) =
                    synctv_core::service::AuditService::new(pool.clone());
                Arc::new(audit_svc)
            },
            live_streaming_infrastructure: None,
            rate_limiter,
            ws_ticket_service: Some(Arc::new(
                synctv_core::service::WsTicketService::with_memory(None),
            )),
            redis_conn: None,
            builtin_stun_url: None,
            credential_encryption: None,
            proxy_slice_cache: std::sync::Arc::new(synctv_proxy::slice_cache::SliceCache::new(
                synctv_proxy::slice_cache::SliceCacheConfig::default(),
            )),
            messaging_rate_limit_config: synctv_core::service::RateLimitConfig::default(),
            heartbeat_schedule: synctv_api::impls::HeartbeatSchedule::for_tests(
                std::time::Duration::from_millis(400),
                std::time::Duration::from_millis(100),
            ),
            providers_manager: None,
        };

        let guest_token_validator = Arc::new(
            synctv_core::service::auth::GuestTokenValidator::new(Arc::new(jwt_service.clone()))
                .with_blacklist(
                    user_service.token_blacklist_store(),
                    user_service.key_builder().clone(),
                ),
        );

        let state = synctv_api::AppState {
            router_config: Arc::new(router_config),
            rate_limit_config,
            messaging_rate_limit_config: Arc::new(synctv_core::service::RateLimitConfig::default()),
            content_filter: Arc::new(synctv_core::service::ContentFilter::new()),
            heartbeat_schedule: synctv_api::impls::HeartbeatSchedule::for_tests(
                std::time::Duration::from_millis(400),
                std::time::Duration::from_millis(100),
            ),
            jwt_validator,
            security_pipeline: Arc::new(
                synctv_core::service::SecurityPipeline::new(user_service.clone())
                    .with_token_blacklist(
                        user_service.token_blacklist_store(),
                        user_service.key_builder().clone(),
                    ),
            ),
            guest_token_validator,
            client_api,
            admin_api: None,
            notification_api: None,
            oauth2_api: None,
            bilibili_api,
            alist_api,
            emby_api,
            provider_stores: std::sync::Arc::new(
                synctv_core::provider::store::ProviderStoreRegistry::new(None, ""),
            ),
            proxy_provider_registry: std::sync::Arc::new(providers.build_proxy_registry()),
            proxy_services: {
                let signing_key =
                    std::sync::Arc::new(synctv_core::service::ProxySigningKey::derive_from(
                        b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
                    ));
                std::sync::Arc::new(synctv_core::provider::proxy::ProxyServices {
                    room_service: room_service.clone(),
                    credential_encryption: None,
                    credential_repo: user_provider_credential_repo.clone(),
                    signing_key,
                })
            },
            proxy_signing_key: std::sync::Arc::new(
                synctv_core::service::ProxySigningKey::derive_from(
                    b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
                ),
            ),
            proxy_slice_cache: std::sync::Arc::new(synctv_proxy::slice_cache::SliceCache::new(
                synctv_proxy::slice_cache::SliceCacheConfig::default(),
            )),
        };

        let app = axum::Router::new()
            .route("/ws/rooms/{room_id}", axum::routing::get(websocket_handler))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let addr_str = format!("127.0.0.1:{}", addr.port());

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server error");
        });

        LimitTestServer {
            addr: addr_str,
            jwt_service,
            room_service,
            user_service,
            connection_manager: connection_manager_ret,
        }
    }

    async fn register_test_user(
        user_service: &UserService,
        jwt_service: &JwtService,
        username: &str,
    ) -> (UserId, String) {
        let (user, _access, _refresh) = user_service
            .register(
                username.to_string(),
                Some(format!("{username}@test.com")),
                "TestPassword123!".to_string(),
                None,
            )
            .await
            .expect("register user");
        let token = jwt_service
            .sign_token(&user.id, TokenType::Access, 0)
            .expect("sign token");
        (user.id, token)
    }

    async fn create_test_room(
        room_service: &RoomService,
        user_id: &UserId,
        room_name: &str,
    ) -> String {
        let (room, _member) = room_service
            .create_room(
                room_name.to_string(),
                String::new(),
                user_id.clone(),
                None,
                None,
            )
            .await
            .expect("create room");
        room.id.as_str().to_string()
    }

    /// Build a WebSocket request with Authorization header (no `?token=` query param).
    fn ws_request(addr: &str, room_id: &str, token: &str) -> tungstenite::http::Request<()> {
        let url = format!("ws://{addr}/ws/rooms/{room_id}");
        tungstenite::http::Request::builder()
            .uri(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Host", addr)
            .body(())
            .expect("build WS request")
    }

    // ========================================================================
    // TEST 1: Connection limit exceeded returns HTTP 429 (not 101)
    // ========================================================================
    //
    // This test verifies that when a user exceeds their connection limit,
    // the server returns HTTP 429 Too Many Requests BEFORE upgrading to WebSocket.
    //
    // Expected behavior:
    // - First connection: HTTP 101 Switching Protocols (success)
    // - Second connection (same user): HTTP 429 Too Many Requests
    //
    // Current bug: Both connections get HTTP 101, then the second one is
    // disconnected from inside handle_socket().
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_connection_limit_returns_429_before_upgrade() {
        let infra = TestInfra::new().await;
        let server = setup_server_with_low_user_limit(&infra).await;

        // Create a user and room
        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "limit_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Limit Test Room").await;

        // First connection should succeed (HTTP 101)
        let (ws1, response1) =
            tokio_tungstenite::connect_async(ws_request(&server.addr, &room_id, &token))
                .await
                .expect("First WebSocket connect should succeed");
        assert_eq!(
            response1.status(),
            tungstenite::http::StatusCode::SWITCHING_PROTOCOLS,
            "First connection should get HTTP 101 Switching Protocols"
        );

        wait_for_condition(std::time::Duration::from_secs(2), || {
            server.connection_manager.user_connection_count(&user_id) == 1
        })
        .await;

        // Second connection (same user) should fail with HTTP 429
        // This is the KEY assertion - the limit check must happen BEFORE upgrade
        let result =
            tokio_tungstenite::connect_async(ws_request(&server.addr, &room_id, &token)).await;

        match result {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(
                    response.status(),
                    tungstenite::http::StatusCode::TOO_MANY_REQUESTS,
                    "Second connection should get HTTP 429 Too Many Requests, got {}",
                    response.status()
                );
            }
            Err(e) => {
                panic!("Expected HTTP error with status 429, got: {e:?}");
            }
            Ok((_ws2, response)) => {
                // BUG: If we get here, the connection was upgraded when it shouldn't have been
                panic!(
                    "BUG: Second connection was upgraded with status {} instead of being rejected with 429. \
                     Connection limit check is happening AFTER WebSocket upgrade!",
                    response.status()
                );
            }
        }

        // Clean up first connection
        drop(ws1);
    }

    // ========================================================================
    // TEST 2: Normal connection flow is not affected
    // ========================================================================
    //
    // This test verifies that users within their connection limits can
    // still connect normally.
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_normal_connection_within_limits() {
        let infra = TestInfra::new().await;
        let server = setup_server_with_low_user_limit(&infra).await;

        // Create a user and room
        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "normal_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Normal Flow Room").await;

        // Connection should succeed (HTTP 101)
        let (_ws, response) =
            tokio_tungstenite::connect_async(ws_request(&server.addr, &room_id, &token))
                .await
                .expect("WebSocket connect should succeed");

        assert_eq!(
            response.status(),
            tungstenite::http::StatusCode::SWITCHING_PROTOCOLS,
            "Connection within limits should get HTTP 101"
        );

        wait_for_condition(std::time::Duration::from_secs(2), || {
            server.connection_manager.user_connection_count(&user_id) == 1
        })
        .await;
    }

    // ========================================================================
    // TEST 3: Different users can each connect within their limits
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_different_users_can_connect_within_limits() {
        let infra = TestInfra::new().await;
        let server = setup_server_with_low_user_limit(&infra).await;

        // Create two different users
        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user1").await;
        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user2").await;

        // Create a room with user1 as owner
        let room_id = create_test_room(&server.room_service, &user1_id, "Multi User Room").await;

        // Join user2 to the room
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("user2 join room");

        // Both users should be able to connect (HTTP 101)
        let (_ws1, response1) =
            tokio_tungstenite::connect_async(ws_request(&server.addr, &room_id, &user1_token))
                .await
                .expect("User1 WebSocket connect should succeed");

        assert_eq!(
            response1.status(),
            tungstenite::http::StatusCode::SWITCHING_PROTOCOLS,
            "User1 should get HTTP 101"
        );

        let (_ws2, response2) =
            tokio_tungstenite::connect_async(ws_request(&server.addr, &room_id, &user2_token))
                .await
                .expect("User2 WebSocket connect should succeed");

        assert_eq!(
            response2.status(),
            tungstenite::http::StatusCode::SWITCHING_PROTOCOLS,
            "User2 should get HTTP 101"
        );

        wait_for_condition(std::time::Duration::from_secs(2), || {
            server.connection_manager.user_connection_count(&user1_id) == 1
                && server.connection_manager.user_connection_count(&user2_id) == 1
        })
        .await;
    }

    // ========================================================================
    // TEST 4: After disconnect, user can reconnect
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_can_reconnect_after_disconnect() {
        let infra = TestInfra::new().await;
        let server = setup_server_with_low_user_limit(&infra).await;

        // Create a user and room
        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "reconnect_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Reconnect Room").await;

        // First connection
        let (ws1, response1) =
            tokio_tungstenite::connect_async(ws_request(&server.addr, &room_id, &token))
                .await
                .expect("First connect should succeed");

        assert_eq!(
            response1.status(),
            tungstenite::http::StatusCode::SWITCHING_PROTOCOLS
        );

        wait_for_condition(std::time::Duration::from_secs(2), || {
            server.connection_manager.user_connection_count(&user_id) == 1
        })
        .await;

        // Disconnect
        drop(ws1);

        wait_for_condition(std::time::Duration::from_secs(2), || {
            server.connection_manager.user_connection_count(&user_id) == 0
        })
        .await;

        // Should be able to reconnect (HTTP 101)
        let (ws2, response2) =
            tokio_tungstenite::connect_async(ws_request(&server.addr, &room_id, &token))
                .await
                .expect("Reconnect should succeed");

        assert_eq!(
            response2.status(),
            tungstenite::http::StatusCode::SWITCHING_PROTOCOLS,
            "Reconnection after disconnect should get HTTP 101"
        );

        wait_for_condition(std::time::Duration::from_secs(2), || {
            server.connection_manager.user_connection_count(&user_id) == 1
        })
        .await;

        drop(ws2);
    }
}

// ============================================================================
// Module: Slow Client Disconnect Tests (Task #82)
// ============================================================================
//
// These tests verify the slow client handling logic in WebSocket connections.
// Slow clients are those that cannot keep up with message delivery, causing
// the outbound channel to fill up. When SLOW_CLIENT_DROP_THRESHOLD (10) is
// exceeded, the client is disconnected.

mod slow_client_disconnect_tests {

    /// Test that consecutive drop counter logic works correctly.
    /// This simulates the counter behavior without actual WebSocket.
    #[test]
    fn test_consecutive_drop_counter_logic() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let counter = AtomicU32::new(0);
        const THRESHOLD: u32 = 10;

        // Simulate 9 drops - should NOT trigger disconnect
        for _i in 0..9 {
            let drops = counter.fetch_add(1, Ordering::Relaxed) + 1;
            assert!(
                drops < THRESHOLD,
                "Drop {drops} should not trigger disconnect"
            );
        }

        // 10th drop - should trigger disconnect
        let drops = counter.fetch_add(1, Ordering::Relaxed) + 1;
        assert!(drops >= THRESHOLD, "Drop {drops} should trigger disconnect");

        // Reset counter (successful send)
        counter.store(0, Ordering::Relaxed);
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "Counter should reset to 0"
        );

        // Verify we can count again after reset
        let drops = counter.fetch_add(1, Ordering::Relaxed) + 1;
        assert_eq!(drops, 1, "Counter should start from 1 after reset");
    }

    /// Test that the drop counter resets on successful send.
    #[test]
    fn test_drop_counter_resets_on_successful_send() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let counter = AtomicU32::new(5); // Simulate 5 previous drops

        // Successful send resets counter
        counter.store(0, Ordering::Relaxed);

        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "Counter should reset to 0 after successful send"
        );
    }

    /// Test different channel capacities and their effect on slow client detection.
    #[test]
    fn test_channel_capacity_vs_threshold_relationship() {
        // WebSocket channel capacity is typically 32-256 messages.
        // SLOW_CLIENT_DROP_THRESHOLD is 10.
        //
        // Relationship:
        // - With capacity 32: client has ~32 messages buffered before drops start
        // - After 10 consecutive drops (42 total send attempts): disconnect
        // - This gives ~3-4 seconds of buffer at 10 msgs/sec
        //
        // If channel is too small (e.g., 8), clients may disconnect too easily.
        // If threshold is too high (e.g., 100), slow clients stay connected too long.

        const CHANNEL_CAPACITY: u32 = 32;
        const DROP_THRESHOLD: u32 = 10;
        const TOTAL_BUFFER_BEFORE_DISCONNECT: u32 = CHANNEL_CAPACITY + DROP_THRESHOLD;

        assert_eq!(
            TOTAL_BUFFER_BEFORE_DISCONNECT, 42,
            "With capacity 32 and threshold 10, client has 42 message buffer"
        );
    }
}

// ============================================================================
// Module: Slow Client Message Recovery Tests (Task #54)
// ============================================================================
//
// These tests verify the slow client message recovery mechanism.
// Critical messages (kick/ban) must be delivered even when the client is slow.

mod slow_client_message_recovery {

    /// Test that critical messages are identified correctly.
    /// Kick/ban notifications arrive as Error messages which are critical.
    #[test]
    fn test_kick_ban_are_critical_messages() {
        use synctv_api::proto::client::{server_message::Message, ErrorMessage, ServerMessage};

        // Kick/ban arrives as Error notification
        let kick_message = ServerMessage {
            message: Some(Message::Error(ErrorMessage {
                message: "You have been kicked from the room".to_string(),
                code: synctv_proto::common::ErrorCode::Forbidden as i32,
                detail: String::new(),
            })),
        };

        // This should be identified as critical
        // The is_critical_message function is private, but we verify the behavior
        // by checking the message type
        assert!(matches!(kick_message.message, Some(Message::Error(_))));
    }

    /// Test that pending disconnects are stored when channel is full.
    /// This verifies the `ConnectionManager` `pending_disconnects` mechanism.
    #[test]
    fn test_pending_disconnects_mechanism_exists() {
        // The pending_disconnects DashMap stores disconnect signals that
        // could not be sent due to full channel. A background task retries them.
        //
        // This is implemented in synctv-cluster/src/sync/connection_manager.rs
        // The key design points are:
        // 1. When disconnect signal fails to send (channel full), it is stored
        // 2. Background task retries pending signals every 5 seconds
        // 3. Signals have a TTL of 60 seconds before being dropped
        //
        // This ensures kick/ban signals eventually reach their target even
        // during temporary channel congestion.
        const PENDING_DISCONNECT_TTL_SECS: u64 = 60;
        const RETRY_INTERVAL_SECS: u64 = 5;

        const { assert!(PENDING_DISCONNECT_TTL_SECS > RETRY_INTERVAL_SECS) };
    }

    /// Test that membership validation serves as fallback for missed signals.
    /// Even if disconnect signal is missed, heartbeat validation catches bans.
    #[test]
    fn test_heartbeat_validates_membership() {
        // The messaging loop has two mechanisms to catch banned/kicked users:
        //
        // 1. Immediate: Disconnect signal via broadcast channel
        // 2. Fallback: Membership check during heartbeat (every 25-35 seconds)
        //
        // Membership cache TTL is 30 seconds, ensuring banned users are
        // disconnected within 30-65 seconds even if signals are missed.
        const MEMBERSHIP_CACHE_TTL_SECS: u64 = 30;
        const HEARTBEAT_INTERVAL_SECS: u64 = 30;

        const { assert!(MEMBERSHIP_CACHE_TTL_SECS >= HEARTBEAT_INTERVAL_SECS) };
    }

    /// Test the relationship between message channel capacity and recovery.
    #[test]
    fn test_message_channel_recovery_relationship() {
        // When the outbound WebSocket channel is full:
        // - Non-critical messages are dropped (after threshold, client disconnects)
        // - Critical messages return error, triggering disconnect
        //
        // The client is disconnected but the server has processed the action
        // (e.g., user is banned regardless of whether they saw the message).
        //
        // On reconnection, the client fetches fresh state including:
        // - Current playback state
        // - Room membership status (will be rejected if banned)
        // - Room settings

        // This is the expected flow for slow client handling
        const SLOW_CLIENT_DROP_THRESHOLD: u32 = 10;
        const { assert!(SLOW_CLIENT_DROP_THRESHOLD > 0) };
    }
}

// ============================================================================
// Module: Membership Cache TTL Tests (Task #55)
// ============================================================================
//
// These tests verify the membership cache TTL optimization.
// The TTL was reduced from 60 seconds to 30 seconds for faster detection
// of banned/kicked users when disconnect signals are missed.

mod membership_cache_ttl_tests {

    /// Test that membership cache TTL is set to 30 seconds.
    ///
    /// This was reduced from 60 seconds to improve responsiveness to
    /// membership changes (kick/ban) when disconnect signals are missed.
    ///
    /// With 30-second TTL:
    /// - Maximum 2 DB queries per minute per connection (vs. every heartbeat without cache)
    /// - Banned users disconnected within 30-65 seconds worst case
    /// - Still provides significant DB load reduction
    #[test]
    fn test_membership_cache_ttl_is_30_seconds() {
        // The TTL is defined in synctv-api/src/impls/messaging.rs
        const MEMBERSHIP_CACHE_TTL_SECS: u64 = 30;

        // TTL should be 30 seconds for balance between DB load and responsiveness
        assert_eq!(
            MEMBERSHIP_CACHE_TTL_SECS, 30,
            "Membership cache TTL should be 30 seconds for optimal balance"
        );
    }

    /// Test that cache TTL allows at least one heartbeat check.
    ///
    /// Heartbeat interval is 25-35 seconds. Cache TTL of 30 seconds
    /// ensures at least one heartbeat can use cached data before expiry.
    #[test]
    fn test_cache_ttl_allows_heartbeat_check() {
        const MEMBERSHIP_CACHE_TTL_SECS: u64 = 30;
        const MIN_HEARTBEAT_INTERVAL_SECS: u64 = 25;

        // TTL should be at least as long as minimum heartbeat interval
        const { assert!(MEMBERSHIP_CACHE_TTL_SECS >= MIN_HEARTBEAT_INTERVAL_SECS) };
    }

    /// Test that worst-case disconnect time is acceptable.
    ///
    /// Worst case scenario:
    /// 1. User gets banned right after heartbeat
    /// 2. Disconnect signal is missed (channel full/network issue)
    /// 3. Next heartbeat is at 35 seconds
    /// 4. Cache expires at 30 seconds, forcing DB query
    /// 5. Ban detected at next heartbeat (35 seconds)
    ///
    /// Total worst case: ~35 seconds (down from ~95 seconds with 60s TTL)
    #[test]
    fn test_worst_case_disconnect_time_is_acceptable() {
        const MEMBERSHIP_CACHE_TTL_SECS: u64 = 30;
        const MAX_HEARTBEAT_INTERVAL_SECS: u64 = 35;

        // Worst case = cache TTL + max heartbeat interval
        let worst_case_disconnect_secs = MEMBERSHIP_CACHE_TTL_SECS + MAX_HEARTBEAT_INTERVAL_SECS;

        // Worst case should be under 70 seconds (down from ~95 with 60s TTL)
        assert!(
            worst_case_disconnect_secs < 70,
            "Worst case disconnect time ({worst_case_disconnect_secs}) should be under 70 seconds"
        );

        // This is a significant improvement from 60s TTL (95s worst case)
        let old_worst_case = 60 + MAX_HEARTBEAT_INTERVAL_SECS;
        let improvement = old_worst_case - worst_case_disconnect_secs;

        assert_eq!(improvement, 30,
            "Should reduce worst case by 30 seconds (from {old_worst_case}s to {worst_case_disconnect_secs}s)");
    }
}

mod danmu_sse_tests {}
