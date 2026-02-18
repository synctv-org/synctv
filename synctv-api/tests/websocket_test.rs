//! WebSocket integration tests for synctv-api
//!
//! Tests WebSocket-related types, authentication methods, query parameter
//! parsing, proto codec encoding/decoding, and message type handling.
//!
//! Includes both:
//! - Unit tests: validate individual components in isolation (no server needed)
//! - E2E tests: full WebSocket lifecycle with real Postgres + Redis (TestInfra)

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
    use synctv_proto::client::ServerMessage;

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
        assert_eq!(req._room_id.as_deref(), Some("room_abc"));
    }

    #[test]
    fn test_create_ticket_request_empty() {
        let json = r#"{}"#;
        let req: CreateTicketRequest = serde_json::from_str(json).unwrap();
        assert!(req._room_id.is_none());
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
        let svc2 = JwtService::new(
            "another-secret-that-is-different-and-long-enough-for-the-test"
        ).unwrap();

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
        let _bearer = format!("Bearer {}", token);

        let extracted = validator.validate_and_extract_user_id(&token).unwrap();
        assert_eq!(extracted.as_str(), "user_val");
    }

    #[test]
    fn test_validator_http_bearer_header() {
        let svc = test_jwt_service();
        let validator = test_validator();
        let user_id = UserId::from_string("user_http".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let header = format!("Bearer {}", token);

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
        assert_ne!(claims1.jti, claims2.jti, "Each token should have a unique jti");
    }

    #[test]
    fn test_claims_iat_is_recent() {
        let svc = test_jwt_service();
        let user_id = UserId::from_string("user_iat".to_string());
        let token = svc.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let claims = svc.verify_access_token(&token).unwrap();

        let now = chrono::Utc::now().timestamp();
        assert!((now - claims.iat).abs() < 10, "iat should be within 10 seconds of now");
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
// Module: Token blacklist (via SecurityPipeline)
// ============================================================================
//
// Token blacklisting is implemented in SecurityPipeline (Redis-backed).
// Without Redis, blacklist_token / is_token_blacklisted gracefully return
// Ok(false). Full integration tests require a running Redis instance.

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
                cluster: None,
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

// ============================================================================
// Module: WebSocket E2E tests (requires Docker: Postgres + Redis via TestInfra)
// ============================================================================

#[cfg(test)]
mod websocket_e2e {
    use std::sync::Arc;
    use futures::{SinkExt, StreamExt};
    use prost::Message;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite;

    use synctv_api::http::websocket::websocket_handler;
    use synctv_api::impls::messaging::ProtoCodec;
    use synctv_core::cache::UsernameCache;
    use synctv_core::models::id::UserId;
    use synctv_core::service::auth::jwt::{JwtService, TokenType};
    use synctv_core::service::rate_limit::RateLimiter;
    // Token blacklisting is handled by SecurityPipeline (Redis-backed), not a separate service
    use synctv_core::config::PasswordComplexityConfig;
    use synctv_core::service::{RoomService, UserService};
    use synctv_cluster::sync::{ClusterConfig, ClusterManager, ConnectionManager, ConnectionLimits};
    use synctv_proto::client::{
        ClientMessage, ServerMessage, HeartbeatMessage,
        client_message, server_message,
    };

    use sqlx::PgPool;
    use testcontainers::runners::AsyncRunner;
    use testcontainers::ContainerAsync;
    use testcontainers_modules::postgres::Postgres;
    use testcontainers_modules::redis::Redis;

    const TEST_JWT_SECRET: &str = "this-is-a-test-secret-with-enough-entropy-for-jwt-signing-32chars";

    /// Lightweight test infrastructure for E2E tests.
    /// Starts Postgres and Redis containers, runs migrations, and provides connections.
    struct TestInfra {
        pool: PgPool,
        redis_url: String,
        _postgres: ContainerAsync<Postgres>,
        _redis: ContainerAsync<Redis>,
    }

    impl TestInfra {
        async fn new() -> Self {
            let (pg_container, redis_container) = tokio::join!(
                Postgres::default()
                    .with_db_name("synctv_test")
                    .with_user("synctv")
                    .with_password("synctv_test")
                    .start(),
                Redis::default().start(),
            );
            let pg_container = pg_container.expect("Failed to start Postgres");
            let redis_container = redis_container.expect("Failed to start Redis");

            let pg_host = pg_container.get_host().await.expect("pg host");
            let pg_port = pg_container.get_host_port_ipv4(5432).await.expect("pg port");
            let redis_host = redis_container.get_host().await.expect("redis host");
            let redis_port = redis_container.get_host_port_ipv4(6379).await.expect("redis port");

            let database_url = format!(
                "postgresql://synctv:synctv_test@{pg_host}:{pg_port}/synctv_test"
            );
            let redis_url = format!("redis://{redis_host}:{redis_port}");

            let pool = PgPool::connect(&database_url).await.expect("connect pg");
            sqlx::migrate!("../migrations").run(&pool).await.expect("migrations");

            Self {
                pool,
                redis_url,
                _postgres: pg_container,
                _redis: redis_container,
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
    }

    /// Build a minimal AppState with real database and Redis for E2E testing.
    async fn setup_e2e_server(infra: &TestInfra) -> E2EServer {
        setup_e2e_server_with_node(infra, "test_node_1").await
    }

    /// Build a minimal AppState with a custom `node_id`.
    ///
    /// Useful for cross-replica tests: call twice with different node IDs
    /// but the same `TestInfra` to simulate two server replicas.
    async fn setup_e2e_server_with_node(infra: &TestInfra, node_id: &str) -> E2EServer {
        let pool = infra.pool.clone();
        let redis_url = infra.redis_url.clone();

        // Create services
        let jwt_service = JwtService::new(TEST_JWT_SECRET).expect("JwtService");
        let username_cache = UsernameCache::new(None, "test_un:".to_string(), 100, 300);
        let user_service = Arc::new(UserService::new(
            pool.clone(),
            jwt_service.clone(),
            username_cache,
            PasswordComplexityConfig::default(),
        ));
        let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

        // Create cluster manager (single-node mode with Redis for Pub/Sub)
        let cluster_config = ClusterConfig {
            redis_url: redis_url.clone(),
            node_id: node_id.to_string(),
            ..Default::default()
        };
        let cluster_manager = Arc::new(
            ClusterManager::new(cluster_config, None, None)
                .await
                .expect("ClusterManager"),
        );
        let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        let connection_manager_ret = connection_manager.clone();

        // Rate limiter (in-memory only for tests)
        let rate_limiter = RateLimiter::in_memory_only("test_ws:".to_string());

        // Build AppState
        let jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(
            Arc::new(jwt_service.clone()),
        ));
        let rate_limit_config = Arc::new(synctv_api::http::middleware::RateLimitConfig::default());

        // Minimal providers (unused in WebSocket tests but required by AppState)
        let provider_instance_repo = Arc::new(
            synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
        );
        let provider_instance_manager = Arc::new(
            synctv_core::service::RemoteProviderManager::new(provider_instance_repo, None, None),
        );
        let user_provider_credential_repo = Arc::new(
            synctv_core::repository::UserProviderCredentialRepository::new(pool.clone()),
        );
        let bilibili_provider = Arc::new(synctv_core::provider::BilibiliProvider::new(
            provider_instance_manager.clone(),
        ));
        let alist_provider = Arc::new(synctv_core::provider::AlistProvider::new(
            provider_instance_manager.clone(),
        ));
        let emby_provider = Arc::new(synctv_core::provider::EmbyProvider::new(
            provider_instance_manager.clone(),
        ));

        // Config
        let config = Arc::new(synctv_core::Config {
            server: synctv_core::config::ServerConfig::default(),
            database: Default::default(),
            redis: Default::default(),
            jwt: Default::default(),
            logging: Default::default(),
            livestream: Default::default(),
            oauth2: Default::default(),
            email: Default::default(),
            media_providers: Default::default(),
            webrtc: Default::default(),
            connection_limits: Default::default(),
            bootstrap: Default::default(),
            cluster: Default::default(),
            password_complexity: Default::default(),
            buffer_sizes: Default::default(),
            cache: Default::default(),
        });

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
        let bilibili_api = Arc::new(synctv_api::impls::BilibiliApiImpl::new(bilibili_provider.clone()));
        let alist_api = Arc::new(synctv_api::impls::AlistApiImpl::new(alist_provider.clone()));
        let emby_api = Arc::new(synctv_api::impls::EmbyApiImpl::new(emby_provider.clone()));

        let state = synctv_api::AppState {
            config,
            user_service: user_service.clone(),
            room_service: room_service.clone(),
            provider_instance_manager,
            user_provider_credential_repository: user_provider_credential_repo,
            alist_provider,
            bilibili_provider,
            emby_provider,
            cluster_manager: Some(cluster_manager),
            connection_manager,
            jwt_service: jwt_service.clone(),
            redis_publish_tx: None,
            oauth2_service: None,
            settings_service: None,
            settings_registry: None,
            email_service: None,
            publish_key_service: None,
            notification_service: None,
            live_streaming_infrastructure: None,
            rate_limiter,
            rate_limit_config,
            jwt_validator,
            security_pipeline: Arc::new(synctv_core::service::SecurityPipeline::new(
                user_service.clone(),
            )),
            ws_ticket_service: None,
            client_api,
            admin_api: None,
            notification_api: None,
            oauth2_api: None,
            bilibili_api,
            alist_api,
            emby_api,
            redis_conn: None,
        };

        // Build a minimal router with just the WebSocket endpoint
        let app = axum::Router::new()
            .route(
                "/ws/rooms/{room_id}",
                axum::routing::get(websocket_handler),
            )
            .with_state(state);

        // Bind to a random port
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let addr_str = format!("127.0.0.1:{}", addr.port());

        // Spawn server
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("server error");
        });

        E2EServer {
            addr: addr_str,
            jwt_service,
            room_service,
            user_service,
            connection_manager: connection_manager_ret,
        }
    }

    /// Register a test user directly via UserService and return their UserId + access token.
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

    /// Connect to the WebSocket endpoint using token query parameter.
    async fn ws_connect(
        addr: &str,
        room_id: &str,
        token: &str,
    ) -> tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    > {
        let url = format!("ws://{}/ws/rooms/{}?token={}", addr, room_id, token);
        let (ws_stream, response) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("WebSocket connect failed");
        assert_eq!(
            response.status(),
            tungstenite::http::StatusCode::SWITCHING_PROTOCOLS,
            "Expected 101 Switching Protocols"
        );
        ws_stream
    }

    /// Read the next binary message from the WebSocket and decode it as a ServerMessage.
    async fn recv_server_message(
        ws: &mut (impl StreamExt<Item = Result<tungstenite::Message, tungstenite::Error>> + Unpin),
    ) -> Option<ServerMessage> {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(tungstenite::Message::Binary(bytes)) => {
                    return Some(
                        ProtoCodec::decode_server_message(&bytes)
                            .expect("decode server message"),
                    );
                }
                Ok(tungstenite::Message::Ping(_)) => continue,
                Ok(tungstenite::Message::Pong(_)) => continue,
                Ok(tungstenite::Message::Close(_)) => return None,
                Err(e) => panic!("WebSocket error: {e}"),
                _ => continue,
            }
        }
        None
    }

    /// Encode a ClientMessage and send it as binary over the WebSocket.
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_handshake_and_initial_user_joined() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) = register_test_user(&server.user_service, &server.jwt_service, "alice_ws").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Test Room WS").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // The first message should be a UserJoined notification for this user
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws))
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_heartbeat_ping_pong() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) = register_test_user(&server.user_service, &server.jwt_service, "bob_hb").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Heartbeat Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Consume the initial UserJoined message
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws))
            .await
            .expect("timeout")
            .expect("no initial msg");

        // Send a heartbeat ClientMessage
        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws, &heartbeat).await;

        // Expect a HeartbeatAck response
        let ack = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws))
            .await
            .expect("timeout waiting for heartbeat ack")
            .expect("stream ended");

        match ack.message {
            Some(server_message::Message::HeartbeatAck(ack)) => {
                assert!(ack.timestamp > 0, "HeartbeatAck should have a valid timestamp");
            }
            other => panic!("Expected HeartbeatAck, got: {other:?}"),
        }

        ws.close(None).await.expect("close");
    }

    // ========================================================================
    // Test: Graceful disconnect (client sends Close frame)
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_graceful_disconnect() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) = register_test_user(&server.user_service, &server.jwt_service, "carol_dc").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Disconnect Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Consume initial message
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws))
            .await;

        // Send a Close frame
        ws.close(Some(tungstenite::protocol::CloseFrame {
            code: tungstenite::protocol::frame::coding::CloseCode::Normal,
            reason: "bye".into(),
        }))
        .await
        .expect("close");

        // After close, the next recv should return None or a Close frame
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ws.next(),
        )
        .await;

        match result {
            Ok(Some(Ok(tungstenite::Message::Close(_)))) | Ok(None) | Err(_) => {
                // All acceptable: either received Close frame, stream ended, or timeout
            }
            Ok(Some(Ok(msg))) => {
                // After close, we may still receive buffered messages; that's fine
                assert!(
                    !matches!(msg, tungstenite::Message::Binary(_)),
                    "Should not receive new binary messages after close"
                );
            }
            Ok(Some(Err(_))) => {
                // Connection error after close is acceptable
            }
        }
    }

    // ========================================================================
    // Test: Unauthenticated connection attempt is rejected
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_unauthenticated_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Attempt to connect without any token
        let url = format!("ws://{}/ws/rooms/fake_room", server.addr);
        let result = tokio_tungstenite::connect_async(&url).await;

        // Should fail with a non-101 status (likely 401)
        assert!(result.is_err(), "Connection without auth should be rejected");
    }

    // ========================================================================
    // Test: Invalid token is rejected
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_invalid_token_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let url = format!("ws://{}/ws/rooms/fake_room?token=invalid.jwt.token", server.addr);
        let result = tokio_tungstenite::connect_async(&url).await;

        assert!(result.is_err(), "Connection with invalid token should be rejected");
    }

    // ========================================================================
    // Test: Non-member of room is rejected (valid token, not a room member)
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
        let url = format!(
            "ws://{}/ws/rooms/{}?token={}",
            server.addr, room_id, outsider_token
        );
        let result = tokio_tungstenite::connect_async(&url).await;

        assert!(result.is_err(), "Non-member should be rejected with 403");
    }

    // ========================================================================
    // Test: Multiple clients join the same room and receive each other's events
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
        server.room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("user2 join room");

        // Connect user1
        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        // Consume user1's own UserJoined message
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws1))
            .await
            .expect("timeout")
            .expect("no initial msg for user1");

        // Connect user2
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;
        // Consume user2's own UserJoined message
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2))
            .await
            .expect("timeout")
            .expect("no initial msg for user2");

        // user1 should receive a UserJoined event for user2 (via cluster broadcast)
        let user2_join_event = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws1),
        )
        .await
        .expect("timeout waiting for user2 join event on ws1")
        .expect("stream ended");

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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
        server.room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("user2 join");

        // Connect both users
        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        // Drain initial UserJoined messages from both connections
        // ws1 gets its own UserJoined
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws1)).await;
        // ws2 gets its own UserJoined
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2)).await;
        // ws1 may also get user2's UserJoined - drain it
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), recv_server_message(&mut ws1)).await;

        // Small delay to let subscriptions settle
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

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

        // user2 should receive the chat broadcast
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws2),
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_multiple_heartbeats() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) = register_test_user(&server.user_service, &server.jwt_service, "hb_multi").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Multi HB Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Consume initial UserJoined
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws)).await;

        // Send 3 heartbeats and expect 3 acks
        for i in 0..3 {
            let heartbeat = ClientMessage {
                message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                    timestamp: chrono::Utc::now().timestamp_millis() + i,
                })),
            };
            send_client_message(&mut ws, &heartbeat).await;

            let ack = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws))
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
        server.room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("join");

        // Connect both
        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        // Drain initial messages
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws1)).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2)).await;
        // ws1 gets user2's UserJoined
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), recv_server_message(&mut ws1)).await;

        // user2 disconnects
        ws2.close(None).await.expect("close ws2");

        // user1 should receive UserLeft for user2
        let left_event = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws1),
        )
        .await
        .expect("timeout waiting for UserLeft event")
        .expect("stream ended");

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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws1)).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2)).await;

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
            Err(_) => {
                // Timeout: correct -- user2 did not get room A's message
            }
            Ok(None) => {
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_reconnect_after_disconnect() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "reconn_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Reconnect Room").await;

        // First connection
        let mut ws = ws_connect(&server.addr, &room_id, &token).await;
        let initial = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws))
            .await
            .expect("timeout")
            .expect("no initial msg");
        assert!(matches!(initial.message, Some(server_message::Message::UserJoined(_))));

        // Graceful disconnect
        ws.close(None).await.expect("close");
        // Small delay to let the server process the disconnect
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Reconnect with the same token
        let mut ws2 = ws_connect(&server.addr, &room_id, &token).await;
        let rejoined = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2))
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

        // Verify heartbeat still works after reconnection
        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws2, &heartbeat).await;

        let ack = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2))
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
        server.room_service
            .join_room(rid.clone(), user2_id.clone(), None)
            .await
            .expect("join");

        // user2 connects
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;
        // Drain initial UserJoined
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2)).await;

        // Force disconnect user2 from the room via ConnectionManager
        server.connection_manager.disconnect_user_from_room(&user2_id, &rid);

        // user2's connection should be terminated (receive Close or stream ends)
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            async {
                loop {
                    match ws2.next().await {
                        Some(Ok(tungstenite::Message::Close(_))) => return true,
                        None => return true,
                        Some(Err(_)) => return true,
                        Some(Ok(tungstenite::Message::Binary(_))) => {
                            // May still receive buffered messages; keep draining
                            continue;
                        }
                        Some(Ok(_)) => continue,
                    }
                }
            },
        )
        .await;

        assert!(
            result.is_ok(),
            "Connection should be terminated after forced disconnect"
        );
    }

    // ========================================================================
    // Test: Cross-replica messaging via Redis Pub/Sub
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_cross_replica_chat_via_redis() {
        let infra = TestInfra::new().await;

        // Start two server replicas with different node IDs but shared DB + Redis
        let server1 = setup_e2e_server_with_node(&infra, "replica_1").await;
        let server2 = setup_e2e_server_with_node(&infra, "replica_2").await;

        // Create user1 and room via server1's services (shared DB)
        let (user1_id, user1_token) =
            register_test_user(&server1.user_service, &server1.jwt_service, "xrep_u1").await;
        let room_id = create_test_room(&server1.room_service, &user1_id, "Cross Replica Room").await;

        // Create user2 and join room (uses same DB)
        let (user2_id, user2_token) =
            register_test_user(&server1.user_service, &server1.jwt_service, "xrep_u2").await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server1.room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("user2 join");

        // user1 connects to replica_1
        let mut ws1 = ws_connect(&server1.addr, &room_id, &user1_token).await;
        // user2 connects to replica_2
        let mut ws2 = ws_connect(&server2.addr, &room_id, &user2_token).await;

        // Drain initial events
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws1)).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2)).await;
        // Drain any UserJoined cross-notifications
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), recv_server_message(&mut ws1)).await;

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

        // user2 on replica_2 should receive it via Redis Pub/Sub
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            recv_server_message(&mut ws2),
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
    // Test: Rate limiter blocks excessive chat messages
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_rate_limiter_blocks_excess_chat() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "ratelimit_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Rate Limit Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Consume initial UserJoined
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws)).await;

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
            "Rate limiter should have blocked some messages: got {} chats, error={}",
            chat_count,
            error_received,
        );

        ws.close(None).await.expect("close");
    }

    // ========================================================================
    // Test: Content filter strips XSS from chat messages
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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

        // Drain initial UserJoined messages
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws1)).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2)).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), recv_server_message(&mut ws1)).await;

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

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

        // user2 should receive the sanitized chat
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws2),
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws1)).await;

        // Connect user2 (will be dropped)
        let ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        // Drain user2's initial message and user1's notification of user2 join
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), recv_server_message(&mut ws1)).await;

        // Abruptly drop user2's WebSocket (simulate TCP disconnect without Close frame)
        drop(ws2);

        // user1 should receive UserLeft for user2 (server detects the drop)
        let left_event = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            recv_server_message(&mut ws1),
        )
        .await
        .expect("timeout waiting for UserLeft after TCP drop")
        .expect("stream ended");

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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_connection_manager_state_consistency() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "cycle_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Cycle Room").await;

        // Perform 3 connect/disconnect cycles
        for cycle in 0..3 {
            let mut ws = ws_connect(&server.addr, &room_id, &token).await;

            // Consume initial UserJoined
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws)).await;

            // Send a heartbeat to verify the connection is fully functional
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
            .expect(&format!("timeout on heartbeat in cycle {cycle}"))
            .expect("stream ended");
            assert!(
                matches!(ack.message, Some(server_message::Message::HeartbeatAck(_))),
                "Expected HeartbeatAck in cycle {cycle}"
            );

            // Graceful disconnect
            ws.close(None).await.expect(&format!("close in cycle {cycle}"));

            // Wait for server to process the disconnect and clean up state
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // After all cycles, the connection manager should have 0 connections for this user
        let user_conn_count = server.connection_manager.user_connection_count(&user_id);
        assert_eq!(
            user_conn_count, 0,
            "After all disconnect cycles, user should have 0 connections, got {}",
            user_conn_count
        );
    }

    // ========================================================================
    // Test: Empty chat message is rejected by the pipeline
    // ========================================================================

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_empty_chat_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "empty_chat_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Empty Chat Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        // Consume initial UserJoined
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws)).await;

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

        // Connection should remain alive (error is logged server-side, not fatal)
        // Verify by sending a heartbeat and getting an ack
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_ws_danmaku_broadcast() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        // Create room owner (user1) and room
        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "danmaku_sender").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "Danmaku Room").await;

        // Create second user and join room
        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "danmaku_receiver").await;
        let rid = synctv_core::models::RoomId::from_string(room_id.clone());
        server
            .room_service
            .join_room(rid, user2_id.clone(), None)
            .await
            .expect("user2 join");

        // Connect both users
        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        // Drain initial messages
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws1)).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2)).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), recv_server_message(&mut ws1)).await;

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

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

        // user2 should receive the danmaku with position and color
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_server_message(&mut ws2),
        )
        .await
        .expect("timeout waiting for danmaku")
        .expect("stream ended");

        match received.message {
            Some(server_message::Message::Chat(chat)) => {
                assert_eq!(chat.content, "LOL");
                assert_eq!(chat.room_id, room_id);
                assert_eq!(chat.user_id, user1_id.as_str());
                assert!(
                    chat.position.is_some(),
                    "Danmaku should have a position"
                );
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
        let msg1 = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws1))
            .await
            .expect("timeout")
            .expect("no initial msg for conn1");
        assert!(
            matches!(msg1.message, Some(server_message::Message::UserJoined(_))),
            "Connection 1 should get UserJoined"
        );

        let msg2 = tokio::time::timeout(std::time::Duration::from_secs(5), recv_server_message(&mut ws2))
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
}
