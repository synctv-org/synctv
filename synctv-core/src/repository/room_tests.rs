use super::*;
use crate::test_helpers::{TestOptionExt, TestResultExt};
use synctv_core_testing::create_test_pool;

#[test]
fn test_room_list_order_clause_supports_name_ascending() {
    let query = RoomListQuery {
        status: None,
        is_banned: None,
        is_public: None,
        search: None,
        sort_by: crate::models::RoomListSortBy::Name,
        sort_direction: crate::models::SortDirection::Asc,
        pagination: PageParams::default(),
        creator_id: None,
        category_id: None,
        label_ids: Vec::new(),
    };

    assert_eq!(RoomRepository::order_by_sql(&query), "r.name ASC, r.id ASC");
}

#[test]
fn test_room_list_order_clause_supports_last_activity_nulls_last() {
    let query = RoomListQuery {
        status: None,
        is_banned: None,
        is_public: None,
        search: None,
        sort_by: crate::models::RoomListSortBy::LastActivityAt,
        sort_direction: crate::models::SortDirection::Desc,
        pagination: PageParams::default(),
        creator_id: None,
        category_id: None,
        label_ids: Vec::new(),
    };

    assert_eq!(
        RoomRepository::order_by_sql(&query),
        "r.last_activity_at DESC NULLS LAST, r.id DESC"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner user first (rooms have FK to users)
    let owner = UserFixture::new().with_username("room_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Test Room")
        .with_description("desc")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");
    assert_eq!(created.name, "Test Room");
    assert_eq!(created.created_by, owner.id);
    assert!(room_repo
        .exists(&created.id)
        .await
        .checked("operation should succeed"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_duplicate_name_for_same_owner_is_repository_allowed() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&UserFixture::new().with_username("room_dup_owner1").build())
        .await
        .checked("operation should succeed");

    let room1 = RoomFixture::new()
        .with_name("Duplicate Room Name")
        .with_owner(owner.id)
        .build();
    room_repo
        .create(&room1)
        .await
        .checked("operation should succeed");

    let room2 = RoomFixture::new()
        .with_name("Duplicate Room Name")
        .with_owner(owner.id)
        .build();
    let result = room_repo.create(&room2).await;

    let created = result.checked("repository should not enforce room-name product policy");
    assert_eq!(created.name, "Duplicate Room Name");
    assert_eq!(created.created_by, owner.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_duplicate_name_for_different_owner_succeeds() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner1 = user_repo
        .create(
            &UserFixture::new()
                .with_username("room_shared_owner1")
                .build(),
        )
        .await
        .checked("operation should succeed");
    let owner2 = user_repo
        .create(
            &UserFixture::new()
                .with_username("room_shared_owner2")
                .build(),
        )
        .await
        .checked("operation should succeed");

    let room1 = RoomFixture::new()
        .with_name("Shared Room Name")
        .with_owner(owner1.id)
        .build();
    let room2 = RoomFixture::new()
        .with_name("Shared Room Name")
        .with_owner(owner2.id)
        .build();

    room_repo
        .create(&room1)
        .await
        .checked("operation should succeed");
    let created = room_repo
        .create(&room2)
        .await
        .checked("operation should succeed");
    assert_eq!(created.name, "Shared Room Name");
    assert_eq!(created.created_by, owner2.id);
}

#[test]
fn test_room_ban_unban() {
    let creator_id = UserId::new();
    let mut room = Room::new("Test".to_string(), creator_id);

    assert!(!room.is_banned());
    assert!(room.is_active());

    room.ban();
    assert!(room.is_banned());
    assert!(room.is_active()); // Ban is independent from lifecycle state.

    room.unban();
    assert!(!room.is_banned());
    assert!(room.is_active()); // Active again after unban
}

#[test]
fn test_room_status() {
    assert_eq!(RoomStatus::Active.as_str(), "active");
    assert_eq!(RoomStatus::Closed.as_str(), "closed");

    assert!(RoomStatus::Active.is_active());
    assert!(RoomStatus::Closed.is_closed());
}

#[test]
fn test_room_is_active_combinations() {
    let creator_id = UserId::new();

    // Active status, not banned, not deleted
    let mut room = Room::new("Test".to_string(), creator_id);
    assert!(room.is_active());

    // Ban does not change lifecycle state.
    room.is_banned = true;
    assert!(room.is_active());

    // Not banned but closed
    room.is_banned = false;
    room.close();
    assert!(!room.is_active());

    // Deleted
    room.reopen();
    room.deleted_at = Some(crate::SystemClock.now());
    assert!(!room.is_active());
}

/// Integration test: Get non-existent room returns None
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_nonexistent_room() {
    let (_postgres, pool) = create_test_pool().await;
    let room_repo = RoomRepository::new(pool.clone());

    let room_id = RoomId::expect_positive(92_001);
    let result = room_repo
        .get_by_id(&room_id)
        .await
        .checked("operation should succeed");
    assert!(result.is_none());
}

/// Integration test: Update room
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("update_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Original Name")
        .with_description("Original description")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Update room
    let mut updated = created.clone();
    updated.name = "Updated Name".to_string();
    updated.description = "Updated description".to_string();

    let result = room_repo
        .update(&updated, created.version)
        .await
        .checked("operation should succeed");
    assert_eq!(result.name, "Updated Name");
    assert_eq!(result.description, "Updated description");
}

/// Integration test: Soft delete room
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_soft_delete_room() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("delete_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Room to Delete")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Soft delete
    let deleted = room_repo
        .delete(&created.id)
        .await
        .checked("operation should succeed");
    assert!(deleted);

    // Verify soft deleted (get_by_id returns None because deleted_at IS NOT NULL)
    let result = room_repo
        .get_by_id(&created.id)
        .await
        .checked("operation should succeed");
    assert!(result.is_none());

    // exists() also returns false
    let exists = room_repo
        .exists(&created.id)
        .await
        .checked("operation should succeed");
    assert!(!exists);

    // Delete again returns false
    let deleted_again = room_repo
        .delete(&created.id)
        .await
        .checked("operation should succeed");
    assert!(!deleted_again);
}

/// Integration test: Hard delete room
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_hard_delete_room() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new()
        .with_username("hard_delete_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Room to Hard Delete")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Hard delete
    let deleted = room_repo
        .hard_delete(&created.id)
        .await
        .checked("operation should succeed");
    assert!(deleted);
}

/// Integration test: Update room status
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room_status() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("status_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Status Test Room")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");
    assert_eq!(created.status, RoomStatus::Active);

    // Update to Closed
    let updated = room_repo
        .update_status(&created.id, RoomStatus::Closed)
        .await
        .checked("operation should succeed");
    assert_eq!(updated.status, RoomStatus::Closed);

    let banned = room_repo
        .update_ban_status(&created.id, true)
        .await
        .checked("operation should succeed");
    assert!(banned.is_banned);

    let reopened = room_repo
        .update_status(&created.id, RoomStatus::Active)
        .await
        .checked("operation should succeed");
    assert_eq!(reopened.status, RoomStatus::Active);
    assert!(
        reopened.is_banned,
        "status updates must preserve the derived active room-ban state"
    );
}

/// Integration test: Update room ban status
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_ban_status() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("ban_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Ban Test Room")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");
    assert!(!created.is_banned);

    // Ban room
    let updated = room_repo
        .update_ban_status(&created.id, true)
        .await
        .checked("operation should succeed");
    assert!(updated.is_banned);

    // Unban room
    let updated = room_repo
        .update_ban_status(&created.id, false)
        .await
        .checked("operation should succeed");
    assert!(!updated.is_banned);
}

/// Integration test: Update room description
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_description() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("desc_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Desc Test Room")
        .with_description("Original description")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Update description
    let updated = room_repo
        .update_description(&created.id, "New description")
        .await
        .checked("operation should succeed");
    assert_eq!(updated.description, "New description");

    room_repo
        .update_ban_status(&created.id, true)
        .await
        .checked("operation should succeed");
    let updated = room_repo
        .update_description(&created.id, "Another description")
        .await
        .checked("operation should succeed");
    assert_eq!(updated.description, "Another description");
    assert!(
        updated.is_banned,
        "description updates must preserve the derived active room-ban state"
    );
}

/// Integration test: List rooms with pagination
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_rooms_pagination() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner
    let owner = UserFixture::new().with_username("list_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    // Create 15 rooms
    for i in 0..15 {
        let room = RoomFixture::new()
            .with_name(&format!("List Room {i}"))
            .with_owner(owner.id)
            .build();
        room_repo
            .create(&room)
            .await
            .checked("operation should succeed");
    }

    // List with pagination
    let query = RoomListQuery {
        pagination: PageParams::new(Some(1), Some(10)),
        status: None,
        search: None,
        is_banned: None,
        creator_id: None,
        sort_by: crate::models::RoomListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
        ..Default::default()
    };
    let (rooms, total) = room_repo
        .list(&query)
        .await
        .checked("operation should succeed");
    assert_eq!(rooms.len(), 10);
    assert_eq!(total, 15);

    // Second page
    let query = RoomListQuery {
        pagination: PageParams::new(Some(2), Some(10)),
        status: None,
        search: None,
        is_banned: None,
        creator_id: None,
        sort_by: crate::models::RoomListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
        ..Default::default()
    };
    let (rooms, total) = room_repo
        .list(&query)
        .await
        .checked("operation should succeed");
    assert_eq!(rooms.len(), 5);
    assert_eq!(total, 15);
}

/// Integration test: List rooms with filters
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_rooms_with_filters() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner
    let owner = UserFixture::new().with_username("filter_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    // Create active room
    let room = RoomFixture::new()
        .with_name("Active Room")
        .with_owner(owner.id)
        .build();
    room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create and ban a room
    let mut banned_room = RoomFixture::new()
        .with_name("Banned Room")
        .with_owner(owner.id)
        .build();
    banned_room.is_banned = true;
    room_repo
        .create(&banned_room)
        .await
        .checked("operation should succeed");

    // Filter by status Active
    let query = RoomListQuery {
        pagination: PageParams::default(),
        status: Some(RoomStatus::Active),
        search: None,
        is_banned: None,
        creator_id: None,
        sort_by: crate::models::RoomListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
        ..Default::default()
    };
    let (rooms, _) = room_repo
        .list(&query)
        .await
        .checked("operation should succeed");
    assert!(rooms.iter().all(|r| r.status == RoomStatus::Active));

    // Filter by not banned
    let query = RoomListQuery {
        pagination: PageParams::default(),
        status: None,
        search: None,
        is_banned: Some(false),
        creator_id: None,
        sort_by: crate::models::RoomListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
        ..Default::default()
    };
    let (rooms, _) = room_repo
        .list(&query)
        .await
        .checked("operation should succeed");
    assert!(rooms.iter().all(|r| !r.is_banned));

    // Filter by search term
    let query = RoomListQuery {
        pagination: PageParams::default(),
        status: None,
        search: Some("Active".to_string()),
        is_banned: None,
        creator_id: None,
        sort_by: crate::models::RoomListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
        ..Default::default()
    };
    let (rooms, _) = room_repo
        .list(&query)
        .await
        .checked("operation should succeed");
    assert!(rooms.iter().all(|r| r.name.contains("Active")));
}

/// Integration test: room member_count counts current member rows.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_with_count_counts_current_members() {
    use crate::models::{RoomMember, RoomRole, User};
    use crate::repository::{RoomMemberRepository, UserRepository};
    use crate::test_helpers::{RoomFixture, UserFixture};

    fn make_user(username: &str) -> User {
        User::new(username.to_string(), crate::models::SignupMethod::Email)
    }

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&UserFixture::new().with_username("count_owner").build())
        .await
        .checked("operation should succeed");
    let active = user_repo
        .create(&make_user("count_active"))
        .await
        .checked("operation should succeed");
    let banned = user_repo
        .create(&make_user("count_banned"))
        .await
        .checked("operation should succeed");
    let rejected = user_repo
        .create(&make_user("count_rejected"))
        .await
        .checked("operation should succeed");

    let room = room_repo
        .create(
            &RoomFixture::new()
                .with_name("Counted Room")
                .with_owner(owner.id)
                .build(),
        )
        .await
        .checked("operation should succeed");

    member_repo
        .add(&RoomMember::new(room.id, active.id, RoomRole::Member))
        .await
        .checked("operation should succeed");

    let _ = banned;
    let _ = rejected;

    let query = RoomListQuery {
        pagination: PageParams::new(Some(1), Some(10)),
        status: None,
        search: Some("Counted".to_string()),
        is_banned: None,
        creator_id: None,
        sort_by: crate::models::RoomListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
        ..Default::default()
    };

    let (rows, total) = room_repo
        .list_with_count(&query)
        .await
        .checked("operation should succeed");
    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].room.id, room.id);
    assert_eq!(
        rows[0].member_count, 1,
        "room member_count should include only current member rows"
    );
    assert_eq!(
        room_repo
            .get_member_count(&room.id)
            .await
            .checked("operation should succeed"),
        1
    );
}

/// Integration test: List rooms by creator
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_creator() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create two users
    let owner1 = UserFixture::new().with_username("creator1").build();
    let owner1 = user_repo
        .create(&owner1)
        .await
        .checked("operation should succeed");

    let owner2 = UserFixture::new().with_username("creator2").build();
    let owner2 = user_repo
        .create(&owner2)
        .await
        .checked("operation should succeed");

    // Create rooms for owner1
    for i in 0..3 {
        let room = RoomFixture::new()
            .with_name(&format!("Owner1 Room {i}"))
            .with_owner(owner1.id)
            .build();
        room_repo
            .create(&room)
            .await
            .checked("operation should succeed");
    }

    // Create rooms for owner2
    for i in 0..2 {
        let room = RoomFixture::new()
            .with_name(&format!("Owner2 Room {i}"))
            .with_owner(owner2.id)
            .build();
        room_repo
            .create(&room)
            .await
            .checked("operation should succeed");
    }

    // List by creator
    let (rooms, total) = room_repo
        .list_by_creator(&owner1.id, PageParams::default())
        .await
        .checked("operation should succeed");
    assert_eq!(rooms.len(), 3);
    assert_eq!(total, 3);

    let (rooms, total) = room_repo
        .list_by_creator(&owner2.id, PageParams::default())
        .await
        .checked("operation should succeed");
    assert_eq!(rooms.len(), 2);
    assert_eq!(total, 2);
}

/// Integration test: `is_accessible`
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_is_accessible() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner
    let owner = UserFixture::new().with_username("accessible_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    // Create active room
    let room = RoomFixture::new()
        .with_name("Accessible Room")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Active room is accessible
    assert!(room_repo
        .is_accessible(&created.id)
        .await
        .checked("operation should succeed"));

    // Ban room
    room_repo
        .update_ban_status(&created.id, true)
        .await
        .checked("operation should succeed");
    assert!(!room_repo
        .is_accessible(&created.id)
        .await
        .checked("operation should succeed"));

    // Unban and close
    room_repo
        .update_ban_status(&created.id, false)
        .await
        .checked("operation should succeed");
    room_repo
        .update_status(&created.id, RoomStatus::Closed)
        .await
        .checked("operation should succeed");
    assert!(!room_repo
        .is_accessible(&created.id)
        .await
        .checked("operation should succeed"));
}

/// Integration test: `get_join_context`
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_join_context() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new()
        .with_username("join_context_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Join Context Room")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Get join context
    let context = room_repo
        .get_join_context(&created.id, &owner.id)
        .await
        .checked("operation should succeed");
    assert!(context.is_some());

    let context = context.checked("operation should succeed");
    assert_eq!(context.room.id, created.id);
    assert!(!context.is_in_kick_cooldown);

    // Non-existent room returns None
    let non_existent = RoomId::expect_positive(92_002);
    let context = room_repo
        .get_join_context(&non_existent, &owner.id)
        .await
        .checked("operation should succeed");
    assert!(context.is_none());
}

/// Integration test: `create_with_executor`
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_with_executor() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner
    let owner = UserFixture::new().with_username("executor_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    // Create room with executor (pool)
    let room = RoomFixture::new()
        .with_name("Executor Room")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create_with_executor(&room, &pool)
        .await
        .checked("operation should succeed");
    assert_eq!(created.name, "Executor Room");
}

/// Integration test: Room not found error handling
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_nonexistent_room() {
    let (_postgres, pool) = create_test_pool().await;
    let room_repo = RoomRepository::new(pool.clone());

    // Try to update non-existent room
    let room = Room::new("Non-existent".to_string(), UserId::new());
    let result = room_repo.update(&room, 0).await;
    assert!(matches!(result, Err(crate::Error::NotFound(_))));
}

/// Integration test: Optimistic lock conflict
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_stale_version_returns_optimistic_lock_conflict() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("optimistic_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Optimistic Room")
        .with_description("original")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");
    let original_version = created.version;

    // First update succeeds
    let mut updated_room = created.clone();
    updated_room.name = "Updated Name V1".to_string();
    updated_room.description = "updated v1".to_string();
    let v1 = room_repo
        .update(&updated_room, original_version)
        .await
        .checked("operation should succeed");
    assert_eq!(v1.version, original_version + 1);
    assert_eq!(v1.name, "Updated Name V1");

    // Second update with stale version (original_version) -> should get OptimisticLockConflict
    let mut stale_room = created.clone();
    stale_room.name = "Updated Name V2".to_string();
    stale_room.description = "updated v2".to_string();
    let err = room_repo
        .update(&stale_room, original_version)
        .await
        .failed("operation should fail");
    assert!(
        matches!(err, crate::Error::OptimisticLockConflict),
        "Expected OptimisticLockConflict, got: {err:?}"
    );
}

/// Integration test: Update soft-deleted room returns `NotFound`
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_soft_deleted_room_returns_not_found() {
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("softdel_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Soft Delete Room")
        .with_owner(owner.id)
        .build();
    let created = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");
    let version = created.version;

    // Soft delete the room
    let deleted = room_repo
        .delete(&created.id)
        .await
        .checked("operation should succeed");
    assert!(deleted);

    // Trying to update the deleted room should return NotFound (not OptimisticLockConflict)
    let mut updated = created.clone();
    updated.name = "Updated Soft Deleted".to_string();
    let err = room_repo
        .update(&updated, version)
        .await
        .failed("operation should fail");
    assert!(
        matches!(err, crate::Error::NotFound(_)),
        "Expected NotFound for soft-deleted room, got: {err:?}"
    );
}
