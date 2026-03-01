//! Room CRUD integration tests
//!
//! Tests the complete room lifecycle: create, read, update, soft delete, CASCADE behavior.
//!
//! Run with: cargo test --test room_integration_tests
#![allow(clippy::unwrap_used)]

use synctv_core_testing::{create_test_pool};
use synctv_core::{
    models::{
        Room, RoomId, RoomStatus, UserId, User, UserRole, UserStatus,
        RoomMember, MemberStatus, Playlist, PlaylistId,
    },
    repository::{RoomRepository, UserRepository, RoomMemberRepository, PlaylistRepository},
};
use chrono::Utc;
/// Default PostgreSQL version for test containers
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

fn make_room(name: &str, description: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: description.to_string(),
        created_by: owner.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_basic() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("room_owner_1")).await.unwrap();
    let room = make_room("Test Room", "A test room", &owner.id);
    let created = room_repo.create(&room).await.unwrap();

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

    let owner = user_repo.create(&make_user("room_owner_2")).await.unwrap();

    // 500 characters should be fine (description is TEXT, no DB-level length constraint,
    // but application layer should enforce <= 500)
    let long_desc = "a".repeat(500);
    let room = make_room("Long Desc Room", &long_desc, &owner.id);
    let created = room_repo.create(&room).await.unwrap();
    assert_eq!(created.description.len(), 500);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room_settings() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("room_owner_3")).await.unwrap();
    let room = make_room("Original Name", "original desc", &owner.id);
    let created = room_repo.create(&room).await.unwrap();

    // Update name and description
    let mut updated_room = created.clone();
    updated_room.name = "Updated Name".to_string();
    updated_room.description = "updated desc".to_string();
    let updated = room_repo.update(&updated_room, created.version).await.unwrap();

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

    let owner = user_repo.create(&make_user("room_owner_4")).await.unwrap();
    let room = make_room("Status Room", "", &owner.id);
    let created = room_repo.create(&room).await.unwrap();
    assert_eq!(created.status, RoomStatus::Active);

    let updated = room_repo.update_status(&created.id, RoomStatus::Closed).await.unwrap();
    assert_eq!(updated.status, RoomStatus::Closed);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_soft_delete_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("room_owner_5")).await.unwrap();
    let room = make_room("Delete Me", "", &owner.id);
    let created = room_repo.create(&room).await.unwrap();

    // Room should exist before delete
    assert!(room_repo.exists(&created.id).await.unwrap());

    // Soft delete
    let deleted = room_repo.delete(&created.id).await.unwrap();
    assert!(deleted);

    // Room should not be found by get_by_id (which filters deleted_at IS NULL)
    let fetched = room_repo.get_by_id(&created.id).await.unwrap();
    assert!(fetched.is_none());

    // exists should return false
    assert!(!room_repo.exists(&created.id).await.unwrap());

    // Double delete should return false (already deleted)
    let deleted_again = room_repo.delete(&created.id).await.unwrap();
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

    let owner = user_repo.create(&make_user("cascade_owner")).await.unwrap();

    // Create two rooms for this owner
    let room1 = room_repo.create(&make_room("Room 1", "", &owner.id)).await.unwrap();
    let room2 = room_repo.create(&make_room("Room 2", "", &owner.id)).await.unwrap();

    assert!(room_repo.exists(&room1.id).await.unwrap());
    assert!(room_repo.exists(&room2.id).await.unwrap());

    // Attempting to delete the user while rooms still exist should fail
    // because of ON DELETE RESTRICT on rooms.created_by.
    let delete_result = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner.id.as_str())
        .execute(&pool)
        .await;
    assert!(
        delete_result.is_err(),
        "Deleting a user with owned rooms should fail due to FK RESTRICT"
    );

    // Delete rooms first, then the user should succeed.
    room_repo.delete(&room1.id).await.unwrap();
    room_repo.delete(&room2.id).await.unwrap();

    // Now that rooms are soft-deleted, hard-delete the rows so the FK is clear.
    sqlx::query("DELETE FROM rooms WHERE id = $1 OR id = $2")
        .bind(room1.id.as_str())
        .bind(room2.id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner.id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    // User should be gone
    let user_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE id = $1"
    )
    .bind(owner.id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(user_count, 0, "User should be deleted after rooms are removed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cascade_delete_room_deletes_members_and_playlists() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("cascade_room_owner")).await.unwrap();
    let member_user = user_repo.create(&make_user("cascade_member")).await.unwrap();
    let room = room_repo.create(&make_room("Cascade Room", "", &owner.id)).await.unwrap();

    // Add a member
    let rm = RoomMember {
        room_id: room.id.clone(),
        user_id: member_user.id.clone(),
        role: synctv_core::models::RoomRole::Member,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: Utc::now(),
        left_at: None,
        version: 0,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };
    member_repo.add(&rm).await.unwrap();

    // Create a root playlist for the room
    let root_playlist = Playlist {
        id: PlaylistId::new(),
        room_id: room.id.clone(),
        creator_id: Some(owner.id.clone()),
        name: String::new(),
        parent_id: None,
        position: 0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    playlist_repo.create(&root_playlist).await.unwrap();

    // Hard delete the room (CASCADE should remove members and playlists)
    let deleted = room_repo.hard_delete(&room.id).await.unwrap();
    assert!(deleted);

    // Check members are gone
    let member_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM room_members WHERE room_id = $1"
    )
    .bind(room.id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(member_count, 0, "Room members should be cascade-deleted");

    // Check playlists are gone
    let playlist_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM playlists WHERE room_id = $1"
    )
    .bind(room.id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(playlist_count, 0, "Playlists should be cascade-deleted");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_creation_unique_ids() {
    use std::sync::Arc;

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = Arc::new(RoomRepository::new(pool.clone()));

    let owner = user_repo.create(&make_user("concurrent_owner")).await.unwrap();
    let owner_id = owner.id.clone();

    // Spawn 10 concurrent room creations
    let mut handles = vec![];
    for i in 0..10 {
        let repo = room_repo.clone();
        let oid = owner_id.clone();
        let handle = tokio::spawn(async move {
            let room = make_room(&format!("Concurrent Room {}", i), "", &oid);
            repo.create(&room).await
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // All should succeed since each room gets a unique nanoid
    let mut created_ids = std::collections::HashSet::new();
    for result in results {
        let room = result.unwrap().unwrap();
        assert!(created_ids.insert(room.id.as_str().to_string()), "Room IDs should be unique");
    }
    assert_eq!(created_ids.len(), 10);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_ban_status() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("ban_owner")).await.unwrap();
    let room = room_repo.create(&make_room("Ban Room", "", &owner.id)).await.unwrap();

    assert!(!room.is_banned);
    assert!(room_repo.is_accessible(&room.id).await.unwrap());

    // Ban the room
    let banned = room_repo.update_ban_status(&room.id, true).await.unwrap();
    assert!(banned.is_banned);

    // Banned room should not be accessible
    assert!(!room_repo.is_accessible(&room.id).await.unwrap());

    // Unban the room
    let unbanned = room_repo.update_ban_status(&room.id, false).await.unwrap();
    assert!(!unbanned.is_banned);
    assert!(room_repo.is_accessible(&room.id).await.unwrap());
}

// ========== Optimistic Lock Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_stale_version_returns_optimistic_lock_conflict() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("optimistic_owner")).await.unwrap();
    let room = make_room("Optimistic Room", "original", &owner.id);
    let created = room_repo.create(&room).await.unwrap();
    let original_version = created.version;

    // First update succeeds
    let mut updated_room = created.clone();
    updated_room.name = "Updated Name V1".to_string();
    updated_room.description = "updated v1".to_string();
    let v1 = room_repo.update(&updated_room, original_version).await.unwrap();
    assert_eq!(v1.version, original_version + 1);
    assert_eq!(v1.name, "Updated Name V1");

    // Second update with stale version (original_version) -> should get OptimisticLockConflict
    let mut stale_room = created.clone();
    stale_room.name = "Updated Name V2".to_string();
    stale_room.description = "updated v2".to_string();
    let err = room_repo.update(&stale_room, original_version).await.unwrap_err();
    assert!(
        matches!(err, synctv_core::Error::OptimisticLockConflict),
        "Expected OptimisticLockConflict, got: {:?}", err
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_soft_deleted_room_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("softdel_owner")).await.unwrap();
    let room = make_room("Soft Delete Room", "", &owner.id);
    let created = room_repo.create(&room).await.unwrap();
    let version = created.version;

    // Soft delete the room
    let deleted = room_repo.delete(&created.id).await.unwrap();
    assert!(deleted);

    // Trying to update the deleted room should return NotFound (not OptimisticLockConflict)
    let mut updated = created.clone();
    updated.name = "Updated Soft Deleted".to_string();
    let err = room_repo.update(&updated, version).await.unwrap_err();
    assert!(
        matches!(err, synctv_core::Error::NotFound(_)),
        "Expected NotFound for soft-deleted room, got: {:?}", err
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_nonexistent_room_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("nonexistent_owner")).await.unwrap();

    // Create a room model but never persist it
    let room = make_room("Nonexistent Room", "", &owner.id);

    // Trying to update should return NotFound
    let err = room_repo.update(&room, 0).await.unwrap_err();
    assert!(
        matches!(err, synctv_core::Error::NotFound(_)),
        "Expected NotFound for nonexistent room, got: {:?}", err
    );
}
