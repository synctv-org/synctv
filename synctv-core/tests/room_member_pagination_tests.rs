//! Room member pagination integration tests
//!
//! Tests that get_room_members uses database-level pagination instead of
//! loading all members into memory. This is critical for rooms with large
//! numbers of members.
//!

use chrono::Utc;
use synctv_core::{
    models::{
        PageParams, Room, RoomId, RoomMember, RoomRole, RoomStatus, User, UserId, UserRole,
        UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;
use synctv_core_testing::TestResultExt;

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

fn make_room(name: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: "test room".to_string(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: *owner,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
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

/// Test that list_by_room_paginated returns the correct page of members
/// and the total count without loading all members into memory.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_paginated_first_page() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_page1"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Pagination", &owner.id))
        .await
        .checked("test operation should succeed");

    // Add owner as creator
    member_repo
        .add(&make_member(room.id, owner.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    // Add 25 regular members
    for i in 0..25 {
        let user = user_repo
            .create(&make_user(&format!("member_page1_{i:02}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room.id, user.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
        // Small delay to ensure distinct joined_at for stable ordering
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }

    // Total members: 1 creator + 25 members = 26

    // Request first page with page_size=10
    let pagination = PageParams::new(Some(1), Some(10));
    let (members, total) = member_repo
        .list_by_room_paginated(&room.id, pagination)
        .await
        .checked("test operation should succeed");

    assert_eq!(
        total, 26,
        "Total count should be 26 (1 creator + 25 members)"
    );
    assert_eq!(members.len(), 10, "First page should have 10 members");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_paginated_second_page() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_page2"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Pagination 2", &owner.id))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, owner.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    // Add 25 regular members
    for i in 0..25 {
        let user = user_repo
            .create(&make_user(&format!("member_page2_{i:02}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room.id, user.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }

    // Request second page with page_size=10
    let pagination = PageParams::new(Some(2), Some(10));
    let (members, total) = member_repo
        .list_by_room_paginated(&room.id, pagination)
        .await
        .checked("test operation should succeed");

    assert_eq!(total, 26);
    assert_eq!(members.len(), 10, "Second page should have 10 members");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_paginated_last_page_partial() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_page3"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Pagination 3", &owner.id))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, owner.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    // Add 25 regular members
    for i in 0..25 {
        let user = user_repo
            .create(&make_user(&format!("member_page3_{i:02}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room.id, user.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }

    // Total = 26, with page_size=10: page 3 should have 6 remaining members
    let pagination = PageParams::new(Some(3), Some(10));
    let (members, total) = member_repo
        .list_by_room_paginated(&room.id, pagination)
        .await
        .checked("test operation should succeed");

    assert_eq!(total, 26);
    assert_eq!(
        members.len(),
        6,
        "Third page should have 6 remaining members"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_paginated_empty_page() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_empty"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Empty Page", &owner.id))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, owner.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    // Only 1 member, requesting page 2 should return an empty page while
    // preserving the total number of matching members.
    let pagination = PageParams::new(Some(2), Some(10));
    let (members, total) = member_repo
        .list_by_room_paginated(&room.id, pagination)
        .await
        .checked("test operation should succeed");

    assert_eq!(total, 1, "Total should reflect all matching members");
    assert!(members.is_empty(), "Page 2 should be empty");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_paginated_no_overlap_between_pages() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_overlap"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Overlap", &owner.id))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, owner.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    // Add 35 members (total 36)
    for i in 0..35 {
        let user = user_repo
            .create(&make_user(&format!("member_overlap_{i:02}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room.id, user.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }

    // Get all pages
    let page1 = PageParams::new(Some(1), Some(10));
    let page2 = PageParams::new(Some(2), Some(10));
    let page3 = PageParams::new(Some(3), Some(10));
    let page4 = PageParams::new(Some(4), Some(10));

    let (members1, _) = member_repo
        .list_by_room_paginated(&room.id, page1)
        .await
        .checked("test operation should succeed");
    let (members2, _) = member_repo
        .list_by_room_paginated(&room.id, page2)
        .await
        .checked("test operation should succeed");
    let (members3, _) = member_repo
        .list_by_room_paginated(&room.id, page3)
        .await
        .checked("test operation should succeed");
    let (members4, _) = member_repo
        .list_by_room_paginated(&room.id, page4)
        .await
        .checked("test operation should succeed");

    // Collect all user IDs
    let ids1: std::collections::HashSet<_> = members1.iter().map(|m| m.user_id).collect();
    let ids2: std::collections::HashSet<_> = members2.iter().map(|m| m.user_id).collect();
    let ids3: std::collections::HashSet<_> = members3.iter().map(|m| m.user_id).collect();
    let ids4: std::collections::HashSet<_> = members4.iter().map(|m| m.user_id).collect();

    // Verify no overlap between pages
    assert!(ids1.is_disjoint(&ids2), "Page 1 and 2 should not overlap");
    assert!(ids1.is_disjoint(&ids3), "Page 1 and 3 should not overlap");
    assert!(ids1.is_disjoint(&ids4), "Page 1 and 4 should not overlap");
    assert!(ids2.is_disjoint(&ids3), "Page 2 and 3 should not overlap");
    assert!(ids2.is_disjoint(&ids4), "Page 2 and 4 should not overlap");
    assert!(ids3.is_disjoint(&ids4), "Page 3 and 4 should not overlap");

    // Verify total unique IDs equals total members
    let total_unique: std::collections::HashSet<_> = ids1
        .union(&ids2)
        .copied()
        .chain(ids3.iter().copied())
        .chain(ids4.iter().copied())
        .collect();
    assert_eq!(
        total_unique.len(),
        36,
        "All pages combined should have 36 unique members"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_paginated_large_member_count() {
    // This test specifically verifies that pagination works efficiently
    // with a large number of members without loading all into memory
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_large"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Large", &owner.id))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, owner.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    // Add 150 members (a realistic large room scenario)
    for i in 0..150 {
        let user = user_repo
            .create(&make_user(&format!("member_large_{i:03}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room.id, user.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
    }

    let (p1, total1) = member_repo
        .list_by_room_paginated(&room.id, PageParams::new(Some(1), Some(50)))
        .await
        .checked("test operation should succeed");
    assert_eq!(total1, 151);
    assert_eq!(p1.len(), 50);

    let (p2, total2) = member_repo
        .list_by_room_paginated(&room.id, PageParams::new(Some(2), Some(50)))
        .await
        .checked("test operation should succeed");
    assert_eq!(total2, 151);
    assert_eq!(p2.len(), 50);

    let (p3, total3) = member_repo
        .list_by_room_paginated(&room.id, PageParams::new(Some(3), Some(50)))
        .await
        .checked("test operation should succeed");
    assert_eq!(total3, 151);
    assert_eq!(p3.len(), 50);

    let (p4, total4) = member_repo
        .list_by_room_paginated(&room.id, PageParams::new(Some(4), Some(50)))
        .await
        .checked("test operation should succeed");
    assert_eq!(total4, 151);
    assert_eq!(p4.len(), 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_paginated_excludes_left_members() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_removed"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Removed", &owner.id))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, owner.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    // Add 5 active members
    let mut active_users = Vec::new();
    for i in 0..5 {
        let user = user_repo
            .create(&make_user(&format!("active_{i}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room.id, user.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
        active_users.push(user);
    }

    // Add 3 members who will be removed
    let mut removed_users = Vec::new();
    for i in 0..3 {
        let user = user_repo
            .create(&make_user(&format!("removed_{i}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room.id, user.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
        removed_users.push(user);
    }

    // Remove those members
    for user in &removed_users {
        member_repo
            .remove(&room.id, &user.id)
            .await
            .checked("test operation should succeed");
    }

    // Paginated query should only return active members (1 creator + 5 active = 6)
    let (members, total) = member_repo
        .list_by_room_paginated(&room.id, PageParams::new(Some(1), Some(20)))
        .await
        .checked("test operation should succeed");

    assert_eq!(total, 6, "Total should only count active members");
    assert_eq!(members.len(), 6);

    // Verify removed members are not in the result
    let member_ids: std::collections::HashSet<_> = members.iter().map(|m| m.user_id).collect();
    for user in removed_users {
        assert!(
            !member_ids.contains(&user.id),
            "Removed member {} should not appear in results",
            user.id
        );
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_paginated_counts_active_only_after_removals() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_removed_count"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Removed Count", &owner.id))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, owner.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    // Add 5 active members
    for i in 0..5 {
        let user = user_repo
            .create(&make_user(&format!("active_banned_{i}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room.id, user.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
    }

    // Add 2 members who will be removed
    let mut removed_users = Vec::new();
    for i in 0..2 {
        let user = user_repo
            .create(&make_user(&format!("removed_count_{i}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room.id, user.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
        removed_users.push(user);
    }

    // Remove these members
    for user in &removed_users {
        member_repo
            .remove(&room.id, &user.id)
            .await
            .checked("test operation should succeed");
    }

    // Paginated query should only return active members (1 creator + 5 active = 6)
    let (_members, total) = member_repo
        .list_by_room_paginated(&room.id, PageParams::new(Some(1), Some(20)))
        .await
        .checked("test operation should succeed");

    assert_eq!(total, 6, "Total should only count active members");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_room_paginated_empty_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_empty_room"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Empty Room", &owner.id))
        .await
        .checked("test operation should succeed");

    let (members, total) = member_repo
        .list_by_room_paginated(&room.id, PageParams::new(Some(1), Some(20)))
        .await
        .checked("test operation should succeed");

    assert_eq!(total, 0);
    assert!(members.is_empty());
}
