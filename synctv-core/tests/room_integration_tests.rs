//! Room CRUD integration tests
//!
//! Tests the complete room lifecycle: create, read, update, soft delete, CASCADE behavior.
//!
//! Run with: cargo test --test room_integration_tests

use synctv_core::{
    models::{
        Room, RoomId, RoomStatus, UserId, User, UserRole, UserStatus,
        RoomMember, MemberStatus, Playlist, PlaylistId,
    },
    repository::{RoomRepository, UserRepository, RoomMemberRepository, PlaylistRepository},
};
use chrono::Utc;
use sqlx::PgPool;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
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
    }
}

#[tokio::test]
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
    let updated = room_repo.update(&updated_room).await.unwrap();

    assert_eq!(updated.name, "Updated Name");
    assert_eq!(updated.description, "updated desc");
    assert!(updated.updated_at >= created.updated_at);
}

#[tokio::test]
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
async fn test_cascade_delete_user_deletes_rooms() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("cascade_owner")).await.unwrap();

    // Create two rooms for this owner
    let room1 = room_repo.create(&make_room("Room 1", "", &owner.id)).await.unwrap();
    let room2 = room_repo.create(&make_room("Room 2", "", &owner.id)).await.unwrap();

    assert!(room_repo.exists(&room1.id).await.unwrap());
    assert!(room_repo.exists(&room2.id).await.unwrap());

    // Hard delete the user (triggers CASCADE)
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner.id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    // Both rooms should be cascade-deleted
    // Use raw query since get_by_id filters on deleted_at IS NULL
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM rooms WHERE id = $1 OR id = $2"
    )
    .bind(room1.id.as_str())
    .bind(room2.id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(count, 0, "Rooms should be cascade-deleted when owner is deleted");
}

#[tokio::test]
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
