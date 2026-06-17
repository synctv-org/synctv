//! `MemberService` permission tests (S6)
//!
//! Tests `set_member_permissions` `GRANT_PERMISSION` check, optimistic lock retry,
//! and `reset_member_permissions` with real `PostgreSQL` via testcontainers.
//!
use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{CacheDomain, KeyBuilder, LocalVersionFenceStore, UsernameCache, VersionFenceStore},
    models::{
        RoomAdminPermissionBits, RoomMemberPermissionBits, RoomPermission, RoomRole, User, UserId,
        UserRole, UserStatus,
    },
    repository::{RoomMemberRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        member::AdminMemberUpdate,
        room::RoomServiceOptions,
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::{create_test_pool, TestOptionExt, TestResultExt};
fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).checked("Failed to create JwtService");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
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

fn make_room_service_with_fence(
    pool: &PgPool,
    version_fence: Arc<dyn VersionFenceStore>,
) -> RoomService {
    let user_service = make_user_service(pool);

    RoomService::new_with_options(
        pool.clone(),
        user_service,
        RoomServiceOptions {
            version_fence,
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .checked("room service should build")
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_permissions_requires_grant_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("smp_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("smp_member"))
        .await
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("smp_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "SMP Room".to_string(),
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
    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    let member_service = room_service.member_service();

    // Member does NOT have GRANT_PERMISSION by default
    let result = member_service
        .set_member_permissions(
            room.id,
            member.id,
            target.id,
            RoomMemberPermissionBits::CHAT,
            0,
        )
        .await;

    assert!(
        result.is_err(),
        "Member without GRANT_PERMISSION should be denied"
    );
    match result.failed("operation should fail") {
        Error::Authorization(_) => {}
        other => std::panic::panic_any(format!("expected Authorization error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_permissions_creator_can_set() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("smp2_creator"))
        .await
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("smp2_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "SMP2 Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    let member_service = room_service.member_service();

    // Creator has GRANT_PERMISSION
    let updated = member_service
        .set_member_permissions(
            room.id,
            creator.id,
            target.id,
            RoomMemberPermissionBits::USE_WEBRTC | RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
            0,
        )
        .await
        .checked("test operation should succeed");

    assert!(
        updated.added_permissions & RoomMemberPermissionBits::USE_WEBRTC != 0,
        "USE_WEBRTC should be added"
    );
    assert!(
        updated.added_permissions & RoomMemberPermissionBits::VIEW_CHAT_HISTORY != 0,
        "VIEW_CHAT_HISTORY should be added"
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
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("smp_admin_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "SMP Admin Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    room_service
        .member_service()
        .set_member_role(room.id, creator.id, target.id, RoomRole::Admin)
        .await
        .checked("test operation should succeed");

    let member_service = room_service.member_service();
    let updated = member_service
        .set_member_permissions(
            room.id,
            creator.id,
            target.id,
            RoomAdminPermissionBits::USE_WEBRTC,
            RoomAdminPermissionBits::KICK_MEMBER,
        )
        .await
        .checked("test operation should succeed");

    assert_eq!(
        updated.admin_added_permissions & RoomAdminPermissionBits::USE_WEBRTC,
        RoomAdminPermissionBits::USE_WEBRTC,
        "admin target must persist allow overrides into admin_added_permissions"
    );
    assert_eq!(
        updated.admin_removed_permissions & RoomAdminPermissionBits::KICK_MEMBER,
        RoomAdminPermissionBits::KICK_MEMBER,
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
        .checked("test operation should succeed");
    assert!(
        effective.has(RoomPermission::USE_WEBRTC),
        "admin allow override should affect effective permissions"
    );
    assert!(
        !effective.has(RoomPermission::KICK_MEMBER),
        "admin deny override should affect effective permissions"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_update_member_role_to_admin_persists_admin_overrides() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let version_fence = Arc::new(LocalVersionFenceStore::new());
    let room_service = make_room_service_with_fence(&pool, version_fence.clone());

    let creator = user_repo
        .create(&make_user("aum_admin_creator"))
        .await
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("aum_admin_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Admin Update Member Override Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    let updated = room_service
        .member_service()
        .admin_update_member(AdminMemberUpdate {
            room_id: room.id,
            actor_id: creator.id,
            actor_username: creator.username.clone(),
            target_user_id: target.id,
            role: Some(RoomRole::Admin),
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: RoomAdminPermissionBits::USE_WEBRTC,
            admin_removed_permissions: RoomAdminPermissionBits::KICK_MEMBER,
        })
        .await
        .checked("test operation should succeed");

    assert_eq!(updated.role, RoomRole::Admin);
    assert_eq!(
        updated.admin_added_permissions & RoomAdminPermissionBits::USE_WEBRTC,
        RoomAdminPermissionBits::USE_WEBRTC,
        "role-to-admin update must persist allow override in admin_added_permissions"
    );
    assert_eq!(
        updated.admin_removed_permissions & RoomAdminPermissionBits::KICK_MEMBER,
        RoomAdminPermissionBits::KICK_MEMBER,
        "role-to-admin update must persist deny override in admin_removed_permissions"
    );
    assert_eq!(
        updated.added_permissions, 0,
        "role-to-admin update must not write admin override into member added_permissions"
    );
    assert_eq!(
        updated.removed_permissions, 0,
        "role-to-admin update must not write admin override into member removed_permissions"
    );

    let state = version_fence
        .current_state(&CacheDomain::Permission {
            room_id: room.id,
            user_id: target.id,
        })
        .await
        .checked("test operation should succeed")
        .checked("permission fence should be committed after update");
    assert_eq!(
        state.pending_version, None,
        "role plus permission update must commit every reservation it created"
    );
    assert_eq!(state.committed_version, updated.version);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_transfer_room_ownership_commits_permission_fences_for_both_members() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let version_fence = Arc::new(LocalVersionFenceStore::new());
    let room_service = make_room_service_with_fence(&pool, version_fence.clone());

    let old_owner = user_repo
        .create(&make_user("transfer_fence_old_owner"))
        .await
        .checked("test operation should succeed");
    let new_owner = user_repo
        .create(&make_user("transfer_fence_new_owner"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Transfer Fence Room".to_string(),
            String::new(),
            old_owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, new_owner.id, None)
        .await
        .checked("test operation should succeed");

    room_service
        .transfer_room_ownership(room.id, old_owner.id, new_owner.id)
        .await
        .checked("test operation should succeed");

    let updated_old_owner = member_repo
        .get(&room.id, &old_owner.id)
        .await
        .checked("test operation should succeed")
        .checked("old owner member should remain active");
    let updated_new_owner = member_repo
        .get(&room.id, &new_owner.id)
        .await
        .checked("test operation should succeed")
        .checked("new owner member should remain active");

    assert_eq!(updated_old_owner.role, RoomRole::Admin);
    assert_eq!(updated_new_owner.role, RoomRole::Creator);

    let old_owner_fence = version_fence
        .current_state(&CacheDomain::Permission {
            room_id: room.id,
            user_id: old_owner.id,
        })
        .await
        .checked("test operation should succeed")
        .checked("old owner permission fence should be committed");
    let new_owner_fence = version_fence
        .current_state(&CacheDomain::Permission {
            room_id: room.id,
            user_id: new_owner.id,
        })
        .await
        .checked("test operation should succeed")
        .checked("new owner permission fence should be committed");

    assert_eq!(old_owner_fence.pending_version, None);
    assert_eq!(new_owner_fence.pending_version, None);
    assert_eq!(
        old_owner_fence.committed_version, updated_old_owner.version,
        "ownership transfer must finalize the previous owner's permission fence"
    );
    assert_eq!(
        new_owner_fence.committed_version, updated_new_owner.version,
        "ownership transfer must finalize the new owner's permission fence"
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
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("deny_delete_room_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Disallow Delete Room Permission".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    let err = room_service
        .member_service()
        .set_member_permissions(room.id, creator.id, target.id, 1 << 21, 0)
        .await
        .failed("operation should fail");

    match err {
        Error::InvalidInput(message) => {
            assert!(
                message.contains("member permission bitspace"),
                "got: {message}"
            );
        }
        other => std::panic::panic_any(format!("expected InvalidInput error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_permissions_optimistic_lock_retry() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let creator = user_repo
        .create(&make_user("olr_creator"))
        .await
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("olr_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "OLR Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    // Bump version concurrently to trigger retries
    let room_id = room.id;
    let target_id = target.id;
    let pool_clone = pool.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();

    let bumper = tokio::spawn(async move {
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            sqlx::query!(
                "UPDATE room_members SET version = version + 1 WHERE room_id = $1 AND user_id = $2",
                room_id.as_i64(),
                target_id.as_i64()
            )
            .execute(&pool_clone)
            .await?;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        Ok::<_, sqlx::Error>(())
    });

    let member_service = room_service.member_service();
    let result = member_service
        .set_member_permissions(
            room.id,
            creator.id,
            target.id,
            RoomMemberPermissionBits::USE_WEBRTC,
            0,
        )
        .await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    bumper
        .await
        .checked("bumper task should not panic")
        .checked("bumper task should update member versions");

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
            std::panic::panic_any("OptimisticLockConflict should not leak to caller");
        }
        Err(other) => {
            std::panic::panic_any(format!("unexpected error: {other:?}"));
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_single_bit_grants_retry_optimistic_conflicts() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let creator = user_repo
        .create(&make_user("grant_race_creator"))
        .await
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("grant_race_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Grant Race Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let first_service = room_service.clone();
    let first_barrier = barrier.clone();
    let first = tokio::spawn(async move {
        first_barrier.wait().await;
        first_service
            .member_service()
            .grant_permission(
                room.id,
                creator.id,
                target.id,
                RoomMemberPermissionBits::USE_WEBRTC,
            )
            .await
    });
    let second_service = room_service.clone();
    let second = tokio::spawn(async move {
        barrier.wait().await;
        second_service
            .member_service()
            .grant_permission(
                room.id,
                creator.id,
                target.id,
                RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
            )
            .await
    });

    first
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    second
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");

    let refreshed = RoomMemberRepository::new(pool)
        .get(&room.id, &target.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    assert_eq!(
        refreshed.added_permissions & RoomMemberPermissionBits::USE_WEBRTC,
        RoomMemberPermissionBits::USE_WEBRTC
    );
    assert_eq!(
        refreshed.added_permissions & RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
        RoomMemberPermissionBits::VIEW_CHAT_HISTORY
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_single_bit_revokes_retry_optimistic_conflicts() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let creator = user_repo
        .create(&make_user("revoke_race_creator"))
        .await
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("revoke_race_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Revoke Race Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let first_service = room_service.clone();
    let first_barrier = barrier.clone();
    let first = tokio::spawn(async move {
        first_barrier.wait().await;
        first_service
            .member_service()
            .revoke_permission(
                room.id,
                creator.id,
                target.id,
                RoomMemberPermissionBits::USE_WEBRTC,
            )
            .await
    });
    let second_service = room_service.clone();
    let second = tokio::spawn(async move {
        barrier.wait().await;
        second_service
            .member_service()
            .revoke_permission(
                room.id,
                creator.id,
                target.id,
                RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
            )
            .await
    });

    first
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    second
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");

    let refreshed = RoomMemberRepository::new(pool)
        .get(&room.id, &target.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    assert_eq!(
        refreshed.removed_permissions & RoomMemberPermissionBits::USE_WEBRTC,
        RoomMemberPermissionBits::USE_WEBRTC
    );
    assert_eq!(
        refreshed.removed_permissions & RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
        RoomMemberPermissionBits::VIEW_CHAT_HISTORY
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_member_permissions_clears_all_overrides() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("reset_creator"))
        .await
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("reset_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Reset Perm Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    let member_service = room_service.member_service();

    // First, set some permissions
    member_service
        .set_member_permissions(
            room.id,
            creator.id,
            target.id,
            RoomMemberPermissionBits::USE_WEBRTC | RoomMemberPermissionBits::VIEW_CHAT_HISTORY,
            RoomMemberPermissionBits::CHAT,
        )
        .await
        .checked("test operation should succeed");

    // Verify overrides were applied
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member_before = member_repo
        .get(&room.id, &target.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    assert!(
        member_before.added_permissions & RoomMemberPermissionBits::VIEW_CHAT_HISTORY != 0,
        "Should have VIEW_CHAT_HISTORY added before reset"
    );
    assert!(
        member_before.removed_permissions & RoomMemberPermissionBits::CHAT != 0,
        "Should have CHAT removed before reset"
    );

    // Reset all permissions
    let updated = member_service
        .reset_member_permissions(room.id, creator.id, target.id)
        .await
        .checked("test operation should succeed");

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
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("resetp_member"))
        .await
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("resetp_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Reset Perm Check Room".to_string(),
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
    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    let member_service = room_service.member_service();

    // Member without GRANT_PERMISSION cannot reset
    let result = member_service
        .reset_member_permissions(room.id, member.id, target.id)
        .await;

    assert!(
        result.is_err(),
        "Member without GRANT_PERMISSION should be denied"
    );
    match result.failed("operation should fail") {
        Error::Authorization(_) => {}
        other => std::panic::panic_any(format!("expected Authorization error, got: {other:?}")),
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
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("grp_admin_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Grant Admin Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    room_service
        .member_service()
        .set_member_role(room.id, creator.id, target.id, RoomRole::Admin)
        .await
        .checked("test operation should succeed");

    let member_service = room_service.member_service();
    let updated = member_service
        .grant_permission(
            room.id,
            creator.id,
            target.id,
            RoomAdminPermissionBits::USE_WEBRTC,
        )
        .await
        .checked("test operation should succeed");

    assert_eq!(
        updated.admin_added_permissions & RoomAdminPermissionBits::USE_WEBRTC,
        RoomAdminPermissionBits::USE_WEBRTC,
        "grant_permission must target admin_added_permissions for admin members"
    );
    assert_eq!(
        updated.added_permissions, 0,
        "member-level added_permissions should remain untouched for admin members"
    );

    let updated = member_service
        .revoke_permission(
            room.id,
            creator.id,
            target.id,
            RoomAdminPermissionBits::KICK_MEMBER,
        )
        .await
        .checked("test operation should succeed");

    assert_eq!(
        updated.admin_removed_permissions & RoomAdminPermissionBits::KICK_MEMBER,
        RoomAdminPermissionBits::KICK_MEMBER,
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
        .checked("test operation should succeed");
    assert!(
        effective.has(RoomPermission::USE_WEBRTC),
        "granted admin override should be visible in effective permissions"
    );
    assert!(
        !effective.has(RoomPermission::KICK_MEMBER),
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
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("deny_delete_room_grant_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Disallow Delete Room Grant".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    let err = room_service
        .member_service()
        .grant_permission(room.id, creator.id, target.id, 1 << 21)
        .await
        .failed("operation should fail");

    match err {
        Error::InvalidInput(message) => {
            assert!(
                message.contains("member permission bitspace"),
                "got: {message}"
            );
        }
        other => std::panic::panic_any(format!("expected InvalidInput error, got: {other:?}")),
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
        .checked("test operation should succeed");
    let target = user_repo
        .create(&make_user("stale_admin_grant_target"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Stale Admin Grant".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, target.id, None)
        .await
        .checked("test operation should succeed");

    room_service
        .member_service()
        .set_member_role(room.id, creator.id, target.id, RoomRole::Admin)
        .await
        .checked("test operation should succeed");

    let stale_admin = member_repo
        .get(&room.id, &target.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");

    room_service
        .member_service()
        .set_member_role(room.id, creator.id, target.id, RoomRole::Member)
        .await
        .checked("test operation should succeed");

    let err = member_repo
        .grant_admin_permission_atomic_for_role(&room.id, &target.id, 1 << 21, stale_admin.role)
        .await
        .failed("operation should fail");
    assert!(matches!(err, synctv_core::Error::OptimisticLockConflict));

    let refreshed = member_repo
        .get(&room.id, &target.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    assert_eq!(refreshed.admin_added_permissions, 0);
    assert_eq!(refreshed.added_permissions, 0);
}
