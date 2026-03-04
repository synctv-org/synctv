//! `ChatService` integration tests
//!
//! Tests `send_message` permission check, `chat_enabled` setting, rate limit mapping,
//! `danmaku_enabled` check, and `delete_message` permission logic with real `PostgreSQL`
//! via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test `chat_service_full_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        room_settings::{ChatEnabled, DanmakuEnabled},
        DanmakuPosition, PermissionBits, RoomId, RoomSettings, SendDanmakuRequest, User, UserId,
        UserRole, UserStatus,
    },
    repository::{
        ChatRepository, RoomMemberRepository, RoomRepository, RoomSettingsRepository,
        UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService},
        notification::{EventBroadcaster, RoomEvent},
        ChatService, ContentFilter, InMemoryTokenBlacklistStore, NotificationService,
        PermissionService, RateLimitConfig, RateLimiter, RoomService, RoomSettingsService,
        UserService,
    },
    Error, Result,
};
use synctv_core_testing::create_test_pool;
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

fn make_chat_service_with_config(
    pool: PgPool,
    rate_limit_config: RateLimitConfig,
) -> (ChatService, UsernameCache) {
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let rate_limiter = RateLimiter::new(None, "test:chat:".to_string());
    let content_filter = ContentFilter::new();
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);

    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool);

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

fn make_chat_service(pool: PgPool) -> (ChatService, UsernameCache) {
    make_chat_service_with_config(pool, RateLimitConfig::default())
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

    let creator = user_repo
        .create(&make_user("chat_perm_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("chat_perm_member"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();
    username_cache
        .set(&member.id, &member.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Chat Perm Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    // Revoke SEND_CHAT permission from the member
    room_service
        .member_service()
        .revoke_permission(
            room.id.clone(),
            creator.id.clone(),
            member.id.clone(),
            PermissionBits::SEND_CHAT,
        )
        .await
        .unwrap();

    // Attempt to send a message -- should fail
    let result = chat_service
        .send_message(room.id.clone(), member.id.clone(), "Hello".to_string())
        .await;

    assert!(
        result.is_err(),
        "send_message should fail without SEND_CHAT permission"
    );
    match result.unwrap_err() {
        Error::Authorization(_) => {}
        other => panic!("Expected Authorization error, got: {other:?}"),
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

    let creator = user_repo
        .create(&make_user("chatdis_creator"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    // Create room with chat disabled
    let settings = RoomSettings {
        chat_enabled: ChatEnabled(false),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Chat Disabled".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    // Creator has all permissions, but chat is disabled for the room
    let result = chat_service
        .send_message(room.id.clone(), creator.id.clone(), "Hello".to_string())
        .await;

    assert!(
        result.is_err(),
        "send_message should fail when chat is disabled"
    );
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("disabled") || msg.contains("Chat"),
                "Error should mention chat disabled: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

// ========== send_message: rate limit mapping ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_rate_limit_triggers() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("chatrl_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Chat RL Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
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
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

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
            .send_message(room.id.clone(), creator.id.clone(), format!("msg{i}"))
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

    let creator = user_repo
        .create(&make_user("danmakudis_creator"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    // Create room with danmaku disabled
    let settings = RoomSettings {
        danmaku_enabled: DanmakuEnabled(false),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Danmaku Disabled".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            Some(settings),
        )
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

    assert!(
        result.is_err(),
        "send_danmaku should fail when danmaku is disabled"
    );
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("disabled") || msg.contains("Danmaku"),
                "Error should mention danmaku disabled: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
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
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Del Msg Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Send a message
    let msg = chat_service
        .send_message(room.id.clone(), creator.id.clone(), "Delete me".to_string())
        .await
        .unwrap();

    // Owner should be able to delete their own message
    let result = chat_service.delete_message(&msg.id, &creator.id).await;

    assert!(
        result.is_ok(),
        "Owner should be able to delete their own message"
    );
}

// ========== delete_message: non-owner requires DELETE_CHAT permission ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_non_owner_requires_delete_chat_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("delmsg_creator"))
        .await
        .unwrap();
    let member = user_repo.create(&make_user("delmsg_member")).await.unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();
    username_cache
        .set(&member.id, &member.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Del Msg Perm Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    // Creator sends a message
    let msg = chat_service
        .send_message(
            room.id.clone(),
            creator.id.clone(),
            "Protected msg".to_string(),
        )
        .await
        .unwrap();

    // Member (non-owner without DELETE_CHAT) tries to delete -- should fail
    let result = chat_service.delete_message(&msg.id, &member.id).await;

    assert!(
        result.is_err(),
        "Non-owner without DELETE_CHAT should be denied"
    );
    match result.unwrap_err() {
        Error::Authorization(_) => {}
        other => panic!("Expected Authorization error, got: {other:?}"),
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

    let creator = user_repo
        .create(&make_user("delmsg2_creator"))
        .await
        .unwrap();
    let admin = user_repo.create(&make_user("delmsg2_admin")).await.unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();
    username_cache
        .set(&admin.id, &admin.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Del Msg Admin Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), admin.id.clone(), None)
        .await
        .unwrap();

    // Grant DELETE_CHAT permission to admin
    room_service
        .member_service()
        .grant_permission(
            room.id.clone(),
            creator.id.clone(),
            admin.id.clone(),
            PermissionBits::DELETE_CHAT,
        )
        .await
        .unwrap();

    // Creator sends a message
    let msg = chat_service
        .send_message(
            room.id.clone(),
            creator.id.clone(),
            "Deletable msg".to_string(),
        )
        .await
        .unwrap();

    // Admin (with DELETE_CHAT) can delete another user's message
    let result = chat_service.delete_message(&msg.id, &admin.id).await;

    assert!(
        result.is_ok(),
        "Non-owner with DELETE_CHAT should be able to delete"
    );
}

// ========== send_message: notification broadcast ==========

/// Mock broadcaster that tracks broadcast calls
struct CountingMockBroadcaster {
    broadcast_count: AtomicUsize,
    last_room_id: std::sync::Mutex<Option<String>>,
    last_event_type: std::sync::Mutex<Option<String>>,
}

impl CountingMockBroadcaster {
    const fn new() -> Self {
        Self {
            broadcast_count: AtomicUsize::new(0),
            last_room_id: std::sync::Mutex::new(None),
            last_event_type: std::sync::Mutex::new(None),
        }
    }

    fn get_broadcast_count(&self) -> usize {
        self.broadcast_count.load(Ordering::SeqCst)
    }

    fn get_last_room_id(&self) -> Option<String> {
        self.last_room_id.lock().unwrap().clone()
    }

    fn get_last_event_type(&self) -> Option<String> {
        self.last_event_type.lock().unwrap().clone()
    }
}

#[async_trait]
impl EventBroadcaster for CountingMockBroadcaster {
    async fn broadcast_to_room(&self, room_id: &RoomId, event: &RoomEvent) -> Result<usize> {
        self.broadcast_count.fetch_add(1, Ordering::SeqCst);
        *self.last_room_id.lock().unwrap() = Some(room_id.as_str().to_string());
        *self.last_event_type.lock().unwrap() = Some(event.event_type().to_string());
        Ok(1)
    }

    async fn send_to_user(
        &self,
        _room_id: &RoomId,
        _user_id: &UserId,
        _event: &RoomEvent,
    ) -> Result<bool> {
        Ok(true)
    }

    async fn broadcast_to_cluster(&self, _room_id: &RoomId, _event: &RoomEvent) -> Result<bool> {
        Ok(true)
    }
}

/// Helper function to create `ChatService` with a counting mock broadcaster
#[allow(dead_code)]
fn make_chat_service_with_broadcaster(
    pool: PgPool,
    broadcaster: Arc<CountingMockBroadcaster>,
) -> (ChatService, UsernameCache) {
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let rate_limiter = RateLimiter::new(None, "test:chat:".to_string());
    let rate_limit_config = RateLimitConfig::default();
    let content_filter = ContentFilter::new();
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);

    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool);

    let mut permission_service = PermissionService::new(
        member_repo,
        room_repo,
        None,
        PermissionService::DEFAULT_CACHE_SIZE,
        PermissionService::DEFAULT_CACHE_TTL_SECS,
    );
    permission_service.set_room_settings_repo(room_settings_repo.clone());

    // Create NotificationService with the counting broadcaster
    let notification_service = NotificationService::new(broadcaster as Arc<dyn EventBroadcaster>);
    let room_settings_service = RoomSettingsService::new(
        room_settings_repo,
        None,
        Arc::new(notification_service.clone()),
        None,
        None,
        None,
    );

    let mut chat_service = ChatService::new(
        chat_repo,
        rate_limiter,
        rate_limit_config,
        content_filter,
        username_cache.clone(),
        permission_service,
        room_settings_service,
    );

    // Set the notification service for broadcasting chat messages
    chat_service.set_notification_service(notification_service);

    (chat_service, username_cache)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_broadcasts_to_room_members() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("chat_broadcast_creator"))
        .await
        .unwrap();

    // Create a counting mock broadcaster
    let broadcaster = Arc::new(CountingMockBroadcaster::new());

    // Build chat service with the counting broadcaster
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let rate_limiter = RateLimiter::new(None, "test:chat_broadcast:".to_string());
    let rate_limit_config = RateLimitConfig::default();
    let content_filter = ContentFilter::new();
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

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

    // Create NotificationService with the counting broadcaster
    let notification_service =
        NotificationService::new(broadcaster.clone() as Arc<dyn EventBroadcaster>);
    let room_settings_service = RoomSettingsService::new(
        room_settings_repo,
        None,
        Arc::new(notification_service.clone()),
        None,
        None,
        None,
    );

    let mut chat_service = ChatService::new(
        chat_repo,
        rate_limiter,
        rate_limit_config,
        content_filter,
        username_cache,
        permission_service,
        room_settings_service,
    );

    // Set the notification service for broadcasting chat messages
    chat_service.set_notification_service(notification_service);

    // Create a room
    let (room, _) = room_service
        .create_room(
            "Broadcast Test Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Verify no broadcasts before sending
    assert_eq!(
        broadcaster.get_broadcast_count(),
        0,
        "No broadcasts should have occurred yet"
    );

    // Send a message
    let msg = chat_service
        .send_message(
            room.id.clone(),
            creator.id.clone(),
            "Hello, world!".to_string(),
        )
        .await
        .expect("send_message should succeed");

    // Verify broadcast was triggered
    assert_eq!(
        broadcaster.get_broadcast_count(),
        1,
        "One broadcast should have been triggered"
    );

    // Verify broadcast was sent to the correct room
    let last_room_id = broadcaster
        .get_last_room_id()
        .expect("Should have a room ID");
    assert_eq!(
        last_room_id,
        room.id.as_str(),
        "Broadcast should be sent to the correct room"
    );

    // Verify broadcast was a chat message event
    let last_event_type = broadcaster
        .get_last_event_type()
        .expect("Should have an event type");
    assert_eq!(
        last_event_type, "chat_message",
        "Event type should be chat_message"
    );

    // Verify the message was persisted
    assert!(!msg.id.is_empty(), "Message should have an ID");
    assert_eq!(msg.content, "Hello, world!", "Message content should match");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_without_notification_service_still_persists() {
    // This test verifies that when notification_service is not set,
    // the message is still persisted (the broadcast just silently does nothing)

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("chat_no_notify"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    // Create a room
    let (room, _) = room_service
        .create_room(
            "No Notify Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Note: The default ChatService from make_chat_service does NOT have
    // notification_service set (it's None), so broadcast will be skipped

    // Send a message - should still succeed even without notification service
    let msg = chat_service
        .send_message(
            room.id.clone(),
            creator.id.clone(),
            "Message without broadcast".to_string(),
        )
        .await
        .expect("send_message should succeed even without notification service");

    // Verify the message was persisted
    assert!(!msg.id.is_empty(), "Message should have an ID");
    assert_eq!(
        msg.content, "Message without broadcast",
        "Message content should match"
    );

    // Verify we can retrieve the message from history
    let (history, next_cursor) = chat_service
        .get_history(&room.id, None, 10)
        .await
        .expect("get_history should succeed");

    assert_eq!(history.len(), 1, "Should have one message in history");
    assert_eq!(
        history[0].content, "Message without broadcast",
        "History message content should match"
    );
    assert!(
        next_cursor.is_none(),
        "No next cursor when all messages fit in one page"
    );
}

// ========== get_history: cursor pagination behavior ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_history_cursor_pagination_basic() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo.create(&make_user("cursor_user")).await.unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Cursor Pagination Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Send 5 messages with slight delays to ensure distinct timestamps
    let mut sent_messages = Vec::new();
    for i in 0..5 {
        let msg = chat_service
            .send_message(room.id.clone(), creator.id.clone(), format!("message_{i}"))
            .await
            .unwrap();
        sent_messages.push(msg);
        // Small delay to ensure ordering by timestamp
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Page 1: Get first 2 messages (newest first, no cursor)
    let (page1, cursor1) = chat_service
        .get_history(&room.id, None, 2)
        .await
        .expect("get_history page 1 should succeed");

    assert_eq!(page1.len(), 2, "Page 1 should have 2 messages");
    assert!(
        cursor1.is_some(),
        "Should have next cursor when more messages exist"
    );
    // Newest messages first: message_4, message_3
    assert_eq!(
        page1[0].content, "message_4",
        "First message should be newest"
    );
    assert_eq!(
        page1[1].content, "message_3",
        "Second message should be second newest"
    );

    // Page 2: Get next 2 messages using cursor
    let cursor1_val = cursor1.unwrap();
    let (page2, cursor2) = chat_service
        .get_history(&room.id, Some((cursor1_val.0, &cursor1_val.1)), 2)
        .await
        .expect("get_history page 2 should succeed");

    assert_eq!(page2.len(), 2, "Page 2 should have 2 messages");
    assert!(
        cursor2.is_some(),
        "Should have next cursor (1 more message)"
    );
    assert_eq!(page2[0].content, "message_2", "Page 2 first message");
    assert_eq!(page2[1].content, "message_1", "Page 2 second message");

    // Page 3: Get last message
    let cursor2_val = cursor2.unwrap();
    let (page3, cursor3) = chat_service
        .get_history(&room.id, Some((cursor2_val.0, &cursor2_val.1)), 2)
        .await
        .expect("get_history page 3 should succeed");

    assert_eq!(page3.len(), 1, "Page 3 should have 1 message (last page)");
    assert!(cursor3.is_none(), "No next cursor on last page");
    assert_eq!(
        page3[0].content, "message_0",
        "Page 3 should have oldest message"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_history_empty_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, _username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("empty_room_user"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Empty Chat Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Get history from room with no messages
    let (history, cursor) = chat_service
        .get_history(&room.id, None, 10)
        .await
        .expect("get_history should succeed for empty room");

    assert!(
        history.is_empty(),
        "History should be empty for room with no messages"
    );
    assert!(cursor.is_none(), "No cursor when room is empty");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_history_single_page() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("single_page_user"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Single Page Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Send 3 messages
    for i in 0..3 {
        chat_service
            .send_message(room.id.clone(), creator.id.clone(), format!("msg_{i}"))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Get history with limit larger than message count
    let (history, cursor) = chat_service
        .get_history(&room.id, None, 100)
        .await
        .expect("get_history should succeed");

    assert_eq!(history.len(), 3, "Should get all 3 messages");
    assert!(
        cursor.is_none(),
        "No cursor when all messages fit in one page"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_history_limit_capped_at_100() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    // Use a high rate limit config to avoid hitting rate limits during bulk insert
    let rate_limit_config = RateLimitConfig {
        chat_per_second: 200,
        danmaku_per_second: 50,
        window_seconds: 1,
    };
    let (chat_service, username_cache) =
        make_chat_service_with_config(pool.clone(), rate_limit_config);

    let creator = user_repo
        .create(&make_user("limit_cap_user"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Limit Cap Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Send 105 messages
    for i in 0..105 {
        chat_service
            .send_message(room.id.clone(), creator.id.clone(), format!("msg_{i}"))
            .await
            .unwrap();
    }

    // Request with limit=200, should be capped at 100
    let (history, cursor) = chat_service
        .get_history(&room.id, None, 200)
        .await
        .expect("get_history should succeed");

    assert_eq!(history.len(), 100, "Should be capped at 100 messages");
    assert!(
        cursor.is_some(),
        "Should have cursor since there are more messages"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_history_messages_from_correct_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("room_isolation_user"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    // Create two rooms
    let (room1, _) = room_service
        .create_room(
            "Room 1".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    let (room2, _) = room_service
        .create_room(
            "Room 2".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Send messages to each room
    chat_service
        .send_message(
            room1.id.clone(),
            creator.id.clone(),
            "room1_message".to_string(),
        )
        .await
        .unwrap();
    chat_service
        .send_message(
            room2.id.clone(),
            creator.id.clone(),
            "room2_message".to_string(),
        )
        .await
        .unwrap();

    // Get history from room1 should only return room1 messages
    let (history1, _) = chat_service
        .get_history(&room1.id, None, 10)
        .await
        .expect("get_history for room1 should succeed");

    assert_eq!(history1.len(), 1, "Room1 should have 1 message");
    assert_eq!(
        history1[0].content, "room1_message",
        "Room1 history should only contain room1 messages"
    );

    // Get history from room2 should only return room2 messages
    let (history2, _) = chat_service
        .get_history(&room2.id, None, 10)
        .await
        .expect("get_history for room2 should succeed");

    assert_eq!(history2.len(), 1, "Room2 should have 1 message");
    assert_eq!(
        history2[0].content, "room2_message",
        "Room2 history should only contain room2 messages"
    );
}

// ========== A1: send_danmaku broadcasts to notification service ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_danmaku_broadcasts_to_room_members() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("danmaku_broadcast_creator"))
        .await
        .unwrap();

    // Create a counting mock broadcaster
    let broadcaster = Arc::new(CountingMockBroadcaster::new());

    // Build chat service with the counting broadcaster
    let (chat_service, username_cache) =
        make_chat_service_with_broadcaster(pool.clone(), broadcaster.clone());

    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    // Create a room
    let (room, _) = room_service
        .create_room(
            "Danmaku Broadcast Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Verify no broadcasts before sending
    assert_eq!(
        broadcaster.get_broadcast_count(),
        0,
        "No broadcasts should have occurred yet"
    );

    // Send a danmaku
    let request = SendDanmakuRequest {
        room_id: room.id.clone(),
        content: "Test danmaku".to_string(),
        color: "#FF0000".to_string(),
        position: DanmakuPosition::Top,
    };

    let danmaku = chat_service
        .send_danmaku(room.id.clone(), creator.id.clone(), request)
        .await
        .expect("send_danmaku should succeed");

    // Verify broadcast was triggered
    assert!(
        broadcaster.get_broadcast_count() > 0,
        "At least one broadcast should have been triggered for danmaku"
    );

    // Verify broadcast was sent to the correct room
    let last_room_id = broadcaster
        .get_last_room_id()
        .expect("Should have a room ID");
    assert_eq!(
        last_room_id,
        room.id.as_str(),
        "Broadcast should be sent to the correct room"
    );

    // Verify broadcast was a danmaku event
    let last_event_type = broadcaster
        .get_last_event_type()
        .expect("Should have an event type");
    assert_eq!(last_event_type, "danmaku", "Event type should be danmaku");

    // Verify the danmaku message content
    assert_eq!(
        danmaku.content, "Test danmaku",
        "Danmaku content should match"
    );
    assert_eq!(danmaku.color, "#FF0000", "Danmaku color should match");
}

// ========== A2: ChatMessage.user_id is None for deleted users ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_history_with_deleted_user_returns_none_user_id() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("deleted_user_chat_creator"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Deleted User Chat Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Send a message
    let msg = chat_service
        .send_message(
            room.id.clone(),
            creator.id.clone(),
            "Message from soon-deleted user".to_string(),
        )
        .await
        .unwrap();

    // Verify user_id is Some before deletion
    assert!(
        msg.user_id.is_some(),
        "user_id should be Some before user deletion"
    );

    // Simulate user deletion: SET NULL on user_id via raw SQL
    // (foreign key ON DELETE SET NULL)
    sqlx::query("UPDATE chat_messages SET user_id = NULL WHERE id = $1 AND created_at = $2")
        .bind(&msg.id)
        .bind(msg.created_at)
        .execute(&pool)
        .await
        .unwrap();

    // Retrieve the message from history
    let (history, _) = chat_service
        .get_history(&room.id, None, 10)
        .await
        .expect("get_history should succeed after user deletion");

    assert_eq!(history.len(), 1, "Should still have the message");
    assert!(
        history[0].user_id.is_none(),
        "user_id should be None for deleted user's message"
    );
    assert_eq!(
        history[0].content, "Message from soon-deleted user",
        "Content should be preserved"
    );
}

// ========== C4: Oversized message rejected ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_oversized_content_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("oversized_msg_creator"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Oversized Msg Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Send a message that exceeds MAX_CHAT_MESSAGE_CHARS (500 chars)
    let oversized_content: String = "x".repeat(501);
    let result = chat_service
        .send_message(room.id.clone(), creator.id.clone(), oversized_content)
        .await;

    assert!(result.is_err(), "Oversized message should be rejected");
    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("500") || msg.contains("characters"),
                "Error should mention character limit: {msg}"
            );
        }
        other => panic!("Expected InvalidInput error, got: {other:?}"),
    }
}

// ========== send_message with valid content ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_valid_content_persisted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("valid_msg_creator"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Valid Msg Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let msg = chat_service
        .send_message(
            room.id.clone(),
            creator.id.clone(),
            "Hello, valid message!".to_string(),
        )
        .await
        .expect("Valid message should be persisted");

    assert_eq!(msg.content, "Hello, valid message!");
    assert_eq!(msg.user_id, Some(creator.id.clone()));
    assert_eq!(msg.room_id, room.id);
    assert!(!msg.id.is_empty());

    // Verify via history
    let (history, _) = chat_service.get_history(&room.id, None, 10).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content, "Hello, valid message!");
}

// ========== Content filtering (HTML/XSS stripping) ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_html_xss_stripped() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("xss_strip_creator"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "XSS Strip Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Send message with HTML/XSS payload
    let msg = chat_service
        .send_message(
            room.id.clone(),
            creator.id.clone(),
            "<script>alert('xss')</script>Hello safe world".to_string(),
        )
        .await
        .expect("Message with HTML should be filtered, not rejected");

    // The content filter strips HTML tags, so <script> should be removed
    assert!(
        !msg.content.contains("<script>"),
        "HTML script tags should be stripped from message content: got '{}'",
        msg.content
    );
    assert!(
        msg.content.contains("Hello safe world"),
        "Safe text content should be preserved: got '{}'",
        msg.content
    );
}

// ========== Danmaku without notification service still works ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_danmaku_without_notification_service_still_returns() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("danmaku_no_notify"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Danmaku No Notify Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Note: make_chat_service does NOT set notification_service (it's None)

    let request = SendDanmakuRequest {
        room_id: room.id.clone(),
        content: "Danmaku without notify".to_string(),
        color: "#00FF00".to_string(),
        position: DanmakuPosition::Scroll,
    };

    // Should succeed even without notification service
    let danmaku = chat_service
        .send_danmaku(room.id.clone(), creator.id.clone(), request)
        .await
        .expect("send_danmaku should succeed without notification service");

    assert_eq!(danmaku.content, "Danmaku without notify");
    assert_eq!(danmaku.color, "#00FF00");
    assert_eq!(danmaku.position, DanmakuPosition::Scroll);
}

// ========== Delete message with None user_id (deleted user) ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_with_deleted_user_requires_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(pool.clone());

    let creator = user_repo
        .create(&make_user("del_msg_null_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("del_msg_null_member"))
        .await
        .unwrap();
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .unwrap();
    username_cache
        .set(&member.id, &member.username)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Del Null User Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    // Creator sends a message
    let msg = chat_service
        .send_message(
            room.id.clone(),
            creator.id.clone(),
            "Orphaned message".to_string(),
        )
        .await
        .unwrap();

    // Simulate user deletion: SET user_id to NULL
    sqlx::query("UPDATE chat_messages SET user_id = NULL WHERE id = $1 AND created_at = $2")
        .bind(&msg.id)
        .bind(msg.created_at)
        .execute(&pool)
        .await
        .unwrap();

    // Member (without DELETE_CHAT permission) tries to delete orphaned message
    // Since user_id is NULL, they are not the sender, so they need DELETE_CHAT permission
    let result = chat_service.delete_message(&msg.id, &member.id).await;

    assert!(
        result.is_err(),
        "Non-owner should be denied deletion of orphaned message without DELETE_CHAT permission"
    );
    match result.unwrap_err() {
        Error::Authorization(_) => {}
        other => panic!("Expected Authorization error, got: {other:?}"),
    }

    // Room creator (has all permissions) should be able to delete
    let result = chat_service.delete_message(&msg.id, &creator.id).await;
    assert!(
        result.is_ok(),
        "Room creator (with DELETE_CHAT) should be able to delete orphaned message"
    );
}
