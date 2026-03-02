//! `RoomMemberRepository` integration tests
//!
//! Tests the core room member operations: `add_with_options`, role-check ban/remove,
//! atomic permission grants/revokes, permission reset, batch counts, pagination,
//! and `diagnose_add_conflict` error branches.
//!
//! Run with: cargo test --test `room_member_repository_tests`
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use synctv_core::{
    models::{
        MemberStatus, PageParams, Room, RoomId, RoomMember, RoomRole, RoomStatus, User, UserId,
        UserRole, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, UserRepository},
    service::AddMemberOptions,
    Error,
};
use synctv_core_testing::create_test_pool;
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
        signup_method: None,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

fn make_room(name: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: "test room".to_string(),
        created_by: owner.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

fn make_member(room_id: RoomId, user_id: UserId, role: RoomRole) -> RoomMember {
    RoomMember::new(room_id, user_id, role)
}

// ========== add_with_options tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_full_flow() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_awo")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room AWO", &owner.id))
        .await
        .unwrap();
    let joiner = user_repo.create(&make_user("joiner_awo")).await.unwrap();

    let member = make_member(room.id.clone(), joiner.id.clone(), RoomRole::Member);
    let options = AddMemberOptions::new();

    let result = member_repo
        .add_with_options(&member, &options)
        .await
        .unwrap();
    assert_eq!(result.user_id, joiner.id);
    assert_eq!(result.role, RoomRole::Member);
    assert_eq!(result.status, MemberStatus::Active);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_capacity_at_max_members() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_cap")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room Cap", &owner.id))
        .await
        .unwrap();

    // Add one member to fill the room (max_members=1)
    let user1 = user_repo.create(&make_user("user1_cap")).await.unwrap();
    let m1 = make_member(room.id.clone(), user1.id.clone(), RoomRole::Member);
    let options_fill = AddMemberOptions::new().with_max_members(1);
    member_repo
        .add_with_options(&m1, &options_fill)
        .await
        .unwrap();

    // Second member should be rejected
    let user2 = user_repo.create(&make_user("user2_cap")).await.unwrap();
    let m2 = make_member(room.id.clone(), user2.id.clone(), RoomRole::Member);
    let options_reject = AddMemberOptions::new().with_max_members(1);
    let err = member_repo
        .add_with_options(&m2, &options_reject)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_inactive_room_rejection() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_inactive"))
        .await
        .unwrap();
    let room = room_repo
        .create(&make_room("Room Inactive", &owner.id))
        .await
        .unwrap();

    // Close the room
    room_repo
        .update_status(&room.id, RoomStatus::Closed)
        .await
        .unwrap();

    let joiner = user_repo
        .create(&make_user("joiner_inactive"))
        .await
        .unwrap();
    let member = make_member(room.id.clone(), joiner.id.clone(), RoomRole::Member);
    let options = AddMemberOptions::new();

    let err = member_repo
        .add_with_options(&member, &options)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_duplicate_membership_check() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_dup")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room Dup", &owner.id))
        .await
        .unwrap();
    let joiner = user_repo.create(&make_user("joiner_dup")).await.unwrap();

    let member = make_member(room.id.clone(), joiner.id.clone(), RoomRole::Member);
    let options = AddMemberOptions::new();

    // First join succeeds
    member_repo
        .add_with_options(&member, &options)
        .await
        .unwrap();

    // Second join with duplicate check fails
    let member2 = make_member(room.id.clone(), joiner.id.clone(), RoomRole::Member);
    let err = member_repo
        .add_with_options(&member2, &options)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::AlreadyExists(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_max_members_zero_bypass() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_zero")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room Zero", &owner.id))
        .await
        .unwrap();

    // max_members=0 means unlimited - even with check enabled, it bypasses
    let mut options = AddMemberOptions::new();
    options.check_max_members = true;
    options.max_members = 0;

    // Add many members - all should succeed
    for i in 0..5 {
        let user = user_repo
            .create(&make_user(&format!("user_zero_{i}")))
            .await
            .unwrap();
        let member = make_member(room.id.clone(), user.id.clone(), RoomRole::Member);
        member_repo
            .add_with_options(&member, &options)
            .await
            .unwrap();
    }

    let count = member_repo.count_by_room(&room.id).await.unwrap();
    assert_eq!(count, 5);
}

// ========== ban_with_role_check / remove_with_role_check tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_with_role_check_member_cannot_ban_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_ban_role"))
        .await
        .unwrap();
    let room = room_repo
        .create(&make_room("Room BanRole", &owner.id))
        .await
        .unwrap();

    let admin_user = user_repo
        .create(&make_user("admin_ban_role"))
        .await
        .unwrap();
    let member_user = user_repo
        .create(&make_user("member_ban_role"))
        .await
        .unwrap();

    member_repo
        .add(&make_member(
            room.id.clone(),
            admin_user.id.clone(),
            RoomRole::Admin,
        ))
        .await
        .unwrap();
    member_repo
        .add(&make_member(
            room.id.clone(),
            member_user.id.clone(),
            RoomRole::Member,
        ))
        .await
        .unwrap();

    // Member (role=3) trying to ban Admin (role=2) => should fail
    let err = member_repo
        .ban_with_role_check(&room.id, &member_user.id, &admin_user.id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_with_role_check_creator_can_ban_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo.create(&make_user("creator_ban")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room CreatorBan", &creator.id))
        .await
        .unwrap();

    // Add creator as Creator role in room_members
    member_repo
        .add(&make_member(
            room.id.clone(),
            creator.id.clone(),
            RoomRole::Creator,
        ))
        .await
        .unwrap();

    let admin_user = user_repo.create(&make_user("admin_ban")).await.unwrap();
    member_repo
        .add(&make_member(
            room.id.clone(),
            admin_user.id.clone(),
            RoomRole::Admin,
        ))
        .await
        .unwrap();

    // Creator (role=1) banning Admin (role=2) => should succeed
    let banned = member_repo
        .ban_with_role_check(
            &room.id,
            &creator.id,
            &admin_user.id,
            Some("test reason".to_string()),
        )
        .await
        .unwrap();

    assert_eq!(banned.status, MemberStatus::Banned);
    assert!(banned.banned_at.is_some());
    assert_eq!(banned.banned_reason, Some("test reason".to_string()));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_remove_with_role_check_equal_rank_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_eqrank")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room EqRank", &owner.id))
        .await
        .unwrap();

    let admin1 = user_repo.create(&make_user("admin1_eqrank")).await.unwrap();
    let admin2 = user_repo.create(&make_user("admin2_eqrank")).await.unwrap();

    member_repo
        .add(&make_member(
            room.id.clone(),
            admin1.id.clone(),
            RoomRole::Admin,
        ))
        .await
        .unwrap();
    member_repo
        .add(&make_member(
            room.id.clone(),
            admin2.id.clone(),
            RoomRole::Admin,
        ))
        .await
        .unwrap();

    // Admin (role=2) trying to remove another Admin (role=2) => equal rank, should fail
    let result = member_repo
        .remove_with_role_check(&room.id, &admin1.id, &admin2.id)
        .await
        .unwrap();
    assert!(!result);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_remove_with_role_check_self_kick_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_selfkick"))
        .await
        .unwrap();
    let room = room_repo
        .create(&make_room("Room SelfKick", &owner.id))
        .await
        .unwrap();

    let admin_user = user_repo
        .create(&make_user("admin_selfkick"))
        .await
        .unwrap();
    member_repo
        .add(&make_member(
            room.id.clone(),
            admin_user.id.clone(),
            RoomRole::Admin,
        ))
        .await
        .unwrap();

    // Self-kick: actor.role == target.role (not strictly less), should fail
    let result = member_repo
        .remove_with_role_check(&room.id, &admin_user.id, &admin_user.id)
        .await
        .unwrap();
    assert!(!result);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_remove_with_role_check_creator_kicks_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo.create(&make_user("creator_kick")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room CreatorKick", &creator.id))
        .await
        .unwrap();

    member_repo
        .add(&make_member(
            room.id.clone(),
            creator.id.clone(),
            RoomRole::Creator,
        ))
        .await
        .unwrap();

    let admin_user = user_repo.create(&make_user("admin_kick")).await.unwrap();
    member_repo
        .add(&make_member(
            room.id.clone(),
            admin_user.id.clone(),
            RoomRole::Admin,
        ))
        .await
        .unwrap();

    // Creator (role=1) kicking Admin (role=2) => should succeed
    let result = member_repo
        .remove_with_role_check(&room.id, &creator.id, &admin_user.id)
        .await
        .unwrap();
    assert!(result);

    // Verify admin is now gone (left_at set)
    let member = member_repo.get(&room.id, &admin_user.id).await.unwrap();
    assert!(member.is_none()); // get() filters left_at IS NULL
}

// ========== grant_permission_atomic / revoke_permission_atomic tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_grant_permission_atomic_bitwise_or() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_grant")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room Grant", &owner.id))
        .await
        .unwrap();
    let user = user_repo.create(&make_user("user_grant")).await.unwrap();

    member_repo
        .add(&make_member(
            room.id.clone(),
            user.id.clone(),
            RoomRole::Member,
        ))
        .await
        .unwrap();

    // Grant permission 0x01
    let m1 = member_repo
        .grant_permission_atomic(&room.id, &user.id, 0x01)
        .await
        .unwrap();
    assert_eq!(m1.added_permissions, 0x01);

    // Grant permission 0x02 (bitwise OR with existing)
    let m2 = member_repo
        .grant_permission_atomic(&room.id, &user.id, 0x02)
        .await
        .unwrap();
    assert_eq!(m2.added_permissions, 0x03); // 0x01 | 0x02 = 0x03
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_revoke_permission_atomic_bitwise_or() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_revoke")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room Revoke", &owner.id))
        .await
        .unwrap();
    let user = user_repo.create(&make_user("user_revoke")).await.unwrap();

    member_repo
        .add(&make_member(
            room.id.clone(),
            user.id.clone(),
            RoomRole::Member,
        ))
        .await
        .unwrap();

    // Revoke permission 0x04
    let m1 = member_repo
        .revoke_permission_atomic(&room.id, &user.id, 0x04)
        .await
        .unwrap();
    assert_eq!(m1.removed_permissions, 0x04);

    // Revoke permission 0x08 (bitwise OR with existing)
    let m2 = member_repo
        .revoke_permission_atomic(&room.id, &user.id, 0x08)
        .await
        .unwrap();
    assert_eq!(m2.removed_permissions, 0x0C); // 0x04 | 0x08 = 0x0C
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_grant_permission_atomic_left_member_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_left_grant"))
        .await
        .unwrap();
    let room = room_repo
        .create(&make_room("Room LeftGrant", &owner.id))
        .await
        .unwrap();
    let user = user_repo
        .create(&make_user("user_left_grant"))
        .await
        .unwrap();

    member_repo
        .add(&make_member(
            room.id.clone(),
            user.id.clone(),
            RoomRole::Member,
        ))
        .await
        .unwrap();

    // Remove the member
    member_repo.remove(&room.id, &user.id).await.unwrap();

    // Attempting to grant on left member should return NotFound
    let err = member_repo
        .grant_permission_atomic(&room.id, &user.id, 0x01)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_revoke_permission_atomic_left_member_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_left_revoke"))
        .await
        .unwrap();
    let room = room_repo
        .create(&make_room("Room LeftRevoke", &owner.id))
        .await
        .unwrap();
    let user = user_repo
        .create(&make_user("user_left_revoke"))
        .await
        .unwrap();

    member_repo
        .add(&make_member(
            room.id.clone(),
            user.id.clone(),
            RoomRole::Member,
        ))
        .await
        .unwrap();

    // Remove the member
    member_repo.remove(&room.id, &user.id).await.unwrap();

    // Attempting to revoke on left member should return NotFound
    let err = member_repo
        .revoke_permission_atomic(&room.id, &user.id, 0x01)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)));
}

// ========== reset_permissions tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_permissions_zeroes_all_four_columns() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_reset")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room Reset", &owner.id))
        .await
        .unwrap();
    let user = user_repo.create(&make_user("user_reset")).await.unwrap();

    member_repo
        .add(&make_member(
            room.id.clone(),
            user.id.clone(),
            RoomRole::Member,
        ))
        .await
        .unwrap();

    // Set various permissions
    member_repo
        .grant_permission_atomic(&room.id, &user.id, 0xFF)
        .await
        .unwrap();
    member_repo
        .revoke_permission_atomic(&room.id, &user.id, 0xAA)
        .await
        .unwrap();

    // Also set admin permissions via raw SQL (since atomic ops only touch member-level)
    sqlx::query("UPDATE room_members SET admin_added_permissions = 42, admin_removed_permissions = 84 WHERE room_id = $1 AND user_id = $2")
        .bind(room.id.as_str())
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    // Get current version
    let current = member_repo.get(&room.id, &user.id).await.unwrap().unwrap();

    // Reset
    let reset = member_repo
        .reset_permissions(&room.id, &user.id, current.version)
        .await
        .unwrap();
    assert_eq!(reset.added_permissions, 0);
    assert_eq!(reset.removed_permissions, 0);
    assert_eq!(reset.admin_added_permissions, 0);
    assert_eq!(reset.admin_removed_permissions, 0);
    assert_eq!(reset.version, current.version + 1);
}

// ========== count_by_rooms_batch tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_by_rooms_batch_basic() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_batch")).await.unwrap();
    let room1 = room_repo
        .create(&make_room("Room Batch1", &owner.id))
        .await
        .unwrap();
    let room2 = room_repo
        .create(&make_room("Room Batch2", &owner.id))
        .await
        .unwrap();
    let room3 = room_repo
        .create(&make_room("Room Batch3", &owner.id))
        .await
        .unwrap();

    // Add 2 members to room1
    for i in 0..2 {
        let u = user_repo
            .create(&make_user(&format!("batch_u1_{i}")))
            .await
            .unwrap();
        member_repo
            .add(&make_member(
                room1.id.clone(),
                u.id.clone(),
                RoomRole::Member,
            ))
            .await
            .unwrap();
    }

    // Add 3 members to room2
    for i in 0..3 {
        let u = user_repo
            .create(&make_user(&format!("batch_u2_{i}")))
            .await
            .unwrap();
        member_repo
            .add(&make_member(
                room2.id.clone(),
                u.id.clone(),
                RoomRole::Member,
            ))
            .await
            .unwrap();
    }

    // room3 has 0 members

    let counts = member_repo
        .count_by_rooms_batch(&[&room1.id, &room2.id, &room3.id])
        .await
        .unwrap();

    assert_eq!(counts.get(room1.id.as_str()), Some(&2));
    assert_eq!(counts.get(room2.id.as_str()), Some(&3));
    // Zero-member room absent from map
    assert!(!counts.contains_key(room3.id.as_str()));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_by_rooms_batch_empty_input() {
    let (_container, pool) = create_test_pool().await;
    let member_repo = RoomMemberRepository::new(pool.clone());

    let counts = member_repo.count_by_rooms_batch(&[]).await.unwrap();
    assert!(counts.is_empty());
}

// ========== list_by_user_with_details tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_user_with_details_pagination() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_pagination"))
        .await
        .unwrap();
    let user = user_repo
        .create(&make_user("user_pagination"))
        .await
        .unwrap();

    // Create 5 rooms and add user to each
    for i in 0..5 {
        let room = room_repo
            .create(&make_room(&format!("Room Pag {i}"), &owner.id))
            .await
            .unwrap();
        let member = make_member(room.id.clone(), user.id.clone(), RoomRole::Member);
        member_repo.add(&member).await.unwrap();
        // Small delay to ensure distinct joined_at timestamps for stable ordering
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Page 1 with page_size=2
    let page1 = PageParams::new(Some(1), Some(2));
    let (rooms_p1, total_p1) = member_repo
        .list_by_user_with_details(&user.id, page1)
        .await
        .unwrap();
    assert_eq!(total_p1, 5);
    assert_eq!(rooms_p1.len(), 2);

    // Page 2 with page_size=2
    let page2 = PageParams::new(Some(2), Some(2));
    let (rooms_p2, total_p2) = member_repo
        .list_by_user_with_details(&user.id, page2)
        .await
        .unwrap();
    assert_eq!(total_p2, 5);
    assert_eq!(rooms_p2.len(), 2);

    // Page 3 with page_size=2 (only 1 remaining)
    let page3 = PageParams::new(Some(3), Some(2));
    let (rooms_p3, total_p3) = member_repo
        .list_by_user_with_details(&user.id, page3)
        .await
        .unwrap();
    assert_eq!(total_p3, 5);
    assert_eq!(rooms_p3.len(), 1);

    // Verify no overlapping room IDs between pages
    let ids_p1: Vec<_> = rooms_p1
        .iter()
        .map(|(r, _, _, _)| r.id.as_str().to_string())
        .collect();
    let ids_p2: Vec<_> = rooms_p2
        .iter()
        .map(|(r, _, _, _)| r.id.as_str().to_string())
        .collect();
    let ids_p3: Vec<_> = rooms_p3
        .iter()
        .map(|(r, _, _, _)| r.id.as_str().to_string())
        .collect();

    for id in &ids_p1 {
        assert!(!ids_p2.contains(id));
        assert!(!ids_p3.contains(id));
    }
    for id in &ids_p2 {
        assert!(!ids_p3.contains(id));
    }
}

// ========== diagnose_add_conflict tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_diagnose_add_conflict_banned_user() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_banned")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room Banned", &owner.id))
        .await
        .unwrap();
    let user = user_repo.create(&make_user("user_banned")).await.unwrap();

    // Add the user first
    member_repo
        .add(&make_member(
            room.id.clone(),
            user.id.clone(),
            RoomRole::Member,
        ))
        .await
        .unwrap();

    // Ban the user
    member_repo
        .ban_member(
            &room.id,
            &user.id,
            &owner.id,
            Some("bad behavior".to_string()),
        )
        .await
        .unwrap();

    // Try to re-add -> should get Authorization error (banned)
    let member = make_member(room.id.clone(), user.id.clone(), RoomRole::Member);
    let err = member_repo.add(&member).await.unwrap_err();
    assert!(matches!(err, Error::Authorization(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_diagnose_add_conflict_left_user() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_left")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room Left", &owner.id))
        .await
        .unwrap();
    let user = user_repo.create(&make_user("user_left")).await.unwrap();

    // Add the user
    member_repo
        .add(&make_member(
            room.id.clone(),
            user.id.clone(),
            RoomRole::Member,
        ))
        .await
        .unwrap();

    // User leaves
    member_repo.remove(&room.id, &user.id).await.unwrap();

    // Try to re-add -- the ON CONFLICT DO UPDATE with WHERE status != Banned
    // will succeed for "Left" status (since Left != Banned), so re-join works.
    let member = make_member(room.id.clone(), user.id.clone(), RoomRole::Member);
    let result = member_repo.add(&member).await;

    // The ON CONFLICT clause should allow re-joining a "Left" member
    // (the WHERE condition is `status != Banned`, so Left passes)
    assert!(result.is_ok());
    let rejoined = result.unwrap();
    assert_eq!(rejoined.status, MemberStatus::Active);
    assert!(rejoined.left_at.is_none());
}

// ========== banned_by ON DELETE SET NULL constraint tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_by_set_null_on_user_delete() {
    // Test that deleting a user who banned someone sets banned_by to NULL
    // instead of blocking the delete (ON DELETE SET NULL constraint)
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create room owner (cannot delete this user due to rooms.created_by ON DELETE RESTRICT)
    let owner = user_repo
        .create(&make_user("owner_banned_by"))
        .await
        .unwrap();
    let room = room_repo
        .create(&make_room("Room BannedBy", &owner.id))
        .await
        .unwrap();

    // Add owner as creator
    member_repo
        .add(&make_member(
            room.id.clone(),
            owner.id.clone(),
            RoomRole::Creator,
        ))
        .await
        .unwrap();

    // Create a separate admin who will ban someone (this user can be deleted)
    let admin = user_repo
        .create(&make_user("admin_banned_by"))
        .await
        .unwrap();
    member_repo
        .add(&make_member(
            room.id.clone(),
            admin.id.clone(),
            RoomRole::Admin,
        ))
        .await
        .unwrap();

    // Create and add a member who will be banned
    let banned_user = user_repo
        .create(&make_user("banned_by_user"))
        .await
        .unwrap();
    member_repo
        .add(&make_member(
            room.id.clone(),
            banned_user.id.clone(),
            RoomRole::Member,
        ))
        .await
        .unwrap();

    // Admin bans the member
    member_repo
        .ban_member(
            &room.id,
            &banned_user.id,
            &admin.id,
            Some("test banned_by constraint".to_string()),
        )
        .await
        .unwrap();

    // Verify banned_by is set
    let banned_member: Option<(Option<String>,)> =
        sqlx::query_as("SELECT banned_by FROM room_members WHERE room_id = $1 AND user_id = $2")
            .bind(room.id.as_str())
            .bind(banned_user.id.as_str())
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(banned_member.is_some());
    assert_eq!(banned_member.unwrap().0, Some(admin.id.to_string()));

    // Now delete the admin user - this should succeed because of ON DELETE SET NULL
    // The admin is not the room creator, so rooms.created_by won't block
    let delete_result = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(admin.id.as_str())
        .execute(&pool)
        .await;

    assert!(
        delete_result.is_ok(),
        "Deleting user who banned someone should succeed with ON DELETE SET NULL"
    );

    // Verify banned_by is now NULL
    let banned_member_after: Option<(Option<String>,)> =
        sqlx::query_as("SELECT banned_by FROM room_members WHERE room_id = $1 AND user_id = $2")
            .bind(room.id.as_str())
            .bind(banned_user.id.as_str())
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(banned_member_after.is_some());
    assert_eq!(
        banned_member_after.unwrap().0,
        None,
        "banned_by should be NULL after admin user is deleted"
    );
}

// ========== Task #30: update_permissions left_at validation ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_permissions_after_member_left_should_fail() {
    // CRITICAL: update_permissions should not allow updating permissions for
    // members who have left the room (left_at IS NOT NULL).
    //
    // BUG: Currently update_permissions doesn't check left_at, allowing
    // "ghost" permission updates on departed members.

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create owner and member
    let owner = user_repo
        .create(&make_user("owner_permissions_test"))
        .await
        .unwrap();
    let member_user = user_repo
        .create(&make_user("member_left_test"))
        .await
        .unwrap();

    // Create room
    let room = room_repo
        .create(&make_room("Permissions Test Room", &owner.id))
        .await
        .unwrap();

    // Add member with no permissions
    let new_member = make_member(room.id.clone(), member_user.id.clone(), RoomRole::Member);
    let _member = member_repo
        .add_with_options(&new_member, &AddMemberOptions::new())
        .await
        .unwrap();

    // Member leaves the room - set left_at directly via SQL
    sqlx::query(
        "UPDATE room_members SET left_at = CURRENT_TIMESTAMP, status = $3 \
         WHERE room_id = $1 AND user_id = $2",
    )
    .bind(room.id.as_str())
    .bind(member_user.id.as_str())
    .bind(MemberStatus::Left)
    .execute(&pool)
    .await
    .unwrap();

    // Get the updated member (use get_any because get only returns active members)
    let left_member = member_repo
        .get_any(&room.id, &member_user.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        left_member.left_at.is_some(),
        "Member should have left_at set"
    );

    // BUG ATTEMPT: Try to update permissions for departed member
    // This should fail with OptimisticLockConflict but currently succeeds (BUG)
    let result = member_repo
        .update_permissions(
            &room.id,
            &member_user.id,
            0b0000_0001, // Add permission bit 0
            0,           // Remove nothing
            left_member.version,
        )
        .await;

    assert!(
        result.is_err(),
        "update_permissions should fail for departed member (left_at IS NOT NULL)"
    );

    match result {
        Err(Error::OptimisticLockConflict) => { /* Expected */ }
        Err(e) => panic!("Expected OptimisticLockConflict, got: {e:?}"),
        Ok(_) => panic!("update_permissions should not succeed for departed member"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_permissions_for_active_member_should_succeed() {
    // Verify that update_permissions still works for active members (the valid case)

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_active_perm"))
        .await
        .unwrap();
    let member_user = user_repo.create(&make_user("member_active")).await.unwrap();

    let room = room_repo
        .create(&make_room("Active Permissions Test", &owner.id))
        .await
        .unwrap();

    // Add active member
    let new_member = make_member(room.id.clone(), member_user.id.clone(), RoomRole::Member);
    let member = member_repo
        .add_with_options(&new_member, &AddMemberOptions::new())
        .await
        .unwrap();

    assert!(
        member.left_at.is_none(),
        "Active member should have left_at = NULL"
    );

    // Update permissions for active member - this should succeed
    let updated = member_repo
        .update_permissions(
            &room.id,
            &member_user.id,
            0b0000_0001, // Add permission bit 0
            0,           // Remove nothing
            member.version,
        )
        .await;

    assert!(
        updated.is_ok(),
        "update_permissions should succeed for active member"
    );

    let updated_member = updated.unwrap();
    assert_eq!(updated_member.added_permissions, 0b0000_0001);
    assert_eq!(updated_member.version, member.version + 1);
}
