//! Room CRUD integration tests
//!
//! Tests the complete room lifecycle: create, read, update, soft delete, CASCADE behavior.
//!

use chrono::Utc;
use synctv_core::{
    models::{
        MemberStatus, Playlist, PlaylistId, Room, RoomId, RoomMember, RoomStatus, User, UserId,
        UserRole, UserStatus,
    },
    repository::{PlaylistRepository, RoomMemberRepository, RoomRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;
use synctv_core_testing::TestResultExt;
/// Default `PostgreSQL` version for test containers
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

fn make_room(name: &str, description: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: description.to_string(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: *owner,
        status: RoomStatus::Active,
        is_banned: false,
        is_public: true,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_basic() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_owner_1"))
        .await
        .checked("test operation should succeed");
    let room = make_room("Test Room", "A test room", &owner.id);
    let created = room_repo
        .create(&room)
        .await
        .checked("test operation should succeed");

    assert_eq!(created.name, "Test Room");
    assert_eq!(created.description, "A test room");
    assert_eq!(created.created_by, owner.id);
    assert_eq!(created.status, RoomStatus::Active);
    assert!(!created.is_banned);
    assert!(created.deleted_at.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_description_length() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_owner_2"))
        .await
        .checked("test operation should succeed");

    // 500 characters should be fine (description is TEXT, no DB-level length constraint,
    // but application layer should enforce <= 500)
    let long_desc = "a".repeat(500);
    let room = make_room("Long Desc Room", &long_desc, &owner.id);
    let created = room_repo
        .create(&room)
        .await
        .checked("test operation should succeed");
    assert_eq!(created.description.len(), 500);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room_settings() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_owner_3"))
        .await
        .checked("test operation should succeed");
    let room = make_room("Original Name", "original desc", &owner.id);
    let created = room_repo
        .create(&room)
        .await
        .checked("test operation should succeed");

    // Update name and description
    let mut updated_room = created.clone();
    updated_room.name = "Updated Name".to_string();
    updated_room.description = "updated desc".to_string();
    let updated = room_repo
        .update(&updated_room, created.version)
        .await
        .checked("test operation should succeed");

    assert_eq!(updated.name, "Updated Name");
    assert_eq!(updated.description, "updated desc");
    assert!(updated.updated_at >= created.updated_at);
    assert_eq!(updated.version, created.version + 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room_status() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_owner_4"))
        .await
        .checked("test operation should succeed");
    let room = make_room("Status Room", "", &owner.id);
    let created = room_repo
        .create(&room)
        .await
        .checked("test operation should succeed");
    assert_eq!(created.status, RoomStatus::Active);

    let updated = room_repo
        .update_status(&created.id, RoomStatus::Closed)
        .await
        .checked("test operation should succeed");
    assert_eq!(updated.status, RoomStatus::Closed);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_soft_delete_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_owner_5"))
        .await
        .checked("test operation should succeed");
    let room = make_room("Delete Me", "", &owner.id);
    let created = room_repo
        .create(&room)
        .await
        .checked("test operation should succeed");

    // Room should exist before delete
    assert!(room_repo
        .exists(&created.id)
        .await
        .checked("test operation should succeed"));

    // Soft delete
    let deleted = room_repo
        .delete(&created.id)
        .await
        .checked("test operation should succeed");
    assert!(deleted);

    // Room should not be found by get_by_id (which filters deleted_at IS NULL)
    let fetched = room_repo
        .get_by_id(&created.id)
        .await
        .checked("test operation should succeed");
    assert!(fetched.is_none());

    // exists should return false
    assert!(!room_repo
        .exists(&created.id)
        .await
        .checked("test operation should succeed"));

    // Double delete should return false (already deleted)
    let deleted_again = room_repo
        .delete(&created.id)
        .await
        .checked("test operation should succeed");
    assert!(!deleted_again);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cascade_delete_user_deletes_rooms() {
    // The `rooms.created_by` FK uses ON DELETE RESTRICT (not CASCADE), so
    // deleting a user who still owns rooms must fail.  The correct application
    // flow is: delete/transfer rooms first, then delete the user.
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("cascade_owner"))
        .await
        .checked("test operation should succeed");

    let room1 = room_repo
        .create(&make_room("Room 1", "", &owner.id))
        .await
        .checked("test operation should succeed");
    let room2 = room_repo
        .create(&make_room("Room 2", "", &owner.id))
        .await
        .checked("test operation should succeed");

    assert!(room_repo
        .exists(&room1.id)
        .await
        .checked("test operation should succeed"));
    assert!(room_repo
        .exists(&room2.id)
        .await
        .checked("test operation should succeed"));

    // Attempting to delete the user while rooms still exist should fail
    // because of ON DELETE RESTRICT on rooms.created_by.
    let delete_result = sqlx::query!("DELETE FROM users WHERE id = $1", owner.id.as_i64())
        .execute(&pool)
        .await;
    assert!(
        delete_result.is_err(),
        "Deleting a user with owned rooms should fail due to FK RESTRICT"
    );

    // Delete rooms first, then the user should succeed.
    room_repo
        .delete(&room1.id)
        .await
        .checked("test operation should succeed");
    room_repo
        .delete(&room2.id)
        .await
        .checked("test operation should succeed");

    // Now that rooms are soft-deleted, hard-delete the rows so the FK is clear.
    sqlx::query!(
        "DELETE FROM rooms WHERE id = $1 OR id = $2",
        room1.id.as_i64(),
        room2.id.as_i64()
    )
    .execute(&pool)
    .await
    .checked("test operation should succeed");

    sqlx::query!("DELETE FROM users WHERE id = $1", owner.id.as_i64())
        .execute(&pool)
        .await
        .checked("test operation should succeed");

    // User should be gone
    let user_count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM users WHERE id = $1"#,
        owner.id.as_i64()
    )
    .fetch_one(&pool)
    .await
    .checked("test operation should succeed");
    assert_eq!(
        user_count, 0,
        "User should be deleted after rooms are removed"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cascade_delete_room_deletes_members_and_playlists() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("cascade_room_owner"))
        .await
        .checked("test operation should succeed");
    let member_user = user_repo
        .create(&make_user("cascade_member"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Cascade Room", "", &owner.id))
        .await
        .checked("test operation should succeed");

    // Add a member
    let rm = RoomMember {
        room_id: room.id,
        user_id: member_user.id,
        role: synctv_core::models::RoomRole::Member,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        remark_name: String::new(),
        display_tag: String::new(),
        joined_at: Utc::now(),
        version: 0,
    };
    member_repo
        .add(&rm)
        .await
        .checked("test operation should succeed");

    let root_playlist = Playlist {
        id: PlaylistId::new(),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: String::new(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: 0.0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    playlist_repo
        .create(&root_playlist)
        .await
        .checked("test operation should succeed");

    // Hard delete the room through the explicit cleanup path.
    let deleted = room_repo
        .hard_delete(&room.id)
        .await
        .checked("test operation should succeed");
    assert!(deleted);

    // Check members are gone
    let member_count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM room_members WHERE room_id = $1"#,
        room.id.as_i64()
    )
    .fetch_one(&pool)
    .await
    .checked("test operation should succeed");
    assert_eq!(member_count, 0, "Room members should be explicitly deleted");

    // Check playlists are gone
    let playlist_count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM playlists WHERE room_id = $1"#,
        room.id.as_i64()
    )
    .fetch_one(&pool)
    .await
    .checked("test operation should succeed");
    assert_eq!(playlist_count, 0, "Playlists should be explicitly deleted");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_creation_unique_ids() {
    use std::sync::Arc;

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = Arc::new(RoomRepository::new(pool.clone()));

    let owner = user_repo
        .create(&make_user("concurrent_owner"))
        .await
        .checked("test operation should succeed");
    let owner_id = owner.id;

    // Spawn 10 concurrent room creations
    let mut handles = vec![];
    for i in 0..10 {
        let repo = room_repo.clone();
        let oid = owner_id;
        let handle = tokio::spawn(async move {
            let room = make_room(&format!("Concurrent Room {i}"), "", &oid);
            repo.create(&room).await
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // All should succeed since each room gets a unique base62 ID
    let mut created_ids = std::collections::HashSet::new();
    for result in results {
        let room = result
            .checked("test operation should succeed")
            .checked("test operation should succeed");
        assert!(
            created_ids.insert(room.id.to_string()),
            "Room IDs should be unique"
        );
    }
    assert_eq!(created_ids.len(), 10);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_ban_status() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("ban_owner"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Ban Room", "", &owner.id))
        .await
        .checked("test operation should succeed");

    assert!(!room.is_banned);
    assert!(room_repo
        .is_accessible(&room.id)
        .await
        .checked("test operation should succeed"));

    // Ban the room
    let banned = room_repo
        .update_ban_status(&room.id, true)
        .await
        .checked("test operation should succeed");
    assert!(banned.is_banned);

    // Banned room should not be accessible
    assert!(!room_repo
        .is_accessible(&room.id)
        .await
        .checked("test operation should succeed"));

    // Unban the room
    let unbanned = room_repo
        .update_ban_status(&room.id, false)
        .await
        .checked("test operation should succeed");
    assert!(!unbanned.is_banned);
    assert!(room_repo
        .is_accessible(&room.id)
        .await
        .checked("test operation should succeed"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_stale_version_returns_optimistic_lock_conflict() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("optimistic_owner"))
        .await
        .checked("test operation should succeed");
    let room = make_room("Optimistic Room", "original", &owner.id);
    let created = room_repo
        .create(&room)
        .await
        .checked("test operation should succeed");
    let original_version = created.version;

    // First update succeeds
    let mut updated_room = created.clone();
    updated_room.name = "Updated Name V1".to_string();
    updated_room.description = "updated v1".to_string();
    let v1 = room_repo
        .update(&updated_room, original_version)
        .await
        .checked("test operation should succeed");
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
        matches!(err, synctv_core::Error::OptimisticLockConflict),
        "Expected OptimisticLockConflict, got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_soft_deleted_room_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("softdel_owner"))
        .await
        .checked("test operation should succeed");
    let room = make_room("Soft Delete Room", "", &owner.id);
    let created = room_repo
        .create(&room)
        .await
        .checked("test operation should succeed");
    let version = created.version;

    // Soft delete the room
    let deleted = room_repo
        .delete(&created.id)
        .await
        .checked("test operation should succeed");
    assert!(deleted);

    // Trying to update the deleted room should return NotFound (not OptimisticLockConflict)
    let mut updated = created.clone();
    updated.name = "Updated Soft Deleted".to_string();
    let err = room_repo
        .update(&updated, version)
        .await
        .failed("operation should fail");
    assert!(
        matches!(err, synctv_core::Error::NotFound(_)),
        "Expected NotFound for soft-deleted room, got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_nonexistent_room_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("nonexistent_owner"))
        .await
        .checked("test operation should succeed");

    let room = make_room("Nonexistent Room", "", &owner.id);

    // Trying to update should return NotFound
    let err = room_repo
        .update(&room, 0)
        .await
        .failed("operation should fail");
    assert!(
        matches!(err, synctv_core::Error::NotFound(_)),
        "Expected NotFound for nonexistent room, got: {err:?}"
    );
}
