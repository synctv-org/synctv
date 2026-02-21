//! ChatService integration tests
//!
//! Tests send_message permission check, chat_enabled setting, rate limit mapping,
//! danmaku_enabled check, and delete_message permission logic with real PostgreSQL
//! via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test chat_service_full_tests -- --nocapture

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, UsernameCache, NoopCacheL2},
    config::PasswordComplexityConfig,
    models::{
        UserId, User, UserRole, UserStatus,
        PermissionBits, RoomSettings,
        room_settings::{ChatEnabled, DanmakuEnabled},
        SendDanmakuRequest, DanmakuPosition,
    },
    repository::{UserRepository, ChatRepository, RoomMemberRepository, RoomRepository, RoomSettingsRepository},
    service::{
        ChatService, RoomService, UserService, InMemoryTokenBlacklistStore,
        ContentFilter, RateLimiter, RateLimitConfig, PermissionService,
        RoomSettingsService, NotificationService,
        auth::{JwtService, BruteForceProtection},
    },
    Error,
};
use chrono::Utc;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres.get_host_port_ipv4(5432).await.expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

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

fn make_chat_service(pool: PgPool) -> (ChatService, UsernameCache) {
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let rate_limiter = RateLimiter::new(None, "test:chat:".to_string());
    let rate_limit_config = RateLimitConfig::default();
    let content_filter = ContentFilter::new();
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);

    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());

    let mut permission_service = PermissionService::new(
        member_repo,
        room_repo,
        None,
        PermissionService::DEFAULT_CACHE_SIZE,
        PermissionService::DEFAULT_CACHE_TTL_SECS,
    );
    permission_service.set_room_settings_repo(room_settings_repo.clone());

    let notification_service = Arc::new(NotificationService::default());
    let room_settings_service = RoomSettingsService::new(
        room_settings_repo,
        None,
        notification_service,
        None,
        None,
        None,
    );

    let service = ChatService::new(
        chat_repo,
        rate_limiter,
        rate_limit_config,
        content_filter,
        username_cache.clone(),
        permission_service,
        room_settings_service,
    );
    (service, username_cache)
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{}@test.com", username)),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: None,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

// ========== send_message: SEND_CHAT permission check ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_without_send_chat_permission_denied() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo.create(&make_user("chat_perm_creator")).await.unwrap();
    let member = user_repo.create(&make_user("chat_perm_member")).await.unwrap();
    username_cache.set(&creator.id, &creator.username).await.unwrap();
    username_cache.set(&member.id, &member.username).await.unwrap();

    let (room, _) = room_service
        .create_room("Chat Perm Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), member.id.clone(), None).await.unwrap();

    // Revoke SEND_CHAT permission from the member
    room_service.member_service().revoke_permission(
        room.id.clone(),
        creator.id.clone(),
        member.id.clone(),
        PermissionBits::SEND_CHAT,
    ).await.unwrap();

    // Attempt to send a message -- should fail
    let result = chat_service
        .send_message(room.id.clone(), member.id.clone(), "Hello".to_string())
        .await;

    assert!(result.is_err(), "send_message should fail without SEND_CHAT permission");
    match result.unwrap_err() {
        Error::Authorization(_) => {}
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

// ========== send_message: chat_enabled room setting ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_chat_disabled_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo.create(&make_user("chatdis_creator")).await.unwrap();
    username_cache.set(&creator.id, &creator.username).await.unwrap();

    // Create room with chat disabled
    let mut settings = RoomSettings::default();
    settings.chat_enabled = ChatEnabled(false);
    let (room, _) = room_service
        .create_room("Chat Disabled".to_string(), String::new(), creator.id.clone(), None, Some(settings))
        .await
        .unwrap();

    // Creator has all permissions, but chat is disabled for the room
    let result = chat_service
        .send_message(room.id.clone(), creator.id.clone(), "Hello".to_string())
        .await;

    assert!(result.is_err(), "send_message should fail when chat is disabled");
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(msg.contains("disabled") || msg.contains("Chat"), "Error should mention chat disabled: {}", msg);
        }
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

// ========== send_message: rate limit mapping ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_rate_limit_triggers() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("chatrl_creator")).await.unwrap();

    let (room, _) = room_service
        .create_room("Chat RL Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    // Build chat service with very restrictive rate limit (1 msg/sec, 1 sec window)
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let rate_limiter = RateLimiter::new(None, "test:chatrl:".to_string());
    let rate_limit_config = RateLimitConfig {
        chat_per_second: 1,
        danmaku_per_second: 1,
        window_seconds: 1,
    };
    let content_filter = ContentFilter::new();
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    username_cache.set(&creator.id, &creator.username).await.unwrap();

    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());

    let mut permission_service = PermissionService::new(
        member_repo,
        room_repo,
        None,
        PermissionService::DEFAULT_CACHE_SIZE,
        PermissionService::DEFAULT_CACHE_TTL_SECS,
    );
    permission_service.set_room_settings_repo(room_settings_repo.clone());

    let notification_service = Arc::new(NotificationService::default());
    let room_settings_service = RoomSettingsService::new(
        room_settings_repo,
        None,
        notification_service,
        None,
        None,
        None,
    );

    let chat_service = ChatService::new(
        chat_repo,
        rate_limiter,
        rate_limit_config,
        content_filter,
        username_cache,
        permission_service,
        room_settings_service,
    );

    // Send messages rapidly -- at least one should hit the rate limit
    let mut rate_limited = false;
    for i in 0..20 {
        let result = chat_service
            .send_message(room.id.clone(), creator.id.clone(), format!("msg{}", i))
            .await;
        if let Err(Error::RateLimited(_)) = &result {
            rate_limited = true;
            break;
        }
    }

    assert!(rate_limited, "Should hit rate limit after rapid messages");
}

// ========== send_danmaku: danmaku_enabled setting ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_danmaku_disabled_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo.create(&make_user("danmakudis_creator")).await.unwrap();
    username_cache.set(&creator.id, &creator.username).await.unwrap();

    // Create room with danmaku disabled
    let mut settings = RoomSettings::default();
    settings.danmaku_enabled = DanmakuEnabled(false);
    let (room, _) = room_service
        .create_room("Danmaku Disabled".to_string(), String::new(), creator.id.clone(), None, Some(settings))
        .await
        .unwrap();

    let request = SendDanmakuRequest {
        room_id: room.id.clone(),
        content: "Hello".to_string(),
        color: "#FFFFFF".to_string(),
        position: DanmakuPosition::Scroll,
    };

    let result = chat_service
        .send_danmaku(room.id.clone(), creator.id.clone(), request)
        .await;

    assert!(result.is_err(), "send_danmaku should fail when danmaku is disabled");
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(msg.contains("disabled") || msg.contains("Danmaku"), "Error should mention danmaku disabled: {}", msg);
        }
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

// ========== delete_message: owner can delete own message ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_owner_can_delete_own() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo.create(&make_user("delmsg_owner")).await.unwrap();
    username_cache.set(&creator.id, &creator.username).await.unwrap();

    let (room, _) = room_service
        .create_room("Del Msg Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    // Send a message
    let msg = chat_service
        .send_message(room.id.clone(), creator.id.clone(), "Delete me".to_string())
        .await
        .unwrap();

    // Owner should be able to delete their own message
    let result = chat_service
        .delete_message(&msg.id, &creator.id)
        .await;

    assert!(result.is_ok(), "Owner should be able to delete their own message");
}

// ========== delete_message: non-owner requires DELETE_CHAT permission ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_non_owner_requires_delete_chat_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo.create(&make_user("delmsg_creator")).await.unwrap();
    let member = user_repo.create(&make_user("delmsg_member")).await.unwrap();
    username_cache.set(&creator.id, &creator.username).await.unwrap();
    username_cache.set(&member.id, &member.username).await.unwrap();

    let (room, _) = room_service
        .create_room("Del Msg Perm Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), member.id.clone(), None).await.unwrap();

    // Creator sends a message
    let msg = chat_service
        .send_message(room.id.clone(), creator.id.clone(), "Protected msg".to_string())
        .await
        .unwrap();

    // Member (non-owner without DELETE_CHAT) tries to delete -- should fail
    let result = chat_service
        .delete_message(&msg.id, &member.id)
        .await;

    assert!(result.is_err(), "Non-owner without DELETE_CHAT should be denied");
    match result.unwrap_err() {
        Error::Authorization(_) => {}
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

// ========== delete_message: non-owner WITH DELETE_CHAT permission succeeds ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_non_owner_with_delete_chat_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo.create(&make_user("delmsg2_creator")).await.unwrap();
    let admin = user_repo.create(&make_user("delmsg2_admin")).await.unwrap();
    username_cache.set(&creator.id, &creator.username).await.unwrap();
    username_cache.set(&admin.id, &admin.username).await.unwrap();

    let (room, _) = room_service
        .create_room("Del Msg Admin Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), admin.id.clone(), None).await.unwrap();

    // Grant DELETE_CHAT permission to admin
    room_service.member_service().grant_permission(
        room.id.clone(),
        creator.id.clone(),
        admin.id.clone(),
        PermissionBits::DELETE_CHAT,
    ).await.unwrap();

    // Creator sends a message
    let msg = chat_service
        .send_message(room.id.clone(), creator.id.clone(), "Deletable msg".to_string())
        .await
        .unwrap();

    // Admin (with DELETE_CHAT) can delete another user's message
    let result = chat_service
        .delete_message(&msg.id, &admin.id)
        .await;

    assert!(result.is_ok(), "Non-owner with DELETE_CHAT should be able to delete");
}
