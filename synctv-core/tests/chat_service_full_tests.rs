//! `ChatService` integration tests
//!
//! Tests `send_message` permission check, `chat_enabled` setting, rate limit mapping,
//! and `delete_message` permission logic with real `PostgreSQL`
//! via testcontainers.
//!
use std::sync::Arc;

use chrono::Utc;
use sha2::Digest;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        room_settings::ChatEnabled, AuditAction, AuditTargetType, ChatEventKind, ChatMessage,
        ChatMessageStatus, ChatMessageType, DeleteChatMessage, EditChatMessage,
        FileReferenceTarget, NewStoredFile, RoomAdminPermissionBits, RoomMemberPermissionBits,
        RoomSettings, SendChatMessage, User, UserId, UserRole, UserStatus,
    },
    repository::{
        ChatRepository, FileStorageRepository, RoomMemberRepository, RoomRepository,
        RoomSettingsRepository, UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService},
        chat::{ChatDependencies, ChatRuntime},
        file_storage::{FileStorageCleanupOrigin, FileStorageService},
        AuditService, ChatService, ContentFilter, DisabledFileStorageService,
        InMemoryTokenBlacklistStore, NotificationService, PermissionService, RateLimitConfig,
        RateLimiter, RequestRateLimiterService, RoomService, RoomSettingsService, UserService,
    },
    Error,
};
use synctv_core_testing::{create_test_pool, TestOptionExt, TestResultExt};

fn make_user_service(pool: &PgPool) -> UserService {
    make_user_service_with_username_cache(
        pool,
        UsernameCache::local_only("test:username:".to_string(), 100, 60),
    )
}

fn make_user_service_with_username_cache(
    pool: &PgPool,
    username_cache: UsernameCache,
) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).checked("Failed to create JwtService");
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);

    RoomService::new_for_tests(pool, user_service).checked("room service should build")
}

fn make_chat_service_with_config(
    pool: &PgPool,
    rate_limit_config: RateLimitConfig,
) -> (ChatService, UsernameCache) {
    make_chat_service_with_options(
        pool,
        rate_limit_config,
        None,
        Arc::new(DisabledFileStorageService),
    )
}

fn make_chat_service_with_config_and_storage(
    pool: &PgPool,
    rate_limit_config: RateLimitConfig,
    file_storage_service: Arc<dyn FileStorageService>,
) -> (ChatService, UsernameCache) {
    make_chat_service_with_options(pool, rate_limit_config, None, file_storage_service)
}

fn make_chat_service_with_config_and_audit(
    pool: &PgPool,
    rate_limit_config: RateLimitConfig,
    audit_service: Option<Arc<AuditService>>,
) -> (ChatService, UsernameCache) {
    make_chat_service_with_options(
        pool,
        rate_limit_config,
        audit_service,
        Arc::new(DisabledFileStorageService),
    )
}

fn make_chat_service_with_options(
    pool: &PgPool,
    rate_limit_config: RateLimitConfig,
    audit_service: Option<Arc<AuditService>>,
    file_storage_service: Arc<dyn FileStorageService>,
) -> (ChatService, UsernameCache) {
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let rate_limiter: Arc<dyn RequestRateLimiterService> =
        Arc::new(RateLimiter::local_only("test:chat:".to_string()));
    let content_filter = ContentFilter::new();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);

    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());

    let permission_service = PermissionService::new_with_runtime(
        member_repo,
        room_repo,
        synctv_core::service::permission::PermissionServiceRuntime {
            room_settings_repo: Some(room_settings_repo.clone()),
            ..synctv_core::service::permission::PermissionServiceRuntime::local_only()
        },
    )
    .checked("permission service should build");

    let notification_service = NotificationService::default();
    let room_settings_service = RoomSettingsService::new(
        room_settings_repo,
        None,
        Arc::new(notification_service.clone()),
        None,
        None,
    );

    let service = ChatService::new(
        chat_repo,
        ChatRuntime {
            rate_limiter,
            rate_limit_config,
            content_filter,
        },
        ChatDependencies {
            permission_service,
            room_settings_service,
            user_service: Arc::new(make_user_service_with_username_cache(
                pool,
                username_cache.clone(),
            )),
            file_storage_service,
            audit_service,
            notification_service,
        },
    );
    (service, username_cache)
}

fn make_chat_service(pool: &PgPool) -> (ChatService, UsernameCache) {
    make_chat_service_with_config(pool, RateLimitConfig::default())
}

fn make_chat_service_with_database_storage(pool: &PgPool) -> (ChatService, UsernameCache) {
    make_chat_service_with_config_and_storage(
        pool,
        RateLimitConfig::default(),
        Arc::new(
            synctv_core::service::file_storage::DatabaseFileStorageService::new(
                "database",
                Arc::new(FileStorageRepository::new(pool.clone())),
                "test-file-storage-secret",
            ),
        ),
    )
}

async fn upload_chat_image_file(
    chat_service: &ChatService,
    session: &synctv_core::models::FileUploadSession,
    payload: Vec<u8>,
) {
    let upload_url = session
        .upload_url
        .as_deref()
        .checked("database upload url should be returned");
    let parsed = url::Url::parse(&format!("http://localhost{upload_url}"))
        .checked("relative database object URL should parse with base");
    let encoded_object_key = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .checked("encoded object key path segment should exist");
    let upload_token = session
        .upload_headers
        .get(synctv_core::service::file_storage::FILE_UPLOAD_TOKEN_HEADER)
        .checked("database upload token header should be returned");
    chat_service
        .store_image_upload_object(
            encoded_object_key,
            upload_token,
            Some("image/webp"),
            payload,
        )
        .await
        .checked("database image object should store");
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

fn checked_idempotent_insert_event(
    result: synctv_core::Result<synctv_core::repository::chat::IdempotentChatEventInsert>,
) -> synctv_core::models::ChatMessageEventLog {
    match result {
        Ok(insert) => insert.event,
        Err(error) => std::panic::panic_any(format!(
            "idempotent chat event insert should succeed: {error:?}"
        )),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_without_chat_permission_denied() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service_with_database_storage(&pool);

    let creator = user_repo
        .create(&make_user("chat_perm_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("chat_perm_member"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&member.id, &member.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Chat Perm Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");

    // Revoke CHAT permission from the member
    room_service
        .member_service()
        .revoke_permission(
            room.id,
            creator.id,
            member.id,
            RoomMemberPermissionBits::CHAT,
        )
        .await
        .checked("test operation should succeed");

    let result = chat_service
        .send_message(room.id, member.id, "Hello".to_string())
        .await;

    assert!(
        result.is_err(),
        "send_message should fail without CHAT permission"
    );
    match result.failed("operation should fail") {
        Error::Authorization(_) => {}
        other => std::panic::panic_any(format!("expected Authorization error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_chat_disabled_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("chatdis_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let settings = RoomSettings {
        chat_enabled: ChatEnabled(false),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Chat Disabled".to_string(),
            String::new(),
            creator.id,
            None,
            Some(settings),
        )
        .await
        .checked("test operation should succeed");

    // Creator has all permissions, but chat is disabled for the room
    let result = chat_service
        .send_message(room.id, creator.id, "Hello".to_string())
        .await;

    assert!(
        result.is_err(),
        "send_message should fail when chat is disabled"
    );
    match result.failed("operation should fail") {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("disabled") || msg.contains("Chat"),
                "Error should mention chat disabled: {msg}"
            );
        }
        other => std::panic::panic_any(format!("expected Authorization error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_rate_limit_triggers() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("chatrl_creator"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Chat RL Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    // Build chat service with very restrictive rate limit (1 msg/sec, 1 sec window)
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let rate_limiter: Arc<dyn RequestRateLimiterService> =
        Arc::new(RateLimiter::local_only("test:chatrl:".to_string()));
    let rate_limit_config = RateLimitConfig {
        chat_per_second: 1,
        window_seconds: 1,
    };
    let content_filter = ContentFilter::new();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());

    let permission_service = PermissionService::new_with_runtime(
        member_repo,
        room_repo,
        synctv_core::service::permission::PermissionServiceRuntime {
            room_settings_repo: Some(room_settings_repo.clone()),
            ..synctv_core::service::permission::PermissionServiceRuntime::local_only()
        },
    )
    .checked("permission service should build");

    let notification_service = Arc::new(NotificationService::default());
    let room_settings_service =
        RoomSettingsService::new(room_settings_repo, None, notification_service, None, None);

    let chat_service = ChatService::new(
        chat_repo,
        ChatRuntime {
            rate_limiter,
            rate_limit_config,
            content_filter,
        },
        ChatDependencies {
            permission_service,
            room_settings_service,
            user_service: Arc::new(make_user_service_with_username_cache(&pool, username_cache)),
            file_storage_service: Arc::new(DisabledFileStorageService),
            audit_service: None,
            notification_service: NotificationService::default(),
        },
    );

    let mut rate_limited = false;
    for i in 0..20 {
        let result = chat_service
            .send_message(room.id, creator.id, format!("msg{i}"))
            .await;
        if let Err(Error::RateLimited(_)) = &result {
            rate_limited = true;
            break;
        }
    }

    assert!(rate_limited, "Should hit rate limit after rapid messages");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_owner_can_delete_own() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("delmsg_owner"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Del Msg Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let msg = chat_service
        .send_message(room.id, creator.id, "Delete me".to_string())
        .await
        .checked("test operation should succeed");

    // Owner should be able to delete their own message
    let result = chat_service
        .delete_message(&room.id, msg.id, &creator.id)
        .await;

    assert!(
        result.is_ok(),
        "Owner should be able to delete their own message"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_non_owner_requires_delete_chat_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("delmsg_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("delmsg_member"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&member.id, &member.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Del Msg Perm Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");

    // Creator sends a message
    let msg = chat_service
        .send_message(room.id, creator.id, "Protected msg".to_string())
        .await
        .checked("test operation should succeed");

    // Member (non-owner without DELETE_CHAT) tries to delete -- should fail
    let result = chat_service
        .delete_message(&room.id, msg.id, &member.id)
        .await;

    assert!(
        result.is_err(),
        "Non-owner without DELETE_CHAT should be denied"
    );
    match result.failed("operation should fail") {
        Error::Authorization(_) => {}
        other => std::panic::panic_any(format!("expected Authorization error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_non_owner_with_delete_chat_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("delmsg2_creator"))
        .await
        .checked("test operation should succeed");
    let admin = user_repo
        .create(&make_user("delmsg2_admin"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&admin.id, &admin.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Del Msg Admin Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, admin.id, None)
        .await
        .checked("test operation should succeed");
    room_service
        .member_service()
        .set_member_role(
            room.id,
            creator.id,
            admin.id,
            synctv_core::models::RoomRole::Admin,
        )
        .await
        .checked("test operation should succeed");

    // Grant DELETE_CHAT permission to admin
    room_service
        .member_service()
        .grant_permission(
            room.id,
            creator.id,
            admin.id,
            RoomAdminPermissionBits::DELETE_CHAT,
        )
        .await
        .checked("test operation should succeed");

    // Creator sends a message
    let msg = chat_service
        .send_message(room.id, creator.id, "Deletable msg".to_string())
        .await
        .checked("test operation should succeed");

    // Admin (with DELETE_CHAT) can delete another user's message
    let result = chat_service
        .delete_message(&room.id, msg.id, &admin.id)
        .await;

    assert!(
        result.is_ok(),
        "Non-owner with DELETE_CHAT should be able to delete"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_records_actor_reason_and_original_author() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let audit_service = Arc::new(AuditService::new_unbuffered(pool.clone()));
    let (chat_service, username_cache) = make_chat_service_with_config_and_audit(
        &pool,
        RateLimitConfig::default(),
        Some(audit_service),
    );

    let creator = user_repo
        .create(&make_user("admin_delete_author"))
        .await
        .checked("test operation should succeed");
    let admin = user_repo
        .create(&make_user("admin_delete_moderator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&admin.id, &admin.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Admin Delete Audit Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    room_service
        .join_room(room.id, admin.id, None)
        .await
        .checked("test operation should succeed");
    room_service
        .member_service()
        .set_member_role(
            room.id,
            creator.id,
            admin.id,
            synctv_core::models::RoomRole::Admin,
        )
        .await
        .checked("test operation should succeed");
    room_service
        .member_service()
        .grant_permission(
            room.id,
            creator.id,
            admin.id,
            RoomAdminPermissionBits::DELETE_CHAT,
        )
        .await
        .checked("test operation should succeed");

    let msg = chat_service
        .send_message(room.id, creator.id, "moderate me".to_string())
        .await
        .checked("test operation should succeed");
    let deleted = chat_service
        .delete_message_event(DeleteChatMessage {
            room_id: room.id,
            message_id: msg.id,
            user_id: admin.id,
            client_operation_id: Some("admin-delete-op".to_string()),
            reason: Some("policy violation".to_string()),
            expected_version: Some(msg.version),
        })
        .await
        .checked("test operation should succeed");

    assert_eq!(deleted.actor_user_id, admin.id);
    assert_eq!(deleted.message.message.user_id, Some(creator.id));
    assert_eq!(deleted.message.message.deleted_by, Some(admin.id));
    assert_eq!(
        deleted.message.message.delete_reason.as_deref(),
        Some("policy violation")
    );

    let audit_row: (String, Option<i16>, Option<String>, serde_json::Value) = sqlx::query_as(
        r"
        SELECT actor_username, target_type, target_id, details
        FROM audit_logs
        WHERE actor_id = $1 AND action = $2
        ",
    )
    .bind(admin.id.as_i64())
    .bind(AuditAction::ChatMessageDeleted.as_i16())
    .fetch_one(&pool)
    .await
    .checked("test operation should succeed");

    assert_eq!(audit_row.0, admin.username);
    assert_eq!(
        audit_row
            .1
            .map(|value| AuditTargetType::try_from(value).checked("test operation should succeed")),
        Some(AuditTargetType::ChatMessage)
    );
    assert_eq!(audit_row.2, Some(format!("{}:{}", room.id, msg.id)));
    let expected_room_id = room.id.to_string();
    let expected_creator_id = creator.id.to_string();
    let expected_admin_id = admin.id.to_string();
    assert_eq!(
        audit_row.3["room_id"].as_str(),
        Some(expected_room_id.as_str())
    );
    assert_eq!(audit_row.3["message_id"].as_i64(), Some(msg.id));
    assert_eq!(
        audit_row.3["original_author_id"].as_str(),
        Some(expected_creator_id.as_str())
    );
    assert_eq!(
        audit_row.3["deleted_by"].as_str(),
        Some(expected_admin_id.as_str())
    );
    assert_eq!(audit_row.3["reason"].as_str(), Some("policy violation"));
    assert_eq!(
        audit_row.3["event_id"].as_str(),
        Some(deleted.event_id.as_str())
    );
    assert_eq!(
        audit_row.3["client_operation_id"].as_str(),
        Some("admin-delete-op")
    );
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
        .checked("test operation should succeed");

    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let rate_limiter: Arc<dyn RequestRateLimiterService> =
        Arc::new(RateLimiter::local_only("test:chat_broadcast:".to_string()));
    let rate_limit_config = RateLimitConfig::default();
    let content_filter = ContentFilter::new();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());

    let permission_service = PermissionService::new_with_runtime(
        member_repo,
        room_repo,
        synctv_core::service::permission::PermissionServiceRuntime {
            room_settings_repo: Some(room_settings_repo.clone()),
            ..synctv_core::service::permission::PermissionServiceRuntime::local_only()
        },
    )
    .checked("permission service should build");

    let notification_service = NotificationService::default();
    let mut notification_rx = notification_service.subscribe();
    let room_settings_service = RoomSettingsService::new(
        room_settings_repo,
        None,
        Arc::new(notification_service.clone()),
        None,
        None,
    );

    let chat_service = ChatService::new(
        chat_repo,
        ChatRuntime {
            rate_limiter,
            rate_limit_config,
            content_filter,
        },
        ChatDependencies {
            permission_service,
            room_settings_service,
            user_service: Arc::new(make_user_service_with_username_cache(&pool, username_cache)),
            file_storage_service: Arc::new(DisabledFileStorageService),
            audit_service: None,
            notification_service,
        },
    );

    let (room, _) = room_service
        .create_room(
            "Broadcast Test Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let msg = chat_service
        .send_message(room.id, creator.id, "Hello, world!".to_string())
        .await
        .checked("send_message should succeed");

    let (event_room_id, event) =
        tokio::time::timeout(std::time::Duration::from_secs(2), notification_rx.recv())
            .await
            .checked("chat notification should arrive")
            .checked("notification channel should remain open");

    assert_eq!(event_room_id, room.id);
    assert_eq!(
        event.event_type(),
        "chat_message",
        "Event type should be chat_message"
    );

    assert!(msg.id > 0, "Message should have a positive ID");
    assert_eq!(msg.content, "Hello, world!", "Message content should match");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_history_cursor_pagination_basic() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("cursor_user"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Cursor Pagination Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let mut sent_messages = Vec::new();
    for i in 0..5 {
        let msg = chat_service
            .send_message(room.id, creator.id, format!("message_{i}"))
            .await
            .checked("test operation should succeed");
        sent_messages.push(msg);
        // Small delay to ensure ordering by timestamp
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Page 1: Get first 2 messages (newest first, no cursor)
    let (page1, cursor1) = chat_service
        .get_history(&room.id, None, 2)
        .await
        .checked("get_history page 1 should succeed");

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
    let cursor1_val = cursor1.checked("test operation should succeed");
    let (page2, cursor2) = chat_service
        .get_history(&room.id, Some((cursor1_val.0, cursor1_val.1)), 2)
        .await
        .checked("get_history page 2 should succeed");

    assert_eq!(page2.len(), 2, "Page 2 should have 2 messages");
    assert!(
        cursor2.is_some(),
        "Should have next cursor (1 more message)"
    );
    assert_eq!(page2[0].content, "message_2", "Page 2 first message");
    assert_eq!(page2[1].content, "message_1", "Page 2 second message");

    // Page 3: Get last message
    let cursor2_val = cursor2.checked("test operation should succeed");
    let (page3, cursor3) = chat_service
        .get_history(&room.id, Some((cursor2_val.0, cursor2_val.1)), 2)
        .await
        .checked("get_history page 3 should succeed");

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
    let (chat_service, _username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("empty_room_user"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Empty Chat Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    // Get history from room with no messages
    let (history, cursor) = chat_service
        .get_history(&room.id, None, 10)
        .await
        .checked("get_history should succeed for empty room");

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
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("single_page_user"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Single Page Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    for i in 0..3 {
        chat_service
            .send_message(room.id, creator.id, format!("msg_{i}"))
            .await
            .checked("test operation should succeed");
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Get history with limit larger than message count
    let (history, cursor) = chat_service
        .get_history(&room.id, None, 100)
        .await
        .checked("get_history should succeed");

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
        window_seconds: 1,
    };
    let (chat_service, username_cache) = make_chat_service_with_config(&pool, rate_limit_config);

    let creator = user_repo
        .create(&make_user("limit_cap_user"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Limit Cap Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    for i in 0..105 {
        chat_service
            .send_message(room.id, creator.id, format!("msg_{i}"))
            .await
            .checked("test operation should succeed");
    }

    // Request with limit=200, should be capped at 100
    let (history, cursor) = chat_service
        .get_history(&room.id, None, 200)
        .await
        .checked("get_history should succeed");

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
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("room_isolation_user"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let (room1, _) = room_service
        .create_room("Room 1".to_string(), String::new(), creator.id, None, None)
        .await
        .checked("test operation should succeed");
    let (room2, _) = room_service
        .create_room("Room 2".to_string(), String::new(), creator.id, None, None)
        .await
        .checked("test operation should succeed");

    chat_service
        .send_message(room1.id, creator.id, "room1_message".to_string())
        .await
        .checked("test operation should succeed");
    chat_service
        .send_message(room2.id, creator.id, "room2_message".to_string())
        .await
        .checked("test operation should succeed");

    // Get history from room1 should only return room1 messages
    let (history1, _) = chat_service
        .get_history(&room1.id, None, 10)
        .await
        .checked("get_history for room1 should succeed");

    assert_eq!(history1.len(), 1, "Room1 should have 1 message");
    assert_eq!(
        history1[0].content, "room1_message",
        "Room1 history should only contain room1 messages"
    );

    // Get history from room2 should only return room2 messages
    let (history2, _) = chat_service
        .get_history(&room2.id, None, 10)
        .await
        .checked("get_history for room2 should succeed");

    assert_eq!(history2.len(), 1, "Room2 should have 1 message");
    assert_eq!(
        history2[0].content, "room2_message",
        "Room2 history should only contain room2 messages"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_history_with_deleted_user_returns_none_user_id() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("deleted_user_chat_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Deleted User Chat Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let msg = chat_service
        .send_message(
            room.id,
            creator.id,
            "Message from soon-deleted user".to_string(),
        )
        .await
        .checked("test operation should succeed");

    // Verify user_id is Some before deletion
    assert!(
        msg.user_id.is_some(),
        "user_id should be Some before user deletion"
    );

    // Simulate user deletion: SET NULL on user_id via raw SQL
    // (foreign key ON DELETE SET NULL)
    sqlx::query("UPDATE chat_messages SET user_id = NULL WHERE id = $1 AND created_at = $2")
        .bind(msg.id)
        .bind(msg.created_at)
        .execute(&pool)
        .await
        .checked("test operation should succeed");

    // Retrieve the message from history
    let (history, _) = chat_service
        .get_history(&room.id, None, 10)
        .await
        .checked("get_history should succeed after user deletion");

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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_oversized_content_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("oversized_msg_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Oversized Msg Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let oversized_content: String = "x".repeat(501);
    let result = chat_service
        .send_message(room.id, creator.id, oversized_content)
        .await;

    assert!(result.is_err(), "Oversized message should be rejected");
    match result.failed("operation should fail") {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("500") || msg.contains("characters"),
                "Error should mention character limit: {msg}"
            );
        }
        other => std::panic::panic_any(format!("expected InvalidInput error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_valid_content_persisted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("valid_msg_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Valid Msg Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let msg = chat_service
        .send_message(room.id, creator.id, "Hello, valid message!".to_string())
        .await
        .checked("Valid message should be persisted");

    assert_eq!(msg.content, "Hello, valid message!");
    assert_eq!(msg.user_id, Some(creator.id));
    assert_eq!(msg.room_id, room.id);
    assert!(msg.id > 0);

    // Verify via history
    let (history, _) = chat_service
        .get_history(&room.id, None, 10)
        .await
        .checked("test operation should succeed");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content, "Hello, valid message!");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_event_idempotency_returns_existing_message() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("idempotent_chat_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Idempotent Chat Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let request = SendChatMessage {
        room_id: room.id,
        user_id: creator.id,
        client_message_id: Some("client-msg-1".to_string()),
        content: "same payload".to_string(),
        message_type: ChatMessageType::Text,
        reply_to_message_id: None,
        metadata: serde_json::Value::Object(Default::default()),
        images: Vec::new(),
        mentions: Vec::new(),
    };

    let first = chat_service
        .send_message_event(request.clone())
        .await
        .checked("test operation should succeed");
    let second = chat_service
        .send_message_event(request)
        .await
        .checked("test operation should succeed");

    assert_eq!(first.event_id, second.event_id);
    assert_eq!(first.message.message.id, second.message.message.id);
    assert_eq!(
        first.message.message.created_at,
        second.message.message.created_at
    );
    assert_eq!(
        first.message.message.client_message_id.as_deref(),
        Some("client-msg-1")
    );

    let replay = chat_service
        .get_events_after(&room.id, Some(&first.event_id), 10)
        .await
        .checked("test operation should succeed");
    assert!(replay.is_empty());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_history_page_returns_event_cursor_for_gapless_observe() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("chat_history_cursor_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Chat History Cursor Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let empty_page = chat_service
        .get_history_page_with_images_for_viewer(&room.id, None, 10, true, Some(&creator.id))
        .await
        .checked("test operation should succeed");
    assert_eq!(empty_page.event_cursor.sequence, 0);
    assert!(empty_page.event_cursor.event_id.is_none());

    let first = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("history-cursor-1".to_string()),
            content: "cursor one".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");
    let second = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("history-cursor-2".to_string()),
            content: "cursor two".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");

    let page = chat_service
        .get_history_page_with_images_for_viewer(&room.id, None, 10, true, Some(&creator.id))
        .await
        .checked("test operation should succeed");
    assert_eq!(page.messages.len(), 2);
    assert_eq!(page.event_cursor.sequence, second.sequence);
    assert_eq!(
        page.event_cursor.event_id.as_deref(),
        Some(second.event_id.as_str())
    );

    let replay_from_empty = chat_service
        .get_events_after_sequence(&room.id, empty_page.event_cursor.sequence, 10)
        .await
        .checked("test operation should succeed");
    assert_eq!(
        replay_from_empty
            .iter()
            .map(|event| event.event.event_id.as_str())
            .collect::<Vec<_>>(),
        vec![first.event_id.as_str(), second.event_id.as_str()]
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_events_after_unknown_event_id_returns_invalid_cursor() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("chat_event_cursor_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Chat Event Cursor Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let error = chat_service
        .get_events_after(&room.id, Some("missing-chat-event"), 10)
        .await
        .failed("unknown event cursor should be invalid input");

    assert!(
        matches!(error, Error::InvalidInput(message) if message == "Invalid chat event cursor")
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_event_idempotency_rejects_different_payload() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("idempotent_conflict_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Idempotent Conflict Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let mut request = SendChatMessage {
        room_id: room.id,
        user_id: creator.id,
        client_message_id: Some("client-msg-conflict".to_string()),
        content: "original payload".to_string(),
        message_type: ChatMessageType::Text,
        reply_to_message_id: None,
        metadata: serde_json::Value::Object(Default::default()),
        images: Vec::new(),
        mentions: Vec::new(),
    };
    chat_service
        .send_message_event(request.clone())
        .await
        .checked("test operation should succeed");

    request.content = "changed payload".to_string();
    let error = chat_service
        .send_message_event(request)
        .await
        .failed("different idempotent payload should conflict");

    match error {
        Error::Conflict(message) => {
            assert!(message.contains("client_message_id"));
        }
        other => std::panic::panic_any(format!("expected Conflict error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_edit_message_increments_version_and_checks_expected_version() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("edit_version_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Edit Version Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let created = chat_service
        .send_message(room.id, creator.id, "before edit".to_string())
        .await
        .checked("test operation should succeed");
    let edited = chat_service
        .edit_message(EditChatMessage {
            room_id: room.id,
            message_id: created.id,
            user_id: creator.id,
            client_operation_id: None,
            content: "after edit".to_string(),
            metadata: serde_json::json!({"edited": true}),
            expected_version: Some(created.version),
        })
        .await
        .checked("test operation should succeed");

    assert_eq!(edited.message.message.content, "after edit");
    assert_eq!(edited.message.message.status, ChatMessageStatus::Edited);
    assert_eq!(edited.message.message.version, created.version + 1);
    assert!(edited.message.message.edited_at.is_some());

    let retry = chat_service
        .edit_message(EditChatMessage {
            room_id: room.id,
            message_id: created.id,
            user_id: creator.id,
            client_operation_id: None,
            content: "after edit".to_string(),
            metadata: serde_json::json!({"edited": true}),
            expected_version: Some(created.version),
        })
        .await
        .checked("same edit retry should return the durable edit event");
    assert_eq!(retry.event_id, edited.event_id);
    assert_eq!(
        retry.message.message.version,
        edited.message.message.version
    );

    let replay = chat_service
        .get_events_after(&room.id, None, 10)
        .await
        .checked("test operation should succeed");
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].event.kind, ChatEventKind::Created);
    assert_eq!(replay[1].event.kind, ChatEventKind::Edited);
    assert_eq!(replay[1].event.event_id, edited.event_id);

    let stale = chat_service
        .edit_message(EditChatMessage {
            room_id: room.id,
            message_id: created.id,
            user_id: creator.id,
            client_operation_id: None,
            content: "stale edit".to_string(),
            metadata: serde_json::Value::Object(Default::default()),
            expected_version: Some(created.version),
        })
        .await
        .failed("stale version should conflict");
    assert!(matches!(stale, Error::OptimisticLockConflict));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_edit_message_client_operation_id_replays_without_expected_version() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("edit_operation_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Edit Operation Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let created = chat_service
        .send_message(room.id, creator.id, "before edit".to_string())
        .await
        .checked("test operation should succeed");
    let request = EditChatMessage {
        room_id: room.id,
        message_id: created.id,
        user_id: creator.id,
        client_operation_id: Some("edit-op-1".to_string()),
        content: "after edit".to_string(),
        metadata: serde_json::json!({"edited": true}),
        expected_version: None,
    };

    let first = chat_service
        .edit_message_outcome(request.clone())
        .await
        .checked("test operation should succeed");
    let replay = chat_service
        .edit_message_outcome(request.clone())
        .await
        .checked("test operation should succeed");
    assert!(first.inserted);
    assert!(!replay.inserted);
    assert_eq!(first.event.event_id, replay.event.event_id);
    assert_eq!(replay.event.message.message.content, "after edit");

    let mut changed = request;
    changed.content = "changed payload".to_string();
    let conflict = chat_service
        .edit_message_outcome(changed)
        .await
        .failed("same client operation id with different payload should conflict");
    assert!(matches!(conflict, Error::Conflict(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_event_soft_deletes_and_checks_expected_version() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("delete_event_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Delete Event Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let created = chat_service
        .send_message(room.id, creator.id, "delete me".to_string())
        .await
        .checked("test operation should succeed");
    let deleted = chat_service
        .delete_message_event(DeleteChatMessage {
            room_id: room.id,
            message_id: created.id,
            user_id: creator.id,
            client_operation_id: None,
            reason: Some("cleanup".to_string()),
            expected_version: Some(created.version),
        })
        .await
        .checked("test operation should succeed");

    assert_eq!(deleted.message.message.status, ChatMessageStatus::Deleted);
    assert_eq!(deleted.message.message.version, created.version + 1);
    assert_eq!(deleted.message.message.deleted_by, Some(creator.id));
    assert_eq!(
        deleted.message.message.delete_reason.as_deref(),
        Some("cleanup")
    );
    assert!(deleted.message.message.deleted_at.is_some());

    let retry = chat_service
        .delete_message_event(DeleteChatMessage {
            room_id: room.id,
            message_id: created.id,
            user_id: creator.id,
            client_operation_id: None,
            reason: Some("cleanup".to_string()),
            expected_version: Some(created.version),
        })
        .await
        .checked("same delete retry should return the durable delete event");
    assert_eq!(retry.event_id, deleted.event_id);
    assert_eq!(retry.kind, ChatEventKind::Deleted);

    let replay = chat_service
        .get_events_after(&room.id, None, 10)
        .await
        .checked("test operation should succeed");
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].event.kind, ChatEventKind::Created);
    assert_eq!(replay[1].event.kind, ChatEventKind::Deleted);
    assert_eq!(replay[1].event.event_id, deleted.event_id);

    let stale = chat_service
        .delete_message_event(DeleteChatMessage {
            room_id: room.id,
            message_id: created.id,
            user_id: creator.id,
            client_operation_id: None,
            reason: None,
            expected_version: Some(created.version),
        })
        .await
        .failed("second delete should conflict");
    assert!(matches!(stale, Error::Conflict(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_client_operation_id_replays_without_expected_version() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("delete_operation_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Delete Operation Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let created = chat_service
        .send_message(room.id, creator.id, "delete me".to_string())
        .await
        .checked("test operation should succeed");
    let request = DeleteChatMessage {
        room_id: room.id,
        message_id: created.id,
        user_id: creator.id,
        client_operation_id: Some("delete-op-1".to_string()),
        reason: Some("cleanup".to_string()),
        expected_version: None,
    };

    let first = chat_service
        .delete_message_event_outcome(request.clone())
        .await
        .checked("test operation should succeed");
    let replay = chat_service
        .delete_message_event_outcome(request.clone())
        .await
        .checked("test operation should succeed");
    assert!(first.inserted);
    assert!(!replay.inserted);
    assert_eq!(first.event.event_id, replay.event.event_id);
    assert_eq!(
        replay.event.message.message.status,
        ChatMessageStatus::Deleted
    );

    let mut changed = request;
    changed.reason = Some("different cleanup".to_string());
    let conflict = chat_service
        .delete_message_event_outcome(changed)
        .await
        .failed("same client operation id with different payload should conflict");
    assert!(matches!(conflict, Error::Conflict(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_image_message_history_returns_image_metadata() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service_with_database_storage(&pool);

    let creator = user_repo
        .create(&make_user("image_history_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Image History Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let payload = b"image-1".to_vec();
    let session = chat_service
        .create_image_upload_session(synctv_core::models::CreateChatImageUploadSession {
            room_id: room.id,
            user_id: creator.id,
            client_image_id: Some("image-1".to_string()),
            mime_type: "image/webp".to_string(),
            size_bytes: i64::try_from(payload.len()).checked("test operation should succeed"),
            width: Some(640),
            height: Some(480),
            checksum_sha256: Some(hex::encode(sha2::Sha256::digest(&payload))),
            metadata: serde_json::json!({"blurhash": "abc"}),
        })
        .await
        .checked("test operation should succeed");
    upload_chat_image_file(&chat_service, &session, payload).await;

    let event = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("image-msg-1".to_string()),
            content: String::new(),
            message_type: ChatMessageType::Image,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: vec![session.file],
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");

    let (history, _) = chat_service
        .get_history_with_images(&room.id, None, 10, true)
        .await
        .checked("test operation should succeed");

    assert_eq!(event.message.message.message_type, ChatMessageType::Image);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].images.len(), 1);
    assert_eq!(history[0].images[0].id, "image-1");
    assert_eq!(
        history[0].images[0].mime_type.as_deref(),
        Some("image/webp")
    );
    assert_eq!(history[0].images[0].width, Some(640));
    assert_eq!(history[0].images[0].height, Some(480));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reused_chat_image_object_keeps_storage_until_last_reference_is_released() {
    let (_container, pool) = create_test_pool().await;
    let file_repo = FileStorageRepository::new(pool.clone());
    let object_key = "database/chat/images/shared.webp";
    let payload = b"shared-image";

    file_repo
        .upsert_blob(
            "database",
            object_key,
            "image/webp",
            payload.to_vec(),
            &serde_json::Value::Object(Default::default()),
        )
        .await
        .checked("test operation should succeed");
    file_repo
        .upsert_object(
            "database",
            object_key,
            "image/webp",
            i64::try_from(payload.len()).checked("test operation should succeed"),
            &hex::encode(sha2::Sha256::digest(payload)),
            &serde_json::Value::Object(Default::default()),
        )
        .await
        .checked("test operation should succeed");

    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let chat_repo = ChatRepository::new(pool.clone());
    let (_chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("shared_image_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Shared Image Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let image = |id: &str| NewStoredFile {
        id: id.to_string(),
        storage_backend: "database".to_string(),
        object_key: object_key.to_string(),
        url: Some("https://example.invalid/shared.webp".to_string()),
        mime_type: Some("image/webp".to_string()),
        size_bytes: Some(i64::try_from(payload.len()).checked("test operation should succeed")),
        width: Some(640),
        height: Some(480),
        metadata: serde_json::Value::Object(Default::default()),
    };

    let mut first_message = ChatMessage::new(room.id, creator.id, String::new());
    first_message.client_message_id = Some("shared-image-msg-1".to_string());
    first_message.message_type = ChatMessageType::Image;
    let first = checked_idempotent_insert_event(
        chat_repo
            .insert_message_event_idempotent(
                &first_message,
                &[image("shared-image-1")],
                &[],
                "shared-image-hash-1",
                "shared-image-event-1",
                Utc::now(),
            )
            .await,
    );
    let mut second_message = ChatMessage::new(room.id, creator.id, String::new());
    second_message.client_message_id = Some("shared-image-msg-2".to_string());
    second_message.message_type = ChatMessageType::Image;
    let second = checked_idempotent_insert_event(
        chat_repo
            .insert_message_event_idempotent(
                &second_message,
                &[image("shared-image-2")],
                &[],
                "shared-image-hash-2",
                "shared-image-event-2",
                Utc::now(),
            )
            .await,
    );

    assert_ne!(
        first.event.message.message.id,
        second.event.message.message.id
    );
    assert_eq!(
        file_repo
            .object_reference_count("database", object_key)
            .await
            .checked("test operation should succeed"),
        2
    );

    let storage = synctv_core::service::file_storage::DatabaseFileStorageService::new(
        "database",
        Arc::new(FileStorageRepository::new(pool.clone())),
        "test-file-storage-secret",
    );
    storage
        .delete_files(
            FileStorageCleanupOrigin::ReferenceReleased,
            &[FileReferenceTarget {
                storage_backend: "database".to_string(),
                object_key: object_key.to_string(),
                reference_kind: "chat_message_image".to_string(),
                reference_id: format!(
                    "{}:{}:{}:{}",
                    first.event.message.message.room_id.as_i64(),
                    first.event.message.message.id,
                    first.event.message.message.created_at.timestamp_micros(),
                    "shared-image-1"
                ),
            }],
        )
        .await
        .checked("test operation should succeed");

    assert!(file_repo
        .blob_exists("database", object_key)
        .await
        .checked("test operation should succeed"));
    assert!(file_repo
        .object_exists("database", object_key)
        .await
        .checked("test operation should succeed"));
    assert_eq!(
        file_repo
            .object_reference_count("database", object_key)
            .await
            .checked("test operation should succeed"),
        1
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_image_message_idempotency_replays_and_rejects_changed_images() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service_with_database_storage(&pool);

    let creator = user_repo
        .create(&make_user("image_idempotent_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Image Idempotent Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let payload = b"idempotent-image-1".to_vec();
    let session = chat_service
        .create_image_upload_session(synctv_core::models::CreateChatImageUploadSession {
            room_id: room.id,
            user_id: creator.id,
            client_image_id: Some("idempotent-image-1".to_string()),
            mime_type: "image/webp".to_string(),
            size_bytes: i64::try_from(payload.len()).checked("test operation should succeed"),
            width: Some(640),
            height: Some(480),
            checksum_sha256: Some(hex::encode(sha2::Sha256::digest(&payload))),
            metadata: serde_json::json!({"blurhash": "abc"}),
        })
        .await
        .checked("test operation should succeed");
    upload_chat_image_file(&chat_service, &session, payload).await;

    let mut request = SendChatMessage {
        room_id: room.id,
        user_id: creator.id,
        client_message_id: Some("image-idempotent-msg".to_string()),
        content: String::new(),
        message_type: ChatMessageType::Image,
        reply_to_message_id: None,
        metadata: serde_json::Value::Object(Default::default()),
        images: vec![session.file],
        mentions: Vec::new(),
    };

    let first = chat_service
        .send_message_event_outcome(request.clone())
        .await
        .checked("test operation should succeed");
    let replay = chat_service
        .send_message_event_outcome(request.clone())
        .await
        .checked("test operation should succeed");

    assert!(first.inserted);
    assert!(!replay.inserted);
    assert_eq!(first.event.event_id, replay.event.event_id);
    assert_eq!(replay.event.message.images.len(), 1);
    assert_eq!(
        replay.event.message.images[0].object_key,
        request.images[0].object_key
    );

    let changed_payload = b"idempotent-image-2".to_vec();
    let changed_session = chat_service
        .create_image_upload_session(synctv_core::models::CreateChatImageUploadSession {
            room_id: room.id,
            user_id: creator.id,
            client_image_id: Some("idempotent-image-2".to_string()),
            mime_type: "image/webp".to_string(),
            size_bytes: i64::try_from(changed_payload.len())
                .checked("test operation should succeed"),
            width: Some(640),
            height: Some(480),
            checksum_sha256: Some(hex::encode(sha2::Sha256::digest(&changed_payload))),
            metadata: serde_json::json!({"blurhash": "abc"}),
        })
        .await
        .checked("test operation should succeed");
    upload_chat_image_file(&chat_service, &changed_session, changed_payload).await;
    request.images[0] = changed_session.file;
    let changed = chat_service
        .send_message_event_outcome(request)
        .await
        .failed("same client_message_id with changed image should conflict");
    assert!(matches!(changed, Error::Conflict(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_message_images_require_matching_room_id() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("image_room_fk_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Image Room FK".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let (other_room, _) = room_service
        .create_room(
            "Other Image Room FK".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let event = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("image-room-fk-message".to_string()),
            content: "message".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");

    let result = sqlx::query(
        r"
        INSERT INTO chat_message_images (
            id, room_id, message_id, message_created_at, storage_backend,
            object_key, url, mime_type, size_bytes, width, height, metadata
        )
        VALUES ($1, $2, $3, $4, $5, $6, NULL, $7, $8, $9, $10, $11)
        ",
    )
    .bind("wrong-room-image")
    .bind(other_room.id)
    .bind(event.message.message.id)
    .bind(event.message.message.created_at)
    .bind("local")
    .bind("rooms/wrong/image.webp")
    .bind("image/webp")
    .bind(42_i64)
    .bind(640_i32)
    .bind(480_i32)
    .bind(serde_json::Value::Object(Default::default()))
    .execute(&pool)
    .await;

    assert!(result.is_err());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleted_image_message_history_hides_image_metadata() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service_with_database_storage(&pool);

    let creator = user_repo
        .create(&make_user("deleted_image_history_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Deleted Image History Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let payload = b"deleted-image-1".to_vec();
    let session = chat_service
        .create_image_upload_session(synctv_core::models::CreateChatImageUploadSession {
            room_id: room.id,
            user_id: creator.id,
            client_image_id: Some("deleted-image-1".to_string()),
            mime_type: "image/webp".to_string(),
            size_bytes: i64::try_from(payload.len()).checked("test operation should succeed"),
            width: Some(640),
            height: Some(480),
            checksum_sha256: Some(hex::encode(sha2::Sha256::digest(&payload))),
            metadata: serde_json::json!({"blurhash": "abc"}),
        })
        .await
        .checked("test operation should succeed");
    upload_chat_image_file(&chat_service, &session, payload).await;

    let event = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("deleted-image-msg-1".to_string()),
            content: String::new(),
            message_type: ChatMessageType::Image,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: vec![session.file],
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");

    chat_service
        .delete_message_event(DeleteChatMessage {
            room_id: room.id,
            message_id: event.message.message.id,
            user_id: creator.id,
            client_operation_id: None,
            reason: Some("cleanup".to_string()),
            expected_version: Some(event.message.message.version),
        })
        .await
        .checked("test operation should succeed");

    let (history, _) = chat_service
        .get_history_with_images(&room.id, None, 10, true)
        .await
        .checked("test operation should succeed");

    assert_eq!(history.len(), 1);
    assert_eq!(history[0].message.status, ChatMessageStatus::Deleted);
    assert!(history[0].message.content.is_empty());
    assert!(history[0].images.is_empty());

    let (visible_history, _) = chat_service
        .get_history_with_images(&room.id, None, 10, false)
        .await
        .checked("test operation should succeed");
    assert!(visible_history.is_empty());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_rejects_missing_or_deleted_reply_target() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("reply_target_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Reply Target Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let missing = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("missing-reply".to_string()),
            content: "reply".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: Some(9_999_999),
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .failed("missing reply target should be rejected");
    assert!(matches!(missing, Error::NotFound(_)));

    let target = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("reply-target".to_string()),
            content: "target".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");
    let reply = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("valid-reply".to_string()),
            content: "valid reply".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: Some(target.message.message.id),
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");
    assert_eq!(
        reply.message.message.reply_to_message_id,
        Some(target.message.message.id)
    );
    assert_eq!(
        reply.message.message.reply_to_message_created_at,
        Some(target.message.message.created_at)
    );

    chat_service
        .delete_message_event(DeleteChatMessage {
            room_id: room.id,
            message_id: target.message.message.id,
            user_id: creator.id,
            client_operation_id: None,
            reason: None,
            expected_version: Some(target.message.message.version),
        })
        .await
        .checked("test operation should succeed");

    let deleted = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("deleted-reply".to_string()),
            content: "reply".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: Some(target.message.message.id),
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .failed("deleted reply target should be rejected");
    assert!(matches!(deleted, Error::Conflict(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_idempotent_reply_send_replays_after_reply_target_is_deleted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("reply_replay_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Reply Replay Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let target = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("reply-replay-target".to_string()),
            content: "target".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");
    let reply_request = SendChatMessage {
        room_id: room.id,
        user_id: creator.id,
        client_message_id: Some("reply-replay".to_string()),
        content: "reply".to_string(),
        message_type: ChatMessageType::Text,
        reply_to_message_id: Some(target.message.message.id),
        metadata: serde_json::Value::Object(Default::default()),
        images: Vec::new(),
        mentions: Vec::new(),
    };

    let first = chat_service
        .send_message_event_outcome(reply_request.clone())
        .await
        .checked("test operation should succeed");
    assert!(first.inserted);

    chat_service
        .delete_message_event(DeleteChatMessage {
            room_id: room.id,
            message_id: target.message.message.id,
            user_id: creator.id,
            client_operation_id: None,
            reason: None,
            expected_version: Some(target.message.message.version),
        })
        .await
        .checked("test operation should succeed");

    let replay = chat_service
        .send_message_event_outcome(reply_request)
        .await
        .checked("test operation should succeed");
    assert!(!replay.inserted);
    assert_eq!(replay.event.event_id, first.event.event_id);
    assert_eq!(
        replay.event.message.message.id,
        first.event.message.message.id
    );
    assert_eq!(
        replay.event.message.message.reply_to_message_created_at,
        Some(target.message.message.created_at)
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_send_message_html_xss_stripped() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("xss_strip_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "XSS Strip Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let msg = chat_service
        .send_message(
            room.id,
            creator.id,
            "<script>alert('xss')</script>Hello safe world".to_string(),
        )
        .await
        .checked("Message with HTML should be filtered, not rejected");

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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_with_deleted_user_requires_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("del_msg_null_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("del_msg_null_member"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&member.id, &member.username)
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Del Null User Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");

    // Creator sends a message
    let msg = chat_service
        .send_message(room.id, creator.id, "Orphaned message".to_string())
        .await
        .checked("test operation should succeed");

    // Simulate user deletion: SET user_id to NULL
    sqlx::query("UPDATE chat_messages SET user_id = NULL WHERE id = $1 AND created_at = $2")
        .bind(msg.id)
        .bind(msg.created_at)
        .execute(&pool)
        .await
        .checked("test operation should succeed");

    // Member (without DELETE_CHAT permission) tries to delete orphaned message
    // Since user_id is NULL, they are not the sender, so they need DELETE_CHAT permission
    let result = chat_service
        .delete_message(&room.id, msg.id, &member.id)
        .await;

    assert!(
        result.is_err(),
        "Non-owner should be denied deletion of orphaned message without DELETE_CHAT permission"
    );
    match result.failed("operation should fail") {
        Error::Authorization(_) => {}
        other => std::panic::panic_any(format!("expected Authorization error, got: {other:?}")),
    }

    // Room creator (has all permissions) should be able to delete
    let result = chat_service
        .delete_message(&room.id, msg.id, &creator.id)
        .await;
    assert!(
        result.is_ok(),
        "Room creator (with DELETE_CHAT) should be able to delete orphaned message"
    );
}
