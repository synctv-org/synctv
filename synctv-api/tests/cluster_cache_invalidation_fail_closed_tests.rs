#![allow(clippy::unwrap_used)]

use chrono::Utc;
use std::sync::Arc;
use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{MemberStatus, RoomRole, SignupMethod, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Config,
};
use synctv_api::impls::{ApiError, ClientApiImpl};

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
    let username_cache =
        UsernameCache::new(Arc::new(NoopCacheL2), "test:username:".to_string(), 100, 60);
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
    .with_redis_publish_tx(Some(tx));

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
    .with_redis_publish_tx(Some(tx));

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
    .with_redis_publish_tx(Some(tx));

    let err = client_api
        .kick_member(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::KickMemberRequest {
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
    .with_redis_publish_tx(Some(tx));

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
        .expect("ban must not commit before fanout reservation");
    assert_eq!(member.status, MemberStatus::Active);
    assert!(member.banned_at.is_none(), "ban state must remain unchanged");
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
    .with_redis_publish_tx(Some(tx));

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
