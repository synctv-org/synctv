//! `MemberService` permission tests (S6)
//!
//! Tests `set_member_permissions` `GRANT_PERMISSION` check, optimistic lock retry,
//! and `reset_member_permissions` with real `PostgreSQL` via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test `member_permission_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{PermissionBits, RoomRole, User, UserId, UserRole, UserStatus},
    repository::{RoomMemberRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;
fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let mut svc = UserService::new(
        pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    );
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);
    let mut svc = RoomService::new(pool, user_service);
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

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
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

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
        .create_room(
            "SMP Room".to_string(),
            String::new(),
            creator.id,
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
        .join_room(room.id, target.id, None)
        .await
        .unwrap();

    let member_service = room_service.member_service();

    // Member does NOT have GRANT_PERMISSION by default
    let result = member_service
        .set_member_permissions(room.id, member.id, target.id, PermissionBits::SEND_CHAT, 0)
        .await;

    assert!(
        result.is_err(),
        "Member without GRANT_PERMISSION should be denied"
    );
    match result.unwrap_err() {
        Error::Authorization(_) => {}
        other => panic!("Expected Authorization error, got: {other:?}"),
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
        .create_room(
            "SMP2 Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();

    let member_service = room_service.member_service();

    // Creator has GRANT_PERMISSION
    let updated = member_service
        .set_member_permissions(
            room.id,
            creator.id,
            target.id,
            PermissionBits::KICK_MEMBER | PermissionBits::USE_WEBRTC,
            0,
        )
        .await
        .unwrap();

    assert!(
        updated.added_permissions & PermissionBits::USE_WEBRTC != 0,
        "USE_WEBRTC should be added"
    );
    assert!(
        updated.added_permissions & PermissionBits::KICK_MEMBER != 0,
        "KICK_MEMBER should be added"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_permissions_updates_admin_override_fields_for_admin_target() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("smp_admin_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("smp_admin_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "SMP Admin Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();

    room_service
        .member_service()
        .set_member_role(room.id, creator.id, target.id, RoomRole::Admin)
        .await
        .unwrap();

    let member_service = room_service.member_service();
    let updated = member_service
        .set_member_permissions(
            room.id,
            creator.id,
            target.id,
            PermissionBits::USE_WEBRTC,
            PermissionBits::KICK_MEMBER,
        )
        .await
        .unwrap();

    assert_eq!(
        updated.admin_added_permissions & PermissionBits::USE_WEBRTC,
        PermissionBits::USE_WEBRTC,
        "admin target must persist allow overrides into admin_added_permissions"
    );
    assert_eq!(
        updated.admin_removed_permissions & PermissionBits::KICK_MEMBER,
        PermissionBits::KICK_MEMBER,
        "admin target must persist deny overrides into admin_removed_permissions"
    );
    assert_eq!(
        updated.added_permissions, 0,
        "admin-specific override writes must not leak into member-level added_permissions"
    );
    assert_eq!(
        updated.removed_permissions, 0,
        "admin-specific override writes must not leak into member-level removed_permissions"
    );

    let effective = room_service
        .permission_service()
        .get_user_permissions_no_cache(&room.id, &target.id)
        .await
        .unwrap();
    assert!(
        effective.has(PermissionBits::USE_WEBRTC),
        "admin allow override should affect effective permissions"
    );
    assert!(
        !effective.has(PermissionBits::KICK_MEMBER),
        "admin deny override should affect effective permissions"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_permissions_rejects_lifecycle_only_delete_room_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("deny_delete_room_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("deny_delete_room_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Disallow Delete Room Permission".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();

    let err = room_service
        .member_service()
        .set_member_permissions(
            room.id,
            creator.id,
            target.id,
            PermissionBits::DELETE_ROOM,
            0,
        )
        .await
        .unwrap_err();

    match err {
        Error::InvalidInput(message) => {
            assert!(message.contains("lifecycle-only"), "got: {message}");
        }
        other => panic!("Expected InvalidInput, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_permissions_optimistic_lock_retry() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let creator = user_repo.create(&make_user("olr_creator")).await.unwrap();
    let target = user_repo.create(&make_user("olr_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "OLR Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();

    // Bump version concurrently to trigger retries
    let room_id_str = room.id.to_string();
    let target_id_str = target.id.to_string();
    let pool_clone = pool.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();

    let bumper = tokio::spawn(async move {
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = sqlx::query(
                "UPDATE room_members SET version = version + 1 WHERE room_id = $1 AND user_id = $2",
            )
            .bind(&room_id_str)
            .bind(&target_id_str)
            .execute(&pool_clone)
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });

    let member_service = room_service.member_service();
    let result = member_service
        .set_member_permissions(
            room.id,
            creator.id,
            target.id,
            PermissionBits::KICK_MEMBER,
            0,
        )
        .await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = bumper.await;

    // Either succeeds (retries worked) or returns Internal (retry exhaustion)
    match result {
        Ok(_) => {} // Retries succeeded
        Err(Error::Internal(msg)) => {
            assert!(
                msg.contains("retry") || msg.contains("maximum"),
                "Should mention retry exhaustion: {msg}"
            );
        }
        Err(Error::OptimisticLockConflict) => {
            panic!("OptimisticLockConflict should not leak to caller");
        }
        Err(other) => {
            panic!("Unexpected error: {other:?}");
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_member_permissions_clears_all_overrides() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("reset_creator")).await.unwrap();
    let target = user_repo.create(&make_user("reset_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Reset Perm Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();

    let member_service = room_service.member_service();

    // First, set some permissions
    member_service
        .set_member_permissions(
            room.id,
            creator.id,
            target.id,
            PermissionBits::KICK_MEMBER | PermissionBits::USE_WEBRTC,
            PermissionBits::SEND_CHAT,
        )
        .await
        .unwrap();

    // Verify overrides were applied
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member_before = member_repo
        .get(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        member_before.added_permissions & PermissionBits::KICK_MEMBER != 0,
        "Should have KICK_MEMBER added before reset"
    );
    assert!(
        member_before.removed_permissions & PermissionBits::SEND_CHAT != 0,
        "Should have SEND_CHAT removed before reset"
    );

    // Reset all permissions
    let updated = member_service
        .reset_member_permissions(room.id, creator.id, target.id)
        .await
        .unwrap();

    assert_eq!(
        updated.added_permissions, 0,
        "Added permissions should be 0 after reset"
    );
    assert_eq!(
        updated.removed_permissions, 0,
        "Removed permissions should be 0 after reset"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_member_permissions_requires_grant_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("resetp_creator"))
        .await
        .unwrap();
    let member = user_repo.create(&make_user("resetp_member")).await.unwrap();
    let target = user_repo.create(&make_user("resetp_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Reset Perm Check Room".to_string(),
            String::new(),
            creator.id,
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
        .join_room(room.id, target.id, None)
        .await
        .unwrap();

    let member_service = room_service.member_service();

    // Member without GRANT_PERMISSION cannot reset
    let result = member_service
        .reset_member_permissions(room.id, member.id, target.id)
        .await;

    assert!(
        result.is_err(),
        "Member without GRANT_PERMISSION should be denied"
    );
    match result.unwrap_err() {
        Error::Authorization(_) => {}
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_grant_and_revoke_permission_target_admin_use_admin_override_fields() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("grp_admin_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("grp_admin_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Grant Admin Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();

    room_service
        .member_service()
        .set_member_role(room.id, creator.id, target.id, RoomRole::Admin)
        .await
        .unwrap();

    let member_service = room_service.member_service();
    let updated = member_service
        .grant_permission(room.id, creator.id, target.id, PermissionBits::USE_WEBRTC)
        .await
        .unwrap();

    assert_eq!(
        updated.admin_added_permissions & PermissionBits::USE_WEBRTC,
        PermissionBits::USE_WEBRTC,
        "grant_permission must target admin_added_permissions for admin members"
    );
    assert_eq!(
        updated.added_permissions, 0,
        "member-level added_permissions should remain untouched for admin members"
    );

    let updated = member_service
        .revoke_permission(room.id, creator.id, target.id, PermissionBits::KICK_MEMBER)
        .await
        .unwrap();

    assert_eq!(
        updated.admin_removed_permissions & PermissionBits::KICK_MEMBER,
        PermissionBits::KICK_MEMBER,
        "revoke_permission must target admin_removed_permissions for admin members"
    );
    assert_eq!(
        updated.removed_permissions, 0,
        "member-level removed_permissions should remain untouched for admin members"
    );

    let effective = room_service
        .permission_service()
        .get_user_permissions_no_cache(&room.id, &target.id)
        .await
        .unwrap();
    assert!(
        effective.has(PermissionBits::USE_WEBRTC),
        "granted admin override should be visible in effective permissions"
    );
    assert!(
        !effective.has(PermissionBits::KICK_MEMBER),
        "revoked admin override should be visible in effective permissions"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_grant_permission_rejects_lifecycle_only_delete_room_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("deny_delete_room_grant_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("deny_delete_room_grant_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Disallow Delete Room Grant".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();

    let err = room_service
        .member_service()
        .grant_permission(room.id, creator.id, target.id, PermissionBits::DELETE_ROOM)
        .await
        .unwrap_err();

    match err {
        Error::InvalidInput(message) => {
            assert!(message.contains("lifecycle-only"), "got: {message}");
        }
        other => panic!("Expected InvalidInput, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_stale_admin_role_grant_fails_closed_without_writing_override_columns() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = synctv_core::repository::RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("stale_admin_grant_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("stale_admin_grant_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Stale Admin Grant".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();

    room_service
        .member_service()
        .set_member_role(room.id, creator.id, target.id, RoomRole::Admin)
        .await
        .unwrap();

    let stale_admin = member_repo
        .get(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();

    room_service
        .member_service()
        .set_member_role(room.id, creator.id, target.id, RoomRole::Member)
        .await
        .unwrap();

    let err = member_repo
        .grant_admin_permission_atomic_for_role(
            &room.id,
            &target.id,
            PermissionBits::DELETE_ROOM,
            stale_admin.role,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, synctv_core::Error::OptimisticLockConflict));

    let refreshed = member_repo
        .get(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.admin_added_permissions, 0);
    assert_eq!(refreshed.added_permissions, 0);
}
