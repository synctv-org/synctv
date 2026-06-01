#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use synctv_api::impls::{
    client::{GuestRoomAccess, RoomActor},
    ApiError, ClientApiImpl,
};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        AuditAction, AuditTargetType, PageParams, RoomAdminPermissionBits,
        RoomMemberPermissionBits, RoomPermissionSet, RoomRole, SignupMethod, User, UserId,
        UserRole, UserStatus,
    },
    repository::{
        AuditLogQuery, AuditLogRepository, ChatRepository, RoomMemberRepository, RoomRepository,
        RoomSettingsRepository, UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService},
        chat::{ChatDependencies, ChatRuntime},
        AuditService, ContentFilter, InMemoryTokenBlacklistStore, NotificationService,
        PermissionService, RateLimitConfig, RateLimiter, RequestRateLimiterService, RoomService,
        RoomSettingsService, UserService,
    },
    Config,
};
use synctv_proto::client::{
    CreateChatImageUploadSessionRequest, DeleteChatMessageRequest, EditChatMessageRequest,
    GetChatHistoryRequest, GetChatMessageContextRequest, GetChatMessageRequest,
    GetChatReadStateRequest, MarkChatReadRequest, SendChatMessageRequest,
};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

const TEST_JWT_SECRET: &str = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
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

fn make_user_service(pool: &sqlx::PgPool, username_cache: UsernameCache) -> UserService {
    UserService::new(
        pool,
        JwtService::new(TEST_JWT_SECRET).unwrap(),
        username_cache,
        PasswordComplexityConfig::default(),
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400)),
        KeyBuilder::new("test_chat_permissions"),
        BruteForceProtection::in_memory("test_chat_permissions:user".to_string()),
    )
}

fn make_chat_service(
    pool: &sqlx::PgPool,
    user_service: Arc<UserService>,
) -> Arc<synctv_core::service::ChatService> {
    make_chat_service_with_audit(pool, user_service, None)
}

fn make_chat_service_with_audit(
    pool: &sqlx::PgPool,
    user_service: Arc<UserService>,
    audit_service: Option<Arc<AuditService>>,
) -> Arc<synctv_core::service::ChatService> {
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let mut permission_service = PermissionService::new(member_repo, room_repo, None, 1000, 300);
    permission_service.set_room_settings_repo(room_settings_repo.clone());

    Arc::new(synctv_core::service::ChatService::new(
        Arc::new(ChatRepository::new(pool.clone())),
        ChatRuntime {
            rate_limiter: Arc::new(RateLimiter::local_only(
                "test_chat_permissions:chat:".to_string(),
            )) as Arc<dyn RequestRateLimiterService>,
            rate_limit_config: RateLimitConfig::default(),
            content_filter: ContentFilter::new(),
        },
        ChatDependencies {
            permission_service,
            room_settings_service: RoomSettingsService::new(
                room_settings_repo,
                None,
                Arc::new(NotificationService::default()),
                None,
                None,
            ),
            user_service,
            audit_service,
            notification_service: NotificationService::default(),
        },
    ))
}

fn make_client_api(
    user_service: Arc<UserService>,
    room_service: Arc<RoomService>,
    chat_service: Arc<synctv_core::service::ChatService>,
) -> ClientApiImpl {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    ClientApiImpl::new(
        user_service,
        room_service,
        connection_manager,
        Arc::new(Config::default()),
        None,
        JwtService::new(TEST_JWT_SECRET).unwrap(),
        None,
        None,
        None,
        Arc::new(synctv_api::PublicIdCodec::default_for_tests()),
    )
    .with_chat_service(Some(chat_service))
}

fn expect_authorization<T>(result: Result<T, ApiError>) {
    match result {
        Err(ApiError::Authorization(_)) => {}
        Err(other) => panic!("expected authorization error, got {other:?}"),
        Ok(_) => panic!("expected authorization error"),
    }
}

fn guest_actor(room_id: synctv_core::models::RoomId) -> RoomActor {
    RoomActor::Guest(GuestRoomAccess {
        room_id,
        guest_id: "guest-denied".to_string(),
        display_name: "Guest".to_string(),
        session_id: "guest-session".to_string(),
        token_jti: "guest-token".to_string(),
        permissions: RoomPermissionSet::default_member(),
        room_guest_version: 1,
    })
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_write_endpoints_require_signed_in_user_and_chat_permission() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let username_cache = UsernameCache::local_only("test:chat-perm:".to_string(), 100, 60);
    let user_service = Arc::new(make_user_service(&pool, username_cache));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    let chat_service = make_chat_service(&pool, user_service.clone());
    let client_api = make_client_api(user_service, room_service.clone(), chat_service);

    let owner = user_repo
        .create(&make_user("chat_write_owner"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("chat_write_member"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Chat Write Permission Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();
    room_service
        .join_room(room.id, member.id, None)
        .await
        .unwrap();
    room_service
        .member_service()
        .revoke_permission(room.id, owner.id, member.id, RoomMemberPermissionBits::CHAT)
        .await
        .unwrap();

    let guest = guest_actor(room.id);
    expect_authorization(
        client_api
            .send_chat_message_for_actor(&guest, SendChatMessageRequest::default())
            .await,
    );
    expect_authorization(
        client_api
            .create_chat_image_upload_session_for_actor(
                &guest,
                CreateChatImageUploadSessionRequest::default(),
            )
            .await,
    );

    let member_actor = RoomActor::User {
        room_id: room.id,
        user_id: member.id,
    };
    expect_authorization(
        client_api
            .send_chat_message_for_actor(
                &member_actor,
                SendChatMessageRequest {
                    content: "blocked".to_string(),
                    ..Default::default()
                },
            )
            .await,
    );
    expect_authorization(
        client_api
            .create_chat_image_upload_session_for_actor(
                &member_actor,
                CreateChatImageUploadSessionRequest {
                    client_image_id: "img-blocked".to_string(),
                    mime_type: "image/png".to_string(),
                    size_bytes: 10,
                    width: 1,
                    height: 1,
                    checksum_sha256: "abc".to_string(),
                    metadata: br#"{"source":"test"}"#.to_vec(),
                },
            )
            .await,
    );

    let owner_actor = RoomActor::User {
        room_id: room.id,
        user_id: owner.id,
    };
    let sent = client_api
        .send_chat_message_for_actor(
            &owner_actor,
            SendChatMessageRequest {
                content: "editable".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let message_id = sent.event.unwrap().message.unwrap().id;

    expect_authorization(
        client_api
            .edit_chat_message_for_actor(
                &member_actor,
                EditChatMessageRequest {
                    message_id,
                    content: "blocked edit".to_string(),
                    metadata: br"{}".to_vec(),
                    ..Default::default()
                },
            )
            .await,
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_read_endpoints_require_view_chat_history_permission() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let username_cache = UsernameCache::local_only("test:chat-read-perm:".to_string(), 100, 60);
    let user_service = Arc::new(make_user_service(&pool, username_cache));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    let chat_service = make_chat_service(&pool, user_service.clone());
    let client_api = make_client_api(user_service, room_service.clone(), chat_service);

    let owner = user_repo
        .create(&make_user("chat_read_owner"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("chat_read_member"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Chat Read Permission Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();
    room_service
        .join_room(room.id, member.id, None)
        .await
        .unwrap();

    let owner_actor = RoomActor::User {
        room_id: room.id,
        user_id: owner.id,
    };
    let sent = client_api
        .send_chat_message_for_actor(
            &owner_actor,
            SendChatMessageRequest {
                content: "read target".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let message_id = sent.event.unwrap().message.unwrap().id;

    room_service
        .member_service()
        .revoke_permission(
            room.id,
            owner.id,
            member.id,
            RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
        )
        .await
        .unwrap();
    let member_actor = RoomActor::User {
        room_id: room.id,
        user_id: member.id,
    };

    expect_authorization(
        client_api
            .get_chat_history_for_actor(&member_actor, GetChatHistoryRequest::default())
            .await,
    );
    expect_authorization(
        client_api
            .get_chat_message_for_actor(
                &member_actor,
                GetChatMessageRequest {
                    message_id: message_id.clone(),
                    include_deleted: false,
                },
            )
            .await,
    );
    expect_authorization(
        client_api
            .get_chat_message_context_for_actor(
                &member_actor,
                GetChatMessageContextRequest {
                    message_id: message_id.clone(),
                    before_limit: 1,
                    after_limit: 1,
                    include_deleted: false,
                },
            )
            .await,
    );
    expect_authorization(
        client_api
            .mark_chat_read_for_actor(&member_actor, MarkChatReadRequest { message_id })
            .await,
    );
    expect_authorization(
        client_api
            .get_chat_read_state_for_actor(&member_actor, GetChatReadStateRequest {})
            .await,
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_delete_endpoint_allows_sender_and_delete_chat_permission() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let username_cache = UsernameCache::local_only("test:chat-delete-perm:".to_string(), 100, 60);
    let user_service = Arc::new(make_user_service(&pool, username_cache));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    let chat_service = make_chat_service(&pool, user_service.clone());
    let client_api = make_client_api(user_service, room_service.clone(), chat_service);

    let owner = user_repo
        .create(&make_user("chat_delete_owner"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("chat_delete_member"))
        .await
        .unwrap();
    let admin = user_repo
        .create(&make_user("chat_delete_admin"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Chat Delete Permission Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();
    room_service
        .join_room(room.id, member.id, None)
        .await
        .unwrap();
    room_service
        .join_room(room.id, admin.id, None)
        .await
        .unwrap();
    room_service
        .member_service()
        .set_member_role(room.id, owner.id, admin.id, RoomRole::Admin)
        .await
        .unwrap();
    room_service
        .member_service()
        .grant_permission(
            room.id,
            owner.id,
            admin.id,
            RoomAdminPermissionBits::DELETE_CHAT,
        )
        .await
        .unwrap();

    let owner_actor = RoomActor::User {
        room_id: room.id,
        user_id: owner.id,
    };
    let member_actor = RoomActor::User {
        room_id: room.id,
        user_id: member.id,
    };
    let admin_actor = RoomActor::User {
        room_id: room.id,
        user_id: admin.id,
    };

    let first = client_api
        .send_chat_message_for_actor(
            &owner_actor,
            SendChatMessageRequest {
                content: "delete by sender".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .event
        .unwrap()
        .message
        .unwrap()
        .id;
    expect_authorization(
        client_api
            .delete_chat_message_for_actor(
                &member_actor,
                DeleteChatMessageRequest {
                    message_id: first.clone(),
                    reason: "blocked".to_string(),
                    ..Default::default()
                },
            )
            .await,
    );
    client_api
        .delete_chat_message_for_actor(
            &owner_actor,
            DeleteChatMessageRequest {
                message_id: first,
                reason: "sender cleanup".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let second = client_api
        .send_chat_message_for_actor(
            &owner_actor,
            SendChatMessageRequest {
                content: "delete by admin".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .event
        .unwrap()
        .message
        .unwrap()
        .id;
    client_api
        .delete_chat_message_for_actor(
            &admin_actor,
            DeleteChatMessageRequest {
                message_id: second,
                reason: "moderation".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_admin_delete_endpoint_writes_audit_log() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let username_cache = UsernameCache::local_only("test:chat-audit:".to_string(), 100, 60);
    let user_service = Arc::new(make_user_service(&pool, username_cache));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    let audit_service = Arc::new(AuditService::new_unbuffered(pool.clone()));
    let chat_service =
        make_chat_service_with_audit(&pool, user_service.clone(), Some(audit_service));
    let client_api = make_client_api(user_service, room_service.clone(), chat_service);

    let owner = user_repo
        .create(&make_user("chat_audit_owner"))
        .await
        .unwrap();
    let admin = user_repo
        .create(&make_user("chat_audit_admin"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Chat Audit Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();
    room_service
        .join_room(room.id, admin.id, None)
        .await
        .unwrap();
    room_service
        .member_service()
        .set_member_role(room.id, owner.id, admin.id, RoomRole::Admin)
        .await
        .unwrap();
    room_service
        .member_service()
        .grant_permission(
            room.id,
            owner.id,
            admin.id,
            RoomAdminPermissionBits::DELETE_CHAT,
        )
        .await
        .unwrap();

    let owner_actor = RoomActor::User {
        room_id: room.id,
        user_id: owner.id,
    };
    let admin_actor = RoomActor::User {
        room_id: room.id,
        user_id: admin.id,
    };
    let message_id = client_api
        .send_chat_message_for_actor(
            &owner_actor,
            SendChatMessageRequest {
                content: "audit target".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .event
        .unwrap()
        .message
        .unwrap()
        .id;
    let deleted_event = client_api
        .delete_chat_message_for_actor(
            &admin_actor,
            DeleteChatMessageRequest {
                message_id: message_id.clone(),
                reason: "policy violation".to_string(),
                client_operation_id: "audit-delete-op".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .event
        .unwrap();

    let audit_repo = AuditLogRepository::new(pool);
    let (rows, total) = audit_repo
        .list(&AuditLogQuery {
            actor_id: Some(admin.id),
            action: Some(AuditAction::ChatMessageDeleted),
            target_type: Some(AuditTargetType::ChatMessage),
            target_id: Some(format!("{}:{message_id}", room.id)),
            page: PageParams::new(Some(1), Some(10)),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(total, 1);
    let row = rows.first().expect("audit row should exist");
    assert_eq!(row.actor_id, Some(admin.id));
    assert_eq!(row.actor_username.as_deref(), Some(admin.username.as_str()));
    assert_eq!(row.action, AuditAction::ChatMessageDeleted);
    assert_eq!(row.target_type, Some(AuditTargetType::ChatMessage));
    assert_eq!(
        row.target_id.as_deref(),
        Some(format!("{}:{message_id}", room.id).as_str())
    );
    let details = row.details.as_ref().expect("audit details should exist");
    assert_eq!(
        details["room_id"].as_str(),
        Some(room.id.to_string().as_str())
    );
    assert_eq!(
        details["message_id"].as_i64(),
        Some(message_id.parse::<i64>().unwrap())
    );
    assert_eq!(
        details["original_author_id"].as_str(),
        Some(owner.id.to_string().as_str())
    );
    assert_eq!(
        details["deleted_by"].as_str(),
        Some(admin.id.to_string().as_str())
    );
    assert_eq!(details["reason"].as_str(), Some("policy violation"));
    assert_eq!(
        details["client_operation_id"].as_str(),
        Some("audit-delete-op")
    );
    assert_eq!(
        details["event_id"].as_str(),
        Some(deleted_event.event_id.as_str())
    );
}
