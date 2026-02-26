//! Playlist tree operation integration tests
//!
//! Tests playlist CRUD, tree structure, position sorting, cycle prevention, and cascade delete.
//!
//! Run with: cargo test --test playlist_integration_tests

use synctv_core::{
    models::{
        Room, RoomId, RoomStatus, UserId, User, UserRole, UserStatus,
        Playlist, PlaylistId,
    },
    repository::{RoomRepository, UserRepository, PlaylistRepository},
};
use chrono::Utc;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

/// Default PostgreSQL version for test containers
const POSTGRES_VERSION: &str = "16-alpine";

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
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

fn make_room(name: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: String::new(),
        created_by: owner.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
    }
}

fn make_playlist(room_id: &RoomId, name: &str, parent_id: Option<&PlaylistId>, position: i32) -> Playlist {
    Playlist {
        id: PlaylistId::new(),
        room_id: room_id.clone(),
        creator_id: None,
        name: name.to_string(),
        parent_id: parent_id.cloned(),
        position,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_root_playlist() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_1")).await.unwrap();
    let room = room_repo.create(&make_room("PL Room 1", &owner.id)).await.unwrap();

    // Create root playlist (empty name, no parent)
    let root = make_playlist(&room.id, "", None, 0);
    let created = playlist_repo.create(&root).await.unwrap();

    assert!(created.parent_id.is_none());
    assert_eq!(created.name, "");
    assert_eq!(created.room_id, room.id);
    assert!(created.is_root());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_nested_playlists() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_2")).await.unwrap();
    let room = room_repo.create(&make_room("PL Room 2", &owner.id)).await.unwrap();

    // Create root
    let root = playlist_repo.create(&make_playlist(&room.id, "", None, 0)).await.unwrap();

    // Create child under root
    let child = playlist_repo.create(&make_playlist(&room.id, "Child 1", Some(&root.id), 0)).await.unwrap();
    assert_eq!(child.parent_id.as_ref().unwrap(), &root.id);
    assert_eq!(child.name, "Child 1");

    // Create grandchild under child
    let grandchild = playlist_repo.create(&make_playlist(&room.id, "Grandchild", Some(&child.id), 0)).await.unwrap();
    assert_eq!(grandchild.parent_id.as_ref().unwrap(), &child.id);

    // Verify tree traversal via get_path
    let path = playlist_repo.get_path(&grandchild.id).await.unwrap();
    assert_eq!(path.len(), 3);
    assert_eq!(path[0].id, root.id);
    assert_eq!(path[1].id, child.id);
    assert_eq!(path[2].id, grandchild.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_position_sorting() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_3")).await.unwrap();
    let room = room_repo.create(&make_room("PL Room 3", &owner.id)).await.unwrap();

    let root = playlist_repo.create(&make_playlist(&room.id, "", None, 0)).await.unwrap();

    // Create children with explicit positions
    let _child_b = playlist_repo.create(&make_playlist(&room.id, "B", Some(&root.id), 1)).await.unwrap();
    let _child_a = playlist_repo.create(&make_playlist(&room.id, "A", Some(&root.id), 0)).await.unwrap();
    let _child_c = playlist_repo.create(&make_playlist(&room.id, "C", Some(&root.id), 2)).await.unwrap();

    // Get children - should be sorted by position ASC
    let children = playlist_repo.get_children(&root.id).await.unwrap();
    assert_eq!(children.len(), 3);
    assert_eq!(children[0].name, "A");
    assert_eq!(children[1].name, "B");
    assert_eq!(children[2].name, "C");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_position_uniqueness_constraint() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_4")).await.unwrap();
    let room = room_repo.create(&make_room("PL Room 4", &owner.id)).await.unwrap();

    let root = playlist_repo.create(&make_playlist(&room.id, "", None, 0)).await.unwrap();
    playlist_repo.create(&make_playlist(&room.id, "First", Some(&root.id), 0)).await.unwrap();

    // Try to create another child at the same position - should fail due to unique constraint
    let duplicate = make_playlist(&room.id, "Duplicate", Some(&root.id), 0);
    let result = playlist_repo.create(&duplicate).await;
    assert!(result.is_err(), "Duplicate position in same parent should fail");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cycle_prevention_trigger() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_5")).await.unwrap();
    let room = room_repo.create(&make_room("PL Room 5", &owner.id)).await.unwrap();

    let root = playlist_repo.create(&make_playlist(&room.id, "", None, 0)).await.unwrap();
    let child = playlist_repo.create(&make_playlist(&room.id, "Child", Some(&root.id), 0)).await.unwrap();
    let grandchild = playlist_repo.create(&make_playlist(&room.id, "Grandchild", Some(&child.id), 0)).await.unwrap();

    // Try to set root's parent to grandchild, creating a cycle: root -> child -> grandchild -> root
    let result = sqlx::query("UPDATE playlists SET parent_id = $1 WHERE id = $2")
        .bind(grandchild.id.as_str())
        .bind(root.id.as_str())
        .execute(&pool)
        .await;

    assert!(result.is_err(), "Circular reference should be prevented by trigger");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("Circular reference detected") || err_msg.contains("cycle"),
        "Error should mention circular reference, got: {}",
        err_msg
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cascade_delete_parent_playlist() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_6")).await.unwrap();
    let room = room_repo.create(&make_room("PL Room 6", &owner.id)).await.unwrap();

    let root = playlist_repo.create(&make_playlist(&room.id, "", None, 0)).await.unwrap();
    let child = playlist_repo.create(&make_playlist(&room.id, "Child", Some(&root.id), 0)).await.unwrap();
    let _grandchild = playlist_repo.create(&make_playlist(&room.id, "GC", Some(&child.id), 0)).await.unwrap();

    // Delete the child - grandchild should be cascade-deleted too
    let deleted = playlist_repo.delete(&child.id).await.unwrap();
    assert!(deleted);

    // Only root should remain
    let all_playlists = playlist_repo.get_by_room(&room.id).await.unwrap();
    assert_eq!(all_playlists.len(), 1);
    assert_eq!(all_playlists[0].id, root.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_auto_position_computation() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_7")).await.unwrap();
    let room = room_repo.create(&make_room("PL Room 7", &owner.id)).await.unwrap();

    let root = playlist_repo.create(&make_playlist(&room.id, "", None, 0)).await.unwrap();

    // Use negative position to trigger auto-position
    let auto_1 = make_playlist(&room.id, "Auto1", Some(&root.id), -1);
    let created_1 = playlist_repo.create(&auto_1).await.unwrap();
    assert_eq!(created_1.position, 0, "First auto-positioned item should be at position 0");

    let auto_2 = make_playlist(&room.id, "Auto2", Some(&root.id), -1);
    let created_2 = playlist_repo.create(&auto_2).await.unwrap();
    assert_eq!(created_2.position, 1, "Second auto-positioned item should be at position 1");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unique_name_constraint() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_8")).await.unwrap();
    let room = room_repo.create(&make_room("PL Room 8", &owner.id)).await.unwrap();

    let root = playlist_repo.create(&make_playlist(&room.id, "", None, 0)).await.unwrap();
    playlist_repo.create(&make_playlist(&room.id, "SameName", Some(&root.id), 0)).await.unwrap();

    // Try to create another child with the same name under the same parent
    let duplicate = make_playlist(&room.id, "SameName", Some(&root.id), 1);
    let result = playlist_repo.create(&duplicate).await;
    assert!(result.is_err(), "Duplicate name in same parent should fail");
}
