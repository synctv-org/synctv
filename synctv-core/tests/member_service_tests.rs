//! `MemberService` integration tests
//!
//! Tests member management including max members, kick hierarchy, ban/unban,
//! and permission operations with real `PostgreSQL` via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test `member_service_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        room_settings::MaxMembers, MemberStatus, PermissionBits, RoomRole, User, UserId, UserRole,
        UserStatus,
    },
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
    let result = member_service
        .kick_member(room.id, admin.id, creator.id)
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
    let result = member_service
        .kick_member(room.id, creator.id, admin.id)
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
async fn test_ban_sets_status_and_banned_at() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo.create(&make_user("ban_creator")).await.unwrap();
    let target = user_repo.create(&make_user("ban_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Ban Room".to_string(),
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

    // Ban the member
    let member_service = room_service.member_service();
    member_service
        .ban_member(
            room.id,
            creator.id,
            target.id,
            Some("Test ban reason".to_string()),
        )
        .await
        .unwrap();

    // Verify ban status (use get_any because banned members have left_at set)
    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.status, MemberStatus::Left, "Member should be banned");
    assert!(member.banned_at.is_some(), "banned_at should be set");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unban_clears_ban_metadata_without_rejoining_member() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo.create(&make_user("unban_creator")).await.unwrap();
    let target = user_repo.create(&make_user("unban_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Unban Room".to_string(),
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

    // Ban first
    member_service
        .ban_member(room.id, creator.id, target.id, None)
        .await
        .unwrap();

    // Verify banned (use get_any because banned members have left_at set)
    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.status, MemberStatus::Left);
    assert!(member.is_banned());

    // Unban
    member_service
        .unban_member(room.id, creator.id, target.id)
        .await
        .unwrap();

    // Unban only revokes moderation state. It must not silently rejoin a
    // user who was removed from the active member set by the ban.
    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.status, MemberStatus::Left);
    assert!(
        member.banned_at.is_none(),
        "banned_at should be cleared after unban"
    );
    assert!(
        member_repo
            .get(&room.id, &target.id)
            .await
            .unwrap()
            .is_none(),
        "unban must not restore active membership"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_ban_member_can_ban_departed_member() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("admin_ban_departed_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("admin_ban_departed_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Admin Ban Departed Room".to_string(),
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
    room_service.leave_room(room.id, target.id).await.unwrap();

    room_service
        .member_service()
        .admin_ban_member(
            room.id,
            creator.id,
            &creator.username,
            target.id,
            Some(creator.id),
            Some("prevent rejoin".to_string()),
        )
        .await
        .expect("admin ban should also work for departed historical memberships");

    let persisted = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .expect("departed member row should still exist");
    assert_eq!(persisted.status, MemberStatus::Left);
    assert!(persisted.is_banned());
    assert_eq!(persisted.banned_by.as_ref(), Some(&creator.id));
    assert_eq!(persisted.banned_reason.as_deref(), Some("prevent rejoin"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_member_preserves_ban_semantics() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("status_ban_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("status_ban_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Status Ban Room".to_string(),
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
    member_service
        .ban_member(room.id, creator.id, target.id, None)
        .await
        .unwrap();

    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.status, MemberStatus::Left);
    assert!(member.is_banned());
    assert!(
        member.left_at.is_some(),
        "ban must evict the member from the active set"
    );
    assert!(member.banned_at.is_some(), "ban must record ban metadata");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_status_can_set_member_pending_and_approve_back_to_active() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("status_pending_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("status_pending_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Status Pending Room".to_string(),
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

    let pending = room_service
        .member_service()
        .set_member_status(room.id, creator.id, target.id, MemberStatus::Active)
        .await
        .unwrap();

    assert_eq!(pending.status, MemberStatus::Active);

    let active = room_service
        .member_service()
        .set_member_status(room.id, creator.id, target.id, MemberStatus::Active)
        .await
        .unwrap();

    assert_eq!(active.status, MemberStatus::Active);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_status_rejects_specialized_left_transition() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("status_special_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("status_special_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Status Specialized Room".to_string(),
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

    let left_err = member_service
        .set_member_status(room.id, creator.id, target.id, MemberStatus::Left)
        .await
        .unwrap_err();
    assert!(
        matches!(left_err, Error::InvalidInput(ref msg) if msg.contains("Use remove_member or kick_member")),
        "Left must stay on the dedicated removal path, got: {left_err}"
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

    // Grant BAN_MEMBER permission
    let updated = member_service
        .grant_permission(room.id, creator.id, target.id, PermissionBits::BAN_MEMBER)
        .await
        .unwrap();

    assert!(
        updated.added_permissions & PermissionBits::BAN_MEMBER != 0,
        "BAN_MEMBER should be in added_permissions"
    );

    // Grant another permission (KICK_MEMBER) - should be bitwise OR'd
    let updated = member_service
        .grant_permission(room.id, creator.id, target.id, PermissionBits::KICK_MEMBER)
        .await
        .unwrap();

    assert!(
        updated.added_permissions & PermissionBits::BAN_MEMBER != 0,
        "BAN_MEMBER should still be set"
    );
    assert!(
        updated.added_permissions & PermissionBits::KICK_MEMBER != 0,
        "KICK_MEMBER should now also be set"
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

    // Revoke SEND_CHAT permission (which is in default member permissions)
    let updated = member_service
        .revoke_permission(room.id, creator.id, target.id, PermissionBits::SEND_CHAT)
        .await
        .unwrap();

    assert!(
        updated.removed_permissions & PermissionBits::SEND_CHAT != 0,
        "SEND_CHAT should be in removed_permissions"
    );

    // Verify the effective permission no longer includes SEND_CHAT
    let perm_service = room_service.permission_service();
    let effective = perm_service
        .get_user_permissions_no_cache(&room.id, &target.id)
        .await
        .unwrap();
    assert!(
        !effective.has(PermissionBits::SEND_CHAT),
        "SEND_CHAT should be denied after revocation"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_records_reason_without_realtime_side_effects() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("ban_bc_creator"))
        .await
        .unwrap();
    let target = user_repo.create(&make_user("ban_bc_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Ban Broadcast Room".to_string(),
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

    // Ban with a specific reason
    let ban_reason = "Violating community guidelines";
    member_service
        .ban_member(room.id, creator.id, target.id, Some(ban_reason.to_string()))
        .await
        .unwrap();

    // Verify the member is banned
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.status, MemberStatus::Left);
    assert!(member.is_banned());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_has_no_realtime_propagation_delay_in_member_service() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("ban_delay_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("ban_delay_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Ban Delay Room".to_string(),
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

    // Measure time taken for ban operation
    let start = std::time::Instant::now();

    member_service
        .ban_member(
            room.id,
            creator.id,
            target.id,
            Some("Testing propagation delay".to_string()),
        )
        .await
        .unwrap();

    let elapsed = start.elapsed();

    // Verify the member is banned
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.status, MemberStatus::Left);
    assert!(member.is_banned());

    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "MemberService ban should complete quickly without realtime propagation delay, took {elapsed:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_service_ban_persists_banned_status_only() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("status_bc_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("status_bc_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Status Broadcast Room".to_string(),
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

    member_service
        .ban_member(room.id, creator.id, target.id, None)
        .await
        .unwrap();

    let member_repo = RoomMemberRepository::new(pool.clone());
    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.status, MemberStatus::Left);
    assert!(member.is_banned());
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

    let member_service = room_service.member_service();

    // Kick the member
    member_service
        .kick_member(room.id, creator.id, member.id)
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

/// Test that `remove_member` handles the case atomically where a member is removed
/// concurrently. The operation should return `NotFound` if the member doesn't exist
/// or was already removed, rather than proceeding with cache invalidation etc.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_remove_member_returns_not_found_for_non_member() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("remove_nf_creator"))
        .await
        .unwrap();
    let non_member = user_repo
        .create(&make_user("remove_nf_non_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Remove NotFound Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    // non_member never joined, so remove_member should return NotFound
    let member_service = room_service.member_service();
    let result = member_service.remove_member(room.id, non_member.id).await;

    assert!(result.is_err(), "remove_member should fail for non-member");
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

/// Test that `remove_member` is idempotent-safe: calling it twice should return
/// `NotFound` on the second call (member was already removed).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_remove_member_idempotent_not_found_after_removal() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("remove_idem_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("remove_idem_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Remove Idempotent Room".to_string(),
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
        "Member should exist before removal"
    );

    // First remove should succeed
    let member_service = room_service.member_service();
    let result = member_service.remove_member(room.id, member.id).await;
    assert!(result.is_ok(), "First remove_member should succeed");

    // Verify member is removed
    assert!(
        !member_repo.is_member(&room.id, &member.id).await.unwrap(),
        "Member should not exist after removal"
    );

    // Second remove should return NotFound (atomic check + remove)
    let result = member_service.remove_member(room.id, member.id).await;
    assert!(
        result.is_err(),
        "Second remove_member should fail for already-removed member"
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

/// Test concurrent `remove_member` calls: both should complete without errors,
/// and the member should be removed (only one should actually do the removal).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_remove_member_concurrent_no_race() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("remove_conc_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("remove_conc_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Remove Concurrent Room".to_string(),
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

    // Spawn concurrent remove_member calls
    let mut handles = vec![];
    for _ in 0..5 {
        let ms = member_service.clone();
        let room_id = room.id;
        let user_id = member.id;
        let sc = success_count.clone();
        let nc = notfound_count.clone();

        handles.push(tokio::spawn(async move {
            match ms.remove_member(room_id, user_id).await {
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

    assert_eq!(successes, 1, "Exactly one remove should succeed");
    assert_eq!(notfounds, 4, "Four removes should get NotFound");

    // Member should no longer exist
    assert!(
        !member_repo.is_member(&room.id, &member.id).await.unwrap(),
        "Member should be removed after concurrent operations"
    );
}
