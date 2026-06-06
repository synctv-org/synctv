//! `MemberService` integration tests
//!
//! Tests member management including max members, kick hierarchy,
//! and permission operations with real `PostgreSQL` via testcontainers.
//!
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        room_settings::MaxMembers, RoomId, RoomMemberPermissionBits, RoomPermission, RoomRole,
        User, UserId, UserRole, UserStatus,
    },
    repository::{RoomMemberRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;

async fn expire_kick_cooldown(pool: &PgPool, room_id: RoomId, user_id: UserId) {
    sqlx::query!(
        "UPDATE room_member_kick_cooldowns
         SET ends_at = CURRENT_TIMESTAMP - INTERVAL '1 second'
         WHERE room_id = $1 AND user_id = $2",
        room_id as RoomId,
        user_id as UserId,
    )
    .execute(pool)
    .await
    .unwrap();
}

fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
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

    RoomService::new_for_tests(pool, user_service).expect("room service should build")
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
async fn test_add_member_respects_max_members() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("max_owner")).await.unwrap();

    let settings = synctv_core::models::RoomSettings {
        max_members: MaxMembers(2),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Max Members Room".to_string(),
            String::new(),
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();

    // First joiner should succeed (member count: 2)
    let joiner1 = user_repo.create(&make_user("max_joiner1")).await.unwrap();
    let result = room_service.join_room(room.id, joiner1.id, None).await;
    assert!(result.is_ok(), "First joiner should succeed");

    // Second joiner should fail (member count would be 3, exceeding max 2)
    let joiner2 = user_repo.create(&make_user("max_joiner2")).await.unwrap();
    let result = room_service.join_room(room.id, joiner2.id, None).await;
    assert!(result.is_err(), "Second joiner should be rejected");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_member_role_hierarchy() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("kick_creator")).await.unwrap();
    let admin = user_repo.create(&make_user("kick_admin")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Kick Hierarchy Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Add admin as member first, then promote to admin
    room_service
        .join_room(room.id, admin.id, None)
        .await
        .unwrap();

    // Promote to admin role
    let member_service = room_service.member_service();
    member_service
        .set_member_role(room.id, creator.id, admin.id, RoomRole::Admin)
        .await
        .unwrap();

    // Admin trying to kick Creator should fail
    let result = room_service
        .kick_member(room.id, admin.id, creator.id, 60)
        .await;

    assert!(result.is_err(), "Admin cannot kick Creator");
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("cannot kick") || msg.contains("equal or higher"),
                "Error should mention role hierarchy: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_member_creator_can_kick_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("kick_c_creator"))
        .await
        .unwrap();
    let admin = user_repo.create(&make_user("kick_c_admin")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Kick Creator Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, admin.id, None)
        .await
        .unwrap();

    // Promote to admin
    let member_service = room_service.member_service();
    member_service
        .set_member_role(room.id, creator.id, admin.id, RoomRole::Admin)
        .await
        .unwrap();

    // Creator should be able to kick admin
    let result = room_service
        .kick_member(room.id, creator.id, admin.id, 60)
        .await;

    assert!(result.is_ok(), "Creator should be able to kick admin");

    // Admin should no longer be a member
    assert!(!member_repo.is_member(&room.id, &admin.id).await.unwrap());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_role_rejects_promoting_another_member_to_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("role_unique_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("role_unique_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Unique Creator Room".to_string(),
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

    let result = room_service
        .member_service()
        .set_member_role(room.id, creator.id, target.id, RoomRole::Creator)
        .await;

    assert!(
        result.is_err(),
        "set_member_role must not create a second Creator distinct from rooms.created_by"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_role_rejects_demoting_the_room_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("role_demote_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Demote Creator Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    let result = room_service
        .member_service()
        .set_member_role(room.id, creator.id, creator.id, RoomRole::Admin)
        .await;

    assert!(
        result.is_err(),
        "room creator must not be able to demote the membership row that represents ownership"
    );

    let creator_member = member_repo
        .get(&room.id, &creator.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        creator_member.role,
        RoomRole::Creator,
        "creator membership must remain Creator to stay consistent with rooms.created_by"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_grant_permission_bitwise_or() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("grant_creator")).await.unwrap();
    let target = user_repo.create(&make_user("grant_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Grant Room".to_string(),
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

    // Grant a member-level permission
    let updated = member_service
        .grant_permission(
            room.id,
            creator.id,
            target.id,
            RoomMemberPermissionBits::USE_WEBRTC,
        )
        .await
        .unwrap();

    assert!(
        updated.added_permissions & RoomMemberPermissionBits::USE_WEBRTC != 0,
        "USE_WEBRTC should now also be set"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_revoke_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("revoke_creator"))
        .await
        .unwrap();
    let target = user_repo.create(&make_user("revoke_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Revoke Room".to_string(),
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

    // Revoke CHAT permission (which is in default member permissions)
    let updated = member_service
        .revoke_permission(
            room.id,
            creator.id,
            target.id,
            RoomMemberPermissionBits::CHAT,
        )
        .await
        .unwrap();

    assert!(
        updated.removed_permissions & RoomMemberPermissionBits::CHAT != 0,
        "CHAT should be in removed_permissions"
    );

    // Verify the effective permission no longer includes CHAT
    let perm_service = room_service.permission_service();
    let effective = perm_service
        .get_user_permissions_no_cache(&room.id, &target.id)
        .await
        .unwrap();
    assert!(
        !effective.has(RoomPermission::CHAT),
        "CHAT should be denied after revocation"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_member_removes_active_membership() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("kick_bc_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("kick_bc_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Kick Broadcast Room".to_string(),
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

    // Kick the member
    room_service
        .kick_member(room.id, creator.id, member.id, 60)
        .await
        .unwrap();

    // Verify the member is no longer active
    let member_repo = RoomMemberRepository::new(pool.clone());
    let is_member = member_repo.is_member(&room.id, &member.id).await.unwrap();
    assert!(
        !is_member,
        "Kicked member should no longer be an active member"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_member_cooldown_blocks_rejoin_until_expired() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("kick_cd_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("kick_cd_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Kick Cooldown Room".to_string(),
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
        .kick_member(room.id, creator.id, member.id, 3600)
        .await
        .unwrap();

    assert!(
        member_repo
            .is_in_kick_cooldown(&room.id, &member.id)
            .await
            .unwrap(),
        "Kicked member should be in room kick cooldown"
    );

    let rejoin_during_cooldown = room_service.join_room(room.id, member.id, None).await;
    assert!(
        matches!(rejoin_during_cooldown, Err(Error::Authorization(ref msg)) if msg.contains("recently kicked")),
        "Rejoin during kick cooldown should be denied, got {rejoin_during_cooldown:?}"
    );

    expire_kick_cooldown(&pool, room.id, member.id).await;

    let rejoin_after_expiry = room_service.join_room(room.id, member.id, None).await;
    assert!(
        rejoin_after_expiry.is_ok(),
        "Rejoin after kick cooldown expiry should succeed, got {rejoin_after_expiry:?}"
    );
    assert!(
        member_repo.is_member(&room.id, &member.id).await.unwrap(),
        "Member should be active again after rejoin"
    );
    let rejoined_member = member_repo
        .get(&room.id, &member.id)
        .await
        .unwrap()
        .expect("rejoined member should be readable");
    assert!(
        rejoined_member.version >= 1,
        "rejoined member version must satisfy the permission fence advanced by the kick"
    );
}

/// Test that `delete_active_membership` handles the case atomically where a membership is deleted
/// concurrently. The operation should return `NotFound` if the member doesn't exist
/// or was already deleted, rather than proceeding with cache invalidation etc.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_active_membership_returns_not_found_for_non_member() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("delete_membership_nf_creator"))
        .await
        .unwrap();
    let non_member = user_repo
        .create(&make_user("delete_membership_nf_non_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Delete Membership NotFound Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    // non_member never joined, so delete_active_membership should return NotFound
    let member_service = room_service.member_service();
    let result = member_service
        .delete_active_membership(room.id, non_member.id)
        .await;

    assert!(
        result.is_err(),
        "delete_active_membership should fail for non-member"
    );
    match result.unwrap_err() {
        Error::NotFound(msg) => {
            assert!(
                msg.contains("Not a member") || msg.contains("not found"),
                "Error should indicate member not found: {msg}"
            );
        }
        other => panic!("Expected NotFound error, got: {other:?}"),
    }
}

/// Test that `delete_active_membership` is idempotent-safe: calling it twice should return
/// `NotFound` on the second call (membership was already deleted).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_active_membership_idempotent_not_found_after_deletion() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("delete_membership_idem_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("delete_membership_idem_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Delete Membership Idempotent Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Member joins
    room_service
        .join_room(room.id, member.id, None)
        .await
        .unwrap();

    // Verify member exists
    assert!(
        member_repo.is_member(&room.id, &member.id).await.unwrap(),
        "Member should exist before membership deletion"
    );

    // First deletion should succeed
    let member_service = room_service.member_service();
    let result = member_service
        .delete_active_membership(room.id, member.id)
        .await;
    assert!(
        result.is_ok(),
        "First delete_active_membership should succeed"
    );

    // Verify membership is deleted
    assert!(
        !member_repo.is_member(&room.id, &member.id).await.unwrap(),
        "Member should not exist after membership deletion"
    );

    // Second deletion should return NotFound.
    let result = member_service
        .delete_active_membership(room.id, member.id)
        .await;
    assert!(
        result.is_err(),
        "Second delete_active_membership should fail for already-removed member"
    );
    match result.unwrap_err() {
        Error::NotFound(msg) => {
            assert!(
                msg.contains("Not a member") || msg.contains("not found"),
                "Error should indicate member not found: {msg}"
            );
        }
        other => panic!("Expected NotFound error, got: {other:?}"),
    }
}

/// Test concurrent `delete_active_membership` calls: both should complete without errors,
/// and the membership should be deleted (only one should actually delete it).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_active_membership_concurrent_no_race() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("delete_membership_conc_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("delete_membership_conc_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Delete Membership Concurrent Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Member joins
    room_service
        .join_room(room.id, member.id, None)
        .await
        .unwrap();

    let member_service = room_service.member_service();
    let success_count = Arc::new(AtomicU32::new(0));
    let notfound_count = Arc::new(AtomicU32::new(0));

    // Spawn concurrent delete_active_membership calls
    let mut handles = vec![];
    for _ in 0..5 {
        let ms = member_service.clone();
        let room_id = room.id;
        let user_id = member.id;
        let sc = success_count.clone();
        let nc = notfound_count.clone();

        handles.push(tokio::spawn(async move {
            match ms.delete_active_membership(room_id, user_id).await {
                Ok(()) => sc.fetch_add(1, Ordering::SeqCst),
                Err(Error::NotFound(_)) => nc.fetch_add(1, Ordering::SeqCst),
                Err(e) => panic!("Unexpected error: {e:?}"),
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Exactly one should succeed, rest should get NotFound
    let successes = success_count.load(Ordering::SeqCst);
    let notfounds = notfound_count.load(Ordering::SeqCst);

    assert_eq!(
        successes, 1,
        "Exactly one membership deletion should succeed"
    );
    assert_eq!(notfounds, 4, "Four deletions should get NotFound");

    // Member should no longer exist
    assert!(
        !member_repo.is_member(&room.id, &member.id).await.unwrap(),
        "Member should be gone after concurrent membership deletion"
    );
}
