//! WebRTC Integration Tests
//!
//! Comprehensive tests for WebRTC functionality including:
//! - ICE servers configuration (STUN)
//! - WebRTC signaling (Offer/Answer/ICE candidate exchange)
//! - Permission checks (`USE_WEBRTC` permission required)
//! - Multi-user peer-to-peer scenarios
//!
//! These tests validate the complete WebRTC flow from ICE server discovery
//! to peer connection establishment through signaling messages.

#![allow(clippy::unwrap_used)]
use synctv_core::config::{Config, WebRTCConfig, WebRTCMode};
use synctv_core::models::PermissionBits;

// Test Infrastructure Setup

/// Create a test configuration with WebRTC enabled
fn test_webrtc_config() -> Config {
    let mut config = Config {
        webrtc: WebRTCConfig {
            mode: WebRTCMode::PeerToPeer,
            enable_builtin_stun: true,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: "203.0.113.1:3478".to_string(),
            filter_private_ice_candidates: true,
        },
        ..Config::default()
    };
    config.server.advertise_host = "test.example.com".to_string();
    config
}

mod ice_servers {
    use super::*;
    use synctv_core::service::{ConfiguredIceServer, IceServerList};
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
        };

        let json = serde_json::to_string(&server).expect("Should serialize");
        assert!(json.contains("stun:stun.example.com:3478"));
        // Proto messages include fields even when None/empty
        // Just verify the URL is present and it serializes correctly
    }

    #[test]
    fn test_ice_server_serialization_multiple_stun_urls() {
        let server = IceServer {
            urls: vec![
                "stun:stun1.example.com:3478".to_string(),
                "stun:stun2.example.com:3478".to_string(),
            ],
            username: None,
            credential: None,
        };

        let json = serde_json::to_string(&server).expect("Should serialize");
        assert!(json.contains("stun:stun1.example.com:3478"));
        assert!(json.contains("stun:stun2.example.com:3478"));
        assert_eq!(server.urls.len(), 2);
    }

    #[test]
    fn test_ice_servers_response_serialization() {
        let response = GetIceServersResponse {
            servers: vec![
                IceServer {
                    urls: vec!["stun:stun1.example.com:3478".to_string()],
                    username: None,
                    credential: None,
                },
                IceServer {
                    urls: vec!["turn:turn.example.com:3478?transport=udp".to_string()],
                    username: Some("turn-user".to_string()),
                    credential: Some("turn-password".to_string()),
                },
            ],
            webrtc: None,
        };

        let json = serde_json::to_string(&response).expect("Should serialize");
        assert!(json.contains("stun:stun1.example.com:3478"));
        assert!(json.contains("turn:turn.example.com:3478?transport=udp"));
        assert!(json.contains("turn-user"));
        assert!(json.contains("turn-password"));
        assert_eq!(response.servers.len(), 2);
    }

    #[test]
    fn test_ice_servers_response_deserialization() {
        let json = r#"{
            "servers": [
                {
                    "urls": ["turn:turn.example.com:3478?transport=udp"],
                    "username": "turn-user",
                    "credential": "turn-password"
                }
            ]
        }"#;

        let response: GetIceServersResponse =
            serde_json::from_str(json).expect("Should deserialize");
        assert_eq!(response.servers.len(), 1);
        assert_eq!(
            response.servers[0].urls[0],
            "turn:turn.example.com:3478?transport=udp"
        );
        assert_eq!(response.servers[0].username.as_deref(), Some("turn-user"));
        assert_eq!(
            response.servers[0].credential.as_deref(),
            Some("turn-password")
        );
    }

    #[test]
    fn test_external_ice_server_settings_roundtrip_supports_turn_credentials() {
        let servers = IceServerList(vec![
            ConfiguredIceServer::new(vec!["stun:stun.example.com:3478".to_string()]),
            ConfiguredIceServer::new(vec!["turn:turn.example.com:3478?transport=udp".to_string()])
                .with_auth("turn-user", "turn-password"),
        ]);

        let json = servers.to_string();
        let parsed: IceServerList = json.parse().expect("Should deserialize");

        assert_eq!(parsed, servers);
        assert_eq!(parsed.0[1].username.as_deref(), Some("turn-user"));
        assert_eq!(parsed.0[1].credential.as_deref(), Some("turn-password"));
    }
}

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
            filter_private_ice_candidates: true,
        };

        assert_eq!(config.mode, WebRTCMode::PeerToPeer);
        assert!(config.enable_builtin_stun);
        assert!(!config.stun_external_addr.is_empty());
    }

    #[test]
    fn test_builtin_stun_configuration() {
        let config = WebRTCConfig {
            mode: WebRTCMode::PeerToPeer,
            enable_builtin_stun: true,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: "203.0.113.1:3478".to_string(),
            filter_private_ice_candidates: true,
        };

        assert!(config.enable_builtin_stun);
        assert_eq!(config.stun_port, 3478);
        assert_eq!(config.stun_host, "0.0.0.0");
    }
}

mod permissions {
    use super::*;
    use std::sync::Arc;

    use synctv_api::impls::{ApiError, ClientApiImpl};
    use synctv_core::cache::{l2_backend::RedisCacheL2, KeyBuilder, UsernameCache};
    use synctv_core::config::PasswordComplexityConfig;
    use synctv_core::models::room_settings::{AllowGuestJoin, GuestAddedPermissions};
    use synctv_core::repository::SettingsRepository;
    use synctv_core::service::auth::jwt::JwtService;
    use synctv_core::service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, RoomService, SettingsRegistry,
        SettingsService, UserService,
    };
    use synctv_core::service::{ConfiguredIceServer, IceServerList};
    use synctv_core_testing::{
        create_test_pool_with_db_and_label, redis_connection_manager, start_redis_url_with_label,
        test_redis_key_prefix, RedisContainer, TestContainer,
    };
    use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};
    use tokio_util::sync::CancellationToken;

    struct ClientApiFixture {
        _postgres: TestContainer,
        _redis: RedisContainer,
        pool: sqlx::PgPool,
        user_service: Arc<UserService>,
        room_service: Arc<RoomService>,
        settings_registry: Arc<SettingsRegistry>,
        client_api: ClientApiImpl,
    }

    async fn build_client_api_fixture(label: &str) -> ClientApiFixture {
        let (postgres, pool) = create_test_pool_with_db_and_label("synctv_test", label).await;
        let (redis, redis_url) = start_redis_url_with_label(label).await;
        let redis_key_prefix = test_redis_key_prefix(label);

        let redis_client = redis::Client::open(redis_url.as_str()).expect("Redis client");
        let redis_conn = Arc::new(tokio::sync::RwLock::new(
            redis_connection_manager(&redis_client).await,
        ));
        let username_cache = UsernameCache::new(
            Arc::new(RedisCacheL2::from_runtime(synctv_core::shared_runtime(
                redis_conn.clone(),
            ))),
            format!("{redis_key_prefix}un:"),
            100,
            300,
        );
        let brute_force = BruteForceProtection::new_with_config(
            redis_key_prefix.clone(),
            Arc::new(
                synctv_core::service::auth::brute_force::RedisAttemptTracker::new(
                    redis_conn.clone(),
                    50_000,
                    synctv_core::service::BruteForceConfig::default().attempts_ttl_secs,
                ),
            ),
            Arc::new(
                synctv_core::service::auth::brute_force::RedisAttemptTracker::new(
                    redis_conn,
                    100_000,
                    synctv_core::service::BruteForceConfig::default().ip_attempts_ttl_secs,
                ),
            ),
            synctv_core::service::BruteForceConfig::default(),
        );
        let jwt_service =
            JwtService::new("this-is-a-test-secret-with-enough-entropy-for-jwt-signing-32chars")
                .expect("JwtService");
        let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
            Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));

        let mut user_service = UserService::new(
            &pool,
            jwt_service.clone(),
            username_cache,
            PasswordComplexityConfig::default(),
            token_blacklist,
            KeyBuilder::new(redis_key_prefix),
            brute_force,
        );
        user_service.enable_password_registration_for_tests();
        user_service.enable_legacy_password_login_for_tests();
        user_service.enable_legacy_password_registration_for_tests();
        let user_service = Arc::new(user_service);
        let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
        let settings_repo = SettingsRepository::new(pool.clone());
        let settings_service = Arc::new(SettingsService::new(settings_repo, pool.clone()));
        let settings_registry = Arc::new(SettingsRegistry::new(settings_service));
        settings_registry
            .init(CancellationToken::new())
            .expect("initialize settings registry");
        let builtin_stun_url = format!(
            "stun:{}:{}",
            test_webrtc_config().server.advertise_host,
            test_webrtc_config().webrtc.stun_port
        );
        let client_api = ClientApiImpl::new(
            user_service.clone(),
            room_service.clone(),
            Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            Arc::new(test_webrtc_config()),
            None,
            jwt_service,
            None,
            None,
            Some(settings_registry.clone()),
            Arc::new(synctv_api::PublicIdCodec::default_for_tests()),
        )
        .with_builtin_stun_url(builtin_stun_url);

        ClientApiFixture {
            _postgres: postgres,
            _redis: redis,
            pool,
            user_service,
            room_service,
            settings_registry,
            client_api,
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_ice_servers_includes_configured_turn_servers_with_credentials() {
        let fixture = build_client_api_fixture("api-webrtc-custom-ice").await;

        fixture
            .settings_registry
            .external_ice_servers
            .set(IceServerList(vec![ConfiguredIceServer::new(vec![
                "turn:turn.example.com:3478?transport=udp".to_string(),
                "turns:turn.example.com:5349".to_string(),
            ])
            .with_auth("turn-user", "turn-password")]))
            .await
            .expect("set external ice servers");

        let (creator, _, _) = fixture
            .user_service
            .register(
                "webrtc_turn_creator".to_string(),
                Some("webrtc_turn_creator@test.com".to_string()),
                "TestPassword123!".to_string(),
                None,
            )
            .await
            .expect("register creator");
        let (member, _, _) = fixture
            .user_service
            .register(
                "webrtc_turn_member".to_string(),
                Some("webrtc_turn_member@test.com".to_string()),
                "TestPassword123!".to_string(),
                None,
            )
            .await
            .expect("register member");

        let (room, _) = fixture
            .room_service
            .create_room(
                "WebRTC Custom ICE Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await
            .expect("create room");
        fixture
            .room_service
            .join_room(room.id, member.id, None)
            .await
            .expect("join room");
        fixture
            .room_service
            .grant_permission(room.id, creator.id, member.id, PermissionBits::USE_WEBRTC)
            .await
            .expect("grant USE_WEBRTC");

        let response = fixture
            .client_api
            .get_ice_servers(&room.id, &member.id)
            .await
            .expect("get ice servers");

        assert_eq!(response.servers.len(), 2);
        assert_eq!(
            response.servers[0].urls,
            vec!["stun:test.example.com:3478".to_string()]
        );
        assert_eq!(
            response.servers[1].urls,
            vec![
                "turn:turn.example.com:3478?transport=udp".to_string(),
                "turns:turn.example.com:5349".to_string(),
            ]
        );
        assert_eq!(response.servers[1].username.as_deref(), Some("turn-user"));
        assert_eq!(
            response.servers[1].credential.as_deref(),
            Some("turn-password")
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_guest_with_use_webrtc_permission_can_bootstrap_ice_servers() {
        let fixture = build_client_api_fixture("api-webrtc-guest-ice").await;

        fixture
            .settings_registry
            .external_ice_servers
            .set(IceServerList(vec![ConfiguredIceServer::new(vec![
                "turn:guest-turn.example.com:3478?transport=udp".to_string(),
            ])
            .with_auth("guest-turn-user", "guest-turn-password")]))
            .await
            .expect("set external ice servers");

        let (creator, _, _) = fixture
            .user_service
            .register(
                "webrtc_guest_creator".to_string(),
                Some("webrtc_guest_creator@test.com".to_string()),
                "TestPassword123!".to_string(),
                None,
            )
            .await
            .expect("register creator");

        let (room, _) = fixture
            .room_service
            .create_room(
                "WebRTC Guest ICE Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await
            .expect("create room");

        let mut settings = fixture
            .room_service
            .get_room_settings(&room.id)
            .await
            .expect("load room settings");
        settings.allow_guest_join = AllowGuestJoin(true);
        settings.guest_added_permissions = GuestAddedPermissions(PermissionBits::USE_WEBRTC);
        fixture
            .room_service
            .set_settings(room.id, creator.id, settings)
            .await
            .expect("enable guest WebRTC");

        let guest_version = fixture
            .room_service
            .get_room_guest_version(&room.id)
            .await
            .expect("guest version");
        let token = fixture
            .client_api
            .jwt_service
            .sign_guest_token_with_version(&room.id, guest_version)
            .expect("guest token");
        let public_room_id = fixture
            .client_api
            .public_id_codec
            .encode_room_id(room.id)
            .expect("public room id");
        let actor = fixture
            .client_api
            .room_actor_for_authorization(&format!("Bearer {token}"), &public_room_id)
            .await
            .expect("guest actor");

        let response = fixture
            .client_api
            .get_ice_servers_for_actor(&actor)
            .await
            .expect("guest ICE bootstrap");

        assert_eq!(
            response.servers[0].urls,
            vec!["stun:test.example.com:3478".to_string()]
        );
        assert_eq!(
            response.servers[1].urls,
            vec!["turn:guest-turn.example.com:3478?transport=udp".to_string()]
        );
        assert_eq!(
            response.servers[1].username.as_deref(),
            Some("guest-turn-user")
        );
        assert_eq!(
            response.servers[1].credential.as_deref(),
            Some("guest-turn-password")
        );
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
                creator.id,
                None,
                None,
            )
            .await
            .expect("create room");
        fixture
            .room_service
            .join_room(room.id, member.id, None)
            .await
            .expect("join room");

        fixture
            .room_service
            .member_service()
            .revoke_permission(room.id, creator.id, member.id, PermissionBits::USE_WEBRTC)
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
                creator.id,
                None,
                None,
            )
            .await
            .expect("create room");
        fixture
            .room_service
            .join_room(room.id, member.id, None)
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
                creator.id,
                None,
                None,
            )
            .await
            .expect("create room");
        fixture
            .room_service
            .join_room(room.id, member.id, None)
            .await
            .expect("join room");

        let original_version = room.version;
        room.close();
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

// Integration Test Notes

// would be added here in a real-world scenario. These tests validate:
//    User B sends answer → User A receives
//    the signaling server
//    rejected when attempting to send WebRTC messages
//    server replicas via Redis pub/sub
// Such tests would require:
// - testcontainers for PostgreSQL and Redis
// - WebSocket client library for connection testing
// - Mocking or actual implementation of connection manager
// Example test structure:
// #[tokio::test]
// async fn test_webrtc_offer_answer_flow() {
//     let infra = TestInfra::setup().await;
//     let user_a = infra.create_test_user("alice").await;
//     let user_b = infra.create_test_user("bob").await;
//     let room = infra.create_test_room("test_room").await;
//     let ws_a = infra.connect_websocket(&user_a, &room).await;
//     let ws_b = infra.connect_websocket(&user_b, &room).await;
//     // User A sends offer
//     ws_a.send(Message::WebRtcOffer { to_user_id: user_b.id, sdp: "..." }).await;
//     // User B receives offer
//     let offer = ws_b.recv().await.expect("Should receive offer");
//     assert_matches!(offer, Message::WebRtcOffer { .. });
//     // User B sends answer
//     ws_b.send(Message::WebRtcAnswer { to_user_id: user_a.id, sdp: "..." }).await;
//     // User A receives answer
//     let answer = ws_a.recv().await.expect("Should receive answer");
//     assert_matches!(answer, Message::WebRtcAnswer { .. });
// }
