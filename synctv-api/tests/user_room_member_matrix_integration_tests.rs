#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use chrono::Utc;
use synctv_api::impls::{
    admin::RequestContext, AdminApiConfig, AdminApiImpl, ApiError, ClientApiImpl,
};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        room_settings::RequireApproval, MemberStatus, ReviewRequestId, ReviewStatus,
        RoomAdminPermissionBits, RoomRole, RoomSettings, RoomStatus, SignupMethod, User, UserId,
        UserRole, UserStatus,
    },
    repository::{
        ProviderInstanceRepository, RoomMemberRepository, RoomRepository, SettingsRepository,
        UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService},
        room::RoomServiceOptions,
        AuditService, EmailConfig, EmailConfigProvider, EmailService, InMemoryTokenBlacklistStore,
        PublishKeyService, RemoteProviderManager, RoomService, SettingsRegistry, SettingsService,
        UserService,
    },
    Config,
};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

struct DisabledEmailConfigProvider;

impl EmailConfigProvider for DisabledEmailConfigProvider {
    fn current_config(&self) -> synctv_core::Result<Option<EmailConfig>> {
        Ok(None)
    }
}

fn public_id_codec() -> synctv_core::PublicIdCodec {
    synctv_core::PublicIdCodec::plain()
}

fn review_request_public_id(id: i64) -> String {
    public_id_codec()
        .encode_review_request_id(ReviewRequestId::expect_positive(id))
        .unwrap()
}

fn make_user(username: &str, role: UserRole, status: UserStatus) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role,
        avatar_file_reference_id: None,
        status,
        is_banned: status == UserStatus::Banned,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
        signup_method: SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
    }
}

fn make_user_service(pool: &sqlx::PgPool) -> UserService {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    )
}

fn make_client_api(
    user_service: Arc<UserService>,
    room_service: Arc<RoomService>,
) -> ClientApiImpl {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));

    ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service,
            connection_service: connection_manager,
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    )
}

async fn make_admin_api(pool: sqlx::PgPool) -> AdminApiImpl {
    let user_service = Arc::new(make_user_service(&pool));
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    settings_service
        .initialize()
        .await
        .expect("settings initialized");
    let settings_registry = Arc::new(SettingsRegistry::new(settings_service.clone()));
    let room_service = RoomService::new_with_options(
        pool.clone(),
        (*user_service).clone(),
        RoomServiceOptions {
            settings_registry: Some(settings_registry.clone()),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .expect("room service should build");
    let email_service =
        Arc::new(EmailService::new(Arc::new(DisabledEmailConfigProvider)).expect("email service"));
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let publish_key_service = Arc::new(
        PublishKeyService::new(
            JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars").unwrap(),
            24,
        )
        .expect("publish key service should build"),
    );

    AdminApiImpl::new_with_runtime(
        AdminApiConfig {
            read_pool: None,
            room_service: Arc::new(room_service),
            user_service,
            settings_service,
            settings_registry: Some(settings_registry),
            email_service,
            connection_service: connection_manager,
            provider_instance_manager,
            live_streaming_infrastructure: None,
            publish_key_service: Some(publish_key_service),
            config: Arc::new(Config::default()),
            audit_service: Arc::new(AuditService::new_unbuffered(pool)),
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
        },
        support::admin_api_runtime(),
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_member_rejects_banned_user_status() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user(
            "status_room_owner",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let banned_user = user_repo
        .create(&make_user(
            "status_banned_target",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    user_repo
        .ban(
            &banned_user.id,
            Some(&owner.id),
            Some("membership guard coverage".to_string()),
        )
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "User Status Matrix Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let banned_user_public_id = codec.encode_user_id(banned_user.id).unwrap();

    let error = client_api
        .add_member(
            &owner.id,
            &room_public_id,
            synctv_proto::client::AddMemberRequest {
                user_id: banned_user_public_id,
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                notify: false,
            },
        )
        .await
        .expect_err("banned target user must be rejected");

    assert!(
        matches!(error, ApiError::Authorization(ref message) if message.contains("banned")),
        "banned user should be rejected with status-aware message, got: {error:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_permission_matrix_controls_moderation_apis() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user(
            "permission_matrix_owner",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let moderator = user_repo
        .create(&make_user(
            "permission_matrix_member",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let pending_target = user_repo
        .create(&make_user(
            "permission_matrix_pending",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let kick_target = user_repo
        .create(&make_user(
            "permission_matrix_guest",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let kick_after_reset_target = user_repo
        .create(&make_user(
            "permission_matrix_reset_target",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();

    let settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Moderation Permission Matrix Room".to_string(),
            String::new(),
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();

    room_service
        .add_member(room.id, owner.id, moderator.id, RoomRole::Member, false)
        .await
        .unwrap();
    room_service
        .join_room(room.id, pending_target.id, None)
        .await
        .unwrap();
    let pending_request_id: i64 = sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM room_join_requests WHERE room_id = $1 AND user_id = $2 AND reviewed_at IS NULL"#,
        room.id.as_i64(),
        pending_target.id.as_i64()
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    room_service
        .add_member(room.id, owner.id, kick_target.id, RoomRole::Guest, false)
        .await
        .unwrap();
    room_service
        .add_member(
            room.id,
            owner.id,
            kick_after_reset_target.id,
            RoomRole::Guest,
            false,
        )
        .await
        .unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let moderator_public_id = codec.encode_user_id(moderator.id).unwrap();
    let pending_target_public_id = codec.encode_user_id(pending_target.id).unwrap();
    let kick_target_public_id = codec.encode_user_id(kick_target.id).unwrap();
    let kick_after_reset_target_public_id =
        codec.encode_user_id(kick_after_reset_target.id).unwrap();

    let pending_list_error = client_api
        .list_room_join_reviews(
            &moderator.id,
            &room_public_id,
            synctv_proto::client::ListRoomJoinReviewsRequest {
                page: 1,
                page_size: 20,
                status: synctv_proto::common::ReviewStatus::Pending as i32,
                user_id: String::new(),
            },
        )
        .await
        .expect_err("non-moderator should not inspect pending queue");
    assert!(
        matches!(pending_list_error, ApiError::Authorization(ref message) if message.contains("Permission denied")),
        "pending-member listing must require moderation permission, got: {pending_list_error:?}"
    );

    let approve_error = client_api
        .approve_room_join_review(
            &moderator.id,
            &room_public_id,
            synctv_proto::client::ApproveRoomJoinReviewRequest {
                request_id: review_request_public_id(pending_request_id),
            },
        )
        .await
        .expect_err("member without approval permission must be rejected");
    assert!(
        matches!(approve_error, ApiError::Authorization(ref message) if message.contains("Permission denied")),
        "approving members without permission must fail, got: {approve_error:?}"
    );

    client_api
        .update_member_permissions(
            &owner.id,
            &room_public_id,
            synctv_proto::client::UpdateMemberPermissionsRequest {
                user_id: moderator_public_id.clone(),
                role: synctv_proto::common::RoomMemberRole::Admin as i32,
                added_permissions: 0,
                removed_permissions: 0,
                admin_added_permissions: RoomAdminPermissionBits::APPROVE_MEMBER
                    | RoomAdminPermissionBits::KICK_MEMBER,
                admin_removed_permissions: 0,
            },
        )
        .await
        .unwrap();

    let pending_response = client_api
        .list_room_join_reviews(
            &moderator.id,
            &room_public_id,
            synctv_proto::client::ListRoomJoinReviewsRequest {
                page: 1,
                page_size: 20,
                status: synctv_proto::common::ReviewStatus::Pending as i32,
                user_id: String::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(pending_response.total, 1);
    assert_eq!(pending_response.reviews.len(), 1);
    assert_eq!(
        pending_response.reviews[0].user_id,
        pending_target_public_id
    );
    assert_eq!(
        pending_response.reviews[0].status,
        synctv_proto::common::ReviewStatus::Pending as i32
    );

    let approved = client_api
        .approve_room_join_review(
            &moderator.id,
            &room_public_id,
            synctv_proto::client::ApproveRoomJoinReviewRequest {
                request_id: review_request_public_id(pending_request_id),
            },
        )
        .await
        .unwrap()
        .member
        .expect("approved member");
    assert_eq!(approved.user_id, pending_target_public_id);

    client_api
        .kick_member(
            &moderator.id,
            &room_public_id,
            synctv_proto::client::KickMemberRequest {
                user_id: kick_target_public_id.clone(),
                kick_cooldown_seconds: 300,
            },
        )
        .await
        .unwrap();

    client_api
        .update_member_permissions(
            &owner.id,
            &room_public_id,
            synctv_proto::client::UpdateMemberPermissionsRequest {
                user_id: moderator_public_id,
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                added_permissions: 0,
                removed_permissions: 0,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
            },
        )
        .await
        .unwrap();

    let kick_error = client_api
        .kick_member(
            &moderator.id,
            &room_public_id,
            synctv_proto::client::KickMemberRequest {
                user_id: kick_after_reset_target_public_id,
                kick_cooldown_seconds: 300,
            },
        )
        .await
        .expect_err("resetting permission overrides must remove moderation powers");
    assert!(
        matches!(kick_error, ApiError::Authorization(ref message) if message.contains("Permission denied")),
        "moderation permission reset must block kick_member, got: {kick_error:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_member_permissions_requires_admin_override_fields_for_admin_role() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user(
            "admin_override_owner",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user(
            "admin_override_target",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Admin Override Matrix Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .add_member(room.id, owner.id, target.id, RoomRole::Member, false)
        .await
        .unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let target_public_id = codec.encode_user_id(target.id).unwrap();

    let wrong_columns_error = client_api
        .update_member_permissions(
            &owner.id,
            &room_public_id,
            synctv_proto::client::UpdateMemberPermissionsRequest {
                user_id: target_public_id.clone(),
                role: synctv_proto::common::RoomMemberRole::Admin as i32,
                added_permissions: 0,
                removed_permissions: RoomAdminPermissionBits::KICK_MEMBER,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
            },
        )
        .await
        .expect_err("promoting to admin must reject member override columns");
    assert!(
        matches!(wrong_columns_error, ApiError::Authorization(ref message) if message.contains("Admin members must use admin_added_permissions")),
        "admin role updates must enforce admin_* permission columns, got: {wrong_columns_error:?}"
    );

    let updated = client_api
        .update_member_permissions(
            &owner.id,
            &room_public_id,
            synctv_proto::client::UpdateMemberPermissionsRequest {
                user_id: target_public_id,
                role: synctv_proto::common::RoomMemberRole::Admin as i32,
                added_permissions: 0,
                removed_permissions: 0,
                admin_added_permissions: 0,
                admin_removed_permissions: RoomAdminPermissionBits::KICK_MEMBER,
            },
        )
        .await
        .unwrap()
        .member
        .expect("updated member");

    assert_eq!(
        updated.role,
        synctv_proto::common::RoomMemberRole::Admin as i32
    );
    assert_eq!(
        updated.admin_removed_permissions,
        RoomAdminPermissionBits::KICK_MEMBER
    );
    assert_eq!(updated.removed_permissions, 0);
    assert_eq!(
        updated.permissions & RoomAdminPermissionBits::KICK_MEMBER,
        0
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_transfer_room_ownership_requires_creator_and_active_member_target() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user(
            "ownership_matrix_owner",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let room_admin = user_repo
        .create(&make_user(
            "ownership_matrix_admin",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let pending_target = user_repo
        .create(&make_user(
            "ownership_matrix_pending",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();

    let settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Ownership Matrix Room".to_string(),
            String::new(),
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();

    room_service
        .add_member(room.id, owner.id, room_admin.id, RoomRole::Admin, false)
        .await
        .unwrap();
    room_service
        .join_room(room.id, pending_target.id, None)
        .await
        .unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let room_admin_public_id = codec.encode_user_id(room_admin.id).unwrap();
    let pending_target_public_id = codec.encode_user_id(pending_target.id).unwrap();

    let non_owner_error = client_api
        .transfer_room_ownership(
            &room_admin.id,
            &room_public_id,
            synctv_proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: room_admin_public_id.clone(),
            },
        )
        .await
        .expect_err("only creator should be able to transfer ownership");
    assert!(
        matches!(non_owner_error, ApiError::Authorization(ref message) if message.contains("Only the current room owner")),
        "non-owner transfer must fail, got: {non_owner_error:?}"
    );

    let pending_target_error = client_api
        .transfer_room_ownership(
            &owner.id,
            &room_public_id,
            synctv_proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: pending_target_public_id,
            },
        )
        .await
        .expect_err("pending membership cannot receive ownership");
    assert!(
        matches!(pending_target_error, ApiError::InvalidInput(ref message) if message.contains("active member")),
        "ownership transfer target must be an active member, got: {pending_target_error:?}"
    );

    let response = client_api
        .transfer_room_ownership(
            &owner.id,
            &room_public_id,
            synctv_proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: room_admin_public_id.clone(),
            },
        )
        .await
        .unwrap();

    let updated_room = response.room.expect("updated room");
    assert_eq!(updated_room.created_by, room_admin_public_id);

    let new_owner_member = room_service
        .get_member(&room.id, &room_admin.id)
        .await
        .unwrap()
        .expect("new owner member");
    assert_eq!(new_owner_member.role, RoomRole::Creator);

    let old_owner_member = room_service
        .get_member(&room.id, &owner.id)
        .await
        .unwrap()
        .expect("old owner member");
    assert_eq!(old_owner_member.role, RoomRole::Admin);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_state_filters_and_member_count_ignore_pending_and_banned_members() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());
    let admin_api = make_admin_api(pool.clone()).await;

    let root_admin = user_repo
        .create(&make_user(
            "rooms_matrix_root",
            UserRole::Root,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let actor = user_repo
        .create(&make_user(
            "rooms_matrix_actor",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let external_owner = user_repo
        .create(&make_user(
            "rooms_matrix_external_owner",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let active_peer = user_repo
        .create(&make_user(
            "rooms_matrix_active_peer",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let pending_peer = user_repo
        .create(&make_user(
            "rooms_matrix_pending_peer",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let kicked_peer = user_repo
        .create(&make_user(
            "rooms_matrix_kicked_peer",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();

    let (_public_room, _) = room_service
        .create_room(
            "Matrix Public Room".to_string(),
            "public room".to_string(),
            actor.id,
            None,
            None,
        )
        .await
        .unwrap();
    let (pending_room, _) = room_service
        .create_room(
            "Matrix Pending Room".to_string(),
            "pending room".to_string(),
            actor.id,
            None,
            None,
        )
        .await
        .unwrap();
    let (rejected_room, _) = room_service
        .create_room(
            "Matrix Rejected Room".to_string(),
            "rejected room".to_string(),
            actor.id,
            None,
            None,
        )
        .await
        .unwrap();
    let (closed_room, _) = room_service
        .create_room(
            "Matrix Closed Room".to_string(),
            "closed room".to_string(),
            actor.id,
            None,
            None,
        )
        .await
        .unwrap();
    let (banned_room, _) = room_service
        .create_room(
            "Matrix Banned Room".to_string(),
            "banned room".to_string(),
            actor.id,
            None,
            None,
        )
        .await
        .unwrap();
    let approval_settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };
    let (joined_room, _) = room_service
        .create_room(
            "Matrix Joined Count Room".to_string(),
            "joined room".to_string(),
            external_owner.id,
            None,
            Some(approval_settings),
        )
        .await
        .unwrap();

    room_repo
        .update_status(&pending_room.id, RoomStatus::Closed)
        .await
        .unwrap();
    room_repo
        .update_status(&rejected_room.id, RoomStatus::Closed)
        .await
        .unwrap();
    room_repo
        .update_status(&closed_room.id, RoomStatus::Closed)
        .await
        .unwrap();
    room_repo
        .update_ban_status(&banned_room.id, true)
        .await
        .unwrap();

    room_service
        .join_room(joined_room.id, actor.id, None)
        .await
        .unwrap();
    let actor_join_request_id = sqlx::query_scalar!(
        r#"
        SELECT id
        FROM room_join_requests
        WHERE room_id = $1
          AND user_id = $2
          AND reviewed_at IS NULL
        "#,
        joined_room.id.as_i64(),
        actor.id.as_i64()
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    room_service
        .approve_join_request(
            joined_room.id,
            external_owner.id,
            ReviewRequestId::expect_positive(actor_join_request_id),
        )
        .await
        .unwrap();
    room_service
        .add_member(
            joined_room.id,
            external_owner.id,
            active_peer.id,
            RoomRole::Member,
            false,
        )
        .await
        .unwrap();
    room_service
        .join_room(joined_room.id, pending_peer.id, None)
        .await
        .unwrap();
    room_service
        .add_member(
            joined_room.id,
            external_owner.id,
            kicked_peer.id,
            RoomRole::Member,
            false,
        )
        .await
        .unwrap();
    room_service
        .kick_member(joined_room.id, external_owner.id, kicked_peer.id, 300)
        .await
        .unwrap();

    let public_rooms = client_api
        .list_rooms(synctv_proto::client::ListRoomsRequest {
            page: 1,
            page_size: 20,
            search: "Matrix".to_string(),
            sort_by: synctv_proto::client::RoomListSortBy::Name as i32,
            sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        })
        .await
        .unwrap();
    let public_names: Vec<&str> = public_rooms
        .rooms
        .iter()
        .map(|room| room.name.as_str())
        .collect();
    assert!(public_names.contains(&"Matrix Public Room"));
    assert!(public_names.contains(&"Matrix Joined Count Room"));
    assert!(
        !public_names.contains(&"Matrix Pending Room")
            && !public_names.contains(&"Matrix Rejected Room")
            && !public_names.contains(&"Matrix Closed Room")
            && !public_names.contains(&"Matrix Banned Room"),
        "public discovery must expose only active, non-banned rooms"
    );

    let pending_only = client_api
        .list_my_rooms(
            &actor.id,
            synctv_proto::client::ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: "Matrix".to_string(),
                status: synctv_proto::common::RoomStatus::Closed as i32,
                is_banned: None,
                relation: synctv_proto::client::MyRoomRelation::Created as i32,
                sort_by: synctv_proto::client::MyRoomListSortBy::Name as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
            },
        )
        .await
        .unwrap();
    assert_eq!(pending_only.total, 3);
    let codec = public_id_codec();
    let closed_room_ids: Vec<_> = pending_only
        .rooms
        .iter()
        .map(|room| room.room.as_ref().unwrap().id.clone())
        .collect();
    assert!(closed_room_ids.contains(&codec.encode_room_id(pending_room.id).unwrap()));
    assert!(closed_room_ids.contains(&codec.encode_room_id(rejected_room.id).unwrap()));
    assert!(closed_room_ids.contains(&codec.encode_room_id(closed_room.id).unwrap()));

    let banned_only = client_api
        .list_my_rooms(
            &actor.id,
            synctv_proto::client::ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: "Matrix".to_string(),
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                is_banned: Some(true),
                relation: synctv_proto::client::MyRoomRelation::Created as i32,
                sort_by: synctv_proto::client::MyRoomListSortBy::Name as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
            },
        )
        .await
        .unwrap();
    assert_eq!(banned_only.total, 1);
    assert_eq!(
        banned_only.rooms[0].room.as_ref().unwrap().id,
        codec.encode_room_id(banned_room.id).unwrap()
    );
    assert!(
        banned_only.rooms[0].room.as_ref().unwrap().is_banned,
        "list_my_rooms must return the same active ban state it filters by"
    );

    let participating_room = client_api
        .list_my_rooms(
            &actor.id,
            synctv_proto::client::ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: "Joined Count".to_string(),
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                is_banned: Some(false),
                relation: synctv_proto::client::MyRoomRelation::Participating as i32,
                sort_by: synctv_proto::client::MyRoomListSortBy::Name as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
            },
        )
        .await
        .unwrap();
    assert_eq!(participating_room.total, 1);
    assert_eq!(
        participating_room.rooms[0]
            .room
            .as_ref()
            .unwrap()
            .member_count,
        3,
        "member_count must include only active members: owner + actor + active_peer"
    );

    let admin_user_rooms = admin_api
        .get_user_rooms(synctv_proto::admin::GetUserRoomsRequest {
            user_id: codec.encode_user_id(actor.id).unwrap(),
            page: 1,
            page_size: 20,
            status: synctv_proto::common::RoomStatus::Unspecified as i32,
            search: "Joined Count".to_string(),
            is_banned: Some(false),
            sort_by: synctv_proto::admin::RoomListSortBy::Name as i32,
            sort_direction: synctv_proto::admin::SortDirection::Asc as i32,
        })
        .await
        .unwrap();
    assert_eq!(admin_user_rooms.total, 1);
    assert_eq!(
        admin_user_rooms.rooms[0].id,
        codec.encode_room_id(joined_room.id).unwrap()
    );
    assert_eq!(
        admin_user_rooms.rooms[0].member_count, 3,
        "admin related-room listing must use the same active-member count semantics"
    );

    let joined_members = member_repo.list_by_room_all(&joined_room.id).await.unwrap();
    assert_eq!(
        joined_members.len(),
        3,
        "fixture should contain only active membership rows"
    );
    assert_eq!(
        joined_members
            .iter()
            .filter(|member| member.status == MemberStatus::Active)
            .count(),
        3,
        "fixture sanity check: exactly 3 memberships should be active"
    );

    let _ = root_admin;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_user_lifecycle_and_role_hierarchy_matrix() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let admin_api = make_admin_api(pool.clone()).await;

    let root = user_repo
        .create(&make_user(
            "user_matrix_root",
            UserRole::Root,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let platform_admin = user_repo
        .create(&make_user(
            "user_matrix_admin",
            UserRole::Admin,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let regular_user = user_repo
        .create(&make_user(
            "user_matrix_regular",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let pending_registration_id = UserId::new();
    sqlx::query!(
        r"
        INSERT INTO user_registration_requests (
            id, username, email, opaque_record,
            opaque_credential_identifier, opaque_ciphersuite,
            opaque_server_setup_version, signup_method, status
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ",
        pending_registration_id.as_i64(),
        "user_matrix_pending",
        "user_matrix_pending@test.com",
        b"opaque-record".as_slice(),
        b"opaque-id".as_slice(),
        "opaque-ristretto255-sha512-argon2id",
        1_i32,
        i16::from(SignupMethod::Email),
        i16::from(ReviewStatus::Pending)
    )
    .execute(&pool)
    .await
    .unwrap();

    let admin_ban_error = admin_api
        .ban_user(
            synctv_proto::admin::BanUserRequest {
                user_id: public_id_codec().encode_user_id(platform_admin.id).unwrap(),
                reason: "role hierarchy".to_string(),
            },
            &platform_admin.id,
            UserRole::Admin,
            &RequestContext::default(),
        )
        .await
        .expect_err("admin must not be allowed to ban another admin");
    assert!(
        matches!(admin_ban_error, ApiError::Authorization(ref message) if message.contains("Only root users can ban admin users")),
        "admin-role hierarchy must be enforced, got: {admin_ban_error:?}"
    );

    let banned_regular = admin_api
        .ban_user(
            synctv_proto::admin::BanUserRequest {
                user_id: public_id_codec().encode_user_id(regular_user.id).unwrap(),
                reason: "coverage".to_string(),
            },
            &platform_admin.id,
            UserRole::Admin,
            &RequestContext::default(),
        )
        .await
        .unwrap()
        .user
        .expect("banned user");
    assert_eq!(
        banned_regular.status,
        synctv_proto::common::UserStatus::Banned as i32
    );
    assert!(banned_regular.is_banned);

    let unbanned_regular = admin_api
        .unban_user(
            synctv_proto::admin::UnbanUserRequest {
                user_id: public_id_codec().encode_user_id(regular_user.id).unwrap(),
            },
            &platform_admin.id,
            &RequestContext::default(),
        )
        .await
        .unwrap()
        .user
        .expect("unbanned user");
    assert_eq!(
        unbanned_regular.status,
        synctv_proto::common::UserStatus::Active as i32
    );
    assert!(!unbanned_regular.is_banned);

    let approved_pending = admin_api
        .approve_user_registration_review(
            synctv_proto::admin::ApproveUserRegistrationReviewRequest {
                request_id: public_id_codec()
                    .encode_user_id(pending_registration_id)
                    .unwrap(),
            },
            &root.id,
            &RequestContext::default(),
        )
        .await
        .unwrap()
        .user
        .expect("approved user");
    assert_eq!(
        approved_pending.status,
        synctv_proto::common::UserStatus::Active as i32
    );

    let approve_active_error = admin_api
        .approve_user_registration_review(
            synctv_proto::admin::ApproveUserRegistrationReviewRequest {
                request_id: public_id_codec().encode_user_id(regular_user.id).unwrap(),
            },
            &root.id,
            &RequestContext::default(),
        )
        .await
        .expect_err("active user must not be approved again");
    assert!(
        matches!(approve_active_error, ApiError::NotFound(ref message) if message.contains("Pending registration request")),
        "approve review should reject non-pending targets, got: {approve_active_error:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_rejects_banned_rooms() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user(
            "join_banned_owner",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let joiner = user_repo
        .create(&make_user(
            "join_banned_joiner",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Join Banned Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();
    room_repo.update_ban_status(&room.id, true).await.unwrap();

    let error = client_api
        .join_room(
            &joiner.id,
            &public_id_codec().encode_room_id(room.id).unwrap(),
            synctv_proto::client::JoinRoomRequest {
                room_id: public_id_codec().encode_room_id(room.id).unwrap(),
                password: String::new(),
            },
            None,
        )
        .await
        .expect_err("banned room must reject join_room");

    assert!(
        matches!(error, ApiError::Authorization(ref message) if message.contains("Room is banned")),
        "join_room must reject banned rooms explicitly, got: {error:?}"
    );
}
