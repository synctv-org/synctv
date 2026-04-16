#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        room_settings::RequireApproval, PermissionBits, RoomRole, RoomSettings, SignupMethod, User,
        UserId, UserRole, UserStatus,
    },
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Config,
};

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
    UserService::new(
        pool,
        jwt_service,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    )
}

fn make_client_api(
    user_service: Arc<UserService>,
    room_service: Arc<RoomService>,
) -> synctv_api::impls::ClientApiImpl {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    synctv_api::impls::ClientApiImpl::new(
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_room_members_requires_view_member_list_permission() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user("member_list_owner"))
        .await
        .unwrap();
    let observer = user_repo
        .create(&make_user("member_list_observer"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Member Visibility Room".to_string(),
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
            observer.id.clone(),
            RoomRole::Member,
            false,
        )
        .await
        .unwrap();

    room_service
        .member_service()
        .revoke_permission(
            room.id.clone(),
            owner.id.clone(),
            observer.id.clone(),
            PermissionBits::VIEW_MEMBER_LIST,
        )
        .await
        .unwrap();

    let err = client_api
        .get_room_members(
            observer.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                status: None,
                sort_by: 0,
                sort_direction: 0,
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, synctv_api::impls::ApiError::Authorization(ref msg) if msg == "Forbidden: Permission denied"),
        "reading member list without VIEW_MEMBER_LIST must be forbidden, got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_room_members_hides_pending_members_from_non_moderators() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user("member_pending_owner"))
        .await
        .unwrap();
    let observer = user_repo
        .create(&make_user("member_pending_observer"))
        .await
        .unwrap();
    let pending_user = user_repo
        .create(&make_user("member_pending_target"))
        .await
        .unwrap();

    let settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Pending Visibility Room".to_string(),
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
            observer.id.clone(),
            RoomRole::Member,
            false,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), pending_user.id.clone(), None)
        .await
        .unwrap();

    let response = client_api
        .get_room_members(
            observer.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                status: None,
                sort_by: 0,
                sort_direction: 0,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        response.total, 2,
        "non-moderators should only see active members in the default member list"
    );
    assert_eq!(response.members.len(), 2);
    assert!(
        response
            .members
            .iter()
            .all(|member| member.status == synctv_proto::common::MemberStatus::Active as i32),
        "non-moderators must not receive pending memberships in the default list"
    );

    let err = client_api
        .get_room_members(
            observer.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                status: Some(synctv_proto::common::MemberStatus::Pending as i32),
                sort_by: 0,
                sort_direction: 0,
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, synctv_api::impls::ApiError::Authorization(ref msg) if msg.contains("requires room moderation permissions")),
        "non-moderators must not query pending members explicitly, got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_room_members_returns_stable_version_until_membership_changes() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user("member_version_owner"))
        .await
        .unwrap();
    let member_one = user_repo
        .create(&make_user("member_version_one"))
        .await
        .unwrap();
    let member_two = user_repo
        .create(&make_user("member_version_two"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Member Version Room".to_string(),
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
            member_one.id.clone(),
            RoomRole::Member,
            false,
        )
        .await
        .unwrap();

    let request = synctv_proto::client::GetRoomMembersRequest {
        page: 1,
        page_size: 20,
        search: String::new(),
        role: None,
        status: None,
        sort_by: 0,
        sort_direction: 0,
    };

    let first = client_api
        .get_room_members(owner.id.as_str(), room.id.as_str(), request.clone())
        .await
        .unwrap();
    let second = client_api
        .get_room_members(owner.id.as_str(), room.id.as_str(), request.clone())
        .await
        .unwrap();

    assert!(!first.version.is_empty());
    assert_eq!(first.version, second.version);

    room_service
        .add_member(
            room.id.clone(),
            owner.id.clone(),
            member_two.id.clone(),
            RoomRole::Member,
            false,
        )
        .await
        .unwrap();

    let third = client_api
        .get_room_members(owner.id.as_str(), room.id.as_str(), request)
        .await
        .unwrap();

    assert_ne!(first.version, third.version);
}
