#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use synctv_api::impls::{admin::RequestContext, AdminApiImpl, ApiError, ClientApiImpl};
use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        room_settings::RequireApproval, MemberStatus, PermissionBits, RoomRole, RoomSettings,
        RoomStatus, SignupMethod, User, UserId, UserRole, UserStatus,
    },
    repository::{
        ProviderInstanceRepository, RoomMemberRepository, RoomRepository, SettingsRepository,
        UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        member::AddMemberOptions,
        AuditService, EmailService, InMemoryTokenBlacklistStore, PublishKeyService,
        RemoteProviderManager, RoomService, SettingsRegistry, SettingsService, UserService,
    },
    Config,
};

fn make_user(username: &str, role: UserRole, status: UserStatus) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role,
        status,
        email_verified: true,
        signup_method: SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

fn make_user_service(pool: sqlx::PgPool) -> UserService {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let mut service = UserService::new(
        pool,
        jwt_service,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    );
    service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    service
}

fn make_client_api(
    user_service: Arc<UserService>,
    room_service: Arc<RoomService>,
) -> ClientApiImpl {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    ClientApiImpl::new(
        user_service,
        room_service,
        connection_manager,
        Arc::new(Config::default()),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
}

async fn make_admin_api(pool: sqlx::PgPool) -> AdminApiImpl {
    let user_service = Arc::new(make_user_service(pool.clone()));
    let mut room_service = RoomService::new(pool.clone(), (*user_service).clone());
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    settings_service
        .initialize()
        .await
        .expect("settings initialized");
    let settings_registry = Arc::new(SettingsRegistry::new(settings_service.clone()));
    room_service.set_settings_registry(settings_registry.clone());
    let email_service = Arc::new(EmailService::new(None).expect("email service"));
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let publish_key_service = Arc::new(PublishKeyService::new(
        JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars").unwrap(),
        24,
    ));

    AdminApiImpl::new(
        Arc::new(room_service),
        user_service,
        settings_service,
        Some(settings_registry),
        email_service,
        connection_manager,
        provider_instance_manager,
        None,
        Some(publish_key_service),
        Arc::new(Config::default()),
        Arc::new(AuditService::new_unbuffered(pool)),
    )
}

fn default_member_list_request() -> synctv_proto::client::GetRoomMembersRequest {
    synctv_proto::client::GetRoomMembersRequest {
        page: 1,
        page_size: 20,
        search: String::new(),
        role: None,
        status: None,
        sort_by: 0,
        sort_direction: 0,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_member_rejects_non_active_user_statuses() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user(
            "status_room_owner",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();
    let pending_user = user_repo
        .create(&make_user(
            "status_pending_target",
            UserRole::User,
            UserStatus::Pending,
        ))
        .await
        .unwrap();
    let banned_user = user_repo
        .create(&make_user(
            "status_banned_target",
            UserRole::User,
            UserStatus::Banned,
        ))
        .await
        .unwrap();
    let rejected_user = user_repo
        .create(&make_user(
            "status_rejected_target",
            UserRole::User,
            UserStatus::Rejected,
        ))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "User Status Matrix Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    for (label, target, expected_status) in [
        ("pending", &pending_user, "pending"),
        ("banned", &banned_user, "banned"),
        ("rejected", &rejected_user, "rejected"),
    ] {
        let error = client_api
            .add_member(
                owner.id.as_str(),
                room.id.as_str(),
                synctv_proto::client::AddMemberRequest {
                    user_id: target.id.as_str().to_string(),
                    role: synctv_proto::common::RoomMemberRole::Member as i32,
                    notify: false,
                },
            )
            .await
            .expect_err("non-active target user must be rejected");

        assert!(
            matches!(error, ApiError::Authorization(ref message) if message.contains(expected_status)),
            "{label} user should be rejected with status-aware message, got: {error:?}"
        );
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_permission_matrix_controls_moderation_apis() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
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
    let ban_target = user_repo
        .create(&make_user(
            "permission_matrix_guest",
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
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    room_service
        .add_member(
            room.id.clone(),
            owner.id.clone(),
            moderator.id.clone(),
            RoomRole::Member,
            false,
        )
        .await
        .unwrap();
    room_service
        .join_room(room.id.clone(), pending_target.id.clone(), None)
        .await
        .unwrap();
    room_service
        .add_member(
            room.id.clone(),
            owner.id.clone(),
            ban_target.id.clone(),
            RoomRole::Guest,
            false,
        )
        .await
        .unwrap();

    let pending_list_error = client_api
        .get_room_members(
            moderator.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::GetRoomMembersRequest {
                status: Some(synctv_proto::common::MemberStatus::Pending as i32),
                ..default_member_list_request()
            },
        )
        .await
        .expect_err("non-moderator should not inspect pending queue");
    assert!(
        matches!(pending_list_error, ApiError::Authorization(ref message) if message.contains("requires room moderation permissions")),
        "pending-member listing must require moderation permission, got: {pending_list_error:?}"
    );

    let approve_error = client_api
        .approve_member(
            moderator.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::ApproveMemberRequest {
                user_id: pending_target.id.as_str().to_string(),
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
            owner.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::UpdateMemberPermissionsRequest {
                user_id: moderator.id.as_str().to_string(),
                role: synctv_proto::common::RoomMemberRole::Unspecified as i32,
                added_permissions: PermissionBits::APPROVE_MEMBER | PermissionBits::BAN_MEMBER,
                removed_permissions: 0,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
            },
        )
        .await
        .unwrap();

    let pending_response = client_api
        .get_room_members(
            moderator.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::GetRoomMembersRequest {
                status: Some(synctv_proto::common::MemberStatus::Pending as i32),
                ..default_member_list_request()
            },
        )
        .await
        .unwrap();
    assert_eq!(pending_response.total, 1);
    assert_eq!(pending_response.members.len(), 1);
    assert_eq!(
        pending_response.members[0].user_id,
        pending_target.id.as_str()
    );
    assert_eq!(
        pending_response.members[0].status,
        synctv_proto::common::MemberStatus::Pending as i32
    );

    let approved = client_api
        .approve_member(
            moderator.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::ApproveMemberRequest {
                user_id: pending_target.id.as_str().to_string(),
            },
        )
        .await
        .unwrap()
        .member
        .expect("approved member");
    assert_eq!(
        approved.status,
        synctv_proto::common::MemberStatus::Active as i32
    );

    client_api
        .ban_member(
            moderator.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::BanMemberRequest {
                user_id: ban_target.id.as_str().to_string(),
                reason: "matrix coverage".to_string(),
            },
        )
        .await
        .unwrap();

    client_api
        .unban_member(
            moderator.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::UnbanMemberRequest {
                user_id: ban_target.id.as_str().to_string(),
            },
        )
        .await
        .unwrap();

    client_api
        .update_member_permissions(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::UpdateMemberPermissionsRequest {
                user_id: moderator.id.as_str().to_string(),
                role: synctv_proto::common::RoomMemberRole::Unspecified as i32,
                added_permissions: 0,
                removed_permissions: 0,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
            },
        )
        .await
        .unwrap();

    let ban_error = client_api
        .ban_member(
            moderator.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::BanMemberRequest {
                user_id: ban_target.id.as_str().to_string(),
                reason: "should fail after reset".to_string(),
            },
        )
        .await
        .expect_err("resetting permission overrides must remove moderation powers");
    assert!(
        matches!(ban_error, ApiError::Authorization(ref message) if message.contains("Permission denied")),
        "moderation permission reset must block ban_member, got: {ban_error:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_member_permissions_requires_admin_override_fields_for_admin_role() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
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
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .add_member(
            room.id.clone(),
            owner.id.clone(),
            target.id.clone(),
            RoomRole::Member,
            false,
        )
        .await
        .unwrap();

    let wrong_columns_error = client_api
        .update_member_permissions(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::UpdateMemberPermissionsRequest {
                user_id: target.id.as_str().to_string(),
                role: synctv_proto::common::RoomMemberRole::Admin as i32,
                added_permissions: 0,
                removed_permissions: PermissionBits::BAN_MEMBER,
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
            owner.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::UpdateMemberPermissionsRequest {
                user_id: target.id.as_str().to_string(),
                role: synctv_proto::common::RoomMemberRole::Admin as i32,
                added_permissions: 0,
                removed_permissions: 0,
                admin_added_permissions: 0,
                admin_removed_permissions: PermissionBits::BAN_MEMBER,
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
        PermissionBits::BAN_MEMBER
    );
    assert_eq!(updated.removed_permissions, 0);
    assert_eq!(updated.permissions & PermissionBits::BAN_MEMBER, 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_transfer_room_ownership_requires_creator_and_active_member_target() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
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
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    room_service
        .add_member(
            room.id.clone(),
            owner.id.clone(),
            room_admin.id.clone(),
            RoomRole::Admin,
            false,
        )
        .await
        .unwrap();
    room_service
        .join_room(room.id.clone(), pending_target.id.clone(), None)
        .await
        .unwrap();

    let non_owner_error = client_api
        .transfer_room_ownership(
            room_admin.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: room_admin.id.as_str().to_string(),
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
            owner.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: pending_target.id.as_str().to_string(),
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
            owner.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: room_admin.id.as_str().to_string(),
            },
        )
        .await
        .unwrap();

    let updated_room = response.room.expect("updated room");
    assert_eq!(updated_room.created_by, room_admin.id.as_str());

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

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
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
    let banned_peer = user_repo
        .create(&make_user(
            "rooms_matrix_banned_peer",
            UserRole::User,
            UserStatus::Active,
        ))
        .await
        .unwrap();

    let (_public_room, _) = room_service
        .create_room(
            "Matrix Public Room".to_string(),
            "public room".to_string(),
            actor.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    let (pending_room, _) = room_service
        .create_room(
            "Matrix Pending Room".to_string(),
            "pending room".to_string(),
            actor.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    let (rejected_room, _) = room_service
        .create_room(
            "Matrix Rejected Room".to_string(),
            "rejected room".to_string(),
            actor.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    let (closed_room, _) = room_service
        .create_room(
            "Matrix Closed Room".to_string(),
            "closed room".to_string(),
            actor.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    let (banned_room, _) = room_service
        .create_room(
            "Matrix Banned Room".to_string(),
            "banned room".to_string(),
            actor.id.clone(),
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
            external_owner.id.clone(),
            None,
            Some(approval_settings),
        )
        .await
        .unwrap();

    room_repo
        .update_status(&pending_room.id, RoomStatus::Pending)
        .await
        .unwrap();
    room_repo
        .update_status(&rejected_room.id, RoomStatus::Rejected)
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
        .join_room(joined_room.id.clone(), actor.id.clone(), None)
        .await
        .unwrap();
    room_service
        .approve_member(
            joined_room.id.clone(),
            external_owner.id.clone(),
            actor.id.clone(),
        )
        .await
        .unwrap();
    room_service
        .add_member(
            joined_room.id.clone(),
            external_owner.id.clone(),
            active_peer.id.clone(),
            RoomRole::Member,
            false,
        )
        .await
        .unwrap();
    room_service
        .member_service()
        .add_member_with_options(
            joined_room.id.clone(),
            pending_peer.id.clone(),
            RoomRole::Member,
            AddMemberOptions::new().with_initial_status(MemberStatus::Pending),
        )
        .await
        .unwrap();
    room_service
        .add_member(
            joined_room.id.clone(),
            external_owner.id.clone(),
            banned_peer.id.clone(),
            RoomRole::Member,
            false,
        )
        .await
        .unwrap();
    room_service
        .member_service()
        .ban_member(
            joined_room.id.clone(),
            external_owner.id.clone(),
            banned_peer.id.clone(),
            Some("coverage".to_string()),
        )
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
            actor.id.as_str(),
            synctv_proto::client::ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: "Matrix".to_string(),
                status: synctv_proto::common::RoomStatus::Pending as i32,
                is_banned: None,
                relation: synctv_proto::client::MyRoomRelation::Created as i32,
                sort_by: synctv_proto::client::MyRoomListSortBy::Name as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
            },
        )
        .await
        .unwrap();
    assert_eq!(pending_only.total, 1);
    assert_eq!(
        pending_only.rooms[0].room.as_ref().unwrap().id,
        pending_room.id.as_str()
    );

    let banned_only = client_api
        .list_my_rooms(
            actor.id.as_str(),
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
        banned_room.id.as_str()
    );

    let participating_room = client_api
        .list_my_rooms(
            actor.id.as_str(),
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
            user_id: actor.id.as_str().to_string(),
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
    assert_eq!(admin_user_rooms.rooms[0].id, joined_room.id.as_str());
    assert_eq!(
        admin_user_rooms.rooms[0].member_count, 3,
        "admin related-room listing must use the same active-member count semantics"
    );

    let joined_members = member_repo.list_by_room_all(&joined_room.id).await.unwrap();
    assert_eq!(
        joined_members.len(),
        5,
        "fixture should contain 5 membership rows"
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
    let pending_user = user_repo
        .create(&make_user(
            "user_matrix_pending",
            UserRole::User,
            UserStatus::Pending,
        ))
        .await
        .unwrap();

    let admin_ban_error = admin_api
        .ban_user(
            synctv_proto::admin::BanUserRequest {
                user_id: platform_admin.id.as_str().to_string(),
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
                user_id: regular_user.id.as_str().to_string(),
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

    let unbanned_regular = admin_api
        .unban_user(
            synctv_proto::admin::UnbanUserRequest {
                user_id: regular_user.id.as_str().to_string(),
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

    let approved_pending = admin_api
        .approve_user(
            synctv_proto::admin::ApproveUserRequest {
                user_id: pending_user.id.as_str().to_string(),
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
        .approve_user(
            synctv_proto::admin::ApproveUserRequest {
                user_id: regular_user.id.as_str().to_string(),
            },
            &root.id,
            &RequestContext::default(),
        )
        .await
        .expect_err("active user must not be approved again");
    assert!(
        matches!(approve_active_error, ApiError::InvalidInput(ref message) if message.contains("not pending approval")),
        "approve_user should reject non-pending targets, got: {approve_active_error:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_rejects_banned_rooms() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
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
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    room_repo.update_ban_status(&room.id, true).await.unwrap();

    let error = client_api
        .join_room(
            joiner.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::JoinRoomRequest {
                room_id: room.id.as_str().to_string(),
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
