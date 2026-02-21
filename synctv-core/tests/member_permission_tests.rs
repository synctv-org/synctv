//! MemberService permission tests (S6)
//!
//! Tests set_member_permissions GRANT_PERMISSION check, optimistic lock retry,
//! and reset_member_permissions with real PostgreSQL via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test member_permission_tests -- --nocapture

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, UsernameCache, NoopCacheL2},
    config::PasswordComplexityConfig,
    models::{
        UserId, User, UserRole, UserStatus,
        PermissionBits,
    },
    repository::{UserRepository, RoomMemberRepository},
    service::{
        RoomService, UserService, InMemoryTokenBlacklistStore,
        auth::{JwtService, BruteForceProtection},
    },
    Error,
};
use chrono::Utc;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres.get_host_port_ipv4(5432).await.expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

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

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{}@test.com", username)),
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

// ========== set_member_permissions: GRANT_PERMISSION check ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_permissions_requires_grant_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("smp_creator")).await.unwrap();
    let member = user_repo.create(&make_user("smp_member")).await.unwrap();
    let target = user_repo.create(&make_user("smp_target")).await.unwrap();

    let (room, _) = room_service
        .create_room("SMP Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), member.id.clone(), None).await.unwrap();
    room_service.join_room(room.id.clone(), target.id.clone(), None).await.unwrap();

    let member_service = room_service.member_service();

    // Member does NOT have GRANT_PERMISSION by default
    let result = member_service.set_member_permissions(
        room.id.clone(),
        member.id.clone(),
        target.id.clone(),
        PermissionBits::SEND_CHAT,
        0,
    ).await;

    assert!(result.is_err(), "Member without GRANT_PERMISSION should be denied");
    match result.unwrap_err() {
        Error::Authorization(_) => {}
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_permissions_creator_can_set() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("smp2_creator")).await.unwrap();
    let target = user_repo.create(&make_user("smp2_target")).await.unwrap();

    let (room, _) = room_service
        .create_room("SMP2 Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), target.id.clone(), None).await.unwrap();

    let member_service = room_service.member_service();

    // Creator has GRANT_PERMISSION
    let updated = member_service.set_member_permissions(
        room.id.clone(),
        creator.id.clone(),
        target.id.clone(),
        PermissionBits::BAN_MEMBER | PermissionBits::KICK_USER,
        0,
    ).await.unwrap();

    assert!(updated.added_permissions & PermissionBits::BAN_MEMBER != 0,
        "BAN_MEMBER should be added");
    assert!(updated.added_permissions & PermissionBits::KICK_USER != 0,
        "KICK_USER should be added");
}

// ========== set_member_permissions: optimistic lock retry ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_permissions_optimistic_lock_retry() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let creator = user_repo.create(&make_user("olr_creator")).await.unwrap();
    let target = user_repo.create(&make_user("olr_target")).await.unwrap();

    let (room, _) = room_service
        .create_room("OLR Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), target.id.clone(), None).await.unwrap();

    // Bump version concurrently to trigger retries
    let room_id_str = room.id.as_str().to_string();
    let target_id_str = target.id.as_str().to_string();
    let pool_clone = pool.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();

    let bumper = tokio::spawn(async move {
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = sqlx::query(
                "UPDATE room_members SET version = version + 1 WHERE room_id = $1 AND user_id = $2"
            )
            .bind(&room_id_str)
            .bind(&target_id_str)
            .execute(&pool_clone)
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });

    let member_service = room_service.member_service();
    let result = member_service.set_member_permissions(
        room.id.clone(),
        creator.id.clone(),
        target.id.clone(),
        PermissionBits::BAN_MEMBER,
        0,
    ).await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = bumper.await;

    // Either succeeds (retries worked) or returns Internal (retry exhaustion)
    match result {
        Ok(_) => {}  // Retries succeeded
        Err(Error::Internal(msg)) => {
            assert!(msg.contains("retry") || msg.contains("maximum"),
                "Should mention retry exhaustion: {}", msg);
        }
        Err(Error::OptimisticLockConflict) => {
            panic!("OptimisticLockConflict should not leak to caller");
        }
        Err(other) => {
            panic!("Unexpected error: {:?}", other);
        }
    }
}

// ========== reset_member_permissions: clears all overrides ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_member_permissions_clears_all_overrides() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("reset_creator")).await.unwrap();
    let target = user_repo.create(&make_user("reset_target")).await.unwrap();

    let (room, _) = room_service
        .create_room("Reset Perm Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), target.id.clone(), None).await.unwrap();

    let member_service = room_service.member_service();

    // First, set some permissions
    member_service.set_member_permissions(
        room.id.clone(),
        creator.id.clone(),
        target.id.clone(),
        PermissionBits::BAN_MEMBER | PermissionBits::KICK_USER,
        PermissionBits::SEND_CHAT,
    ).await.unwrap();

    // Verify overrides were applied
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member_before = member_repo.get(&room.id, &target.id).await.unwrap().unwrap();
    assert!(member_before.added_permissions & PermissionBits::BAN_MEMBER != 0,
        "Should have BAN_MEMBER added before reset");
    assert!(member_before.removed_permissions & PermissionBits::SEND_CHAT != 0,
        "Should have SEND_CHAT removed before reset");

    // Reset all permissions
    let updated = member_service.reset_member_permissions(
        room.id.clone(),
        creator.id.clone(),
        target.id.clone(),
    ).await.unwrap();

    assert_eq!(updated.added_permissions, 0, "Added permissions should be 0 after reset");
    assert_eq!(updated.removed_permissions, 0, "Removed permissions should be 0 after reset");
}

// ========== reset_member_permissions: requires GRANT_PERMISSION ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_member_permissions_requires_grant_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("resetp_creator")).await.unwrap();
    let member = user_repo.create(&make_user("resetp_member")).await.unwrap();
    let target = user_repo.create(&make_user("resetp_target")).await.unwrap();

    let (room, _) = room_service
        .create_room("Reset Perm Check Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), member.id.clone(), None).await.unwrap();
    room_service.join_room(room.id.clone(), target.id.clone(), None).await.unwrap();

    let member_service = room_service.member_service();

    // Member without GRANT_PERMISSION cannot reset
    let result = member_service.reset_member_permissions(
        room.id.clone(),
        member.id.clone(),
        target.id.clone(),
    ).await;

    assert!(result.is_err(), "Member without GRANT_PERMISSION should be denied");
    match result.unwrap_err() {
        Error::Authorization(_) => {}
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}
