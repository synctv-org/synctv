#![allow(clippy::unwrap_used)]

use chrono::Utc;
use std::sync::Arc;
use synctv_api::impls::{ApiError, ClientApiImpl};
use synctv_cluster::sync::{CacheTarget, ClusterEvent, ConnectionLimits, ConnectionManager};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{MemberStatus, RoomRole, SignupMethod, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService},
        media::AddMediaRequest,
        playlist::CreatePlaylistRequest,
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo.create(&make_user("room_creator")).await.unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .create_room(
            owner.id.as_str(),
            synctv_api::proto::client::CreateRoomRequest {
                name: "Fanout Room".to_string(),
                description: "Room creation fanout regression".to_string(),
                password: String::new(),
                settings: Vec::new(),
            },
        )
        .await
        .expect_err("cluster mode must fail closed when room creation fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out RoomCreated to cluster replicas"
    );
    assert_eq!(
        room_service
            .list_accessible_rooms(&synctv_core::models::RoomListQuery::default())
            .await
            .unwrap()
            .1,
        0,
        "room creation must not commit before cluster fanout capacity is reserved"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_room_password_fails_closed_when_cluster_cache_invalidation_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo.create(&make_user("room_owner")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Protected Room".to_string(),
            "Room with password".to_string(),
            owner.id.clone(),
            Some("CorrectPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .set_room_password(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::SetRoomPasswordRequest {
                password: "NewPassword123".to_string(),
            },
        )
        .await
        .expect_err("cluster mode must fail closed when room cache invalidation fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out room cache invalidation to cluster replicas"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_room_password_fails_closed_when_room_settings_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo
        .create(&make_user("room_owner_settings"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Protected Room".to_string(),
            "Room with password".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, _rx) = tokio::sync::mpsc::channel(1);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .set_room_password(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::SetRoomPasswordRequest {
                password: "NewPassword123".to_string(),
            },
        )
        .await
        .expect_err("cluster mode must fail closed when room settings fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out RoomSettingsChanged to cluster replicas"
    );

    let settings = room_service.get_room_settings(&room.id).await.unwrap();
    assert!(
        !settings.require_password.0,
        "room password must remain unchanged when room settings fanout reservation fails"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_member_permissions_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo.create(&make_user("room_owner")).await.unwrap();
    let target = user_repo.create(&make_user("room_target")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Permission Room".to_string(),
            "Room for permission updates".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .member_service()
        .add_member(room.id.clone(), target.id.clone(), RoomRole::Member)
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .update_member_permissions(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::UpdateMemberPermissionsRequest {
                user_id: target.id.as_str().to_string(),
                added_permissions: 1,
                removed_permissions: 0,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
                role: synctv_proto::common::RoomMemberRole::Unspecified as i32,
            },
        )
        .await
        .expect_err("cluster mode must fail closed when permission fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out permission changes to cluster replicas"
    );

    let member = room_service
        .get_member(&room.id, &target.id)
        .await
        .unwrap()
        .expect("member should still exist after failed request");
    assert_eq!(
        member.added_permissions, 0,
        "permission mutation must not commit before fanout capacity is reserved"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_member_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo.create(&make_user("kick_owner")).await.unwrap();
    let target = user_repo.create(&make_user("kick_target")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Kick Room".to_string(),
            "Room for kick regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .member_service()
        .add_member(room.id.clone(), target.id.clone(), RoomRole::Member)
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .kick_member(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::KickMemberRequest {
                user_id: target.id.as_str().to_string(),
            },
        )
        .await
        .expect_err("cluster mode must fail closed when kick fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out KickUserFromRoom to cluster replicas"
    );

    let member = room_service
        .get_member(&room.id, &target.id)
        .await
        .unwrap()
        .expect("kick must not remove member before fanout reservation");
    assert_eq!(member.status, MemberStatus::Active);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_member_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo.create(&make_user("ban_owner")).await.unwrap();
    let target = user_repo.create(&make_user("ban_target")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Ban Room".to_string(),
            "Room for ban regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .member_service()
        .add_member(room.id.clone(), target.id.clone(), RoomRole::Member)
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .ban_member(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::BanMemberRequest {
                user_id: target.id.as_str().to_string(),
                reason: "ban".to_string(),
            },
        )
        .await
        .expect_err("cluster mode must fail closed when kick fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out KickUserFromRoom to cluster replicas"
    );

    let member = room_service
        .get_member(&room.id, &target.id)
        .await
        .unwrap()
        .expect("ban must not commit before fanout reservation");
    assert_eq!(member.status, MemberStatus::Active);
    assert!(
        member.banned_at.is_none(),
        "ban state must remain unchanged"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unban_member_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo.create(&make_user("unban_owner")).await.unwrap();
    let target = user_repo.create(&make_user("unban_target")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Unban Room".to_string(),
            "Room for unban regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .member_service()
        .add_member(room.id.clone(), target.id.clone(), RoomRole::Member)
        .await
        .unwrap();
    room_service
        .member_service()
        .ban_member(room.id.clone(), owner.id.clone(), target.id.clone(), None)
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .unban_member(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::UnbanMemberRequest {
                user_id: target.id.as_str().to_string(),
            },
        )
        .await
        .expect_err("cluster mode must fail closed when permission fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out permission changes to cluster replicas"
    );

    let member = room_service.get_member(&room.id, &target.id).await.unwrap();
    assert!(
        member.is_none(),
        "banned member must not reappear as an active room member after failed unban"
    );
    assert!(
        room_service
            .member_service()
            .is_banned(&room.id, &target.id)
            .await
            .unwrap(),
        "unban must not clear ban state before fanout reservation"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_transfer_room_ownership_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo
        .create(&make_user("room_owner_transfer_fail_closed"))
        .await
        .unwrap();
    let successor = user_repo
        .create(&make_user("room_successor_transfer_fail_closed"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Ownership Transfer Room".to_string(),
            "Room for ownership transfer fail-closed regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    room_service
        .member_service()
        .add_member(room.id.clone(), successor.id.clone(), RoomRole::Member)
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, _rx) = tokio::sync::mpsc::channel(2);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .transfer_room_ownership(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: successor.id.as_str().to_string(),
            },
        )
        .await
        .expect_err("cluster mode must fail closed when ownership transfer fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out room cache invalidation to cluster replicas"
    );

    let room_after = room_service.get_room(&room.id).await.unwrap();
    assert_eq!(
        room_after.created_by, owner.id,
        "ownership transfer must not commit before all cluster fanout capacity is reserved"
    );

    let owner_member = room_service
        .get_member(&room.id, &owner.id)
        .await
        .unwrap()
        .expect("owner membership should still exist");
    assert_eq!(
        owner_member.role,
        RoomRole::Creator,
        "current owner must keep creator role after failed transfer"
    );

    let successor_member = room_service
        .get_member(&room.id, &successor.id)
        .await
        .unwrap()
        .expect("successor membership should still exist");
    assert_eq!(
        successor_member.role,
        RoomRole::Member,
        "successor role must remain unchanged after failed transfer"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_transfer_room_ownership_publishes_permission_and_cache_invalidation_events() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo
        .create(&make_user("room_owner_transfer_publish"))
        .await
        .unwrap();
    let successor = user_repo
        .create(&make_user("room_successor_transfer_publish"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Ownership Transfer Publish Room".to_string(),
            "Room for ownership transfer fanout verification".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    room_service
        .member_service()
        .add_member(room.id.clone(), successor.id.clone(), RoomRole::Member)
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, mut rx) = tokio::sync::mpsc::channel(3);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service,
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let response = client_api
        .transfer_room_ownership(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: successor.id.as_str().to_string(),
            },
        )
        .await
        .expect("ownership transfer should succeed");

    assert_eq!(
        response
            .room
            .expect("room response should exist")
            .created_by,
        successor.id.as_str()
    );

    let mut old_owner_permission_changed = false;
    let mut new_owner_permission_changed = false;
    let mut cache_invalidated = false;

    for _ in 0..3 {
        let request = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("fanout event should arrive promptly")
            .expect("fanout event should exist");

        match request.event {
            ClusterEvent::PermissionChanged {
                room_id,
                target_user_id,
                changed_by,
                role,
                ..
            } => {
                assert_eq!(room_id, room.id);
                assert_eq!(changed_by, owner.id);

                if target_user_id == owner.id {
                    assert_eq!(
                        role,
                        synctv_proto::common::RoomMemberRole::Admin as i32,
                        "previous owner must be downgraded to admin"
                    );
                    old_owner_permission_changed = true;
                } else if target_user_id == successor.id {
                    assert_eq!(
                        role,
                        synctv_proto::common::RoomMemberRole::Creator as i32,
                        "new owner must be upgraded to creator"
                    );
                    new_owner_permission_changed = true;
                } else {
                    panic!("unexpected permission change target: {target_user_id:?}");
                }
            }
            ClusterEvent::CacheInvalidate { targets, .. } => {
                assert_eq!(targets.len(), 1);
                match &targets[0] {
                    CacheTarget::Room { room_id } => assert_eq!(room_id, room.id.as_str()),
                    other => panic!("expected room cache invalidation, got {other:?}"),
                }
                cache_invalidated = true;
            }
            other => panic!("unexpected cluster event for ownership transfer: {other:?}"),
        }
    }

    assert!(
        old_owner_permission_changed,
        "transfer must publish permission change for the previous owner"
    );
    assert!(
        new_owner_permission_changed,
        "transfer must publish permission change for the new owner"
    );
    assert!(
        cache_invalidated,
        "transfer must publish room cache invalidation"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_room_settings_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo
        .create(&make_user("room_settings_owner"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Room Settings Room".to_string(),
            "Room for reset regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let customized = synctv_core::models::RoomSettings {
        chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
        ..synctv_core::models::RoomSettings::default()
    };
    room_service
        .set_room_settings(&room.id, &customized)
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, _rx) = tokio::sync::mpsc::channel(1);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .reset_room_settings(owner.id.as_str(), room.id.as_str())
        .await
        .expect_err("cluster mode must fail closed when room settings fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out RoomSettingsChanged to cluster replicas"
    );

    let settings = room_service.get_room_settings(&room.id).await.unwrap();
    assert!(
        !settings.chat_enabled.0,
        "reset must not commit before cluster fanout capacity is reserved"
    );
    assert!(
        settings.allow_guest_join.0,
        "customized settings must remain unchanged after failed reset"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room_settings_fails_closed_when_room_cache_invalidation_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo
        .create(&make_user("room_settings_update_owner"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Room Settings Update Room".to_string(),
            "Room for update cache invalidation regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let original = room_service.get_room_settings(&room.id).await.unwrap();
    let updated = synctv_core::models::RoomSettings {
        chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
        ..synctv_core::models::RoomSettings::default()
    };

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .update_room_settings(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::UpdateRoomSettingsRequest {
                settings: serde_json::to_vec(&updated).unwrap(),
            },
        )
        .await
        .expect_err(
            "cluster mode must fail closed when room cache invalidation fanout fails for settings update",
        );

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out room cache invalidation to cluster replicas"
    );

    let settings = room_service.get_room_settings(&room.id).await.unwrap();
    assert_eq!(
        settings.chat_enabled.0, original.chat_enabled.0,
        "settings update must not commit before room cache invalidation capacity is reserved"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_room_settings_fails_closed_when_room_cache_invalidation_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo
        .create(&make_user("room_settings_reset_owner"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Room Settings Reset Cache Room".to_string(),
            "Room for reset cache invalidation regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let customized = synctv_core::models::RoomSettings {
        chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
        ..synctv_core::models::RoomSettings::default()
    };
    room_service
        .set_room_settings(&room.id, &customized)
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .reset_room_settings(owner.id.as_str(), room.id.as_str())
        .await
        .expect_err(
            "cluster mode must fail closed when room cache invalidation fanout fails for settings reset",
        );

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out room cache invalidation to cluster replicas"
    );

    let settings = room_service.get_room_settings(&room.id).await.unwrap();
    assert!(
        !settings.chat_enabled.0,
        "reset must not commit before room cache invalidation capacity is reserved"
    );
    assert!(
        settings.allow_guest_join.0,
        "customized settings must remain unchanged after failed reset"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_playlist_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo
        .create(&make_user("playlist_creator"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Playlist Fanout Room".to_string(),
            "Playlist creation fanout regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .create_playlist(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::CreatePlaylistRequest {
                name: "Should Fail".to_string(),
                parent_id: String::new(),
                source_provider: String::new(),
                source_config: Vec::new(),
                provider_instance_name: String::new(),
            },
        )
        .await
        .expect_err("cluster mode must fail closed when playlist create fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out PlaylistCreated to cluster replicas"
    );
    assert!(
        room_service
            .playlist_service()
            .get_top_level_playlists(&room.id)
            .await
            .unwrap()
            .is_empty(),
        "playlist creation must not commit before cluster fanout capacity is reserved"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_playlist_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo
        .create(&make_user("playlist_updater"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Playlist Update Room".to_string(),
            "Playlist update fanout regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    let playlist = room_service
        .playlist_service()
        .create_playlist(
            room.id.clone(),
            owner.id.clone(),
            synctv_core::service::playlist::CreatePlaylistRequest {
                room_id: room.id.clone(),
                name: "Original Name".to_string(),
                parent_id: None,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .update_playlist(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::UpdatePlaylistRequest {
                playlist_id: playlist.id.as_str().to_string(),
                name: "Updated Name".to_string(),
            },
        )
        .await
        .expect_err("cluster mode must fail closed when playlist update fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out PlaylistUpdated to cluster replicas"
    );
    let persisted = room_service
        .playlist_service()
        .get_playlist(&playlist.id)
        .await
        .unwrap()
        .expect("playlist should still exist");
    assert_eq!(
        persisted.name, "Original Name",
        "playlist update must not commit before cluster fanout capacity is reserved"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_playlist_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo
        .create(&make_user("playlist_deleter"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Playlist Delete Room".to_string(),
            "Playlist delete fanout regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    let playlist = room_service
        .playlist_service()
        .create_playlist(
            room.id.clone(),
            owner.id.clone(),
            synctv_core::service::playlist::CreatePlaylistRequest {
                room_id: room.id.clone(),
                name: "Delete Me".to_string(),
                parent_id: None,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, _rx) = tokio::sync::mpsc::channel(1);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .delete_playlist(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::DeletePlaylistRequest {
                playlist_id: playlist.id.as_str().to_string(),
                force: false,
            },
        )
        .await
        .expect_err("cluster mode must fail closed when playlist delete fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out PlaylistDeleted to cluster replicas"
    );
    assert!(
        room_service
            .playlist_service()
            .get_playlist(&playlist.id)
            .await
            .unwrap()
            .is_some(),
        "playlist deletion must not commit before cluster fanout capacity is reserved"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_entries_playlist_delete_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));

    let owner = user_repo
        .create(&make_user("playlist_delete_entries_deleter"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Playlist Delete Entries Room".to_string(),
            "Playlist delete-entries fanout regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    let playlist = room_service
        .playlist_service()
        .create_playlist(
            room.id.clone(),
            owner.id.clone(),
            synctv_core::service::playlist::CreatePlaylistRequest {
                room_id: room.id.clone(),
                name: "Delete Me Via Entries".to_string(),
                parent_id: None,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, _rx) = tokio::sync::mpsc::channel(1);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .delete_entries(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::DeleteEntriesRequest {
                playlist_ids: vec![playlist.id.as_str().to_string()],
                media_ids: Vec::new(),
                force: false,
            },
        )
        .await
        .expect_err("cluster mode must fail closed when delete_entries playlist fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out PlaylistDeleted to cluster replicas"
    );
    assert!(
        room_service
            .playlist_service()
            .get_playlist(&playlist.id)
            .await
            .unwrap()
            .is_some(),
        "delete_entries playlist deletion must not commit before cluster fanout capacity is reserved"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_entries_publishes_cascaded_playlist_and_media_events() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    room_service
        .media_service()
        .providers_manager()
        .create_builtin_defaults()
        .await
        .unwrap();

    let owner = user_repo
        .create(&make_user("delete_entries_cascade_owner"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Cascade Delete Room".to_string(),
            "Room for delete_entries cascade fanout regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let parent_playlist = room_service
        .playlist_service()
        .create_playlist(
            room.id.clone(),
            owner.id.clone(),
            CreatePlaylistRequest {
                room_id: room.id.clone(),
                name: "Parent Playlist".to_string(),
                parent_id: None,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .unwrap();
    let child_playlist = room_service
        .playlist_service()
        .create_playlist(
            room.id.clone(),
            owner.id.clone(),
            CreatePlaylistRequest {
                room_id: room.id.clone(),
                name: "Child Playlist".to_string(),
                parent_id: Some(parent_playlist.id.clone()),
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
            },
        )
        .await
        .unwrap();
    let nested_media = room_service
        .media_service()
        .add_media(
            room.id.clone(),
            owner.id.clone(),
            AddMediaRequest {
                playlist_id: Some(child_playlist.id.clone()),
                name: "Nested Media".to_string(),
                source_provider: "direct_url".to_string(),
                provider_instance_name: None,
                source_config: serde_json::json!({
                    "url": "https://example.com/nested-media.mp4"
                }),
            },
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, mut rx) = tokio::sync::mpsc::channel(8);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let response = client_api
        .delete_entries(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::DeleteEntriesRequest {
                playlist_ids: vec![parent_playlist.id.as_str().to_string()],
                media_ids: Vec::new(),
                force: false,
            },
        )
        .await
        .expect("delete_entries should succeed");

    assert_eq!(response.deleted_playlists, 2);
    assert_eq!(response.deleted_media, 1);

    let mut deleted_playlist_ids = Vec::new();
    let mut deleted_media_ids = Vec::new();
    let mut kicked_media_ids = Vec::new();

    while let Ok(request) = rx.try_recv() {
        match request.event {
            ClusterEvent::PlaylistDeleted { playlist_id, .. } => {
                deleted_playlist_ids.push(playlist_id.as_str().to_string());
            }
            ClusterEvent::MediaRemoved { media_id, .. } => {
                deleted_media_ids.push(media_id.as_str().to_string());
            }
            ClusterEvent::KickPublisher { media_id, .. } => {
                kicked_media_ids.push(media_id.as_str().to_string());
            }
            ClusterEvent::CacheInvalidate { .. } => {}
            other => panic!("unexpected delete_entries cascade event: {other:?}"),
        }
    }

    deleted_playlist_ids.sort_unstable();
    deleted_media_ids.sort_unstable();
    kicked_media_ids.sort_unstable();
    let mut expected_playlist_ids = vec![
        child_playlist.id.as_str().to_string(),
        parent_playlist.id.as_str().to_string(),
    ];
    expected_playlist_ids.sort_unstable();

    assert_eq!(
        deleted_playlist_ids,
        expected_playlist_ids,
        "cluster fanout must publish PlaylistDeleted for every playlist removed by recursive delete"
    );
    assert_eq!(
        deleted_media_ids,
        vec![nested_media.id.as_str().to_string()],
        "cluster fanout must publish MediaRemoved for media deleted through playlist cascade"
    );
    assert_eq!(
        kicked_media_ids,
        vec![nested_media.id.as_str().to_string()],
        "delete_entries must also kick publishers for media deleted through playlist cascade"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_fails_closed_when_cluster_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    room_service
        .media_service()
        .providers_manager()
        .create_builtin_defaults()
        .await
        .unwrap();

    let owner = user_repo.create(&make_user("media_owner")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Media Room".to_string(),
            "Room for media fanout regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .add_media(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::AddMediaRequest {
                playlist_id: None,
                provider: "direct_url".to_string(),
                provider_instance_name: String::new(),
                source_config: serde_json::to_vec(&serde_json::json!({
                    "url": "https://example.com/media.mp4"
                }))
                .unwrap(),
                title: "fanout-test-media".to_string(),
            },
        )
        .await
        .expect_err("cluster mode must fail closed when media add fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out MediaAdded to cluster replicas"
    );
    assert_eq!(
        room_service
            .media_service()
            .count_room_root_media(&room.id)
            .await
            .unwrap(),
        0,
        "media add must not commit before cluster fanout capacity is reserved"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_batch_fails_closed_when_cluster_fanout_capacity_is_insufficient() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    room_service
        .media_service()
        .providers_manager()
        .create_builtin_defaults()
        .await
        .unwrap();

    let owner = user_repo
        .create(&make_user("media_batch_owner"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Media Batch Room".to_string(),
            "Room for media batch fanout regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, _rx) = tokio::sync::mpsc::channel(1);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .add_media_batch(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::AddMediaBatchRequest {
                items: vec![
                    synctv_api::proto::client::AddMediaRequest {
                        playlist_id: None,
                        provider: "direct_url".to_string(),
                        provider_instance_name: String::new(),
                        source_config: serde_json::to_vec(&serde_json::json!({
                            "url": "https://example.com/media-a.mp4"
                        }))
                        .unwrap(),
                        title: "fanout-batch-a".to_string(),
                    },
                    synctv_api::proto::client::AddMediaRequest {
                        playlist_id: None,
                        provider: "direct_url".to_string(),
                        provider_instance_name: String::new(),
                        source_config: serde_json::to_vec(&serde_json::json!({
                            "url": "https://example.com/media-b.mp4"
                        }))
                        .unwrap(),
                        title: "fanout-batch-b".to_string(),
                    },
                ],
            },
        )
        .await
        .expect_err(
            "cluster mode must fail closed when batch media fanout capacity is insufficient",
        );

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out MediaAdded to cluster replicas"
    );
    assert_eq!(
        room_service
            .media_service()
            .count_room_root_media(&room.id)
            .await
            .unwrap(),
        0,
        "batch media add must not commit until all cluster fanout capacity is reserved"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_media_fails_closed_when_media_updated_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    room_service
        .media_service()
        .providers_manager()
        .create_builtin_defaults()
        .await
        .unwrap();

    let owner = user_repo
        .create(&make_user("media_reorder_owner"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Media Reorder Room".to_string(),
            "Room for move-media fanout regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let media_one = room_service
        .media_service()
        .add_media(
            room.id.clone(),
            owner.id.clone(),
            AddMediaRequest {
                playlist_id: None,
                name: "fanout-reorder-a".to_string(),
                source_provider: "direct_url".to_string(),
                provider_instance_name: None,
                source_config: serde_json::json!({
                    "url": "https://example.com/reorder-a.mp4"
                }),
            },
        )
        .await
        .unwrap();
    let media_two = room_service
        .media_service()
        .add_media(
            room.id.clone(),
            owner.id.clone(),
            AddMediaRequest {
                playlist_id: None,
                name: "fanout-reorder-b".to_string(),
                source_provider: "direct_url".to_string(),
                provider_instance_name: None,
                source_config: serde_json::json!({
                    "url": "https://example.com/reorder-b.mp4"
                }),
            },
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .move_media(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::MoveMediaRequest {
                media_ids: vec![media_two.id.as_str().to_string()],
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: Some(media_one.id.as_str().to_string()),
                after_media_id: None,
            },
        )
        .await
        .expect_err("cluster mode must fail closed when move-media fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out MediaUpdated to cluster replicas"
    );

    let media_after = room_service
        .media_service()
        .get_room_root_media(&room.id)
        .await
        .unwrap();
    let ordered_ids: Vec<&str> = media_after.iter().map(|media| media.id.as_str()).collect();
    assert_eq!(
        ordered_ids,
        vec![media_one.id.as_str(), media_two.id.as_str()],
        "media reorder must not commit before playlist fanout capacity is reserved"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_media_batch_fails_closed_when_playlist_reordered_fanout_fails() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    room_service
        .media_service()
        .providers_manager()
        .create_builtin_defaults()
        .await
        .unwrap();

    let owner = user_repo
        .create(&make_user("media_reorder_batch_owner"))
        .await
        .unwrap();
    let (room, _member) = room_service
        .create_room(
            "Media Reorder Batch Room".to_string(),
            "Room for playlist reordered fail-closed regression".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let media_one = room_service
        .media_service()
        .add_media(
            room.id.clone(),
            owner.id.clone(),
            AddMediaRequest {
                playlist_id: None,
                name: "fanout-reorder-batch-a".to_string(),
                source_provider: "direct_url".to_string(),
                provider_instance_name: None,
                source_config: serde_json::json!({
                    "url": "https://example.com/reorder-batch-a.mp4"
                }),
            },
        )
        .await
        .unwrap();
    let media_two = room_service
        .media_service()
        .add_media(
            room.id.clone(),
            owner.id.clone(),
            AddMediaRequest {
                playlist_id: None,
                name: "fanout-reorder-batch-b".to_string(),
                source_provider: "direct_url".to_string(),
                provider_instance_name: None,
                source_config: serde_json::json!({
                    "url": "https://example.com/reorder-batch-b.mp4"
                }),
            },
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let mut config = Config::default();
    config.cluster.enabled = true;
    config.redis.url = "redis://127.0.0.1:6379".to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        connection_manager,
        Arc::new(config),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_cluster_fanout_service(synctv_api::cluster_fanout::default_cluster_fanout_service(
        Some(tx),
        true,
    ));

    let err = client_api
        .move_media(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::MoveMediaRequest {
                media_ids: vec![
                    media_two.id.as_str().to_string(),
                    media_one.id.as_str().to_string(),
                ],
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: None,
                after_media_id: None,
            },
        )
        .await
        .expect_err("cluster mode must fail closed when playlist reordered fanout fails");

    assert!(matches!(err, ApiError::ServiceUnavailable(_)));
    assert_eq!(
        err.message(),
        "failed to fan out PlaylistReordered to cluster replicas"
    );

    let media_after = room_service
        .media_service()
        .get_room_root_media(&room.id)
        .await
        .unwrap();
    let ordered_ids: Vec<&str> = media_after.iter().map(|media| media.id.as_str()).collect();
    assert_eq!(
        ordered_ids,
        vec![media_one.id.as_str(), media_two.id.as_str()],
        "batch media reorder must not commit before playlist reordered fanout capacity is reserved"
    );
}
