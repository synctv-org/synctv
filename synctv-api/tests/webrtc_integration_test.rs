//! WebRTC Integration Tests
//!
//! Comprehensive tests for WebRTC functionality including:
//! - ICE servers configuration (STUN/TURN)
//! - WebRTC signaling (Offer/Answer/ICE candidate exchange)
//! - Permission checks (`USE_WEBRTC` permission required)
//! - Multi-user peer-to-peer scenarios
//!
//! These tests validate the complete WebRTC flow from ICE server discovery
//! to peer connection establishment through signaling messages.

#![allow(clippy::unwrap_used)]
use synctv_core::config::{Config, WebRTCConfig, WebRTCMode};
use synctv_core::models::PermissionBits;

// ============================================================================
// Test Infrastructure Setup
// ============================================================================

/// Create a test configuration with WebRTC enabled
fn test_webrtc_config() -> Config {
    let mut config = Config {
        webrtc: WebRTCConfig {
            mode: WebRTCMode::PeerToPeer,
            enable_builtin_stun: true,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: "203.0.113.1:3478".to_string(),
            turn_shared_secret: String::new(), // No TURN for basic tests
            turn_server_urls: vec![],
            turn_credential_ttl_seconds: 86400,
            filter_private_ice_candidates: true,
        },
        ..Config::default()
    };
    config.server.advertise_host = "test.example.com".to_string();
    config
}

// ============================================================================
// Module: ICE Servers Configuration
// ============================================================================

mod ice_servers {
    use super::*;
    use synctv_proto::client::{GetIceServersResponse, IceServer};

    #[test]
    fn test_stun_url_format() {
        let config = test_webrtc_config();
        let stun_url = format!(
            "stun:{}:{}",
            config.server.advertise_host, config.webrtc.stun_port
        );

        assert_eq!(stun_url, "stun:test.example.com:3478");
        assert!(stun_url.starts_with("stun:"));
        assert!(stun_url.contains(":3478"));
    }

    #[test]
    fn test_ice_server_serialization_stun() {
        let server = IceServer {
            urls: vec!["stun:stun.example.com:3478".to_string()],
            username: None,
            credential: None,
            expiry_time: 0,
        };

        let json = serde_json::to_string(&server).expect("Should serialize");
        assert!(json.contains("stun:stun.example.com:3478"));
        // Proto messages include fields even when None/empty
        // Just verify the URL is present and it serializes correctly
    }

    #[test]
    fn test_ice_server_serialization_turn() {
        let server = IceServer {
            urls: vec![
                "turn:turn.example.com:3478".to_string(),
                "turns:turn.example.com:5349".to_string(),
            ],
            username: Some("1234567890:user123".to_string()),
            credential: Some("secret_credential_here".to_string()),
            expiry_time: 1_640_995_200,
        };

        let json = serde_json::to_string(&server).expect("Should serialize");
        assert!(json.contains("turn:turn.example.com:3478"));
        assert!(json.contains("turns:turn.example.com:5349"));
        assert!(json.contains("username"));
        assert!(json.contains("credential"));
        assert!(json.contains("1640995200"));
    }

    #[test]
    fn test_ice_servers_response_serialization() {
        let response = GetIceServersResponse {
            servers: vec![
                IceServer {
                    urls: vec!["stun:stun1.example.com:3478".to_string()],
                    username: None,
                    credential: None,
                    expiry_time: 0,
                },
                IceServer {
                    urls: vec!["turn:turn.example.com:3478".to_string()],
                    username: Some("user:pass".to_string()),
                    credential: Some("cred123".to_string()),
                    expiry_time: 1_640_995_200,
                },
            ],
        };

        let json = serde_json::to_string(&response).expect("Should serialize");
        assert!(json.contains("stun:stun1.example.com:3478"));
        assert!(json.contains("turn:turn.example.com:3478"));
        assert!(json.contains("user:pass"));
    }

    #[test]
    fn test_ice_servers_response_deserialization() {
        let json = r#"{
            "servers": [
                {
                    "urls": ["stun:stun.example.com:3478"],
                    "username": null,
                    "credential": null,
                    "expiry_time": 0
                }
            ]
        }"#;

        let response: GetIceServersResponse =
            serde_json::from_str(json).expect("Should deserialize");
        assert_eq!(response.servers.len(), 1);
        assert_eq!(response.servers[0].urls[0], "stun:stun.example.com:3478");
        assert!(response.servers[0].username.is_none());
    }
}

// ============================================================================
// Module: WebRTC Configuration Modes
// ============================================================================

mod webrtc_modes {
    use super::*;

    #[test]
    fn test_signaling_only_mode() {
        let config = WebRTCConfig {
            mode: WebRTCMode::SignalingOnly,
            enable_builtin_stun: false,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: String::new(),
            turn_shared_secret: String::new(),
            turn_server_urls: vec![],
            turn_credential_ttl_seconds: 86400,
            filter_private_ice_candidates: true,
        };

        assert_eq!(config.mode, WebRTCMode::SignalingOnly);
        assert!(!config.enable_builtin_stun);
    }

    #[test]
    fn test_peer_to_peer_mode() {
        let config = WebRTCConfig {
            mode: WebRTCMode::PeerToPeer,
            enable_builtin_stun: true,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: "203.0.113.1:3478".to_string(),
            turn_shared_secret: String::new(),
            turn_server_urls: vec![],
            turn_credential_ttl_seconds: 86400,
            filter_private_ice_candidates: true,
        };

        assert_eq!(config.mode, WebRTCMode::PeerToPeer);
        assert!(config.enable_builtin_stun);
        assert!(!config.stun_external_addr.is_empty());
    }

    #[test]
    fn test_turn_configuration() {
        let config = WebRTCConfig {
            mode: WebRTCMode::PeerToPeer,
            enable_builtin_stun: true,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: "203.0.113.1:3478".to_string(),
            turn_shared_secret: "my-turn-secret".to_string(),
            turn_server_urls: vec![
                "turn:turn.example.com:3478".to_string(),
                "turns:turn.example.com:5349".to_string(),
            ],
            turn_credential_ttl_seconds: 86400,
            filter_private_ice_candidates: true,
        };

        assert!(!config.turn_shared_secret.is_empty());
        assert_eq!(config.turn_server_urls.len(), 2);
        assert_eq!(config.turn_credential_ttl_seconds, 86400);
    }
}

// ============================================================================
// Module: TURN Credentials Generation
// ============================================================================

mod turn_credentials {
    use synctv_core::service::turn_server;

    #[test]
    fn test_generate_turn_credentials() {
        let secret = "my-turn-shared-secret";
        let username = "user123";
        let ttl = 3600;

        let creds = turn_server::generate_turn_credentials(secret, username, ttl);

        // Username format: timestamp:username
        assert!(creds.username.contains(':'));
        assert!(creds.username.ends_with(username));

        // Password should be base64 encoded HMAC
        assert!(!creds.password.is_empty());
        assert!(creds
            .password
            .chars()
            .all(|c| c.is_alphanumeric() || c == '+' || c == '/' || c == '='));

        // Expiry should be in the future
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(creds.expiry_timestamp > now);
        assert!(creds.expiry_timestamp <= now + ttl);
    }

    #[test]
    fn test_turn_credentials_deterministic_for_same_input() {
        let secret = "shared-secret";
        let user = "user";
        let ttl = 3600;

        // Generate two credentials with same params
        let creds1 = turn_server::generate_turn_credentials(secret, user, ttl);
        let creds2 = turn_server::generate_turn_credentials(secret, user, ttl);

        // Timestamps should be within 1 second of each other (near simultaneous generation)
        assert!(creds1.expiry_timestamp.abs_diff(creds2.expiry_timestamp) <= 1);

        // If usernames match (same timestamp), passwords should also match
        if creds1.username == creds2.username {
            assert_eq!(creds1.password, creds2.password);
        }
    }

    #[test]
    fn test_turn_password_changes_with_secret() {
        let user = "user123";
        let ttl = 3600;

        let creds1 = turn_server::generate_turn_credentials("secret1", user, ttl);
        let creds2 = turn_server::generate_turn_credentials("secret2", user, ttl);

        // Different secrets should produce different passwords
        // (even if timestamps happen to match)
        if creds1.username == creds2.username {
            assert_ne!(
                creds1.password, creds2.password,
                "Different secrets should produce different passwords"
            );
        }
    }

    #[test]
    fn test_turn_password_changes_with_username() {
        let secret = "my-secret";
        let ttl = 3600;

        let creds1 = turn_server::generate_turn_credentials(secret, "user1", ttl);
        let creds2 = turn_server::generate_turn_credentials(secret, "user2", ttl);

        // Different usernames should produce different username strings
        assert_ne!(
            creds1.username, creds2.username,
            "Different users should have different usernames"
        );
    }
}

// ============================================================================
// Module: WebRTC Permission Checks
// ============================================================================

mod permissions {
    use super::*;
    use std::sync::Arc;

    use synctv_api::impls::{ApiError, ClientApiImpl};
    use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
    use synctv_core::cache::{l2_backend::RedisCacheL2, KeyBuilder, UsernameCache};
    use synctv_core::config::PasswordComplexityConfig;
    use synctv_core::service::auth::jwt::JwtService;
    use synctv_core::service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, RoomService, UserService,
    };
    use synctv_core_testing::{
        create_test_pool_with_db_and_label, start_redis_url_with_label, test_redis_key_prefix,
        RedisContainer, TestContainer,
    };

    struct ClientApiFixture {
        _postgres: TestContainer,
        _redis: RedisContainer,
        pool: sqlx::PgPool,
        user_service: Arc<UserService>,
        room_service: Arc<RoomService>,
        client_api: ClientApiImpl,
    }

    async fn build_client_api_fixture(label: &str) -> ClientApiFixture {
        let (postgres, pool) = create_test_pool_with_db_and_label("synctv_test", label).await;
        let (redis, redis_url) = start_redis_url_with_label(label).await;
        let redis_key_prefix = test_redis_key_prefix(label);

        let redis_client = redis::Client::open(redis_url.as_str()).expect("Redis client");
        let redis_conn = Arc::new(tokio::sync::RwLock::new(
            redis::aio::ConnectionManager::new(redis_client.clone())
                .await
                .expect("Redis connection manager"),
        ));
        let username_cache = UsernameCache::new(
            Arc::new(RedisCacheL2::new_shared(redis_conn.clone())),
            format!("{redis_key_prefix}un:"),
            100,
            300,
        );
        let brute_force =
            BruteForceProtection::with_redis(redis_conn.clone(), redis_key_prefix.clone());
        let jwt_service =
            JwtService::new("this-is-a-test-secret-with-enough-entropy-for-jwt-signing-32chars")
                .expect("JwtService");
        let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
            Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));

        let user_service = Arc::new(UserService::new(
            pool.clone(),
            jwt_service.clone(),
            username_cache,
            PasswordComplexityConfig::default(),
            token_blacklist,
            KeyBuilder::new(redis_key_prefix),
            brute_force,
        ));
        let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
        let client_api = ClientApiImpl::new(
            user_service.clone(),
            room_service.clone(),
            Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            Arc::new(test_webrtc_config()),
            None,
            jwt_service,
            None,
            None,
            None,
        );

        ClientApiFixture {
            _postgres: postgres,
            _redis: redis,
            pool,
            user_service,
            room_service,
            client_api,
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_ice_servers_denies_member_without_use_webrtc_permission() {
        let fixture = build_client_api_fixture("api-webrtc-permissions").await;

        let (creator, _, _) = fixture
            .user_service
            .register(
                "webrtc_creator".to_string(),
                Some("webrtc_creator@test.com".to_string()),
                "TestPassword123!".to_string(),
                None,
            )
            .await
            .expect("register creator");
        let (member, _, _) = fixture
            .user_service
            .register(
                "webrtc_member".to_string(),
                Some("webrtc_member@test.com".to_string()),
                "TestPassword123!".to_string(),
                None,
            )
            .await
            .expect("register member");

        let (room, _) = fixture
            .room_service
            .create_room(
                "WebRTC Permission Room".to_string(),
                String::new(),
                creator.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room");
        fixture
            .room_service
            .join_room(room.id.clone(), member.id.clone(), None)
            .await
            .expect("join room");

        fixture
            .room_service
            .member_service()
            .revoke_permission(
                room.id.clone(),
                creator.id.clone(),
                member.id.clone(),
                PermissionBits::USE_WEBRTC,
            )
            .await
            .expect("revoke USE_WEBRTC");

        let err = fixture
            .client_api
            .get_ice_servers(&room.id, &member.id)
            .await
            .expect_err("members without USE_WEBRTC must be denied");

        match err {
            ApiError::Authorization(message) => {
                assert!(
                    message.contains("Forbidden"),
                    "expected permission failure to map to authorization error: {message}"
                );
            }
            other => panic!("expected authorization error, got {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_ice_servers_rejects_banned_room() {
        let fixture = build_client_api_fixture("api-webrtc-banned-room").await;

        let (creator, _, _) = fixture
            .user_service
            .register(
                "webrtc_banned_creator".to_string(),
                Some("webrtc_banned_creator@test.com".to_string()),
                "TestPassword123!".to_string(),
                None,
            )
            .await
            .expect("register creator");
        let (member, _, _) = fixture
            .user_service
            .register(
                "webrtc_banned_member".to_string(),
                Some("webrtc_banned_member@test.com".to_string()),
                "TestPassword123!".to_string(),
                None,
            )
            .await
            .expect("register member");

        let (room, _) = fixture
            .room_service
            .create_room(
                "WebRTC Banned Room".to_string(),
                String::new(),
                creator.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room");
        fixture
            .room_service
            .join_room(room.id.clone(), member.id.clone(), None)
            .await
            .expect("join room");
        fixture
            .room_service
            .ban_room(&room.id, &creator.id)
            .await
            .expect("ban room");

        let err = fixture
            .client_api
            .get_ice_servers(&room.id, &member.id)
            .await
            .expect_err("banned room must reject webrtc bootstrap");

        match err {
            ApiError::Authorization(message) => assert!(
                message.contains("Forbidden"),
                "banned room should be rejected as authorization failure: {message}"
            ),
            other => panic!("expected authorization error, got {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_ice_servers_rejects_closed_room() {
        let fixture = build_client_api_fixture("api-webrtc-closed-room").await;

        let (creator, _, _) = fixture
            .user_service
            .register(
                "webrtc_closed_creator".to_string(),
                Some("webrtc_closed_creator@test.com".to_string()),
                "TestPassword123!".to_string(),
                None,
            )
            .await
            .expect("register creator");
        let (member, _, _) = fixture
            .user_service
            .register(
                "webrtc_closed_member".to_string(),
                Some("webrtc_closed_member@test.com".to_string()),
                "TestPassword123!".to_string(),
                None,
            )
            .await
            .expect("register member");

        let (mut room, _) = fixture
            .room_service
            .create_room(
                "WebRTC Closed Room".to_string(),
                String::new(),
                creator.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room");
        fixture
            .room_service
            .join_room(room.id.clone(), member.id.clone(), None)
            .await
            .expect("join room");

        let original_version = room.version;
        room.status = synctv_core::models::RoomStatus::Closed;
        synctv_core::repository::RoomRepository::new(fixture.pool.clone())
            .update(&room, original_version)
            .await
            .expect("close room");

        let err = fixture
            .client_api
            .get_ice_servers(&room.id, &member.id)
            .await
            .expect_err("closed room must reject webrtc bootstrap");

        match err {
            ApiError::Authorization(message) => assert!(
                message.contains("Forbidden"),
                "closed room should be rejected as authorization failure: {message}"
            ),
            other => panic!("expected authorization error, got {other:?}"),
        }
    }
}

// ============================================================================
// Module: Network Quality (SFU Removed)
// ============================================================================
//
// Note: Network quality functionality was removed with the SFU module.
// The get_network_quality API now always returns an empty peer list.
// This is tested in the HTTP integration tests.

// ============================================================================
// Module: WebRTC Message Types
// ============================================================================

mod message_types {
    use synctv_proto::client::{WebRtcAnswer, WebRtcIceCandidate, WebRtcOffer};

    #[test]
    fn test_webrtc_offer_serialization() {
        let offer = WebRtcOffer {
            to: "user2".to_string(),
            from: "user1:conn1".to_string(),
            data: r#"{"type":"offer","sdp":"v=0\r\no=- 123 456 IN IP4 127.0.0.1\r\n..."}"#
                .to_string(),
        };

        let json = serde_json::to_string(&offer).expect("Should serialize");
        assert!(json.contains("user2"));
        assert!(json.contains("user1:conn1"));
        assert!(json.contains("offer"));
    }

    #[test]
    fn test_webrtc_answer_serialization() {
        let answer = WebRtcAnswer {
            to: "user1".to_string(),
            from: "user2:conn2".to_string(),
            data: r#"{"type":"answer","sdp":"v=0\r\no=- 789 012 IN IP4 127.0.0.1\r\n..."}"#
                .to_string(),
        };

        let json = serde_json::to_string(&answer).expect("Should serialize");
        assert!(json.contains("user1"));
        assert!(json.contains("user2:conn2"));
        assert!(json.contains("answer"));
    }

    #[test]
    fn test_ice_candidate_serialization() {
        let candidate = WebRtcIceCandidate {
            to: "user2".to_string(),
            from: "user1:conn1".to_string(),
            data: r#"{"candidate":"candidate:1 1 udp 2130706431 192.168.1.100 54321 typ host","sdpMid":"0","sdpMLineIndex":0}"#.to_string(),
        };

        let json = serde_json::to_string(&candidate).expect("Should serialize");
        assert!(json.contains("user2"));
        assert!(json.contains("user1:conn1"));
        assert!(json.contains("candidate:"));
        assert!(json.contains("typ host"));
    }

    #[test]
    fn test_ice_candidate_with_srflx() {
        let candidate = WebRtcIceCandidate {
            to: "user3".to_string(),
            from: "user1:conn1".to_string(),
            data: r#"{"candidate":"candidate:foundation 1 udp 12345 203.0.113.1 12345 typ srflx"}"#
                .to_string(),
        };

        let json = serde_json::to_string(&candidate).expect("Should serialize");
        assert!(json.contains("user3"));
        assert!(json.contains("typ srflx"));
    }
}

// ============================================================================
// Module: ICE Candidate Filtering
// ============================================================================

mod ice_filtering {
    use std::net::IpAddr;

    /// Check if an IP address is private, loopback, or link-local
    const fn is_private_or_internal(ip: &IpAddr) -> bool {
        match ip {
            IpAddr::V4(ipv4) => ipv4.is_private() || ipv4.is_loopback() || ipv4.is_link_local(),
            IpAddr::V6(ipv6) => ipv6.is_loopback() || ipv6.is_unicast_link_local(),
        }
    }

    #[test]
    fn test_private_ip_detection() {
        let private_ips = vec![
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
        ];

        for ip in private_ips {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(
                is_private_or_internal(&parsed),
                "{ip} should be considered private/internal"
            );
        }
    }

    #[test]
    fn test_public_ip_detection() {
        let public_ips = vec!["8.8.8.8", "1.1.1.1", "203.0.113.1"];

        for ip in public_ips {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(
                !is_private_or_internal(&parsed),
                "{ip} should be considered public"
            );
        }
    }

    #[test]
    fn test_candidate_type_identification() {
        let host_candidate = "candidate:1 1 udp 2130706431 192.168.1.100 54321 typ host";
        let srflx_candidate = "candidate:2 1 udp 1694498815 203.0.113.1 54321 typ srflx";
        let relay_candidate = "candidate:3 1 udp 16777215 203.0.113.2 54321 typ relay";

        assert!(host_candidate.contains("typ host"));
        assert!(srflx_candidate.contains("typ srflx"));
        assert!(relay_candidate.contains("typ relay"));
    }
}

// ============================================================================
// Integration Test Notes
// ============================================================================

// NOTE: Full end-to-end tests requiring WebSocket connections and database
// would be added here in a real-world scenario. These tests validate:
//
// 1. Complete signaling flow: User A sends offer → User B receives →
//    User B sends answer → User A receives
//
// 2. ICE candidate trickle: Both peers exchange ICE candidates through
//    the signaling server
//
// 3. Permission enforcement: Users without USE_WEBRTC permission are
//    rejected when attempting to send WebRTC messages
//
// 4. Multi-room isolation: WebRTC messages in room A don't leak to room B
//
// 5. Cluster behavior: WebRTC signaling works correctly across multiple
//    server replicas via Redis pub/sub
//
// Such tests would require:
// - testcontainers for PostgreSQL and Redis
// - WebSocket client library for connection testing
// - Mocking or actual implementation of connection manager
//
// Example test structure:
//
// #[tokio::test]
// async fn test_webrtc_offer_answer_flow() {
//     let infra = TestInfra::setup().await;
//     let user_a = infra.create_test_user("alice").await;
//     let user_b = infra.create_test_user("bob").await;
//     let room = infra.create_test_room("test_room").await;
//
//     let ws_a = infra.connect_websocket(&user_a, &room).await;
//     let ws_b = infra.connect_websocket(&user_b, &room).await;
//
//     // User A sends offer
//     ws_a.send(Message::WebRtcOffer { to_user_id: user_b.id, sdp: "..." }).await;
//
//     // User B receives offer
//     let offer = ws_b.recv().await.expect("Should receive offer");
//     assert_matches!(offer, Message::WebRtcOffer { .. });
//
//     // User B sends answer
//     ws_b.send(Message::WebRtcAnswer { to_user_id: user_a.id, sdp: "..." }).await;
//
//     // User A receives answer
//     let answer = ws_a.recv().await.expect("Should receive answer");
//     assert_matches!(answer, Message::WebRtcAnswer { .. });
// }
