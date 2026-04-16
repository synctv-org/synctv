//! Playlist optimistic lock integration tests
//!
//! Tests for version-based optimistic locking on playlist updates.
//!
//! Run with: cargo test --test `playlist_optimistic_lock_tests`
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use synctv_core::{
    models::{Playlist, PlaylistId, Room, RoomId, RoomStatus, User, UserId, UserRole, UserStatus},
    repository::{PlaylistRepository, RoomRepository, UserRepository},
    Error,
};
use synctv_core_testing::create_test_pool;
/// Default `PostgreSQL` version for test containers
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
        last_activity_at: now,
    }
}

fn make_playlist(
    room_id: &RoomId,
    name: &str,
    parent_id: Option<&PlaylistId>,
    position: i32,
) -> Playlist {
    Playlist {
        id: PlaylistId::new(),
        room_id: room_id.clone(),
        creator_id: None,
        name: name.to_string(),
        parent_id: parent_id.cloned(),
        position: f64::from(position),
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    }
}

/// Test: Version mismatch should return `OptimisticLockConflict` error
///
/// Scenario:
/// 1. Create a playlist (version = 0)
/// 2. Fetch the playlist
/// 3. Manually increment version in DB to simulate concurrent update
/// 4. Try to update using old version (0) - should fail with `OptimisticLockConflict`
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_version_mismatch() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("lock_owner_1")).await.unwrap();
    let room = room_repo
        .create(&make_room("Lock Room 1", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();
    let playlist = playlist_repo
        .create(&make_playlist(&room.id, "Test Playlist", Some(&root.id), 0))
        .await
        .unwrap();

    // Verify initial version
    assert_eq!(playlist.version, 0, "Initial version should be 0");

    // Simulate concurrent update by incrementing version in DB
    sqlx::query("UPDATE playlists SET version = version + 1 WHERE id = $1")
        .bind(playlist.id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    // Now try to update with the old playlist struct (version = 0)
    // This should fail because DB version is now 1
    let mut outdated_playlist = playlist.clone();
    outdated_playlist.name = "Updated Name".to_string();

    // Use update_with_version with the stale version (0)
    let result = playlist_repo
        .update_with_version(&outdated_playlist, playlist.version)
        .await;

    // Should return OptimisticLockConflict error
    assert!(
        matches!(result, Err(Error::OptimisticLockConflict)),
        "Expected OptimisticLockConflict error, got: {result:?}"
    );
}

/// Test: Version match should succeed and increment version
///
/// Scenario:
/// 1. Create a playlist (version = 0)
/// 2. Update the playlist with correct version (0)
/// 3. Verify update succeeds and version is incremented to 1
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_version_match_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("lock_owner_2")).await.unwrap();
    let room = room_repo
        .create(&make_room("Lock Room 2", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();
    let playlist = playlist_repo
        .create(&make_playlist(&room.id, "Test Playlist", Some(&root.id), 0))
        .await
        .unwrap();

    // Verify initial version
    assert_eq!(playlist.version, 0, "Initial version should be 0");

    // Update with correct version
    let mut updated_playlist = playlist.clone();
    updated_playlist.name = "Updated Name".to_string();

    let result = playlist_repo
        .update_with_version(&updated_playlist, playlist.version)
        .await;

    // Should succeed
    assert!(
        result.is_ok(),
        "Update with correct version should succeed: {:?}",
        result.err()
    );

    let updated = result.unwrap();
    assert_eq!(updated.name, "Updated Name");
    assert_eq!(updated.version, 1, "Version should be incremented to 1");

    // Fetch again to verify version in DB
    let fetched = playlist_repo
        .get_by_id(&playlist.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.version, 1, "Persisted version should be 1");
}

/// Test: Multiple sequential updates should work
///
/// Scenario:
/// 1. Create a playlist (version = 0)
/// 2. First update (version 0 -> 1)
/// 3. Second update with version 1 (version 1 -> 2)
/// 4. Third update with version 2 (version 2 -> 3)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_sequential_updates() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("lock_owner_3")).await.unwrap();
    let room = room_repo
        .create(&make_room("Lock Room 3", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();
    let mut playlist = playlist_repo
        .create(&make_playlist(&room.id, "Test", Some(&root.id), 0))
        .await
        .unwrap();

    assert_eq!(playlist.version, 0);

    // First update
    playlist.name = "Update 1".to_string();
    playlist = playlist_repo
        .update_with_version(&playlist, 0)
        .await
        .unwrap();
    assert_eq!(playlist.version, 1);
    assert_eq!(playlist.name, "Update 1");

    // Second update
    playlist.name = "Update 2".to_string();
    playlist = playlist_repo
        .update_with_version(&playlist, 1)
        .await
        .unwrap();
    assert_eq!(playlist.version, 2);
    assert_eq!(playlist.name, "Update 2");

    // Third update
    playlist.name = "Update 3".to_string();
    playlist = playlist_repo
        .update_with_version(&playlist, 2)
        .await
        .unwrap();
    assert_eq!(playlist.version, 3);
    assert_eq!(playlist.name, "Update 3");
}

/// Test: Concurrent update attempts should detect conflict
///
/// This test simulates two concurrent updaters where one "wins" and the other
/// gets an optimistic lock conflict.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_concurrent_conflict() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("lock_owner_4")).await.unwrap();
    let room = room_repo
        .create(&make_room("Lock Room 4", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();
    let playlist = playlist_repo
        .create(&make_playlist(
            &room.id,
            "Concurrent Test",
            Some(&root.id),
            0,
        ))
        .await
        .unwrap();

    // Both "threads" get the same version of the playlist
    let mut updater1_copy = playlist.clone();
    let mut updater2_copy = playlist.clone();

    // Updater 1 wins - updates first with version 0
    updater1_copy.name = "Updater 1 Wins".to_string();
    let result1 = playlist_repo
        .update_with_version(&updater1_copy, playlist.version)
        .await;
    assert!(result1.is_ok(), "First update should succeed");
    let updated1 = result1.unwrap();
    assert_eq!(updated1.version, 1);

    // Updater 2 tries to update with stale version (0) - should fail
    // because version is now 1
    updater2_copy.name = "Updater 2 Loses".to_string();
    let result2 = playlist_repo
        .update_with_version(&updater2_copy, playlist.version)
        .await;
    assert!(
        matches!(result2, Err(Error::OptimisticLockConflict)),
        "Second update with stale version should fail with OptimisticLockConflict, got: {result2:?}"
    );

    // Verify the playlist still has updater1's changes
    let current = playlist_repo
        .get_by_id(&playlist.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.name, "Updater 1 Wins");
    assert_eq!(current.version, 1);
}

/// Test: New playlist should start with version 0
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_new_playlist_version_zero() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("lock_owner_5")).await.unwrap();
    let room = room_repo
        .create(&make_room("Lock Room 5", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();
    assert_eq!(
        root.version, 0,
        "New top-level playlist should have version 0"
    );

    let child = playlist_repo
        .create(&make_playlist(&room.id, "Child", Some(&root.id), 0))
        .await
        .unwrap();
    assert_eq!(child.version, 0, "New child playlist should have version 0");
}
