//! `ChatService` integration tests
//!
//! Tests chat message permission checks, `chat_enabled` settings, rate limit mapping,
//! and message deletion permissions with real `PostgreSQL`
//! via testcontainers.
//!
use std::sync::Arc;

use chrono::Utc;
use image::ImageEncoder;
use sha2::Digest;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        room_settings::ChatEnabled, AuditAction, AuditTargetType, ChatEventKind,
        ChatMemberJoinedMetadata, ChatMessage, ChatMessageSelection, ChatMessageStatus,
        ChatMessageType, ChatMetadata, DeleteChatMessage, EditChatMessage, FileBlobCompression,
        FileReferenceTarget, FileUploadManifestPart, FileUploadSessionCreateResult, NewStoredFile,
        RoomAdminPermissionBits, RoomId, RoomMemberPermissionBits, RoomRole, RoomSettings,
        SendChatMessage, SubmittedFileReference, User, UserId, UserRole, UserStatus,
    },
    repository::{
        ChatModerationJobRepository, ChatModerationProgress, ChatRepository, FileStorageRepository,
        NewChatModerationJob, RoomMemberRepository, RoomRepository, RoomSettingsRepository,
        UpsertFileBlob, UpsertFileObject, UserRepository,
    },
    service::{
        AuditService, AuthorizedAdminActor, BruteForceProtection, ChatDependencies, ChatRuntime,
        ChatService, ContentFilter, DisabledFileStorageService, FileStorageCleanupOrigin,
        FileStorageService, InMemoryTokenBlacklistStore, JwtService, NotificationService,
        PermissionService, RateLimitConfig, RateLimiter, RequestRateLimiterService, RoomService,
        RoomSettingsService, UserService,
    },
    Error,
};
use synctv_core_testing::{create_test_pool, TestOptionExt, TestResultExt};

trait ChatServiceTestExt {
    async fn send_message(
        &self,
        room_id: RoomId,
        user_id: UserId,
        content: String,
    ) -> synctv_core::Result<ChatMessage>;

    async fn delete_message(
        &self,
        room_id: &RoomId,
        message_id: i64,
        user_id: &UserId,
    ) -> synctv_core::Result<bool>;
}

impl ChatServiceTestExt for ChatService {
    async fn send_message(
        &self,
        room_id: RoomId,
        user_id: UserId,
        content: String,
    ) -> synctv_core::Result<ChatMessage> {
        self.send_message_event(SendChatMessage {
            room_id,
            user_id,
            client_message_id: None,
            content,
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .map(|event| event.message.message)
    }

    async fn delete_message(
        &self,
        room_id: &RoomId,
        message_id: i64,
        user_id: &UserId,
    ) -> synctv_core::Result<bool> {
        self.delete_message_event(DeleteChatMessage {
            room_id: *room_id,
            message_id,
            user_id: *user_id,
            client_operation_id: None,
            reason: None,
            expected_version: None,
        })
        .await
        .map(|_| true)
    }
}

fn png_test_image() -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&[0, 0, 0, 255], 1, 1, image::ColorType::Rgba8.into())
        .expect("test png image should encode");
    out
}

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
        synctv_core::service::PermissionServiceRuntime {
            room_settings_repo: Some(room_settings_repo.clone()),
            ..synctv_core::service::PermissionServiceRuntime::local_only()
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
            clock: Arc::new(synctv_core::SystemClock),
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
            runtime_settings_store: None,
        },
    );
    (service, username_cache)
}

fn make_chat_service(pool: &PgPool) -> (ChatService, UsernameCache) {
    make_chat_service_with_config(pool, RateLimitConfig::default())
}

fn send_user_chat_request(
    room_id: synctv_core::models::RoomId,
    user_id: UserId,
    client_message_id: &str,
    content: &str,
) -> SendChatMessage {
    SendChatMessage {
        room_id,
        user_id,
        client_message_id: Some(client_message_id.to_string()),
        content: content.to_string(),
        message_type: ChatMessageType::User,
        reply_to_message_id: None,
        metadata: None,
        attachments: Vec::new(),
        mentions: Vec::new(),
    }
}

async fn insert_member_joined_chat_event(
    chat_repo: &ChatRepository,
    room_id: synctv_core::models::RoomId,
    target: &User,
    actor: &User,
    event_id: &str,
) -> synctv_core::models::ChatMessageEventLog {
    let mut message = ChatMessage::new(
        room_id,
        target.id,
        format!("{} joined the room", target.username),
    );
    message.message_type = ChatMessageType::SystemMemberJoined;
    message.metadata = Some(ChatMetadata::MemberJoined(ChatMemberJoinedMetadata {
        user_id: target.id,
        username: target.username.clone(),
        actor_user_id: Some(actor.id),
        actor_username: Some(actor.username.clone()),
        role: RoomRole::Member,
    }));

    chat_repo
        .insert_message_event(&message, &[], &[], actor.id, event_id, Utc::now())
        .await
        .checked("member joined chat event should be inserted")
}

fn make_chat_service_with_database_storage(pool: &PgPool) -> (ChatService, UsernameCache) {
    make_chat_service_with_config_and_storage(
        pool,
        RateLimitConfig::default(),
        Arc::new(synctv_core::service::DatabaseFileStorageService::new(
            "database",
            Arc::new(FileStorageRepository::new(pool.clone())),
            "test-file-storage-secret",
        )),
    )
}

async fn upload_chat_attachment_file(
    chat_service: &ChatService,
    session: &synctv_core::models::FileUploadSession,
    payload: Vec<u8>,
) {
    let encoded_object_key = session
        .upload_object_access
        .as_ref()
        .map(|endpoint| endpoint.encoded_object_key.as_str())
        .checked("database upload endpoint should be returned");
    let upload_token = session
        .upload_headers
        .get(synctv_core::service::FILE_UPLOAD_TOKEN_HEADER)
        .checked("database upload token header should be returned");
    let content_type = session
        .file
        .mime_type
        .as_deref()
        .checked("attachment session mime_type should be present");
    chat_service
        .store_attachment_upload_object(
            encoded_object_key,
            upload_token,
            Some(content_type),
            None,
            payload.into(),
        )
        .await
        .checked("database attachment object should store");
}

fn manifest_parts_from_payload(
    payload: &[u8],
    plan: &synctv_core::models::FileUploadPlan,
) -> Vec<FileUploadManifestPart> {
    plan.parts
        .iter()
        .map(|part| {
            let start = usize::try_from(part.offset_bytes).checked("part offset should fit");
            let len = usize::try_from(part.size_bytes).checked("part size should fit");
            let end = start.checked_add(len).checked("part range should fit");
            FileUploadManifestPart {
                part_number: part.part_number,
                offset_bytes: part.offset_bytes,
                size_bytes: part.size_bytes,
                checksum_sha256: hex::encode(sha2::Sha256::digest(&payload[start..end])),
            }
        })
        .collect()
}

async fn create_chat_attachment_upload_session_for_payload(
    chat_service: &ChatService,
    mut request: synctv_core::models::CreateChatAttachmentUploadSession,
    payload: &[u8],
) -> synctv_core::models::FileUploadSession {
    request.parts = Vec::new();
    let plan = match chat_service
        .create_attachment_upload_session(request.clone())
        .await
        .checked("chat attachment upload plan should be returned")
    {
        FileUploadSessionCreateResult::Plan(plan) => plan,
        FileUploadSessionCreateResult::Session(_) => {
            panic!("chat attachment upload should return a plan before manifest parts")
        }
    };
    request.parts = manifest_parts_from_payload(payload, &plan);
    match chat_service
        .create_attachment_upload_session(request)
        .await
        .checked("chat attachment upload session should be returned")
    {
        FileUploadSessionCreateResult::Session(session) => session,
        FileUploadSessionCreateResult::Plan(_) => {
            panic!("chat attachment upload should return a session after manifest parts")
        }
    }
}

fn submitted_file_reference(file: &NewStoredFile) -> SubmittedFileReference {
    synctv_core::service::submitted_file_reference_from_session_file(file)
        .checked("submitted file reference should build")
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

    let room = room_service
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
            RoomMemberPermissionBits::SEND_CHAT_MESSAGES,
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
    let room = room_service
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

    let room = room_service
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
        synctv_core::service::PermissionServiceRuntime {
            room_settings_repo: Some(room_settings_repo.clone()),
            ..synctv_core::service::PermissionServiceRuntime::local_only()
        },
    )
    .checked("permission service should build");

    let notification_service = Arc::new(NotificationService::default());
    let room_settings_service =
        RoomSettingsService::new(room_settings_repo, None, notification_service, None, None);

    let chat_service = ChatService::new(
        chat_repo,
        ChatRuntime {
            clock: Arc::new(synctv_core::SystemClock),
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
            runtime_settings_store: None,
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

    let room = room_service
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
async fn test_delete_message_non_owner_requires_delete_chat_messages_permission() {
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

    let room = room_service
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

    // Member (non-owner without DELETE_CHAT_MESSAGES) tries to delete -- should fail
    let result = chat_service
        .delete_message(&room.id, msg.id, &member.id)
        .await;

    assert!(
        result.is_err(),
        "Non-owner without DELETE_CHAT_MESSAGES should be denied"
    );
    match result.failed("operation should fail") {
        Error::Authorization(_) => {}
        other => std::panic::panic_any(format!("expected Authorization error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_message_non_owner_with_delete_chat_messages_succeeds() {
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

    let room = room_service
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

    // Grant DELETE_CHAT_MESSAGES permission to admin
    room_service
        .member_service()
        .grant_permission(
            room.id,
            creator.id,
            admin.id,
            RoomAdminPermissionBits::DELETE_CHAT_MESSAGES,
        )
        .await
        .checked("test operation should succeed");

    // Creator sends a message
    let msg = chat_service
        .send_message(room.id, creator.id, "Deletable msg".to_string())
        .await
        .checked("test operation should succeed");

    // Admin (with DELETE_CHAT_MESSAGES) can delete another user's message
    let result = chat_service
        .delete_message(&room.id, msg.id, &admin.id)
        .await;

    assert!(
        result.is_ok(),
        "Non-owner with DELETE_CHAT_MESSAGES should be able to delete"
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

    let room = room_service
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
            RoomAdminPermissionBits::DELETE_CHAT_MESSAGES,
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

    let audit_row = sqlx::query!(
        r#"
        SELECT actor_username AS "actor_username!",
               target_type,
               target_id,
               details AS "details!: serde_json::Value"
        FROM audit_logs
        WHERE actor_id = $1 AND action = $2
        "#,
        admin.id.as_i64(),
        AuditAction::ChatMessageDeleted.as_i16()
    )
    .fetch_one(&pool)
    .await
    .checked("test operation should succeed");

    assert_eq!(audit_row.actor_username, admin.username);
    assert_eq!(
        audit_row
            .target_type
            .map(|value| AuditTargetType::try_from(value).checked("test operation should succeed")),
        Some(AuditTargetType::ChatMessage)
    );
    assert_eq!(audit_row.target_id, Some(format!("{}:{}", room.id, msg.id)));
    let expected_room_id = room.id.to_string();
    let expected_creator_id = creator.id.to_string();
    let expected_admin_id = admin.id.to_string();
    assert_eq!(
        audit_row.details["roomId"].as_str(),
        Some(expected_room_id.as_str())
    );
    assert_eq!(
        audit_row.details["messageId"].as_str(),
        Some(msg.id.to_string().as_str())
    );
    assert_eq!(
        audit_row.details["originalAuthorId"].as_str(),
        Some(expected_creator_id.as_str())
    );
    assert_eq!(
        audit_row.details["deletedBy"].as_str(),
        Some(expected_admin_id.as_str())
    );
    assert_eq!(
        audit_row.details["reason"].as_str(),
        Some("policy violation")
    );
    assert_eq!(
        audit_row.details["eventId"].as_str(),
        Some(deleted.event_id.as_str())
    );
    assert_eq!(
        audit_row.details["clientOperationId"].as_str(),
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
        synctv_core::service::PermissionServiceRuntime {
            room_settings_repo: Some(room_settings_repo.clone()),
            ..synctv_core::service::PermissionServiceRuntime::local_only()
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
            clock: Arc::new(synctv_core::SystemClock),
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
            runtime_settings_store: None,
        },
    );

    let room = room_service
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

    let room = room_service
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

    let room = room_service
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

    let room = room_service
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

    let room = room_service
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

    let room1 = room_service
        .create_room("Room 1".to_string(), String::new(), creator.id, None, None)
        .await
        .checked("test operation should succeed");
    let room2 = room_service
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

    let room = room_service
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
    sqlx::query!(
        "UPDATE chat_messages SET user_id = NULL WHERE id = $1 AND created_at = $2",
        msg.id,
        msg.created_at
    )
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

    let room = room_service
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

    let room = room_service
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
    let room = room_service
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
        message_type: ChatMessageType::User,
        reply_to_message_id: None,
        metadata: None,
        attachments: Vec::new(),
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
        .get_events_after(
            &room.id,
            Some(&first.event_id),
            10,
            &ChatMessageSelection::user_default(),
        )
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
    let room = room_service
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
        .get_history_page_with_attachments_for_viewer(
            &room.id,
            None,
            10,
            true,
            Some(&creator.id),
            &ChatMessageSelection::user_default(),
        )
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
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
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
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");

    let page = chat_service
        .get_history_page_with_attachments_for_viewer(
            &room.id,
            None,
            10,
            true,
            Some(&creator.id),
            &ChatMessageSelection::user_default(),
        )
        .await
        .checked("test operation should succeed");
    assert_eq!(page.messages.len(), 2);
    assert_eq!(page.event_cursor.sequence, second.sequence);
    assert_eq!(
        page.event_cursor.event_id.as_deref(),
        Some(second.event_id.as_str())
    );

    let replay_from_empty = chat_service
        .get_events_after_sequence(
            &room.id,
            empty_page.event_cursor.sequence,
            10,
            &ChatMessageSelection::user_default(),
        )
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
async fn test_chat_history_and_events_use_include_message_types_for_system_join() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let chat_repo = ChatRepository::new(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("chat_include_cursor_creator"))
        .await
        .checked("test operation should succeed");
    let joined = user_repo
        .create(&make_user("chat_include_cursor_joined"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&joined.id, &joined.username)
        .await
        .checked("test operation should succeed");
    let room = room_service
        .create_room(
            "Chat Include Cursor Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let first = chat_service
        .send_message_event(send_user_chat_request(
            room.id,
            creator.id,
            "include-cursor-user-1",
            "visible one",
        ))
        .await
        .checked("test operation should succeed");
    let second = chat_service
        .send_message_event(send_user_chat_request(
            room.id,
            creator.id,
            "include-cursor-user-2",
            "visible two",
        ))
        .await
        .checked("test operation should succeed");
    let system_one =
        insert_member_joined_chat_event(&chat_repo, room.id, &joined, &creator, "member-joined-1")
            .await;
    let system_two =
        insert_member_joined_chat_event(&chat_repo, room.id, &joined, &creator, "member-joined-2")
            .await;

    let default_page = chat_service
        .get_history_page_with_attachments_for_viewer(
            &room.id,
            None,
            10,
            true,
            Some(&creator.id),
            &ChatMessageSelection::user_default(),
        )
        .await
        .checked("test operation should succeed");
    assert_eq!(
        default_page
            .messages
            .iter()
            .map(|message| message.message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["visible two", "visible one"]
    );
    assert_eq!(default_page.event_cursor.sequence, second.sequence);
    assert_eq!(
        default_page.event_cursor.event_id.as_deref(),
        Some(second.event_id.as_str())
    );

    let include_system = ChatMessageSelection {
        include_message_types: vec![ChatMessageType::User, ChatMessageType::SystemMemberJoined],
    };
    let system_page = chat_service
        .get_history_page_with_attachments_for_viewer(
            &room.id,
            None,
            10,
            true,
            Some(&creator.id),
            &include_system,
        )
        .await
        .checked("test operation should succeed");
    assert_eq!(
        system_page
            .messages
            .iter()
            .map(|message| message.message.message_type)
            .collect::<Vec<_>>(),
        vec![
            ChatMessageType::SystemMemberJoined,
            ChatMessageType::SystemMemberJoined,
            ChatMessageType::User,
            ChatMessageType::User,
        ]
    );
    assert_eq!(system_page.event_cursor.sequence, system_two.sequence);
    assert_eq!(
        system_page.event_cursor.event_id.as_deref(),
        Some(system_two.event.event_id.as_str())
    );

    let default_replay = chat_service
        .get_events_after_sequence(
            &room.id,
            second.sequence,
            10,
            &ChatMessageSelection::user_default(),
        )
        .await
        .checked("test operation should succeed");
    assert!(default_replay.is_empty());

    let system_replay = chat_service
        .get_events_after_sequence(&room.id, second.sequence, 10, &include_system)
        .await
        .checked("test operation should succeed");
    assert_eq!(
        system_replay
            .iter()
            .map(|event| event.event.event_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            system_one.event.event_id.as_str(),
            system_two.event.event_id.as_str()
        ]
    );

    let from_start_default = chat_service
        .get_events_after(&room.id, Some(&first.event_id), 10, &Default::default())
        .await
        .checked("test operation should succeed");
    assert_eq!(
        from_start_default
            .iter()
            .map(|event| event.event.event_id.as_str())
            .collect::<Vec<_>>(),
        vec![second.event_id.as_str()]
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
    let room = room_service
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
        .get_events_after(
            &room.id,
            Some("missing-chat-event"),
            10,
            &ChatMessageSelection::user_default(),
        )
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
    let room = room_service
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
        message_type: ChatMessageType::User,
        reply_to_message_id: None,
        metadata: None,
        attachments: Vec::new(),
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
    let room = room_service
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
            metadata: None,
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
            metadata: None,
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
        .get_events_after(&room.id, None, 10, &ChatMessageSelection::user_default())
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
            metadata: None,
            expected_version: Some(created.version),
        })
        .await
        .failed("stale version should conflict");
    assert!(matches!(stale, Error::OptimisticLockConflict));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_edit_message_rejects_system_message_owned_by_actor() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);
    let chat_repo = ChatRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("system_edit_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let room = room_service
        .create_room(
            "System Edit Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let system_event = insert_member_joined_chat_event(
        &chat_repo,
        room.id,
        &creator,
        &creator,
        "system-edit-event",
    )
    .await;
    let system_message = system_event.event.message.message;

    let error = chat_service
        .edit_message(EditChatMessage {
            room_id: room.id,
            message_id: system_message.id,
            user_id: creator.id,
            client_operation_id: None,
            content: "tampered system event".to_string(),
            metadata: None,
            expected_version: Some(system_message.version),
        })
        .await
        .failed("system messages must remain immutable");

    assert!(matches!(
        error,
        Error::Authorization(message) if message == "System messages cannot be edited"
    ));
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
    let room = room_service
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
        metadata: None,
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
    let room = room_service
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
        .get_events_after(&room.id, None, 10, &ChatMessageSelection::user_default())
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
async fn test_admin_moderation_delete_rejects_expired_worker_lease() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let target = user_repo
        .create(&make_user("moderation_lease_target"))
        .await
        .checked("target should be created");
    let mut actor = make_user("moderation_lease_admin");
    actor.role = UserRole::Admin;
    let actor = user_repo
        .create(&actor)
        .await
        .checked("actor should be created");
    username_cache
        .set(&target.id, &target.username)
        .await
        .checked("target username should be cached");
    let room = room_service
        .create_room(
            "Moderation Lease Room".to_string(),
            String::new(),
            target.id,
            None,
            None,
        )
        .await
        .checked("room should be created");
    let message = chat_service
        .send_message(room.id, target.id, "keep after stale lease".to_string())
        .await
        .checked("message should be created");

    let job_repository = ChatModerationJobRepository::new(pool.clone());
    job_repository
        .insert(&NewChatModerationJob {
            id: "service-expired-moderation-lease".to_string(),
            room_id: room.id,
            target_user_id: target.id,
            actor_user_id: actor.id,
            actor_username: actor.username.clone(),
            actor_role: actor.role,
            message_id: Some(message.id),
            ban_user: false,
            delete_all_messages: false,
            delete_all_reactions: false,
            reason: Some("test".to_string()),
            snapshot_at: Utc::now(),
        })
        .await
        .checked("job should be inserted");
    let claimed = job_repository
        .claim_batch("expired-service-worker", 1)
        .await
        .checked("job should be claimed")
        .pop()
        .checked("one job should be claimed");
    sqlx::query!(
        r#"
        UPDATE chat_moderation_jobs
        SET locked_at = NOW() - INTERVAL '1 hour'
        WHERE id = $1
        "#,
        &claimed.id,
    )
    .execute(&pool)
    .await
    .checked("lease should be expired");
    assert_eq!(
        job_repository
            .requeue_stale_processing(1)
            .await
            .checked("stale job should be requeued"),
        1
    );

    let actor = AuthorizedAdminActor::for_persisted_job(actor.id, actor.username);
    let error = chat_service
        .delete_message_event_outcome_for_author_as_admin_with_progress(
            DeleteChatMessage {
                room_id: room.id,
                message_id: message.id,
                user_id: *actor.user_id(),
                client_operation_id: None,
                reason: Some("test".to_string()),
                expected_version: Some(message.version),
            },
            &target.id,
            &actor,
            Some(ChatModerationProgress {
                job_id: &claimed.id,
                worker_id: "expired-service-worker",
                lock_version: claimed.lock_version,
            }),
        )
        .await
        .failed("expired worker must not delete the message");
    assert!(matches!(error, Error::LockConflict(_)));
    let stored = ChatRepository::new(pool)
        .get_by_room_and_id_from_primary(&room.id, message.id)
        .await
        .checked("message should be readable")
        .checked("message should exist");
    assert_eq!(stored.status, ChatMessageStatus::Active);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_moderation_anchor_remains_idempotent_after_enqueue() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service(&pool);

    let target = user_repo
        .create(&make_user("moderation_anchor_target"))
        .await
        .checked("target should be created");
    let mut first_admin = make_user("moderation_anchor_admin_one");
    first_admin.role = UserRole::Admin;
    let first_admin = user_repo
        .create(&first_admin)
        .await
        .checked("first admin should be created");
    let mut second_admin = make_user("moderation_anchor_admin_two");
    second_admin.role = UserRole::Admin;
    let second_admin = user_repo
        .create(&second_admin)
        .await
        .checked("second admin should be created");
    username_cache
        .set(&target.id, &target.username)
        .await
        .checked("target username should be cached");
    let room = room_service
        .create_room(
            "Moderation Anchor Room".to_string(),
            String::new(),
            target.id,
            None,
            None,
        )
        .await
        .checked("room should be created");
    let edited_after_enqueue = chat_service
        .send_message(room.id, target.id, "edited after enqueue".to_string())
        .await
        .checked("message should be created");

    chat_service
        .validate_moderation_message_anchor(&room.id, edited_after_enqueue.id, &target.id)
        .await
        .checked("anchor should be valid when the command is accepted");
    sqlx::query(
        r"
        UPDATE chat_messages
        SET version = version + 1,
            content = 'edited while moderation was queued'
        WHERE room_id = $1 AND id = $2 AND created_at = $3
        ",
    )
    .bind(room.id.as_i64())
    .bind(edited_after_enqueue.id)
    .bind(edited_after_enqueue.created_at)
    .execute(&pool)
    .await
    .checked("message should be edited");
    chat_service
        .validate_moderation_message_anchor(&room.id, edited_after_enqueue.id, &target.id)
        .await
        .checked("editing the anchor must not prevent moderation");

    let first_actor =
        AuthorizedAdminActor::for_persisted_job(first_admin.id, first_admin.username.clone());
    let deleted = chat_service
        .delete_moderation_message_event_outcome_as_admin_with_progress(
            &room.id,
            edited_after_enqueue.id,
            &target.id,
            &first_actor,
            Some("queued moderation"),
            None,
        )
        .await
        .checked("an edit after enqueue must not cancel the accepted command")
        .checked("the active anchor should be deleted");
    assert!(deleted.inserted);

    let deleted_by_another_admin = chat_service
        .send_message(room.id, target.id, "deleted by another admin".to_string())
        .await
        .checked("second message should be created");
    let second_actor =
        AuthorizedAdminActor::for_persisted_job(second_admin.id, second_admin.username.clone());
    chat_service
        .delete_message_event_outcome_for_author_as_admin(
            DeleteChatMessage {
                room_id: room.id,
                message_id: deleted_by_another_admin.id,
                user_id: second_admin.id,
                client_operation_id: None,
                reason: Some("other moderation".to_string()),
                expected_version: None,
            },
            &target.id,
            &second_actor,
        )
        .await
        .checked("second admin should delete the anchor");

    let already_deleted = chat_service
        .delete_moderation_message_event_outcome_as_admin_with_progress(
            &room.id,
            deleted_by_another_admin.id,
            &target.id,
            &first_actor,
            Some("queued moderation"),
            None,
        )
        .await
        .checked("an already deleted anchor should be treated as complete");
    assert!(already_deleted.is_none());
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
    let room = room_service
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
async fn test_attachment_message_history_returns_attachment_metadata() {
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
    let room = room_service
        .create_room(
            "Attachment History Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let payload = png_test_image();
    let session = create_chat_attachment_upload_session_for_payload(
        &chat_service,
        synctv_core::models::CreateChatAttachmentUploadSession {
            room_id: room.id,
            user_id: creator.id,
            client_attachment_id: Some("image-1".to_string()),
            filename: None,
            mime_type: "image/png".to_string(),
            size_bytes: i64::try_from(payload.len()).checked("test operation should succeed"),
            width: Some(1),
            height: Some(1),
            duration_seconds: None,
            bitrate_bps: None,
            parts: Vec::new(),
            metadata: synctv_core::models::FileMetadata {
                blurhash: Some("abc".to_string()),
                ..Default::default()
            },
        },
        &payload,
    )
    .await;
    upload_chat_attachment_file(&chat_service, &session, payload).await;

    let event = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("image-msg-1".to_string()),
            content: String::new(),
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: vec![submitted_file_reference(&session.file)],
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");

    let (history, _) = chat_service
        .get_history_with_attachments_for_viewer(&room.id, None, 10, true, None)
        .await
        .checked("test operation should succeed");

    assert_eq!(event.message.message.message_type, ChatMessageType::User);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].attachments.len(), 1);
    assert_eq!(history[0].attachments[0].id, session.file.id);
    assert_eq!(
        history[0].attachments[0].mime_type.as_deref(),
        Some("image/png")
    );
    assert_eq!(history[0].attachments[0].width, Some(1));
    assert_eq!(history[0].attachments[0].height, Some(1));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reused_chat_attachment_object_keeps_storage_until_last_reference_is_released() {
    let (_container, pool) = create_test_pool().await;
    let file_repo = FileStorageRepository::new(pool.clone());
    let object_key = "database/chat/attachments/shared.webp";
    let payload = b"shared-attachment";
    let checksum_sha256 = hex::encode(sha2::Sha256::digest(payload));
    let metadata = synctv_core::models::FileMetadata::default();

    file_repo
        .upsert_blob(UpsertFileBlob {
            storage_backend: "database",
            object_key,
            mime_type: "image/webp",
            size_bytes: i64::try_from(payload.len()).checked("payload length should fit"),
            checksum_sha256: &checksum_sha256,
            compression: FileBlobCompression::None,
            data: payload.to_vec(),
            metadata: &metadata,
        })
        .await
        .checked("test operation should succeed");
    file_repo
        .upsert_object(UpsertFileObject {
            storage_backend: "database",
            object_key,
            mime_type: "image/webp",
            size_bytes: i64::try_from(payload.len()).checked("test operation should succeed"),
            content_manifest_sha256: &checksum_sha256,
            metadata: &metadata,
        })
        .await
        .checked("test operation should succeed");

    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let chat_repo = ChatRepository::new(pool.clone());
    let (_chat_service, username_cache) = make_chat_service(&pool);

    let creator = user_repo
        .create(&make_user("shared_attachment_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let room = room_service
        .create_room(
            "Shared Attachment Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let image = |id: &str| NewStoredFile {
        filename: None,
        id: id.to_string(),
        storage_backend: "database".to_string(),
        object_key: object_key.to_string(),
        object_access: None,
        url: Some("https://example.invalid/shared.webp".to_string()),
        mime_type: Some("image/webp".to_string()),
        size_bytes: Some(i64::try_from(payload.len()).checked("test operation should succeed")),
        width: Some(640),
        height: Some(480),
        metadata: synctv_core::models::FileMetadata::default(),
    };

    let mut first_message = ChatMessage::new(room.id, creator.id, String::new());
    first_message.client_message_id = Some("shared-attachment-msg-1".to_string());
    first_message.message_type = ChatMessageType::User;
    let first = checked_idempotent_insert_event(
        chat_repo
            .insert_message_event_idempotent(
                &first_message,
                &[image("shared-attachment-1")],
                &[],
                "shared-attachment-hash-1",
                "shared-attachment-event-1",
                Utc::now(),
            )
            .await,
    );
    let mut second_message = ChatMessage::new(room.id, creator.id, String::new());
    second_message.client_message_id = Some("shared-attachment-msg-2".to_string());
    second_message.message_type = ChatMessageType::User;
    let second = checked_idempotent_insert_event(
        chat_repo
            .insert_message_event_idempotent(
                &second_message,
                &[image("shared-attachment-2")],
                &[],
                "shared-attachment-hash-2",
                "shared-attachment-event-2",
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

    let storage = synctv_core::service::DatabaseFileStorageService::new(
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
                reference_kind: "chat_message_attachment".to_string(),
                reference_id: format!(
                    "{}:{}:{}:{}",
                    first.event.message.message.room_id.as_i64(),
                    first.event.message.message.id,
                    first.event.message.message.created_at.timestamp_micros(),
                    "shared-attachment-1"
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
async fn test_attachment_message_idempotency_replays_and_rejects_changed_attachments() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let (chat_service, username_cache) = make_chat_service_with_database_storage(&pool);

    let creator = user_repo
        .create(&make_user("attachment_idempotent_creator"))
        .await
        .checked("test operation should succeed");
    username_cache
        .set(&creator.id, &creator.username)
        .await
        .checked("test operation should succeed");
    let room = room_service
        .create_room(
            "Attachment Idempotent Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let payload = png_test_image();
    let session = create_chat_attachment_upload_session_for_payload(
        &chat_service,
        synctv_core::models::CreateChatAttachmentUploadSession {
            room_id: room.id,
            user_id: creator.id,
            client_attachment_id: Some("idempotent-attachment-1".to_string()),
            filename: None,
            mime_type: "image/png".to_string(),
            size_bytes: i64::try_from(payload.len()).checked("test operation should succeed"),
            width: Some(1),
            height: Some(1),
            duration_seconds: None,
            bitrate_bps: None,
            parts: Vec::new(),
            metadata: synctv_core::models::FileMetadata {
                blurhash: Some("abc".to_string()),
                ..Default::default()
            },
        },
        &payload,
    )
    .await;
    upload_chat_attachment_file(&chat_service, &session, payload).await;

    let mut request = SendChatMessage {
        room_id: room.id,
        user_id: creator.id,
        client_message_id: Some("image-idempotent-msg".to_string()),
        content: String::new(),
        message_type: ChatMessageType::User,
        reply_to_message_id: None,
        metadata: None,
        attachments: vec![submitted_file_reference(&session.file)],
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
    assert_eq!(replay.event.message.attachments.len(), 1);
    assert_eq!(
        replay.event.message.attachments[0].object_key,
        session.file.object_key
    );

    let mut changed_payload = png_test_image();
    changed_payload.extend_from_slice(b"changed");
    let changed_session = create_chat_attachment_upload_session_for_payload(
        &chat_service,
        synctv_core::models::CreateChatAttachmentUploadSession {
            room_id: room.id,
            user_id: creator.id,
            client_attachment_id: Some("idempotent-attachment-2".to_string()),
            filename: None,
            mime_type: "image/png".to_string(),
            size_bytes: i64::try_from(changed_payload.len())
                .checked("test operation should succeed"),
            width: Some(1),
            height: Some(1),
            duration_seconds: None,
            bitrate_bps: None,
            parts: Vec::new(),
            metadata: synctv_core::models::FileMetadata {
                blurhash: Some("abc".to_string()),
                ..Default::default()
            },
        },
        &changed_payload,
    )
    .await;
    upload_chat_attachment_file(&chat_service, &changed_session, changed_payload).await;
    request.attachments[0] = submitted_file_reference(&changed_session.file);
    let changed = chat_service
        .send_message_event_outcome(request)
        .await
        .failed("same client_message_id with changed attachment should conflict");
    assert!(matches!(changed, Error::Conflict(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_message_attachments_require_matching_room_id() {
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
    let room = room_service
        .create_room(
            "Attachment Room FK".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let other_room = room_service
        .create_room(
            "Other Attachment Room FK".to_string(),
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
            client_message_id: Some("attachment-room-fk-message".to_string()),
            content: "message".to_string(),
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");

    let result = sqlx::query!(
        r"
        INSERT INTO chat_message_attachments (
            id, kind, room_id, message_id, message_created_at, filename, storage_backend,
            object_key, url, mime_type, size_bytes, width, height, metadata
        )
        VALUES ($1, 2, $2, $3, $4, NULL, $5, $6, NULL, $7, $8, $9, $10, $11)
        ",
        "wrong-room-attachment",
        other_room.id.as_i64(),
        event.message.message.id,
        event.message.message.created_at,
        "local",
        "rooms/wrong/image.webp",
        "image/webp",
        42_i64,
        640_i32,
        480_i32,
        serde_json::Value::Object(Default::default())
    )
    .execute(&pool)
    .await;

    assert!(result.is_err());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleted_attachment_message_history_hides_attachment_metadata() {
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
    let room = room_service
        .create_room(
            "Deleted Attachment History Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let payload = png_test_image();
    let session = create_chat_attachment_upload_session_for_payload(
        &chat_service,
        synctv_core::models::CreateChatAttachmentUploadSession {
            room_id: room.id,
            user_id: creator.id,
            client_attachment_id: Some("deleted-attachment-1".to_string()),
            filename: None,
            mime_type: "image/png".to_string(),
            size_bytes: i64::try_from(payload.len()).checked("test operation should succeed"),
            width: Some(1),
            height: Some(1),
            duration_seconds: None,
            bitrate_bps: None,
            parts: Vec::new(),
            metadata: synctv_core::models::FileMetadata {
                blurhash: Some("abc".to_string()),
                ..Default::default()
            },
        },
        &payload,
    )
    .await;
    upload_chat_attachment_file(&chat_service, &session, payload).await;

    let event = chat_service
        .send_message_event(SendChatMessage {
            room_id: room.id,
            user_id: creator.id,
            client_message_id: Some("deleted-attachment-msg-1".to_string()),
            content: String::new(),
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: vec![submitted_file_reference(&session.file)],
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
        .get_history_with_attachments_for_viewer(&room.id, None, 10, true, None)
        .await
        .checked("test operation should succeed");

    assert_eq!(history.len(), 1);
    assert_eq!(history[0].message.status, ChatMessageStatus::Deleted);
    assert!(history[0].message.content.is_empty());
    assert!(history[0].attachments.is_empty());

    let (visible_history, _) = chat_service
        .get_history_with_attachments_for_viewer(&room.id, None, 10, false, None)
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
    let room = room_service
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
            message_type: ChatMessageType::User,
            reply_to_message_id: Some(9_999_999),
            metadata: None,
            attachments: Vec::new(),
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
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
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
            message_type: ChatMessageType::User,
            reply_to_message_id: Some(target.message.message.id),
            metadata: None,
            attachments: Vec::new(),
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
            message_type: ChatMessageType::User,
            reply_to_message_id: Some(target.message.message.id),
            metadata: None,
            attachments: Vec::new(),
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
    let room = room_service
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
            message_type: ChatMessageType::User,
            reply_to_message_id: None,
            metadata: None,
            attachments: Vec::new(),
            mentions: Vec::new(),
        })
        .await
        .checked("test operation should succeed");
    let reply_request = SendChatMessage {
        room_id: room.id,
        user_id: creator.id,
        client_message_id: Some("reply-replay".to_string()),
        content: "reply".to_string(),
        message_type: ChatMessageType::User,
        reply_to_message_id: Some(target.message.message.id),
        metadata: None,
        attachments: Vec::new(),
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

    let room = room_service
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

    let room = room_service
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
    sqlx::query!(
        "UPDATE chat_messages SET user_id = NULL WHERE id = $1 AND created_at = $2",
        msg.id,
        msg.created_at
    )
    .execute(&pool)
    .await
    .checked("test operation should succeed");

    // Member (without DELETE_CHAT_MESSAGES permission) tries to delete orphaned message
    // Since user_id is NULL, they are not the sender, so they need DELETE_CHAT_MESSAGES permission
    let result = chat_service
        .delete_message(&room.id, msg.id, &member.id)
        .await;

    assert!(
        result.is_err(),
        "Non-owner should be denied deletion of orphaned message without DELETE_CHAT_MESSAGES permission"
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
        "Room creator (with DELETE_CHAT_MESSAGES) should be able to delete orphaned message"
    );
}
