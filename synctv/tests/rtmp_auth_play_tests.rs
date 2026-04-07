//! RTMP auth on_play validation tests.
//!
//! Tests the `validate_play_request` method which validates:
//! - Room exists
//! - Room is not banned
//! - Room status is not Pending, Rejected, or Closed
//! - Room has rtmp_player enabled in settings
//!
//! Run with: cargo test -p synctv --test rtmp_auth_play_tests -- --nocapture

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv::rtmp_auth::SyncTvRtmpAuth;
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{RoomId, RoomStatus, User, UserId, UserRole, UserStatus},
    repository::{RoomRepository, RoomSettingsRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, PublishKeyService, RoomService, UserService,
    },
};
use synctv_core_testing::create_test_pool;
use synctv_livestream::{
    api::StreamTracker,
    relay::{
        registry::PublisherInfo,
        registry_trait::{PublisherRefreshOutcome, StreamRegistryTrait},
    },
};
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

// ========== Helper Functions ==========

fn make_user_service(pool: PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new(
        pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(pool.clone());
    RoomService::new(pool, user_service)
}

fn make_publish_key_service() -> PublishKeyService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    PublishKeyService::with_default_ttl(jwt_service)
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

/// Create a SyncTvRtmpAuth instance for testing
fn make_rtmp_auth(
    room_service: Arc<RoomService>,
    user_service: Arc<UserService>,
    publish_key_service: Arc<PublishKeyService>,
) -> SyncTvRtmpAuth {
    let user_stream_tracker = Arc::new(StreamTracker::new());
    let registry = Arc::new(MockStreamRegistry::new());

    SyncTvRtmpAuth::new(
        room_service,
        user_service,
        publish_key_service,
        user_stream_tracker,
        registry,
        "test-node".to_string(),
        "127.0.0.1:50051".to_string(),
        None,
        "test:".to_string(),
    )
}

// ========== Mock Stream Registry ==========

/// Mock implementation of StreamRegistryTrait for testing
struct MockStreamRegistry;

impl MockStreamRegistry {
    const fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl StreamRegistryTrait for MockStreamRegistry {
    async fn register_publisher(
        &self,
        _room_id: &str,
        _media_id: &str,
        _node_id: &str,
        _app_name: &str,
        _api_address: &str,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn try_register_publisher(
        &self,
        _room_id: &str,
        _media_id: &str,
        _node_id: &str,
        _user_id: &str,
        _api_address: &str,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn refresh_publisher_ttl(
        &self,
        _room_id: &str,
        _media_id: &str,
        _user_id: &str,
    ) -> anyhow::Result<PublisherRefreshOutcome> {
        Ok(PublisherRefreshOutcome::Refreshed)
    }

    async fn unregister_publisher(&self, _room_id: &str, _media_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn unregister_publisher_if_epoch_matches(
        &self,
        _room_id: &str,
        _media_id: &str,
        _expected_epoch: u64,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn get_publisher(
        &self,
        _room_id: &str,
        _media_id: &str,
    ) -> anyhow::Result<Option<PublisherInfo>> {
        Ok(None)
    }

    async fn is_stream_active(&self, _room_id: &str, _media_id: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn list_active_streams(&self) -> anyhow::Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    async fn get_user_publishers(&self, _user_id: &str) -> anyhow::Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    async fn unregister_all_user_publishers(&self, _user_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn validate_epoch(
        &self,
        _room_id: &str,
        _media_id: &str,
        _epoch: u64,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn cleanup_all_publishers_for_node(&self, _node_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

// ========== Test Cases ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_on_play_allows_active_room_with_rtmp_player_enabled() {
    let (_container, pool) = create_test_pool().await;
    let room_service = Arc::new(make_room_service(pool.clone()));
    let user_service = Arc::new(make_user_service(pool.clone()));
    let user_repo = UserRepository::new(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    // Create owner and room
    let owner = user_repo.create(&make_user("play_owner")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Play Test Room".to_string(),
            "A room for play testing".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Enable rtmp_player in room settings
    let mut settings = settings_repo.get(&room.id).await.unwrap();
    settings.rtmp_player.0 = true;
    settings_repo
        .set_settings(&room.id, &settings)
        .await
        .unwrap();

    // Create RTMP auth and validate
    let publish_key_service = Arc::new(make_publish_key_service());
    let rtmp_auth = make_rtmp_auth(room_service, user_service, publish_key_service);

    let result = rtmp_auth.validate_play_request(room.id.as_str()).await;
    assert!(
        result.is_ok(),
        "Expected play to be allowed for active room with rtmp_player enabled"
    );
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_on_play_rejects_banned_room() {
    let (_container, pool) = create_test_pool().await;
    let room_service = Arc::new(make_room_service(pool.clone()));
    let user_service = Arc::new(make_user_service(pool.clone()));
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    // Create owner and room
    let owner = user_repo.create(&make_user("banned_owner")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Banned Room".to_string(),
            "A banned room".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Enable rtmp_player (should still be rejected due to ban)
    let mut settings = settings_repo.get(&room.id).await.unwrap();
    settings.rtmp_player.0 = true;
    settings_repo
        .set_settings(&room.id, &settings)
        .await
        .unwrap();

    // Ban the room
    room_repo.update_ban_status(&room.id, true).await.unwrap();

    // Create RTMP auth and validate
    let publish_key_service = Arc::new(make_publish_key_service());
    let rtmp_auth = make_rtmp_auth(room_service, user_service, publish_key_service);

    let result = rtmp_auth.validate_play_request(room.id.as_str()).await;
    assert!(
        result.is_err(),
        "Expected play to be rejected for banned room"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("banned"),
        "Expected error to mention 'banned', got: {err}"
    );
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_on_play_rejects_pending_room() {
    let (_container, pool) = create_test_pool().await;
    let room_service = Arc::new(make_room_service(pool.clone()));
    let user_service = Arc::new(make_user_service(pool.clone()));
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    // Create owner and room
    let owner = user_repo.create(&make_user("pending_owner")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Pending Room".to_string(),
            "A pending room".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Enable rtmp_player (should still be rejected due to pending status)
    let mut settings = settings_repo.get(&room.id).await.unwrap();
    settings.rtmp_player.0 = true;
    settings_repo
        .set_settings(&room.id, &settings)
        .await
        .unwrap();

    // Set room to pending status
    room_repo
        .update_status(&room.id, RoomStatus::Pending)
        .await
        .unwrap();

    // Create RTMP auth and validate
    let publish_key_service = Arc::new(make_publish_key_service());
    let rtmp_auth = make_rtmp_auth(room_service, user_service, publish_key_service);

    let result = rtmp_auth.validate_play_request(room.id.as_str()).await;
    assert!(
        result.is_err(),
        "Expected play to be rejected for pending room"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("pending"),
        "Expected error to mention 'pending', got: {err}"
    );
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_on_play_rejects_rejected_room() {
    let (_container, pool) = create_test_pool().await;
    let room_service = Arc::new(make_room_service(pool.clone()));
    let user_service = Arc::new(make_user_service(pool.clone()));
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("rejected_owner"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Rejected Room".to_string(),
            "A rejected room".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut settings = settings_repo.get(&room.id).await.unwrap();
    settings.rtmp_player.0 = true;
    settings_repo
        .set_settings(&room.id, &settings)
        .await
        .unwrap();

    room_repo
        .update_status(&room.id, RoomStatus::Rejected)
        .await
        .unwrap();

    let publish_key_service = Arc::new(make_publish_key_service());
    let rtmp_auth = make_rtmp_auth(room_service, user_service, publish_key_service);

    let result = rtmp_auth.validate_play_request(room.id.as_str()).await;
    assert!(
        result.is_err(),
        "Expected play to be rejected for rejected room"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("rejected"),
        "Expected error to mention 'rejected', got: {err}"
    );
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_on_play_rejects_closed_room() {
    let (_container, pool) = create_test_pool().await;
    let room_service = Arc::new(make_room_service(pool.clone()));
    let user_service = Arc::new(make_user_service(pool.clone()));
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    // Create owner and room
    let owner = user_repo.create(&make_user("closed_owner")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Closed Room".to_string(),
            "A closed room".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Enable rtmp_player (should still be rejected due to closed status)
    let mut settings = settings_repo.get(&room.id).await.unwrap();
    settings.rtmp_player.0 = true;
    settings_repo
        .set_settings(&room.id, &settings)
        .await
        .unwrap();

    // Set room to closed status
    room_repo
        .update_status(&room.id, RoomStatus::Closed)
        .await
        .unwrap();

    // Create RTMP auth and validate
    let publish_key_service = Arc::new(make_publish_key_service());
    let rtmp_auth = make_rtmp_auth(room_service, user_service, publish_key_service);

    let result = rtmp_auth.validate_play_request(room.id.as_str()).await;
    assert!(
        result.is_err(),
        "Expected play to be rejected for closed room"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("closed"),
        "Expected error to mention 'closed', got: {err}"
    );
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_on_play_rejects_when_rtmp_player_disabled() {
    let (_container, pool) = create_test_pool().await;
    let room_service = Arc::new(make_room_service(pool.clone()));
    let user_service = Arc::new(make_user_service(pool.clone()));
    let user_repo = UserRepository::new(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    // Create owner and room
    let owner = user_repo
        .create(&make_user("disabled_owner"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Disabled RTMP Room".to_string(),
            "A room with RTMP disabled".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Explicitly disable rtmp_player (default is disabled, but let's be explicit)
    let mut settings = settings_repo.get(&room.id).await.unwrap();
    settings.rtmp_player.0 = false;
    settings_repo
        .set_settings(&room.id, &settings)
        .await
        .unwrap();

    // Create RTMP auth and validate
    let publish_key_service = Arc::new(make_publish_key_service());
    let rtmp_auth = make_rtmp_auth(room_service, user_service, publish_key_service);

    let result = rtmp_auth.validate_play_request(room.id.as_str()).await;
    assert!(
        result.is_err(),
        "Expected play to be rejected when rtmp_player is disabled"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("rtmp_player") || err.contains("HTTP-FLV"),
        "Expected error to mention 'rtmp_player' or 'HTTP-FLV', got: {err}"
    );
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_on_play_rejects_nonexistent_room() {
    let (_container, pool) = create_test_pool().await;
    let room_service = Arc::new(make_room_service(pool.clone()));
    let user_service = Arc::new(make_user_service(pool.clone()));

    // Create RTMP auth and validate with a nonexistent room ID
    let publish_key_service = Arc::new(make_publish_key_service());
    let rtmp_auth = make_rtmp_auth(room_service, user_service, publish_key_service);

    let fake_room_id = RoomId::new();
    let result = rtmp_auth.validate_play_request(fake_room_id.as_str()).await;
    assert!(
        result.is_err(),
        "Expected play to be rejected for nonexistent room"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Failed to load room"),
        "Expected error to mention 'Failed to load room', got: {err}"
    );
    pool.close().await;
}

// Allow unused variable warning for container
#[allow(dead_code)]
type TestContainer = ContainerAsync<Postgres>;
