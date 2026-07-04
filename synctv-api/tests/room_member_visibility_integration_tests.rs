#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use chrono::Utc;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        room_settings::RequireApproval, RoomMemberPermissionBits, RoomRole, RoomSettings,
        SignupMethod, User, UserId, UserRole, UserStatus,
    },
    repository::UserRepository,
    service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, RoomService, UserService,
    },
    Config,
};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        is_banned: false,
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
) -> synctv_api::ClientApiImpl {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let mut runtime = support::client_api_runtime();
    runtime.presence_service = connection_manager.presence_service();

    synctv_api::ClientApiImpl::new_with_runtime(
        synctv_api::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service,
            connection_service: connection_manager,
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        runtime,
    )
}

fn make_client_api_with_connections(
    user_service: Arc<UserService>,
    room_service: Arc<RoomService>,
) -> (synctv_api::ClientApiImpl, Arc<ConnectionManager>) {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let mut runtime = support::client_api_runtime();
    runtime.presence_service = connection_manager.presence_service();

    let client_api = synctv_api::ClientApiImpl::new_with_runtime(
        synctv_api::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service,
            connection_service: connection_manager.clone(),
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        runtime,
    );

    (client_api, connection_manager)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_room_members_requires_view_member_list_permission() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
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
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .add_member(room.id, owner.id, observer.id, RoomRole::Member, false)
        .await
        .unwrap();

    room_service
        .member_service()
        .revoke_permission(
            room.id,
            owner.id,
            observer.id,
            RoomMemberPermissionBits::VIEW_MEMBER_LIST,
        )
        .await
        .unwrap();

    let public_id_codec = synctv_api::PublicIdCodec::plain();
    let room_id = public_id_codec.encode_room_id(room.id).unwrap();
    let err = client_api
        .get_room_members(
            &observer.id,
            &room_id,
            synctv_proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                sort_by: 0,
                sort_direction: 0,
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, synctv_api::ApiError::Authorization(ref msg) if msg == "Forbidden: Permission denied"),
        "reading member list without VIEW_MEMBER_LIST must be forbidden, got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_room_members_hides_pending_members_from_non_moderators() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
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
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();

    room_service
        .add_member(room.id, owner.id, observer.id, RoomRole::Member, false)
        .await
        .unwrap();

    room_service
        .join_room(room.id, pending_user.id, None)
        .await
        .unwrap();

    let public_id_codec = synctv_api::PublicIdCodec::plain();
    let room_id = public_id_codec.encode_room_id(room.id).unwrap();
    let response = client_api
        .get_room_members(
            &observer.id,
            &room_id,
            synctv_proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
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
            .all(|member| !member.user_id.is_empty()),
        "non-moderators must receive active member records in the default list"
    );

    let err = client_api
        .list_room_join_reviews(
            &observer.id,
            &room_id,
            synctv_proto::client::ListRoomJoinReviewsRequest {
                page: 1,
                page_size: 20,
                status: synctv_proto::common::ReviewStatus::Pending as i32,
                user_id: String::new(),
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, synctv_api::ApiError::Authorization(ref msg) if msg == "Forbidden: Permission denied"),
        "non-moderators must not list room join reviews, got: {err:?}"
    );

    let reviews = client_api
        .list_room_join_reviews(
            &owner.id,
            &room_id,
            synctv_proto::client::ListRoomJoinReviewsRequest {
                page: 1,
                page_size: 20,
                status: synctv_proto::common::ReviewStatus::Pending as i32,
                user_id: String::new(),
            },
        )
        .await
        .unwrap();

    assert_eq!(reviews.total, 1);
    assert_eq!(reviews.reviews.len(), 1);
    assert_eq!(
        reviews.reviews[0].user_id,
        public_id_codec.encode_user_id(pending_user.id).unwrap()
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_room_members_returns_stable_version_until_membership_changes() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
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
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .add_member(room.id, owner.id, member_one.id, RoomRole::Member, false)
        .await
        .unwrap();

    let request = synctv_proto::client::GetRoomMembersRequest {
        page: 1,
        page_size: 20,
        search: String::new(),
        role: None,
        sort_by: 0,
        sort_direction: 0,
    };

    let public_id_codec = synctv_api::PublicIdCodec::plain();
    let room_id = public_id_codec.encode_room_id(room.id).unwrap();
    let first = client_api
        .get_room_members(&owner.id, &room_id, request.clone())
        .await
        .unwrap();
    let second = client_api
        .get_room_members(&owner.id, &room_id, request.clone())
        .await
        .unwrap();

    assert!(!first.version.is_empty());
    assert_eq!(first.version, second.version);

    room_service
        .add_member(room.id, owner.id, member_two.id, RoomRole::Member, false)
        .await
        .unwrap();

    let third = client_api
        .get_room_members(&owner.id, &room_id, request)
        .await
        .unwrap();

    assert_ne!(first.version, third.version);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_room_members_marks_realtime_connections_online() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let (client_api, connection_manager) =
        make_client_api_with_connections(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user("member_online_owner"))
        .await
        .unwrap();
    let online_user = user_repo
        .create(&make_user("member_online_user"))
        .await
        .unwrap();
    let offline_user = user_repo
        .create(&make_user("member_offline_user"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Member Online Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .add_member(room.id, owner.id, online_user.id, RoomRole::Member, false)
        .await
        .unwrap();
    room_service
        .add_member(room.id, owner.id, offline_user.id, RoomRole::Member, false)
        .await
        .unwrap();

    connection_manager
        .register("online-member-conn".to_string(), online_user.id)
        .await
        .unwrap();
    connection_manager
        .join_room("online-member-conn", room.id)
        .await
        .unwrap();

    let public_id_codec = synctv_api::PublicIdCodec::plain();
    let room_id = public_id_codec.encode_room_id(room.id).unwrap();
    let online_user_id = public_id_codec.encode_user_id(online_user.id).unwrap();
    let offline_user_id = public_id_codec.encode_user_id(offline_user.id).unwrap();

    let response = client_api
        .get_room_members(
            &owner.id,
            &room_id,
            synctv_proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                sort_by: 0,
                sort_direction: 0,
            },
        )
        .await
        .unwrap();

    let online_member = response
        .members
        .iter()
        .find(|member| member.user_id == online_user_id)
        .unwrap();
    let offline_member = response
        .members
        .iter()
        .find(|member| member.user_id == offline_user_id)
        .unwrap();

    assert!(online_member.is_online);
    assert!(!offline_member.is_online);
}
