//! WebRTC Integration Tests
//!
//! Comprehensive tests for WebRTC functionality including:
//! - ICE servers configuration (STUN)
//! - WebRTC signaling (Offer/Answer/ICE candidate exchange)
//! - Permission checks (`USE_VOICE_CHAT` permission required)
//! - Multi-user peer-to-peer scenarios
//!
//! These tests validate the complete WebRTC flow from ICE server discovery
//! to peer connection establishment through signaling messages.

#![allow(clippy::unwrap_used)]

mod support;
use synctv_core::models::{
    RoomAdminPermissionBits, RoomGuestPermissionBits, RoomMemberPermissionBits,
};

// Test Infrastructure Setup

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebRTCMode {
    SignalingOnly,
    PeerToPeer,
}

#[derive(Debug, Clone)]
struct WebRTCConfig {
    mode: WebRTCMode,
    enable_builtin_stun: bool,
    stun_port: u16,
    stun_host: String,
    stun_external_addr: String,
}

#[derive(Debug, Clone, Default)]
struct ServerConfig {
    advertise_host: String,
}

#[derive(Debug, Clone)]
struct Config {
    server: ServerConfig,
    webrtc: WebRTCConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            webrtc: WebRTCConfig {
                mode: WebRTCMode::PeerToPeer,
                enable_builtin_stun: true,
                stun_port: 3478,
                stun_host: "0.0.0.0".to_string(),
                stun_external_addr: String::new(),
            },
        }
    }
}

/// Create a test configuration with WebRTC enabled
fn test_webrtc_config() -> Config {
    let mut config = Config {
        webrtc: WebRTCConfig {
            mode: WebRTCMode::PeerToPeer,
            enable_builtin_stun: true,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: "203.0.113.1:3478".to_string(),
        },
        ..Config::default()
    };
    config.server.advertise_host = "test.example.com".to_string();
    config
}

mod ice_servers {
    use super::*;
    use synctv_core::service::{ConfiguredIceServer, IceServerList};

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
        };

        assert!(config.enable_builtin_stun);
        assert_eq!(config.stun_port, 3478);
        assert_eq!(config.stun_host, "0.0.0.0");
    }
}

mod permissions {
    use super::*;
    use std::sync::Arc;

    use synctv_api::{
        ApiError, ClientApiImpl, EndpointRateLimitCategory, RequestMetadata, TransportProtocol,
    };
    use synctv_core::cache::{l2_backend::RedisCacheL2, KeyBuilder, UsernameCache};
    use synctv_core::models::room_settings::{AllowGuestJoin, GuestAddedPermissions};
    use synctv_core::repository::SettingsRepository;
    use synctv_core::service::JwtService;
    use synctv_core::service::UserServiceRuntimeOptions;
    use synctv_core::service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, RoomService, RuntimeSettingsStore,
        SettingsService, UserService,
    };
    use synctv_core::service::{ConfiguredIceServer, IceServerList};
    use synctv_core_testing::{
        create_test_pool_with_db_and_label, opaque_register_user, redis_connection_manager,
        start_redis_url_with_label, test_redis_key_prefix, RedisContainer, TestContainer,
    };
    use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};
    use tokio_util::sync::CancellationToken;

    struct ClientApiFixture {
        _postgres: TestContainer,
        _redis: RedisContainer,
        pool: sqlx::PgPool,
        user_service: Arc<UserService>,
        room_service: Arc<RoomService>,
        runtime_settings_store: Arc<RuntimeSettingsStore>,
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
            Arc::new(synctv_core::service::RedisAttemptTracker::new(
                redis_conn.clone(),
                50_000,
                synctv_core::service::BruteForceConfig::default().attempts_ttl_secs,
            )),
            Arc::new(synctv_core::service::RedisAttemptTracker::new(
                redis_conn,
                100_000,
                synctv_core::service::BruteForceConfig::default().ip_attempts_ttl_secs,
            )),
            synctv_core::service::BruteForceConfig::default(),
        );
        let jwt_service =
            JwtService::new("this-is-a-test-secret-with-enough-entropy-for-jwt-signing-32chars")
                .expect("JwtService");
        let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
            Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));

        let user_service = UserService::new_with_runtime(
            &pool,
            jwt_service.clone(),
            username_cache,
            token_blacklist,
            KeyBuilder::new(redis_key_prefix),
            brute_force,
            UserServiceRuntimeOptions {
                password_registration_policy_override: Some(
                    synctv_core::service::RegistrationPolicy::Immediate,
                ),
                ..synctv_core::service::UserServiceRuntimeOptions::test_defaults()
            },
        );
        let user_service = Arc::new(user_service);
        let room_service = Arc::new(
            RoomService::new_for_tests(pool.clone(), (*user_service).clone())
                .expect("room service should build"),
        );
        let settings_repo = SettingsRepository::new(pool.clone());
        let settings_service = Arc::new(SettingsService::new(settings_repo, pool.clone()));
        let runtime_settings_store = Arc::new(RuntimeSettingsStore::new(settings_service));
        runtime_settings_store
            .init(CancellationToken::new())
            .expect("initialize runtime settings store");
        let builtin_stun_url = format!(
            "stun:{}:{}",
            test_webrtc_config().server.advertise_host,
            test_webrtc_config().webrtc.stun_port
        );
        let client_api = ClientApiImpl::new_with_runtime(
            synctv_api::ClientApiOptions {
                read_pool: None,
                user_service: user_service.clone(),
                room_service: room_service.clone(),
                connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
                runtime_settings: Arc::new(synctv_api::ApiRuntimeSettings::default()),
                publish_key_service: None,
                jwt_service,
                live_streaming_infrastructure: None,
                runtime_settings_store: Some(runtime_settings_store.clone()),
                public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
                chat_service: None,
                provider_stores: Arc::new(
                    synctv_core::provider::ProviderStoreRegistry::local_only("test:provider:"),
                ),
                email_api: None,
                passkey_service: None,
            },
            synctv_api::ClientApiRuntime {
                builtin_stun_url: Some(builtin_stun_url),
                ..support::client_api_runtime()
            },
        );

        ClientApiFixture {
            _postgres: postgres,
            _redis: redis,
            pool,
            user_service,
            room_service,
            runtime_settings_store,
            client_api,
        }
    }

    async fn register_fixture_user(
        fixture: &ClientApiFixture,
        username: &str,
    ) -> synctv_core::models::User {
        opaque_register_user(
            fixture.user_service.as_ref(),
            username,
            Some(format!("{username}@test.com")),
            "TestPassword123!",
        )
        .await
        .expect("register user")
        .0
    }

    async fn persist_external_ice_servers(fixture: &ClientApiFixture, servers: IceServerList) {
        let mut settings = fixture
            .runtime_settings_store
            .runtime_settings()
            .expect("runtime settings should load");
        settings.webrtc.external_ice_servers = servers;
        fixture
            .runtime_settings_store
            .persist_runtime_settings(&settings)
            .await
            .expect("external ice servers should persist");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_ice_servers_includes_configured_turn_servers_with_credentials() {
        let fixture = build_client_api_fixture("api-webrtc-custom-ice").await;

        persist_external_ice_servers(
            &fixture,
            IceServerList(vec![ConfiguredIceServer::new(vec![
                "turn:turn.example.com:3478?transport=udp".to_string(),
                "turns:turn.example.com:5349".to_string(),
            ])
            .with_auth("turn-user", "turn-password")]),
        )
        .await;

        let creator = register_fixture_user(&fixture, "webrtc_turn_creator").await;
        let member = register_fixture_user(&fixture, "webrtc_turn_member").await;

        let room = fixture
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
            .grant_permission(
                room.id,
                creator.id,
                member.id,
                RoomMemberPermissionBits::USE_VOICE_CHAT,
            )
            .await
            .expect("grant USE_VOICE_CHAT");

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
    async fn test_guest_with_use_voice_chat_permission_can_bootstrap_ice_servers() {
        let fixture = build_client_api_fixture("api-webrtc-guest-ice").await;

        persist_external_ice_servers(
            &fixture,
            IceServerList(vec![ConfiguredIceServer::new(vec![
                "turn:guest-turn.example.com:3478?transport=udp".to_string(),
            ])
            .with_auth("guest-turn-user", "guest-turn-password")]),
        )
        .await;

        let creator = register_fixture_user(&fixture, "webrtc_guest_creator").await;

        let room = fixture
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
        settings.guest_added_permissions =
            GuestAddedPermissions(RoomGuestPermissionBits::USE_VOICE_CHAT);
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
        let metadata = RequestMetadata::new(TransportProtocol::Http)
            .with_authorization(Some(format!("Bearer {token}")));
        let response = ClientApiImpl::execute_room_actor_endpoint(
            Arc::new(fixture.client_api.clone()),
            &metadata,
            public_room_id,
            EndpointRateLimitCategory::Read,
            |client_api, actor| async move { client_api.get_ice_servers_for_actor(&actor).await },
        )
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
    async fn test_get_ice_servers_accepts_either_business_permission_and_denies_without_both() {
        let fixture = build_client_api_fixture("api-webrtc-permissions").await;

        let creator = register_fixture_user(&fixture, "webrtc_creator").await;
        let member = register_fixture_user(&fixture, "webrtc_member").await;

        let room = fixture
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
            .revoke_permission(
                room.id,
                creator.id,
                member.id,
                RoomMemberPermissionBits::USE_VOICE_CHAT,
            )
            .await
            .expect("revoke USE_VOICE_CHAT");

        fixture
            .client_api
            .get_ice_servers(&room.id, &member.id)
            .await
            .expect("USE_P2P_MEDIA alone should authorize ICE bootstrap");

        fixture
            .room_service
            .member_service()
            .revoke_permission(
                room.id,
                creator.id,
                member.id,
                RoomAdminPermissionBits::USE_P2P_MEDIA,
            )
            .await
            .expect("revoke USE_P2P_MEDIA");

        let err = fixture
            .client_api
            .get_ice_servers(&room.id, &member.id)
            .await
            .expect_err("members without an RTC business permission must be denied");

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

        let creator = register_fixture_user(&fixture, "webrtc_banned_creator").await;
        let member = register_fixture_user(&fixture, "webrtc_banned_member").await;

        let room = fixture
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

        let creator = register_fixture_user(&fixture, "webrtc_closed_creator").await;
        let member = register_fixture_user(&fixture, "webrtc_closed_member").await;

        let mut room = fixture
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
