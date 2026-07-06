//! WebSocket integration tests for synctv-api
//!
//! Tests WebSocket-related types, authentication methods, query parameter
//! parsing, proto codec encoding/decoding, and message type handling.
//!
//! Includes both:
//! - Unit tests: validate individual components in isolation (no server needed)
//! - E2E tests: full WebSocket lifecycle with real Postgres + Redis (`TestInfra`)

#![allow(clippy::unwrap_used)]
use synctv_api::AuthMethod;
use synctv_proto::client::WebSocketConnectRequest;

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

async fn await_server_shutdown(
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    server_handle: tokio::task::JoinHandle<()>,
) {
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server_handle).await;
}

mod ws_query {
    use super::*;

    #[test]
    fn test_deserialize_with_ticket() {
        let params = "ticket=abc123def456";
        let query: WebSocketConnectRequest = serde_urlencoded::from_str(params).unwrap();
        assert_eq!(query.ticket, "abc123def456");
    }

    #[test]
    fn test_deserialize_empty() {
        let params = "";
        let query: WebSocketConnectRequest = serde_urlencoded::from_str(params).unwrap();
        assert!(query.ticket.is_empty());
        synctv_proto::validate(&query).expect("empty ticket should be allowed for header auth");
    }

    #[test]
    fn test_deserialize_rejects_unknown_params() {
        #[derive(Debug, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Query {
            #[serde(default, rename = "ticket")]
            _ticket: String,
        }

        let params = "ticket=tix&unknown=value";
        let error = serde_urlencoded::from_str::<Query>(params).unwrap_err();
        assert!(error.to_string().contains("unknown"));
    }

    #[test]
    fn test_deserialize_invalid_ticket_still_needs_proto_validation() {
        let params = "ticket=bad%20ticket";
        let query: WebSocketConnectRequest = serde_urlencoded::from_str(params).unwrap();
        assert_eq!(query.ticket, "bad ticket");
        let error = synctv_proto::validate(&query).expect_err("ticket format must be invalid");
        assert!(error.to_string().contains("ticket"));
    }
}

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
}

mod proto_codec {
    use prost::Message;
    use synctv_api::ProtoCodec;
    use synctv_proto::client::{server_message, HeartbeatAck, ServerMessage};

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
}

mod ticket_types {
    use synctv_proto::client::{CreateWebSocketTicketRequest, CreateWebSocketTicketResponse};

    #[test]
    fn test_create_ticket_request_deserialize() {
        let json = r#"{"roomId": "room_abc"}"#;
        let req: CreateWebSocketTicketRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.room_id.as_str(), "room_abc");
    }

    #[test]
    fn test_ticket_response_serializes() {
        let resp = CreateWebSocketTicketResponse {
            ticket: "ticket_abc123".to_string(),
            room_id: "room_abc".to_string(),
            expires_in_secs: 30,
            usage: "Use in WebSocket URL".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["ticket"], "ticket_abc123");
        assert_eq!(json["roomId"], "room_abc");
        assert_eq!(json["expiresInSecs"], "30");
        assert!(json["usage"].as_str().unwrap().contains("WebSocket"));
    }

    #[test]
    fn test_ticket_response_fields_present() {
        let resp = CreateWebSocketTicketResponse {
            ticket: "t".to_string(),
            room_id: "r".to_string(),
            expires_in_secs: 30,
            usage: "u".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("ticket"));
        assert!(obj.contains_key("roomId"));
        assert!(obj.contains_key("expiresInSecs"));
        assert!(obj.contains_key("usage"));
    }

    #[test]
    fn test_ticket_response_usage_points_to_registered_websocket_route() {
        let room_id = "room_abc";
        let resp = CreateWebSocketTicketResponse {
            ticket: "ticket_abc123".to_string(),
            room_id: room_id.to_string(),
            expires_in_secs: 30,
            usage: format!("Use in WebSocket URL: ws://host/ws/rooms/{room_id}?ticket=xxx"),
        };

        assert!(
            resp.usage.contains("/ws/rooms/"),
            "ticket usage must point at the registered websocket route"
        );
        assert!(
            !resp.usage.contains("/ws/room/"),
            "removed singular websocket route must not appear in ticket usage"
        );
    }
}

mod jwt_auth {
    use std::sync::Arc;
    use synctv_core::models::id::UserId;
    use synctv_core::service::JwtService;
    use synctv_core::service::JwtValidator;
    use synctv_core::service::TokenCredentialBinding;

    // Use a 32+ character secret for testing
    const TEST_SECRET: &str = "this-is-a-test-secret-with-enough-entropy-for-jwt-signing-32chars";

    fn test_jwt_service() -> JwtService {
        JwtService::new(TEST_SECRET).expect("JwtService creation should succeed")
    }

    fn sign_test_refresh_token(svc: &JwtService, user_id: &UserId) -> String {
        svc.sign_refresh_token_with_session(
            user_id,
            0,
            None,
            "websocket-refresh-session",
            &TokenCredentialBinding::Password { version: 0 },
        )
        .unwrap()
    }

    fn test_validator() -> JwtValidator {
        JwtValidator::new(Arc::new(test_jwt_service()))
    }

    #[test]
    fn test_sign_access_token() {
        let svc = test_jwt_service();
        let user_id = UserId::expect_positive(10_000_013);
        let token = svc.sign_access_token(&user_id, 0).unwrap();
        assert!(!token.is_empty());
        // JWT has 3 parts separated by dots
        assert_eq!(token.split('.').count(), 3);
    }

    #[test]
    fn test_sign_refresh_token() {
        let svc = test_jwt_service();
        let user_id = UserId::expect_positive(10_000_014);
        let token = sign_test_refresh_token(&svc, &user_id);
        assert!(!token.is_empty());
    }

    #[test]
    fn test_verify_access_token() {
        let svc = test_jwt_service();
        let user_id = UserId::expect_positive(10_000_015);
        let token = svc.sign_access_token(&user_id, 0).unwrap();

        let claims = svc.verify_access_token(&token).unwrap();
        assert_eq!(claims.sub, user_id.to_string());
        assert!(claims.is_access_token());
        assert!(!claims.is_refresh_token());
    }

    #[test]
    fn test_verify_refresh_token() {
        let svc = test_jwt_service();
        let user_id = UserId::expect_positive(10_000_016);
        let token = sign_test_refresh_token(&svc, &user_id);

        let claims = svc.verify_refresh_token(&token).unwrap();
        assert_eq!(claims.sub, user_id.to_string());
        assert!(claims.is_refresh_token());
        assert!(!claims.is_access_token());
    }

    #[test]
    fn test_verify_with_wrong_secret_fails() {
        let svc1 = test_jwt_service();
        let svc2 = JwtService::new("another-secret-that-is-different-and-long-enough-for-the-test")
            .unwrap();

        let user_id = UserId::expect_positive(10_000_017);
        let token = svc1.sign_access_token(&user_id, 0).unwrap();

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
        let user_id = UserId::expect_positive(10_000_018);
        let token = svc.sign_access_token(&user_id, 0).unwrap();
        let _bearer = format!("Bearer {token}");

        let extracted = validator.validate_and_extract_user_id(&token).unwrap();
        assert_eq!(extracted, user_id);
    }

    #[test]
    fn test_validator_http_bearer_header() {
        let svc = test_jwt_service();
        let validator = test_validator();
        let user_id = UserId::expect_positive(10_000_019);
        let token = svc.sign_access_token(&user_id, 0).unwrap();
        let header = format!("Bearer {token}");

        let claims = validator.validate_authorization_header(&header).unwrap();
        assert_eq!(claims.sub, user_id.to_string());
    }

    #[test]
    fn test_validator_rejects_missing_bearer_prefix() {
        let svc = test_jwt_service();
        let validator = test_validator();
        let user_id = UserId::expect_positive(10_000_020);
        let token = svc.sign_access_token(&user_id, 0).unwrap();

        // Without "Bearer " prefix
        assert!(validator.validate_authorization_header(&token).is_err());
    }

    #[test]
    fn test_access_token_has_jti() {
        let svc = test_jwt_service();
        let user_id = UserId::expect_positive(10_000_021);
        let token = svc.sign_access_token(&user_id, 0).unwrap();
        let claims = svc.verify_access_token(&token).unwrap();
        assert!(!claims.jti.is_empty(), "JWT ID (jti) should be set");
    }

    #[test]
    fn test_unique_jti_per_token() {
        let svc = test_jwt_service();
        let user_id = UserId::expect_positive(10_000_022);
        let token1 = svc.sign_access_token(&user_id, 0).unwrap();
        let token2 = svc.sign_access_token(&user_id, 0).unwrap();
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
        let user_id = UserId::expect_positive(10_000_023);
        let token = svc.sign_access_token(&user_id, 0).unwrap();
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
        let user_id = UserId::expect_positive(10_000_024);
        let token = svc.sign_access_token(&user_id, 0).unwrap();
        let claims = svc.verify_access_token(&token).unwrap();

        let now = chrono::Utc::now().timestamp();
        assert!(claims.exp > now, "exp should be in the future");
    }
}

// SecurityPipeline enforces password version and user status checks.
// Token revocation is stateless: clients discard tokens on logout.
// Full integration tests require a running database instance.

mod rate_limiter {
    use synctv_core::service::RateLimiter;

    #[tokio::test]
    async fn test_in_memory_allows_within_limit() {
        let limiter = RateLimiter::local_only("test:".to_string());
        let result = limiter.check_rate_limit("test_key", 5, 60).await;
        assert!(result.is_ok(), "First request should be allowed");
    }

    #[tokio::test]
    async fn test_in_memory_blocks_after_exceeding_limit() {
        let limiter = RateLimiter::local_only("test:".to_string());
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
        let limiter = RateLimiter::local_only("test:".to_string());

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
        let limiter = RateLimiter::local_only("sync_test:".to_string());
        let result = limiter.check_rate_limit_sync("grpc_key", 10, 60);
        assert!(result.is_ok());
    }

    #[test]
    fn test_sync_rate_limit_blocks_after_exceeding() {
        let limiter = RateLimiter::local_only("sync_test:".to_string());
        for _ in 0..10 {
            let _ = limiter.check_rate_limit_sync("grpc_burst", 10, 60);
        }
        let result = limiter.check_rate_limit_sync("grpc_burst", 10, 60);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_health_check_without_redis() {
        let limiter = RateLimiter::local_only("test:".to_string());
        let result = limiter.health_check().await;
        assert!(result.is_err(), "Should error when Redis not configured");
        assert!(result.unwrap_err().contains("not configured"));
    }
}

mod health_types {
    use synctv_proto::client::{HealthDetails, HealthResponse};

    #[test]
    fn test_health_response_ok_serializes() {
        let resp = HealthResponse {
            status: "ok".to_string(),
            details: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "ok");
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
                webrtc: None,
            }),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "healthy");
        assert_eq!(json["details"]["database"], "healthy");
        assert_eq!(json["details"]["redis"], "healthy");
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
                webrtc: None,
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

mod ws_auth_scenarios {
    use axum::http::StatusCode;
    use synctv_api::AppError;

    #[test]
    fn test_missing_all_auth_methods() {
        let err = AppError::unauthorized(
            "Missing authentication: provide token via Authorization header or ?ticket=",
        );
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert!(err.message().contains("Missing authentication"));
    }

    #[test]
    fn test_invalid_token_error() {
        let err = AppError::unauthorized("Invalid or expired token");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.message(), "Invalid or expired token");
    }

    #[test]
    fn test_token_revoked_error() {
        let err = AppError::unauthorized("Token has been revoked");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_not_member_error() {
        let err = AppError::forbidden("Not a member of this room");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_expired_ticket_error() {
        let err = AppError::unauthorized("Invalid or expired ticket");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.message(), "Invalid or expired ticket");
    }
}

#[cfg(test)]
mod websocket_e2e {
    use futures::{SinkExt, StreamExt};
    use prost::Message;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use tokio_tungstenite::tungstenite;

    use synctv_api::websocket_handler;
    use synctv_api::ProtoCodec;
    use synctv_core::cache::UsernameCache;
    use synctv_core::models::id::{RoomId, UserId};
    use synctv_core::service::JwtService;
    use synctv_core::service::RateLimiter;
    use synctv_core::service::UserServiceRuntimeOptions;
    // Security checks (password version, user status) handled by SecurityPipeline
    use synctv_core::service::{RoomService, UserService};
    use synctv_core::SharedStateProfile;
    use synctv_core_testing::{
        create_test_pool_with_options_and_label, opaque_register_user, redis_connection_manager,
        start_redis_url_with_label, test_redis_key_prefix, RedisContainer, TestContainer,
    };
    use synctv_proto::client::{
        client_message, server_message, ClientMessage, HeartbeatMessage, ServerMessage,
        WebRtcCommand, WebRtcJoin,
    };
    use synctv_realtime::sync::{
        build_connection_manager, ConnectionLimits, ConnectionManager, RealtimeConfig,
        RealtimeManager,
    };

    use sqlx::PgPool;

    const TEST_JWT_SECRET: &str =
        "this-is-a-test-secret-with-enough-entropy-for-jwt-signing-32chars";
    const TEST_DB_MAX_CONNECTIONS: u32 = 5;

    /// Lightweight test infrastructure for E2E tests.
    /// Starts Postgres and Redis containers, runs migrations, and provides connections.
    pub(super) struct TestInfra {
        pool: PgPool,
        redis_url: String,
        redis_key_prefix: String,
        postgres: Option<TestContainer>,
        redis: Option<RedisContainer>,
    }

    impl TestInfra {
        pub(super) async fn new() -> Self {
            let (postgres, pool) = create_test_pool_with_options_and_label(
                "synctv_test",
                "api-ws-rate-limiter",
                TEST_DB_MAX_CONNECTIONS,
                std::time::Duration::from_secs(30),
            )
            .await;
            let (redis, redis_url) = start_redis_url_with_label("api-ws-rate-limiter").await;

            Self {
                pool,
                redis_url,
                redis_key_prefix: test_redis_key_prefix("api-ws-rate-limiter"),
                postgres: Some(postgres),
                redis: Some(redis),
            }
        }

        pub(super) async fn cleanup(mut self) {
            self.pool.close().await;
            if let Some(redis) = self.redis.take() {
                redis.cleanup();
            }
            if let Some(postgres) = self.postgres.take() {
                postgres.cleanup().await;
            }
        }
    }

    /// Returned from `setup_e2e_server` so tests can access all shared components.
    pub(super) struct E2EServer {
        pub(super) addr: String,
        pub(super) jwt_service: JwtService,
        pub(super) room_service: Arc<RoomService>,
        pub(super) user_service: Arc<UserService>,
        pub(super) connection_manager: Arc<ConnectionManager>,
        realtime_manager: Arc<RealtimeManager>,
        ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
        server_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
        server_handle: Option<JoinHandle<()>>,
    }

    impl E2EServer {
        pub(super) async fn shutdown(&mut self) {
            if let Some(shutdown_tx) = self.server_shutdown_tx.take() {
                if let Some(server_handle) = self.server_handle.take() {
                    super::await_server_shutdown(shutdown_tx, server_handle).await;
                }
            }

            self.connection_manager.shutdown().await;
            self.realtime_manager.shutdown().await;
        }
    }

    impl Drop for E2EServer {
        fn drop(&mut self) {
            if let Some(shutdown_tx) = self.server_shutdown_tx.take() {
                let _ = shutdown_tx.send(());
            }
            if let Some(server_handle) = self.server_handle.take() {
                server_handle.abort();
            }
        }
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
        let chat_rate_limiter: Arc<dyn synctv_core::service::RequestRateLimiterService> =
            Arc::new(RateLimiter::local_only("test_chat:".to_string()));
        let content_filter = synctv_core::service::ContentFilter::new();
        let member_repo = synctv_core::repository::RoomMemberRepository::new(pool.clone());
        let room_repo = synctv_core::repository::RoomRepository::new(pool.clone());
        let permission_service =
            synctv_core::service::PermissionService::new(member_repo, room_repo, None, 1000, 300)
                .expect("permission service should build");
        let room_settings_repo = synctv_core::repository::RoomSettingsRepository::new(pool.clone());
        let notification_service = Arc::new(synctv_core::service::NotificationService::default());
        let room_settings_service = synctv_core::service::RoomSettingsService::new(
            room_settings_repo,
            None,
            notification_service,
            None,
            None,
        );
        Arc::new(synctv_core::service::ChatService::new(
            chat_repo,
            synctv_core::service::ChatRuntime {
                rate_limiter: chat_rate_limiter,
                rate_limit_config,
                content_filter,
            },
            synctv_core::service::ChatDependencies {
                permission_service,
                room_settings_service,
                user_service: Arc::new(UserService::new_for_tests(
                    pool,
                    JwtService::new(TEST_JWT_SECRET).expect("JwtService"),
                    username_cache,
                    Arc::new(synctv_core::service::InMemoryTokenBlacklistStore::new(
                        10_000, 3600, 86400,
                    )),
                    synctv_core::cache::KeyBuilder::new("test_chat"),
                    synctv_core::service::BruteForceProtection::in_memory("test_chat:".to_string()),
                )),
                file_storage_service: Arc::new(synctv_core::service::DisabledFileStorageService),
                audit_service: None,
                notification_service: synctv_core::service::NotificationService::default(),
                runtime_settings_store: None,
            },
        ))
    }

    /// Build a minimal `AppState` with real database and Redis for E2E testing.
    async fn setup_e2e_server(infra: &TestInfra) -> E2EServer {
        setup_e2e_server_with_origins_and_chat_rate_limit(
            infra,
            Vec::new(),
            synctv_core::service::RateLimitConfig::default(),
        )
        .await
    }

    async fn setup_e2e_server_with_origins(
        infra: &TestInfra,
        cors_allowed_origins: Vec<String>,
    ) -> E2EServer {
        setup_e2e_server_with_origins_and_chat_rate_limit(
            infra,
            cors_allowed_origins,
            synctv_core::service::RateLimitConfig::default(),
        )
        .await
    }

    async fn setup_e2e_server_with_chat_rate_limit(
        infra: &TestInfra,
        chat_rate_limit_config: synctv_core::service::RateLimitConfig,
    ) -> E2EServer {
        setup_e2e_server_with_origins_and_chat_rate_limit(infra, Vec::new(), chat_rate_limit_config)
            .await
    }

    async fn setup_e2e_server_with_origins_and_chat_rate_limit(
        infra: &TestInfra,
        cors_allowed_origins: Vec<String>,
        chat_rate_limit_config: synctv_core::service::RateLimitConfig,
    ) -> E2EServer {
        setup_e2e_server_with_node_origins_and_chat_rate_limit(
            infra,
            "test_node_1",
            cors_allowed_origins,
            chat_rate_limit_config,
        )
        .await
    }

    /// Build a minimal `AppState` with a custom `node_id`.
    ///
    /// Useful for cross-replica tests: call twice with different node IDs
    /// but the same `TestInfra` to simulate two server replicas.
    async fn setup_e2e_server_with_node(infra: &TestInfra, node_id: &str) -> E2EServer {
        setup_e2e_server_with_node_origins_and_chat_rate_limit(
            infra,
            node_id,
            Vec::new(),
            synctv_core::service::RateLimitConfig::default(),
        )
        .await
    }

    async fn setup_e2e_server_with_node_origins_and_chat_rate_limit(
        infra: &TestInfra,
        node_id: &str,
        cors_allowed_origins: Vec<String>,
        chat_rate_limit_config: synctv_core::service::RateLimitConfig,
    ) -> E2EServer {
        setup_e2e_server_with_node_origins_chat_rate_limit_and_connection_limits(
            infra,
            node_id,
            cors_allowed_origins,
            chat_rate_limit_config,
            ConnectionLimits::default(),
        )
        .await
    }

    pub(super) async fn setup_e2e_server_with_connection_limits(
        infra: &TestInfra,
        connection_limits: ConnectionLimits,
    ) -> E2EServer {
        setup_e2e_server_with_node_origins_chat_rate_limit_and_connection_limits(
            infra,
            "test_node_1",
            Vec::new(),
            synctv_core::service::RateLimitConfig::default(),
            connection_limits,
        )
        .await
    }

    async fn setup_e2e_server_with_node_origins_chat_rate_limit_and_connection_limits(
        infra: &TestInfra,
        node_id: &str,
        cors_allowed_origins: Vec<String>,
        chat_rate_limit_config: synctv_core::service::RateLimitConfig,
        connection_limits: ConnectionLimits,
    ) -> E2EServer {
        let pool = infra.pool.clone();
        let redis_url = infra.redis_url.clone();
        let redis_key_prefix = infra.redis_key_prefix.clone();

        let jwt_service = JwtService::new(TEST_JWT_SECRET).expect("JwtService");
        let redis_client = redis::Client::open(infra.redis_url.as_str()).expect("Redis client");
        let redis_conn = Arc::new(tokio::sync::RwLock::new(
            redis_connection_manager(&redis_client).await,
        ));

        // UsernameCache with Redis L2 backend
        let l2_backend = Arc::new(synctv_core::cache::l2_backend::RedisCacheL2::from_runtime(
            synctv_core::shared_runtime(redis_conn.clone()),
        ));
        let username_cache =
            UsernameCache::new(l2_backend, format!("{redis_key_prefix}un:"), 100, 300);
        let username_cache_for_chat = username_cache.clone();

        let key_builder = synctv_core::cache::KeyBuilder::new(redis_key_prefix.clone());

        // BruteForceProtection with Redis backend
        let brute_force = synctv_core::service::BruteForceProtection::new_with_config(
            redis_key_prefix.clone(),
            Arc::new(synctv_core::service::RedisAttemptTracker::new(
                redis_conn.clone(),
                50_000,
                synctv_core::service::BruteForceConfig::default().attempts_ttl_secs,
            )),
            Arc::new(synctv_core::service::RedisAttemptTracker::new(
                redis_conn.clone(),
                100_000,
                synctv_core::service::BruteForceConfig::default().ip_attempts_ttl_secs,
            )),
            synctv_core::service::BruteForceConfig::default(),
        );

        // Token blacklist with in-memory backend (sufficient for tests)
        let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> = Arc::new(
            synctv_core::service::InMemoryTokenBlacklistStore::new(10_000, 3600, 86400),
        );

        let user_service = UserService::new_with_runtime(
            &pool,
            jwt_service.clone(),
            username_cache,
            token_blacklist,
            key_builder,
            brute_force,
            UserServiceRuntimeOptions {
                password_registration_policy_override: Some(
                    synctv_core::service::RegistrationPolicy {
                        enabled: true,
                        need_review: false,
                    },
                ),
                ..synctv_core::service::UserServiceRuntimeOptions::test_defaults()
            },
        );
        let user_service = Arc::new(user_service);
        let room_service = Arc::new(
            RoomService::new_for_tests(pool.clone(), (*user_service).clone())
                .expect("room service should build"),
        );

        // These helpers are used by cross-replica websocket tests, so cluster
        // mode must be explicitly enabled to start distributed fan-out.
        let redis_client_for_cluster =
            redis::Client::open(redis_url.clone()).expect("Failed to open Redis client");
        let redis_conn_for_cluster = redis_connection_manager(&redis_client_for_cluster).await;
        let shared_runtime: Arc<dyn synctv_core::RedisConnectionRuntime> = Arc::new(
            synctv_core::DirectRedisConnectionRuntime::new(redis_conn_for_cluster.clone()),
        );
        let realtime_config = RealtimeConfig {
            distributed_transport_factory: Some(
                synctv_realtime::sync::build_realtime_message_transport_factory(
                    synctv_core::coordination_runtime_from_client(redis_client_for_cluster),
                ),
            ),
            message_runtime: synctv_realtime::sync::build_room_message_runtime(
                &SharedStateProfile::for_cluster_runtime(
                    Some(shared_runtime),
                    &redis_key_prefix,
                    true,
                ),
            )
            .expect("shared message runtime should initialize"),
            distributed_enabled: true,
            node_id: node_id.to_string(),
            key_prefix: redis_key_prefix.clone(),
            ..Default::default()
        };
        let realtime_manager = Arc::new(
            RealtimeManager::new(realtime_config)
                .await
                .expect("RealtimeManager"),
        );
        let redis_client_for_connections =
            redis::Client::open(redis_url.clone()).expect("Redis client for connection manager");
        let redis_conn_for_connections =
            redis_connection_manager(&redis_client_for_connections).await;
        let connection_profile = SharedStateProfile::for_cluster_runtime(
            Some(synctv_core::direct_runtime(redis_conn_for_connections)),
            &redis_key_prefix,
            true,
        );
        let presence_service = Arc::new(
            synctv_core::service::OnlinePresenceService::from_shared_state_profile(
                &connection_profile,
            )
            .expect("shared presence service should initialize"),
        );
        let connection_manager = Arc::new(
            build_connection_manager(
                connection_limits,
                &connection_profile,
                presence_service.clone(),
                node_id,
            )
            .expect("shared realtime connection runtime should initialize"),
        );
        let connection_manager_ret = connection_manager.clone();

        // Rate limiter (in-memory only for tests)
        let rate_limiter: Arc<dyn synctv_core::service::RequestRateLimiterService> = Arc::new(
            RateLimiter::local_only(format!("{redis_key_prefix}ws-rate-limit:")),
        );

        let provider_instance_manager =
            synctv_core_testing::create_empty_provider_instance_manager();
        let providers_manager = Arc::new(
            synctv_core::service::ProvidersManager::new(provider_instance_manager.clone())
                .expect("providers manager should build"),
        );
        let user_provider_credential_repo =
            Arc::new(synctv_core::repository::UserProviderCredentialRepository::new(pool.clone()));
        let providers = synctv_core::provider::ProviderSet::new_with_ssrf_guard(
            provider_instance_manager.clone(),
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .expect("provider set should build");
        let shared_provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> =
            Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            ));
        let provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService> =
            Arc::new(
                synctv_core::provider::CachedProviderAccessService::new(
                    user_provider_credential_repo.clone(),
                    providers.alist.clone(),
                )
                .with_store(shared_provider_stores.load("credentials")),
            );
        let playback_transport_services =
            Arc::new(synctv_core::provider::PlaybackTransportServices {
                room_service: room_service.clone(),
                permission_service: room_service.permission_service().clone(),
                credential_encryption: None,
                credential_repo: user_provider_credential_repo.clone(),
                provider_access_service: provider_access_service.clone(),
            });
        let playback_provider_deps = synctv_core::service::PlaybackProviderServiceDeps {
            providers: providers.clone(),
            provider_stores: shared_provider_stores.clone(),
            playback_transport_services: playback_transport_services.clone(),
            provider_access_service: provider_access_service.clone(),
        };
        let alist_playback_provider_service = Arc::new(
            synctv_core::service::AlistPlaybackProviderService::new(playback_provider_deps.clone()),
        );
        let bilibili_playback_provider_service =
            Arc::new(synctv_core::service::BilibiliPlaybackProviderService::new(
                playback_provider_deps.clone(),
            ));
        let direct_url_playback_provider_service =
            Arc::new(synctv_core::service::DirectUrlPlaybackProviderService::new(
                playback_provider_deps.clone(),
            ));
        let emby_playback_provider_service = Arc::new(
            synctv_core::service::EmbyPlaybackProviderService::new(playback_provider_deps.clone()),
        );
        let rtmp_playback_provider_service = Arc::new(
            synctv_core::service::RtmpPlaybackProviderService::new(playback_provider_deps.clone()),
        );
        let live_proxy_playback_provider_service = Arc::new(
            synctv_core::service::LiveProxyPlaybackProviderService::new(playback_provider_deps),
        );

        let mut config_inner = synctv_core::Config::default();
        config_inner.server.cors_allowed_origins = cors_allowed_origins;
        let config = Arc::new(config_inner);
        let user_cache = Arc::new(synctv_core::cache::UserCache::local_only(
            128,
            60,
            300,
            "test:user:".to_string(),
        ));
        let security_pipeline = Arc::new(synctv_core::service::SecurityPipeline::new_with_runtime(
            user_service.clone(),
            synctv_core::service::SecurityPipelineRuntime {
                user_cache: Some(user_cache.clone()),
                token_blacklist: user_service.token_blacklist_store(),
                key_builder: user_service.key_builder().clone(),
            },
        ));
        let jwt_validator = Arc::new(synctv_core::service::JwtValidator::new(Arc::new(
            jwt_service.clone(),
        )));
        let public_id_codec = Arc::new(
            synctv_api::PublicIdCodec::from_config(&config.external_ids)
                .expect("test public ID codec should build"),
        );
        let request_executor = Arc::new(synctv_api::RequestExecutor::new(
            config.clone(),
            jwt_validator.clone(),
            security_pipeline.clone(),
            rate_limiter.clone(),
        ));
        let audit_service = {
            let (audit_svc, _audit_handle) = synctv_core::service::AuditService::new(pool.clone());
            Arc::new(audit_svc)
        };
        let provider_common_api = Arc::new(synctv_api::ProviderCommonApiImpl::new_with_runtime(
            provider_instance_manager.clone(),
            user_service.clone(),
            audit_service.clone(),
            synctv_api::ProviderCommonApiRuntime {
                providers_manager: providers_manager.clone(),
                request_executor: request_executor.clone(),
            },
        ));
        let provider_api_runtime = synctv_api::ProviderApiRuntime {
            access_service: provider_access_service.clone(),
            event_service: realtime_manager.clone(),
        };
        let bilibili_api = Arc::new(
            synctv_api::BilibiliApiImpl::new_with_runtime(
                &providers.bilibili,
                user_provider_credential_repo.clone(),
                b"test-secret-key-for-websocket-tests-minimum-32-chars",
                provider_api_runtime.clone(),
            )
            .expect("test Bilibili API should build"),
        );
        let alist_api = Arc::new(synctv_api::AlistApiImpl::new_with_runtime(
            &providers.alist,
            user_provider_credential_repo.clone(),
            provider_api_runtime.clone(),
        ));
        let emby_api = Arc::new(synctv_api::EmbyApiImpl::new_with_runtime(
            &providers.emby,
            user_provider_credential_repo.clone(),
            provider_api_runtime,
        ));

        let ws_ticket_service = Arc::new(synctv_core::service::WsTicketService::local_only(None));

        let chat_service =
            build_test_chat_service(&pool, username_cache_for_chat, chat_rate_limit_config);
        let shared_proxy_signing_key = Arc::new(
            synctv_api::ProxySigningKey::try_derive_from(
                b"test-proxy-signing-key-minimum-32-bytes!!",
            )
            .expect("test proxy signing key should derive"),
        );
        let client_api = Arc::new(synctv_api::ClientApiImpl::new_with_runtime(
            synctv_api::ClientApiConfig {
                user_service: user_service.clone(),
                read_pool: None,
                room_service: room_service.clone(),
                connection_service: connection_manager.clone(),
                config: config.clone(),
                publish_key_service: None,
                jwt_service: jwt_service.clone(),
                live_streaming_infrastructure: None,
                runtime_settings_store: None,
                public_id_codec: public_id_codec.clone(),
                chat_service: Some(chat_service.clone()),
                provider_stores: shared_provider_stores.clone(),
                email_api: None,
                passkey_service: None,
            },
            synctv_api::ClientApiRuntime::new_with_services(synctv_api::ClientApiRuntimeServices {
                realtime_fanout: synctv_api::disabled_realtime_fanout_service(),
                realtime_event_service: realtime_manager.clone(),
                redis_runtime: None,
                builtin_stun_url: None,
                webrtc_status:
                    synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
                provider_access_service: provider_access_service.clone(),
                signing_key: shared_proxy_signing_key.clone(),
                presence_service: presence_service.clone(),
                jwt_validator: jwt_validator.clone(),
                request_executor: request_executor.clone(),
                ws_ticket_service: ws_ticket_service.clone(),
                playback_duration_probe: None,
            }),
        ));

        let router_config = synctv_api::RouterConfig {
            config,
            user_service: user_service.clone(),
            read_pool: None,
            user_cache,
            room_service: room_service.clone(),
            content_filter: synctv_core::service::ContentFilter::new(),
            provider_instance_manager,
            user_provider_credential_repository: user_provider_credential_repo.clone(),
            provider_access_service,
            providers: providers.clone(),
            event_service: realtime_manager.clone(),
            connection_manager,
            presence_service,
            jwt_service: jwt_service.clone(),
            jwt_validator,
            security_pipeline,
            public_id_codec,
            request_executor,
            metrics_access_controller: Arc::new(synctv_api::MetricsAccessController::new()),
            client_api,
            admin_api: None,
            email_api: None,
            notification_api: None,
            oauth2_api: None,
            realtime_fanout_service: synctv_api::disabled_realtime_fanout_service(),
            oauth2_service: None,
            passkey_service: None,
            settings_service: None,
            runtime_settings_store: None,
            email_service: None,
            email_token_service: None,
            publish_key_service: None,
            notification_service: None,
            chat_service: Some(chat_service.clone()),
            audit_service,
            live_streaming_infrastructure: None,
            rate_limiter,
            ws_ticket_service: ws_ticket_service.clone(),
            redis_runtime: None,
            shared_provider_stores,
            playback_transport_services,
            alist_playback_provider_service,
            bilibili_playback_provider_service,
            direct_url_playback_provider_service,
            emby_playback_provider_service,
            rtmp_playback_provider_service,
            live_proxy_playback_provider_service,
            provider_common_api,
            bilibili_api,
            alist_api,
            emby_api,
            shared_proxy_signing_key,
            builtin_stun_url: None,
            webrtc_status: synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
            credential_encryption: None,
            proxy_slice_cache: std::sync::Arc::new(
                synctv_proxy::slice_cache::SliceCache::new(
                    synctv_proxy::slice_cache::SliceCacheConfig::default(),
                )
                .expect("test slice cache should build"),
            ),
            ssrf_guard: synctv_common::ssrf::SsrfGuard::disabled(),
            proxy_http_client: synctv_proxy::build_proxy_http_client(
                synctv_common::ssrf::SsrfGuard::disabled(),
            )
            .expect("proxy HTTP client should build for tests"),
            messaging_rate_limit_config: synctv_core::service::RateLimitConfig::default(),
            heartbeat_schedule: synctv_api::HeartbeatSchedule::fixed(
                std::time::Duration::from_millis(400),
                std::time::Duration::from_millis(100),
            ),
            providers_manager,
            playback_duration_probe: None,
        };

        let state = synctv_api::create_app_state_from_config(router_config)
            .expect("test HTTP app state should build");

        let app = axum::Router::new()
            .route("/ws/rooms/{roomId}", axum::routing::get(websocket_handler))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let addr_str = format!("127.0.0.1:{}", addr.port());
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("server error");
        });

        E2EServer {
            addr: addr_str,
            jwt_service,
            room_service,
            user_service,
            connection_manager: connection_manager_ret,
            realtime_manager,
            ws_ticket_service,
            server_shutdown_tx: Some(shutdown_tx),
            server_handle: Some(server_handle),
        }
    }

    /// Register a test user and return their `UserId` + access token.
    pub(super) async fn register_test_user(
        user_service: &UserService,
        jwt_service: &JwtService,
        username: &str,
    ) -> (UserId, String) {
        let (user, _access, _refresh) = opaque_register_user(
            user_service,
            username,
            Some(format!("{username}@test.com")),
            "TestPassword123!",
        )
        .await
        .expect("register user");
        let token = jwt_service
            .sign_access_token(&user.id, 0)
            .expect("sign token");
        (user.id, token)
    }

    /// Create a test room and add the user as creator/member.
    pub(super) async fn create_test_room(
        room_service: &RoomService,
        user_id: &UserId,
        room_name: &str,
    ) -> String {
        let (room, _member) = room_service
            .create_room(room_name.to_string(), String::new(), *user_id, None, None)
            .await
            .expect("create room");
        synctv_api::PublicIdCodec::plain()
            .encode_room_id(room.id)
            .expect("room id should encode")
    }

    pub(super) fn decode_test_room_id(room_id: &str) -> synctv_core::models::RoomId {
        synctv_api::PublicIdCodec::plain()
            .decode_room_id(room_id)
            .expect("test room id should decode")
    }

    fn encode_test_user_id(user_id: &UserId) -> String {
        synctv_api::PublicIdCodec::plain()
            .encode_user_id(*user_id)
            .expect("test user id should encode")
    }

    fn encode_test_media_id(media_id: &synctv_core::models::MediaId) -> String {
        synctv_api::PublicIdCodec::plain()
            .encode_media_id(*media_id)
            .expect("test media id should encode")
    }

    /// Connect to the WebSocket endpoint using Authorization header.
    async fn ws_connect(
        addr: &str,
        room_id: &str,
        token: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let url = format!("ws://{addr}/ws/rooms/{room_id}?format=protobuf");
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

    async fn ws_connect_with_origin(
        addr: &str,
        room_id: &str,
        token: &str,
        origin: &str,
    ) -> Result<
        (
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            tungstenite::handshake::client::Response,
        ),
        tokio_tungstenite::tungstenite::Error,
    > {
        let url = format!("ws://{addr}/ws/rooms/{room_id}?format=protobuf");
        let request = tungstenite::http::Request::builder()
            .uri(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Origin", origin)
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
        tokio_tungstenite::connect_async(request).await
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
                _ => {}
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

    /// Read the next server message, skipping room member events.
    /// Useful after draining initial messages when you want to read a specific
    /// event type (Chat, `HeartbeatAck`, etc.) without being tripped up by
    /// membership notifications that arrive at unpredictable times.
    async fn recv_server_message_skip_membership(
        ws: &mut (impl StreamExt<Item = Result<tungstenite::Message, tungstenite::Error>> + Unpin),
    ) -> Option<ServerMessage> {
        loop {
            let msg = recv_server_message(ws).await?;
            if resource_room_member_event(&msg).is_none() {
                return Some(msg);
            }
        }
    }

    async fn recv_matching_server_message(
        ws: &mut (impl StreamExt<Item = Result<tungstenite::Message, tungstenite::Error>> + Unpin),
        timeout: std::time::Duration,
        mut predicate: impl FnMut(&ServerMessage) -> bool,
        label: &str,
    ) -> ServerMessage {
        tokio::time::timeout(timeout, async {
            loop {
                let msg = recv_server_message(ws).await.expect("stream ended");
                if predicate(&msg) {
                    return msg;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timeout waiting for {label}"))
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

    async fn publish_realtime_event_confirmed(
        server: &E2EServer,
        event: synctv_realtime::sync::RealtimeEvent,
    ) {
        let event_type = event.event_type();
        server
            .realtime_manager
            .publish_only_confirmed(event, std::time::Duration::from_secs(5))
            .await
            .unwrap_or_else(|error| panic!("cross-replica {event_type} publish failed: {error}"));
    }

    async fn wait_for_distributed_room_subscribers(
        server: &E2EServer,
        room_id: RoomId,
        expected: usize,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let subscribers = server
                .realtime_manager
                .message_hub()
                .get_room_subscribers_replicas_wide(&room_id)
                .await
                .expect("distributed room subscriber lookup should succeed");
            if subscribers.len() >= expected {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "distributed room subscribers did not reach {expected}; got {}",
                subscribers.len()
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    fn observe_playback_state_message(observe_id: &str) -> ClientMessage {
        ClientMessage {
            message: Some(client_message::Message::ObserveResource(
                synctv_proto::client::ObserveResource {
                    observe_id: observe_id.to_string(),
                    delivery_mode: synctv_proto::client::ResourceDeliveryMode::PushSnapshot as i32,
                    resource: Some(
                        synctv_proto::client::observe_resource::Resource::PlaybackState(
                            synctv_proto::client::ObservePlaybackState::default(),
                        ),
                    ),
                },
            )),
        }
    }

    fn observe_chat_events_message(observe_id: &str) -> ClientMessage {
        ClientMessage {
            message: Some(client_message::Message::ObserveResource(
                synctv_proto::client::ObserveResource {
                    observe_id: observe_id.to_string(),
                    delivery_mode: synctv_proto::client::ResourceDeliveryMode::NotifyOnly as i32,
                    resource: Some(
                        synctv_proto::client::observe_resource::Resource::ChatEvents(
                            synctv_proto::client::ObserveChatEvents {
                                after_event_sequence: None,
                                include_message_types: Vec::new(),
                            },
                        ),
                    ),
                },
            )),
        }
    }

    fn observe_room_member_events_message(observe_id: &str) -> ClientMessage {
        ClientMessage {
            message: Some(client_message::Message::ObserveResource(
                synctv_proto::client::ObserveResource {
                    observe_id: observe_id.to_string(),
                    delivery_mode: synctv_proto::client::ResourceDeliveryMode::NotifyOnly as i32,
                    resource: Some(
                        synctv_proto::client::observe_resource::Resource::RoomMemberEvents(
                            synctv_proto::client::ObserveRoomMemberEvents {
                                after_event_sequence: None,
                            },
                        ),
                    ),
                },
            )),
        }
    }

    fn webrtc_command_message(
        command: synctv_proto::client::web_rtc_command::Command,
    ) -> ClientMessage {
        ClientMessage {
            message: Some(client_message::Message::Webrtc(WebRtcCommand {
                command: Some(command),
            })),
        }
    }

    fn resource_chat_event(
        message: &ServerMessage,
    ) -> Option<&synctv_proto::client::ChatMessageEvent> {
        match &message.message {
            Some(server_message::Message::ResourceEvent(changed)) => {
                match changed.payload.as_ref() {
                    Some(synctv_proto::client::resource_event::Payload::ChatEvent(event)) => {
                        Some(event)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn resource_room_member_event(
        message: &ServerMessage,
    ) -> Option<&synctv_proto::client::RoomMemberEvent> {
        match &message.message {
            Some(server_message::Message::ResourceEvent(changed)) => {
                match changed.payload.as_ref() {
                    Some(synctv_proto::client::resource_event::Payload::RoomMemberEvent(event)) => {
                        Some(event)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn resource_webrtc_event(
        message: &ServerMessage,
    ) -> Option<&synctv_proto::client::WebRtcEvent> {
        match &message.message {
            Some(server_message::Message::ResourceEvent(changed)) => {
                match changed.payload.as_ref() {
                    Some(synctv_proto::client::resource_event::Payload::WebrtcEvent(event)) => {
                        Some(event)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn resource_room_member_joined(
        message: &ServerMessage,
    ) -> Option<&synctv_proto::client::RoomMemberEvent> {
        resource_room_member_event(message)
            .filter(|event| event.kind == synctv_proto::client::RoomMemberEventKind::Joined as i32)
    }

    fn resource_room_member_left(
        message: &ServerMessage,
    ) -> Option<&synctv_proto::client::RoomMemberEvent> {
        resource_room_member_event(message)
            .filter(|event| event.kind == synctv_proto::client::RoomMemberEventKind::Left as i32)
    }

    fn resource_playback_state_matches(
        message: &ServerMessage,
        observe_id: &str,
        predicate: impl FnOnce(&synctv_proto::client::PlaybackState) -> bool,
    ) -> bool {
        match &message.message {
            Some(server_message::Message::ResourceEvent(changed))
                if changed.observe_id == observe_id =>
            {
                match changed.payload.as_ref() {
                    Some(synctv_proto::client::resource_event::Payload::PlaybackState(state)) => {
                        predicate(state)
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_handshake_and_initial_user_joined() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "alice_ws").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Test Room WS").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws, &heartbeat).await;
        let ack = recv_matching_server_message(
            &mut ws,
            std::time::Duration::from_secs(5),
            |message| {
                matches!(
                    message.message,
                    Some(server_message::Message::HeartbeatAck(_))
                )
            },
            "heartbeat ack after websocket auth",
        )
        .await;
        assert!(matches!(
            ack.message,
            Some(server_message::Message::HeartbeatAck(_))
        ));

        ws.close(None).await.expect("close");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_heartbeat_ping_pong() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "bob_hb").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Heartbeat Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        drain_until_quiet(&mut ws, 2000).await;

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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_graceful_disconnect() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "carol_dc").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Disconnect Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        drain_until_quiet(&mut ws, 2000).await;

        ws.close(Some(tungstenite::protocol::CloseFrame {
            code: tungstenite::protocol::frame::coding::CloseCode::Normal,
            reason: "bye".into(),
        }))
        .await
        .expect("close");

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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_unauthenticated_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let url = format!("ws://{}/ws/rooms/invalid_room", server.addr);
        let result = tokio_tungstenite::connect_async(&url).await;

        assert!(
            result.is_err(),
            "Connection without auth should be rejected"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_invalid_token_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let url = format!("ws://{}/ws/rooms/invalid_room", server.addr);
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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_non_member_rejected() {
        let infra = TestInfra::new().await;
        let mut server = setup_e2e_server(&infra).await;

        let (owner_id, _owner_token) =
            register_test_user(&server.user_service, &server.jwt_service, "owner_nm").await;
        let room_id = create_test_room(&server.room_service, &owner_id, "Private Room").await;

        let (_outsider_id, outsider_token) =
            register_test_user(&server.user_service, &server.jwt_service, "outsider_nm").await;

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
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio_tungstenite::connect_async(request),
        )
        .await
        .expect("non-member websocket handshake should fail promptly");

        match result {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(
                    response.status(),
                    tungstenite::http::StatusCode::FORBIDDEN,
                    "non-member websocket should be rejected with HTTP 403"
                );
            }
            Err(other) => {
                panic!("expected HTTP 403 rejection for non-member websocket, got: {other:?}");
            }
            Ok((_ws, response)) => {
                panic!(
                    "non-member websocket must not upgrade successfully, got status {}",
                    response.status()
                );
            }
        }

        server.shutdown().await;
        infra.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_cross_origin_rejected_when_not_in_allowlist() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (owner_id, owner_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "owner_origin_deny",
        )
        .await;
        let room_id = create_test_room(&server.room_service, &owner_id, "Origin Deny Room").await;

        let result = ws_connect_with_origin(
            &server.addr,
            &room_id,
            &owner_token,
            "https://evil.example.com",
        )
        .await;

        assert!(
            result.is_err(),
            "cross-origin browser websocket must be rejected when the origin is not allowlisted"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_cross_origin_allowed_when_in_allowlist() {
        let infra = TestInfra::new().await;
        let server =
            setup_e2e_server_with_origins(&infra, vec!["https://app.example.com".to_string()])
                .await;

        let (owner_id, owner_token) = register_test_user(
            &server.user_service,
            &server.jwt_service,
            "owner_origin_allow",
        )
        .await;
        let room_id = create_test_room(&server.room_service, &owner_id, "Origin Allow Room").await;

        let (_ws, response) = ws_connect_with_origin(
            &server.addr,
            &room_id,
            &owner_token,
            "https://app.example.com",
        )
        .await
        .expect("allowlisted cross-origin websocket should succeed");

        assert_eq!(
            response.status(),
            tungstenite::http::StatusCode::SWITCHING_PROTOCOLS
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_multi_client_room_sync() {
        let infra = TestInfra::new().await;
        let mut server = setup_e2e_server(&infra).await;

        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user1_mc").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "Sync Room").await;

        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user2_mc").await;
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
            .await
            .expect("user2 join room");

        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        send_client_message(&mut ws1, &observe_room_member_events_message("ws1-members")).await;
        drain_until_quiet(&mut ws1, 2000).await;

        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;
        drain_until_quiet(&mut ws2, 2000).await;

        let user2_join_event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message(&mut ws1).await.expect("stream ended");
                if let Some(joined) = resource_room_member_joined(&msg) {
                    if joined.member.as_ref().map(|m| m.user_id.as_str())
                        == Some(encode_test_user_id(&user2_id).as_str())
                    {
                        return msg;
                    }
                }
            }
        })
        .await
        .expect("timeout waiting for user2 join event on ws1");

        let joined = resource_room_member_joined(&user2_join_event).unwrap_or_else(|| {
            panic!(
                "Expected joined room member event for user2, got: {:?}",
                user2_join_event.message
            )
        });
        assert_eq!(joined.room_id, room_id);
        let member = joined.member.as_ref().expect("member");
        assert_eq!(member.user_id, encode_test_user_id(&user2_id));

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
        server.shutdown().await;
        infra.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_chat_presentation_fields_broadcast() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "sender_cb").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "Chat Room").await;

        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "receiver_cb").await;
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
            .await
            .expect("user2 join");

        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        send_client_message(&mut ws1, &observe_room_member_events_message("ws1-members")).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );
        send_client_message(&mut ws2, &observe_chat_events_message("chat-events")).await;
        recv_matching_server_message(
            &mut ws2,
            std::time::Duration::from_secs(5),
            |message| {
                matches!(
                    message.message,
                    Some(server_message::Message::ResourceObserved(_))
                )
            },
            "chat_events observation acknowledgement",
        )
        .await;

        // Let subscriptions settle
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let chat_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "Hello from user1!".to_string(),
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: String::new(),
                    attachments: Vec::new(),
                    reply_to_message_id: String::new(),
                    metadata: None,
                    mentions: Vec::new(),
                },
            )),
        };
        send_client_message(&mut ws1, &chat_msg).await;

        // user2 should receive the chat event through explicit chat_events observation.
        let received = recv_matching_server_message(
            &mut ws2,
            std::time::Duration::from_secs(10),
            |message| {
                resource_chat_event(message).is_some_and(|event| {
                    event.message.as_ref().is_some_and(|chat| {
                        chat.content == "Hello from user1!"
                            && chat.room_id == room_id
                            && chat.user_id == encode_test_user_id(&user1_id)
                    })
                })
            },
            "chat_events resource update on ws2",
        )
        .await;

        assert!(resource_chat_event(&received).is_some());

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_multiple_heartbeats() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "hb_multi").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Multi HB Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        drain_until_quiet(&mut ws, 2000).await;

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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_user_left_on_disconnect() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "stayer_ul").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "Leave Room").await;

        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "leaver_ul").await;
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
            .await
            .expect("join");

        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        send_client_message(&mut ws1, &observe_room_member_events_message("ws1-members")).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );

        ws2.close(None).await.expect("close ws2");

        // Read messages, skipping any stale UserJoined that may still be in flight
        let left_event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message(&mut ws1).await.expect("stream ended");
                if resource_room_member_left(&msg).is_some() {
                    return msg;
                }
            }
        })
        .await
        .expect("timeout waiting for UserLeft event");

        let left = resource_room_member_left(&left_event).unwrap_or_else(|| {
            panic!(
                "Expected left room member event, got: {:?}",
                left_event.message
            )
        });
        assert_eq!(left.room_id, room_id);
        assert_eq!(left.user_id, encode_test_user_id(&user2_id));

        ws1.close(None).await.expect("close ws1");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_room_isolation() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user1_iso").await;
        let room_a_id = create_test_room(&server.room_service, &user1_id, "Room A").await;

        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user2_iso").await;
        let room_b_id = create_test_room(&server.room_service, &user2_id, "Room B").await;

        let mut ws1 = ws_connect(&server.addr, &room_a_id, &user1_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_b_id, &user2_token).await;

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

        let chat_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "Room A only".to_string(),
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: String::new(),
                    attachments: Vec::new(),
                    reply_to_message_id: String::new(),
                    metadata: None,
                    mentions: Vec::new(),
                },
            )),
        };
        send_client_message(&mut ws1, &chat_msg).await;

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
                if resource_chat_event(&msg).is_some_and(|event| {
                    event
                        .message
                        .as_ref()
                        .is_some_and(|chat| chat.content == "Room A only")
                }) {
                    panic!("Room isolation violated: user2 in Room B received chat from Room A");
                }
                // Other message types (like a heartbeat timeout) are acceptable
            }
        }

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_reconnect_after_disconnect() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "reconn_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Reconnect Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        ws.close(None).await.expect("close");
        // Small delay to let the server process the disconnect
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let mut ws2 = ws_connect(&server.addr, &room_id, &token).await;
        drain_until_quiet(&mut ws2, 500).await;

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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_forced_disconnect_via_kick() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (owner_id, _owner_token) =
            register_test_user(&server.user_service, &server.jwt_service, "owner_kick").await;
        let room_id = create_test_room(&server.room_service, &owner_id, "Kick Room").await;

        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "victim_kick").await;
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
            .await
            .expect("join");

        // user2 connects
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;
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
                    Some(Ok(_)) => {}
                }
            }
        })
        .await;

        assert!(
            result.is_ok(),
            "Connection should be terminated after forced disconnect"
        );
    }

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
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
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
                    Some(Ok(_)) => {}
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
                if resource_room_member_left(&msg).is_some() {
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
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
            .await
            .expect("join");

        let mut ws1 = ws_connect(&server.addr, &room_id, &owner_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;
        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );

        let event = synctv_realtime::sync::RealtimeEvent::KickUserFromRoom {
            event_id: synctv_common::snanoid!(16),
            room_id: rid,
            user_id: user2_id,
            reason: "admin kick".to_string(),
            timestamp: chrono::Utc::now(),
        };
        server
            .realtime_manager
            .admin_event_tx()
            .send(event)
            .expect("admin event send");

        let disconnect_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws2.next().await {
                    Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None => return true,
                    Some(Ok(_)) => {}
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
                if resource_room_member_left(&msg).is_some() {
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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_cross_replica_chat_via_redis() {
        let infra = TestInfra::new().await;

        let server1 = setup_e2e_server_with_node(&infra, "replica_1").await;
        let server2 = setup_e2e_server_with_node(&infra, "replica_2").await;

        let (user1_id, user1_token) =
            register_test_user(&server1.user_service, &server1.jwt_service, "xrep_u1").await;
        let room_id =
            create_test_room(&server1.room_service, &user1_id, "Cross Replica Room").await;

        let (user2_id, user2_token) =
            register_test_user(&server1.user_service, &server1.jwt_service, "xrep_u2").await;
        let rid = decode_test_room_id(&room_id);
        server1
            .room_service
            .join_room(rid, user2_id, None)
            .await
            .expect("user2 join");

        // user1 connects to replica_1
        let mut ws1 = ws_connect(&server1.addr, &room_id, &user1_token).await;
        // user2 connects to replica_2
        let mut ws2 = ws_connect(&server2.addr, &room_id, &user2_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );
        send_client_message(&mut ws2, &observe_chat_events_message("chat-events")).await;
        recv_matching_server_message(
            &mut ws2,
            std::time::Duration::from_secs(5),
            |message| {
                matches!(
                    message.message,
                    Some(server_message::Message::ResourceObserved(_))
                )
            },
            "chat_events observation acknowledgement",
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // user1 sends a chat on replica_1
        let chat_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "Cross-replica hello!".to_string(),
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: String::new(),
                    attachments: Vec::new(),
                    reply_to_message_id: String::new(),
                    metadata: None,
                    mentions: Vec::new(),
                },
            )),
        };
        send_client_message(&mut ws1, &chat_msg).await;

        // user2 on replica_2 should receive it via Redis Pub/Sub after observing chat_events.
        let received = recv_matching_server_message(
            &mut ws2,
            std::time::Duration::from_secs(30),
            |message| {
                resource_chat_event(message).is_some_and(|event| {
                    event.message.as_ref().is_some_and(|chat| {
                        chat.content == "Cross-replica hello!"
                            && chat.room_id == room_id
                            && chat.user_id == encode_test_user_id(&user1_id)
                    })
                })
            },
            "cross-replica chat_events resource update",
        )
        .await;

        assert!(resource_chat_event(&received).is_some());

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

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
        let rid = decode_test_room_id(&room_id);
        server1
            .room_service
            .join_room(rid, user2_id, None)
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
                if resource_room_member_left(&msg).is_some() {
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
        let rid = decode_test_room_id(&room_id);
        server1
            .room_service
            .join_room(rid, user2_id, None)
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
                if let Some(joined) = resource_room_member_joined(&msg) {
                    if joined.member.as_ref().map(|m| m.user_id.as_str())
                        == Some(encode_test_user_id(&user2_id).as_str())
                    {
                        return msg;
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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_cross_replica_room_realtime_message_matrix_via_realtime_events() {
        let infra = TestInfra::new().await;

        let server1 = setup_e2e_server_with_node(&infra, "matrix_replica_1").await;
        let server2 = setup_e2e_server_with_node(&infra, "matrix_replica_2").await;

        let (owner_id, owner_token) =
            register_test_user(&server1.user_service, &server1.jwt_service, "matrix_owner").await;
        let room_id = create_test_room(&server1.room_service, &owner_id, "Matrix Room").await;

        let (member_id, member_token) =
            register_test_user(&server1.user_service, &server1.jwt_service, "matrix_member").await;
        let room = decode_test_room_id(&room_id);
        server1
            .room_service
            .join_room(room, member_id, None)
            .await
            .expect("member join");

        let mut ws_owner = ws_connect(&server1.addr, &room_id, &owner_token).await;
        let mut ws_member = ws_connect(&server2.addr, &room_id, &member_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws_owner, 1500),
            drain_until_quiet(&mut ws_member, 1500),
        );
        wait_for_distributed_room_subscribers(&server2, room, 2).await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        send_client_message(&mut ws_member, &observe_chat_events_message("chat_events")).await;
        let _ = recv_matching_server_message(
            &mut ws_member,
            std::time::Duration::from_secs(10),
            |message| {
                matches!(
                    &message.message,
                    Some(server_message::Message::ResourceObserved(observed))
                        if observed.observe_id == "chat_events"
                )
            },
            "chat_events observed",
        )
        .await;
        let public_owner_id = encode_test_user_id(&owner_id);
        send_client_message(
            &mut ws_owner,
            &ClientMessage {
                message: Some(client_message::Message::Chat(
                    synctv_proto::client::ChatMessageSend {
                        content: "cross-replica chat".to_string(),
                        display_position: "top".to_string(),
                        display_color: "#ff6600".to_string(),
                        client_message_id: String::new(),
                        attachments: Vec::new(),
                        reply_to_message_id: String::new(),
                        metadata: None,
                        mentions: Vec::new(),
                    },
                )),
            },
        )
        .await;
        let chat_msg = recv_matching_server_message(
            &mut ws_member,
            std::time::Duration::from_secs(10),
            |message| {
                resource_chat_event(message).is_some_and(|event| {
                    event.message.as_ref().is_some_and(|chat| {
                        chat.content == "cross-replica chat"
                            && chat.user_id == public_owner_id
                            && chat.display_position == "top"
                            && chat.display_color == "#ff6600"
                    })
                })
            },
            "cross-replica chat",
        )
        .await;
        assert!(
            resource_chat_event(&chat_msg).is_some(),
            "chat event should arrive as a resource event payload"
        );

        publish_realtime_event_confirmed(
            &server1,
            synctv_realtime::sync::RealtimeEvent::RoomDeleted {
                event_id: synctv_common::snanoid!(16),
                room_id: room,
                deleted_by: owner_id,
                timestamp: chrono::Utc::now(),
            },
        )
        .await;
        let room_deleted_msg = recv_matching_server_message(
            &mut ws_member,
            std::time::Duration::from_secs(10),
            |message| {
                matches!(
                    &message.message,
                    Some(server_message::Message::Error(error))
                        if error.message.contains("deleted")
                )
            },
            "cross-replica room deleted error",
        )
        .await;
        assert!(
            matches!(
                room_deleted_msg.message,
                Some(server_message::Message::Error(_))
            ),
            "RoomDeleted event should be forwarded as terminal error"
        );

        ws_owner.close(None).await.expect("close owner");
        ws_member.close(None).await.expect("close member");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_cross_replica_playback_operation_matrix_via_realtime_events() {
        let infra = TestInfra::new().await;

        let server1 = setup_e2e_server_with_node(&infra, "playback_matrix_replica_1").await;
        let server2 = setup_e2e_server_with_node(&infra, "playback_matrix_replica_2").await;

        let (owner_id, owner_token) = register_test_user(
            &server1.user_service,
            &server1.jwt_service,
            "playback_matrix_owner",
        )
        .await;
        let room_id =
            create_test_room(&server1.room_service, &owner_id, "Playback Matrix Room").await;

        let (member_id, member_token) = register_test_user(
            &server1.user_service,
            &server1.jwt_service,
            "playback_matrix_member",
        )
        .await;
        let room = decode_test_room_id(&room_id);
        server1
            .room_service
            .join_room(room, member_id, None)
            .await
            .expect("member join");

        let mut ws_owner = ws_connect(&server1.addr, &room_id, &owner_token).await;
        let mut ws_member = ws_connect(&server2.addr, &room_id, &member_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws_owner, 1500),
            drain_until_quiet(&mut ws_member, 1500),
        );
        wait_for_distributed_room_subscribers(&server2, room, 2).await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        send_client_message(
            &mut ws_member,
            &observe_playback_state_message("playback_state"),
        )
        .await;
        let _ = recv_matching_server_message(
            &mut ws_member,
            std::time::Duration::from_secs(10),
            |message| {
                matches!(
                    &message.message,
                    Some(server_message::Message::ResourceObserved(observed))
                        if observed.observe_id == "playback_state"
                )
            },
            "playback_state observed",
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let media_id = synctv_core::models::MediaId::expect_positive(1);
        let public_media_id = encode_test_media_id(&media_id);

        let mut started_state = synctv_core::models::RoomPlaybackState::new(room);
        started_state.playing_media_id = Some(media_id);
        started_state.is_playing = true;
        started_state.version = 1;
        publish_realtime_event_confirmed(
            &server1,
            synctv_realtime::sync::RealtimeEvent::PlaybackStateChanged {
                event_id: synctv_common::snanoid!(16),
                room_id: room,
                user_id: owner_id,
                username: "playback_matrix_owner".to_string(),
                state: started_state,
                source_changed: false,
                timestamp: chrono::Utc::now(),
            },
        )
        .await;
        let started_msg = recv_matching_server_message(
            &mut ws_member,
            std::time::Duration::from_secs(10),
            |message| {
                resource_playback_state_matches(message, "playback_state", |state| {
                    state.is_playing
                        && state.playing_media_id == public_media_id
                        && state.position >= 0.0
                        && state.position.is_finite()
                        && (state.speed - 1.0).abs() < f64::EPSILON
                        && state.version == 1
                })
            },
            "cross-replica playback start",
        )
        .await;
        assert!(
            matches!(
                started_msg.message,
                Some(server_message::Message::ResourceEvent(_))
            ),
            "Playback start should be forwarded through playback_state ResourceEvent"
        );

        let mut paused_state = synctv_core::models::RoomPlaybackState::new(room);
        paused_state.playing_media_id = Some(media_id);
        paused_state.position = 17.5;
        paused_state.is_playing = false;
        paused_state.version = 2;
        publish_realtime_event_confirmed(
            &server1,
            synctv_realtime::sync::RealtimeEvent::PlaybackStateChanged {
                event_id: synctv_common::snanoid!(16),
                room_id: room,
                user_id: owner_id,
                username: "playback_matrix_owner".to_string(),
                state: paused_state,
                source_changed: false,
                timestamp: chrono::Utc::now(),
            },
        )
        .await;
        let paused_msg = recv_matching_server_message(
            &mut ws_member,
            std::time::Duration::from_secs(10),
            |message| {
                resource_playback_state_matches(message, "playback_state", |state| {
                    !state.is_playing
                        && state.playing_media_id == public_media_id
                        && state.position >= 17.5
                        && state.position.is_finite()
                        && (state.speed - 1.0).abs() < f64::EPSILON
                        && state.version == 2
                })
            },
            "cross-replica playback pause and seek",
        )
        .await;
        assert!(
            matches!(
                paused_msg.message,
                Some(server_message::Message::ResourceEvent(_))
            ),
            "Playback pause/seek should be forwarded through playback_state ResourceEvent"
        );

        let mut resumed_state = synctv_core::models::RoomPlaybackState::new(room);
        resumed_state.playing_media_id = Some(media_id);
        resumed_state.position = 17.5;
        resumed_state.speed = 1.5;
        resumed_state.is_playing = true;
        resumed_state.version = 3;
        publish_realtime_event_confirmed(
            &server1,
            synctv_realtime::sync::RealtimeEvent::PlaybackStateChanged {
                event_id: synctv_common::snanoid!(16),
                room_id: room,
                user_id: owner_id,
                username: "playback_matrix_owner".to_string(),
                state: resumed_state,
                source_changed: false,
                timestamp: chrono::Utc::now(),
            },
        )
        .await;
        let resumed_msg = recv_matching_server_message(
            &mut ws_member,
            std::time::Duration::from_secs(10),
            |message| {
                resource_playback_state_matches(message, "playback_state", |state| {
                    state.is_playing
                        && state.playing_media_id == public_media_id
                        && state.position >= 17.5
                        && state.position.is_finite()
                        && (state.speed - 1.5).abs() < f64::EPSILON
                        && state.version == 3
                })
            },
            "cross-replica playback resume with speed",
        )
        .await;
        assert!(
            matches!(
                resumed_msg.message,
                Some(server_message::Message::ResourceEvent(_))
            ),
            "Playback resume/speed should be forwarded through playback_state ResourceEvent"
        );

        let mut stopped_state = synctv_core::models::RoomPlaybackState::new(room);
        stopped_state.version = 4;
        publish_realtime_event_confirmed(
            &server1,
            synctv_realtime::sync::RealtimeEvent::PlaybackStateChanged {
                event_id: synctv_common::snanoid!(16),
                room_id: decode_test_room_id(&room_id),
                user_id: owner_id,
                username: "playback_matrix_owner".to_string(),
                state: stopped_state,
                source_changed: false,
                timestamp: chrono::Utc::now(),
            },
        )
        .await;
        let stopped_msg = recv_matching_server_message(
            &mut ws_member,
            std::time::Duration::from_secs(10),
            |message| {
                resource_playback_state_matches(message, "playback_state", |state| {
                    !state.is_playing
                        && state.playing_media_id.is_empty()
                        && (state.position - 0.0).abs() < f64::EPSILON
                        && (state.speed - 1.0).abs() < f64::EPSILON
                        && state.version == 4
                })
            },
            "cross-replica playback stop",
        )
        .await;
        assert!(
            matches!(
                stopped_msg.message,
                Some(server_message::Message::ResourceEvent(_))
            ),
            "Playback stop should be forwarded through playback_state ResourceEvent"
        );

        ws_owner.close(None).await.expect("close owner");
        ws_member.close(None).await.expect("close member");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_rate_limiter_blocks_excess_chat() {
        let infra = TestInfra::new().await;
        let mut server = setup_e2e_server_with_chat_rate_limit(
            &infra,
            synctv_core::service::RateLimitConfig {
                chat_per_second: 2,
                window_seconds: 60,
            },
        )
        .await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "ratelimit_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Rate Limit Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        drain_until_quiet(&mut ws, 2000).await;

        // We send 15 to exceed the limit
        for i in 0..15 {
            let chat_msg = ClientMessage {
                message: Some(client_message::Message::Chat(
                    synctv_proto::client::ChatMessageSend {
                        content: format!("Spam message {i}"),
                        display_position: String::new(),
                        display_color: String::new(),
                        client_message_id: String::new(),
                        attachments: Vec::new(),
                        reply_to_message_id: String::new(),
                        metadata: None,
                        mentions: Vec::new(),
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
                    Some(server_message::Message::ResourceEvent(event))
                        if matches!(
                            event.payload.as_ref(),
                            Some(synctv_proto::client::resource_event::Payload::ChatEvent(_))
                        ) =>
                    {
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
        server.shutdown().await;
        infra.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_content_filter_strips_xss() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "xss_sender").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "XSS Room").await;

        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "xss_receiver").await;
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
            .await
            .expect("user2 join");

        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );
        send_client_message(&mut ws2, &observe_chat_events_message("chat-events")).await;
        recv_matching_server_message(
            &mut ws2,
            std::time::Duration::from_secs(5),
            |message| {
                matches!(
                    message.message,
                    Some(server_message::Message::ResourceObserved(_))
                )
            },
            "chat_events observation acknowledgement",
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // user1 sends a message with XSS payload
        let xss_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "<script>alert('xss')</script>Hello safe world".to_string(),
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: String::new(),
                    attachments: Vec::new(),
                    reply_to_message_id: String::new(),
                    metadata: None,
                    mentions: Vec::new(),
                },
            )),
        };
        send_client_message(&mut ws1, &xss_msg).await;

        // user2 should receive the sanitized chat through explicit chat_events observation.
        let received = recv_matching_server_message(
            &mut ws2,
            std::time::Duration::from_secs(10),
            |message| {
                resource_chat_event(message).is_some_and(|event| {
                    event.message.as_ref().is_some_and(|chat| {
                        !chat.content.contains("<script>")
                            && chat.content.contains("Hello safe world")
                    })
                })
            },
            "sanitized chat_events resource update",
        )
        .await;

        let chat = resource_chat_event(&received)
            .and_then(|event| event.message.as_ref())
            .expect("chat event should include message");
        assert!(!chat.content.contains("<script>"));
        assert!(chat.content.contains("Hello safe world"));

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

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
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
            .await
            .expect("join");

        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        send_client_message(&mut ws1, &observe_room_member_events_message("ws1-members")).await;
        drain_until_quiet(&mut ws1, 2000).await;

        let ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        drain_until_quiet(&mut ws1, 2000).await;

        // Abruptly drop user2's WebSocket (simulate TCP disconnect without Close frame)
        drop(ws2);

        // user1 should receive a left member event for user2.
        let left_event = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let msg = recv_server_message(&mut ws1).await.expect("stream ended");
                if resource_room_member_left(&msg).is_some() {
                    return msg;
                }
            }
        })
        .await
        .expect("timeout waiting for left member event after TCP drop");

        let left = resource_room_member_left(&left_event).unwrap_or_else(|| {
            panic!(
                "Expected left room member event after TCP drop, got: {:?}",
                left_event.message
            )
        });
        assert_eq!(left.room_id, room_id);
        assert_eq!(left.user_id, encode_test_user_id(&user2_id));

        ws1.close(None).await.expect("close ws1");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_connection_manager_state_consistency() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "cycle_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Cycle Room").await;

        for cycle in 0..3 {
            let mut ws = ws_connect(&server.addr, &room_id, &token).await;

            drain_until_quiet(&mut ws, 2000).await;

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

            ws.close(None)
                .await
                .unwrap_or_else(|_| panic!("close in cycle {cycle}"));

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // After all cycles, the connection manager should have 0 connections for this user
        let user_conn_count = server.connection_manager.user_connection_count(&user_id);
        assert_eq!(
            user_conn_count, 0,
            "After all disconnect cycles, user should have 0 connections, got {user_conn_count}"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_empty_chat_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "empty_chat_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Empty Chat Room").await;

        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        drain_until_quiet(&mut ws, 2000).await;

        let empty_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: String::new(),
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: String::new(),
                    attachments: Vec::new(),
                    reply_to_message_id: String::new(),
                    metadata: None,
                    mentions: Vec::new(),
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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_chat_presentation_fields_broadcast_via_event() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "chat_sender").await;
        let room_id = create_test_room(&server.room_service, &user1_id, "Chat Room").await;

        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "chat_receiver").await;
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
            .await
            .expect("user2 join");

        let mut ws1 = ws_connect(&server.addr, &room_id, &user1_token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &user2_token).await;

        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );
        send_client_message(&mut ws2, &observe_chat_events_message("chat-events")).await;
        recv_matching_server_message(
            &mut ws2,
            std::time::Duration::from_secs(5),
            |message| {
                matches!(
                    message.message,
                    Some(server_message::Message::ResourceObserved(_))
                )
            },
            "chat_events observation acknowledgement",
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let chat_msg = ClientMessage {
            message: Some(client_message::Message::Chat(
                synctv_proto::client::ChatMessageSend {
                    content: "LOL".to_string(),
                    display_position: "top".to_string(),
                    display_color: "#FF0000".to_string(),
                    client_message_id: String::new(),
                    attachments: Vec::new(),
                    reply_to_message_id: String::new(),
                    metadata: None,
                    mentions: Vec::new(),
                },
            )),
        };
        send_client_message(&mut ws1, &chat_msg).await;

        let received = recv_matching_server_message(
            &mut ws2,
            std::time::Duration::from_secs(5),
            |message| {
                resource_chat_event(message).is_some_and(|event| {
                    event.message.as_ref().is_some_and(|chat| {
                        chat.content == "LOL"
                            && chat.room_id == room_id
                            && chat.user_id == encode_test_user_id(&user1_id)
                            && chat.display_position == "top"
                            && chat.display_color == "#FF0000"
                    })
                })
            },
            "chat presentation chat_events resource update",
        )
        .await;

        assert!(resource_chat_event(&received).is_some());

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_concurrent_connections_same_user() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "multi_conn_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Multi Conn Room").await;

        let mut ws1 = ws_connect(&server.addr, &room_id, &token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &token).await;

        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws1, &heartbeat).await;

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
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
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
                if let Some(joined) = resource_room_member_joined(&msg) {
                    if joined.member.as_ref().map(|m| m.user_id.as_str())
                        == Some(encode_test_user_id(&user2_id).as_str())
                    {
                        return msg;
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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_same_user_one_of_multiple_connections_disconnect_does_not_emit_user_left() {
        let infra = TestInfra::new().await;
        let mut server = setup_e2e_server(&infra).await;

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
        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
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
                if resource_room_member_left(&msg).is_some() {
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
        server.shutdown().await;
        infra.cleanup().await;
    }

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
        let room = decode_test_room_id(&room_id);

        let mut ws1 = ws_connect(&server.addr, &room_id, &token).await;
        let mut ws2 = ws_connect(&server.addr, &room_id, &token).await;

        tokio::join!(
            drain_until_quiet(&mut ws1, 1500),
            drain_until_quiet(&mut ws2, 1500),
        );
        let rtc_join = webrtc_command_message(
            synctv_proto::client::web_rtc_command::Command::Join(WebRtcJoin {
                user_id: String::new(),
                conn_id: String::new(),
                username: String::new(),
            }),
        );
        send_client_message(&mut ws1, &rtc_join).await;
        drain_until_quiet(&mut ws1, 500).await;

        send_client_message(&mut ws2, &rtc_join).await;

        let join_event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = recv_server_message(&mut ws1).await.expect("stream ended");
                if resource_webrtc_event(&msg).is_some_and(|event| {
                    matches!(
                        event.event,
                        Some(synctv_proto::client::web_rtc_event::Event::Join(_))
                    )
                }) {
                    return msg;
                }
            }
        })
        .await
        .expect("timeout waiting for WebRTC join");

        let joined_conn_id =
            match resource_webrtc_event(&join_event).and_then(|event| event.event.as_ref()) {
                Some(synctv_proto::client::web_rtc_event::Event::Join(joined)) => {
                    joined.conn_id.clone()
                }
                other => panic!("Expected WebRTC join resource event, got: {other:?}"),
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
            2,
            "both same-user connections can independently join WebRTC"
        );
        assert!(
            rtc_joined_connections
                .iter()
                .any(|connection| connection.connection_id == joined_conn_id),
            "the reported WebRTC join connection must be marked rtc_joined"
        );

        ws1.close(None).await.expect("close ws1");
        ws2.close(None).await.expect("close ws2");
    }

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
        let room = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(room, peer_user_id, None)
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
                    Some(Ok(_)) => {}
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
                        Some(Ok(_)) => {}
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
        let offer = webrtc_command_message(synctv_proto::client::web_rtc_command::Command::Offer(
            synctv_proto::client::WebRtcOffer {
                to: encode_test_user_id(&peer_user_id),
                from: String::new(),
                data: "{\"type\":\"offer\",\"sdp\":\"test-sdp\"}".to_string(),
            },
        ));
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
                    error.message.contains("public_actor_id:conn_id"),
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
                                Some(server_message::Message::ResourceEvent(event))
                                    if matches!(
                                        event.payload.as_ref(),
                                        Some(synctv_proto::client::resource_event::Payload::WebrtcEvent(
                                            webrtc
                                        )) if matches!(
                                            webrtc.event.as_ref(),
                                            Some(synctv_proto::client::web_rtc_event::Event::Offer(_))
                                        )
                                    )
                            ) =>
                        {
                            return true;
                        }
                        Some(_) => {}
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
        let room = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(room, peer_user_id, None)
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

        let offer = webrtc_command_message(synctv_proto::client::web_rtc_command::Command::Offer(
            synctv_proto::client::WebRtcOffer {
                to: format!("{}:{}", encode_test_user_id(&peer_user_id), peer_conn_id),
                from: String::new(),
                data: "{\"type\":\"offer\",\"sdp\":\"test-sdp\"}".to_string(),
            },
        ));
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
                                Some(server_message::Message::ResourceEvent(event))
                                    if matches!(
                                        event.payload.as_ref(),
                                        Some(synctv_proto::client::resource_event::Payload::WebrtcEvent(
                                            webrtc
                                        )) if matches!(
                                            webrtc.event.as_ref(),
                                            Some(synctv_proto::client::web_rtc_event::Event::Offer(_))
                                        )
                                    )
                            ) =>
                        {
                            return true;
                        }
                        Some(_) => {}
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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_invalid_ticket_rejected() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let url = format!(
            "ws://{}/ws/rooms/invalid_room?ticket=totally_invalid_ticket_xyz123",
            server.addr
        );
        let result = tokio_tungstenite::connect_async(&url).await;

        assert!(
            result.is_err(),
            "Connection with invalid ticket should be rejected by the server"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_expired_ticket_rejected() {
        // memory-backed store.
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let url = format!(
            "ws://{}/ws/rooms/invalid_room?ticket=expired_or_invalid_ticket_aabbcc",
            server.addr
        );
        let result = tokio_tungstenite::connect_async(&url).await;

        assert!(
            result.is_err(),
            "Connection with expired/invalid ticket should be rejected"
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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_ticket_rejected_by_membership_does_not_consume_ticket() {
        let infra = TestInfra::new().await;
        let server = setup_e2e_server(&infra).await;

        let (owner_id, _owner_token) =
            register_test_user(&server.user_service, &server.jwt_service, "ticket_owner").await;
        let (outsider_id, _outsider_token) =
            register_test_user(&server.user_service, &server.jwt_service, "ticket_outsider").await;
        let room_id = create_test_room(&server.room_service, &owner_id, "Ticket Membership").await;

        let ticket = server
            .ws_ticket_service
            .create_ticket(&outsider_id, &decode_test_room_id(&room_id), 0)
            .await
            .expect("create websocket ticket");

        let url = format!("ws://{}/ws/rooms/{}?ticket={ticket}", server.addr, room_id);
        let result = tokio_tungstenite::connect_async(&url).await;

        match result {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(
                    response.status(),
                    tungstenite::http::StatusCode::FORBIDDEN,
                    "non-member ticket must be rejected before websocket upgrade"
                );
            }
            other => panic!("Expected HTTP 403 for non-member ticket, got: {other:?}"),
        }

        let validated = server
            .ws_ticket_service
            .validate_and_consume(&ticket, &decode_test_room_id(&room_id))
            .await
            .expect("membership rejection must not consume the ticket");
        assert_eq!(validated.user_id().expect("user ticket"), outsider_id);
    }

    // We test the Authorization header path: a raw JWT passed via Bearer
    // header succeeds, proving the WebSocket upgrade + auth pipeline is
    // working. The ticket path is tested at the unit level in the ticket module.

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_valid_token_query_auth_succeeds() {
        let infra = TestInfra::new().await;
        let mut server = setup_e2e_server(&infra).await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "valid_auth_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Auth Test Room").await;

        // ws_connect already asserts HTTP 101 Switching Protocols
        let mut ws = ws_connect(&server.addr, &room_id, &token).await;

        let heartbeat = ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };
        send_client_message(&mut ws, &heartbeat).await;
        recv_matching_server_message(
            &mut ws,
            std::time::Duration::from_secs(5),
            |message| {
                matches!(
                    message.message,
                    Some(server_message::Message::HeartbeatAck(_))
                )
            },
            "heartbeat ack after valid auth",
        )
        .await;

        ws.close(None).await.expect("close");
        server.shutdown().await;
        infra.cleanup().await;
    }

    // Sending invalid message format returns an error response

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

        drain_until_quiet(&mut ws, 2000).await;

        let garbage = vec![0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA, 0x01, 0x02];
        ws.send(tungstenite::Message::Binary(garbage.into()))
            .await
            .expect("send garbage bytes");

        // The server should either:
        // a) Send an Error ServerMessage back, OR
        // b) Close the connection
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
                    _ => {}
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
}

#[cfg(test)]
mod websocket_connection_limit_timing {
    use super::wait_for_condition;
    use tokio_tungstenite::tungstenite;

    use synctv_realtime::sync::ConnectionLimits;

    use super::websocket_e2e::{
        create_test_room, decode_test_room_id, register_test_user,
        setup_e2e_server_with_connection_limits, TestInfra,
    };

    fn single_connection_per_user_limits() -> ConnectionLimits {
        ConnectionLimits {
            webrtc_session_timeout: Default::default(),
            max_per_user: 1,
            max_per_room: 200,
            max_total: 10000,
            idle_timeout: std::time::Duration::from_mins(5),
            max_duration: std::time::Duration::from_hours(24),
        }
    }

    /// Build a WebSocket request with Authorization header (no `?token=` query param).
    fn ws_request(addr: &str, room_id: &str, token: &str) -> tungstenite::http::Request<()> {
        let url = format!("ws://{addr}/ws/rooms/{room_id}?format=protobuf");
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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_connection_limit_returns_429_before_upgrade() {
        let infra = TestInfra::new().await;
        let mut server =
            setup_e2e_server_with_connection_limits(&infra, single_connection_per_user_limits())
                .await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "limit_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Limit Test Room").await;

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
                panic!(
                    "BUG: Second connection was upgraded with status {} instead of being rejected with 429. \
                     Connection limit check is happening AFTER WebSocket upgrade!",
                    response.status()
                );
            }
        }

        drop(ws1);
        server.shutdown().await;
        infra.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_normal_connection_within_limits() {
        let infra = TestInfra::new().await;
        let mut server =
            setup_e2e_server_with_connection_limits(&infra, single_connection_per_user_limits())
                .await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "normal_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Normal Flow Room").await;

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
        server.shutdown().await;
        infra.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_different_users_can_connect_within_limits() {
        let infra = TestInfra::new().await;
        let mut server =
            setup_e2e_server_with_connection_limits(&infra, single_connection_per_user_limits())
                .await;

        let (user1_id, user1_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user1").await;
        let (user2_id, user2_token) =
            register_test_user(&server.user_service, &server.jwt_service, "user2").await;

        let room_id = create_test_room(&server.room_service, &user1_id, "Multi User Room").await;

        let rid = decode_test_room_id(&room_id);
        server
            .room_service
            .join_room(rid, user2_id, None)
            .await
            .expect("user2 join room");

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
        server.shutdown().await;
        infra.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_can_reconnect_after_disconnect() {
        let infra = TestInfra::new().await;
        let mut server =
            setup_e2e_server_with_connection_limits(&infra, single_connection_per_user_limits())
                .await;

        let (user_id, token) =
            register_test_user(&server.user_service, &server.jwt_service, "reconnect_user").await;
        let room_id = create_test_room(&server.room_service, &user_id, "Reconnect Room").await;

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

        drop(ws1);

        wait_for_condition(std::time::Duration::from_secs(2), || {
            server.connection_manager.user_connection_count(&user_id) == 0
        })
        .await;

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
        server.shutdown().await;
        infra.cleanup().await;
    }
}
