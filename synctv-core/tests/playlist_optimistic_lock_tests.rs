//! Playlist optimistic lock integration tests
//!
//! Tests for version-based optimistic locking on playlist updates.
//!

use chrono::Utc;
use synctv_core::{
    models::{Playlist, PlaylistId, Room, RoomId, RoomStatus, User, UserId, UserRole, UserStatus},
    repository::{PlaylistRepository, RoomRepository, UserRepository},
    Error,
};
use synctv_core_testing::{create_test_pool, ok, some};

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
        description: String::new(),
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

fn make_playlist(
    room_id: &RoomId,
    name: &str,
    parent_id: Option<&PlaylistId>,
    position: i32,
) -> Playlist {
    Playlist {
        id: PlaylistId::new(),
        room_id: *room_id,
        creator_id: None,
        name: name.to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: parent_id.copied(),
        position: f64::from(position),
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
async fn test_optimistic_lock_version_mismatch() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = ok(
        user_repo.create(&make_user("lock_owner_1")).await,
        "lock owner should be created",
    );
    let room = ok(
        room_repo.create(&make_room("Lock Room 1", &owner.id)).await,
        "lock room should be created",
    );

    let root = ok(
        playlist_repo
            .create(&make_playlist(&room.id, "", None, 0))
            .await,
        "root playlist should be created",
    );
    let playlist = ok(
        playlist_repo
            .create(&make_playlist(&room.id, "Test Playlist", Some(&root.id), 0))
            .await,
        "test playlist should be created",
    );

    // Verify initial version
    assert_eq!(playlist.version, 0, "Initial version should be 0");

    // Simulate concurrent update by incrementing version in DB
    ok(
        sqlx::query!(
            "UPDATE playlists SET version = version + 1 WHERE id = $1",
            playlist.id.as_i64()
        )
        .execute(&pool)
        .await,
        "playlist version should be manually incremented",
    );

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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_version_match_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = ok(
        user_repo.create(&make_user("lock_owner_2")).await,
        "lock owner should be created",
    );
    let room = ok(
        room_repo.create(&make_room("Lock Room 2", &owner.id)).await,
        "lock room should be created",
    );

    let root = ok(
        playlist_repo
            .create(&make_playlist(&room.id, "", None, 0))
            .await,
        "root playlist should be created",
    );
    let playlist = ok(
        playlist_repo
            .create(&make_playlist(&room.id, "Test Playlist", Some(&root.id), 0))
            .await,
        "test playlist should be created",
    );

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

    let updated = ok(
        result,
        "playlist update with current version should succeed",
    );
    assert_eq!(updated.name, "Updated Name");
    assert_eq!(updated.version, 1, "Version should be incremented to 1");

    // Fetch again to verify version in DB
    let fetched = some(
        ok(
            playlist_repo.get_by_id(&playlist.id).await,
            "updated playlist should be fetched",
        ),
        "updated playlist should exist",
    );
    assert_eq!(fetched.version, 1, "Persisted version should be 1");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_sequential_updates() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = ok(
        user_repo.create(&make_user("lock_owner_3")).await,
        "lock owner should be created",
    );
    let room = ok(
        room_repo.create(&make_room("Lock Room 3", &owner.id)).await,
        "lock room should be created",
    );

    let root = ok(
        playlist_repo
            .create(&make_playlist(&room.id, "", None, 0))
            .await,
        "root playlist should be created",
    );
    let mut playlist = ok(
        playlist_repo
            .create(&make_playlist(&room.id, "Test", Some(&root.id), 0))
            .await,
        "test playlist should be created",
    );

    assert_eq!(playlist.version, 0);

    // First update
    playlist.name = "Update 1".to_string();
    playlist = ok(
        playlist_repo.update_with_version(&playlist, 0).await,
        "first playlist update should succeed",
    );
    assert_eq!(playlist.version, 1);
    assert_eq!(playlist.name, "Update 1");

    // Second update
    playlist.name = "Update 2".to_string();
    playlist = ok(
        playlist_repo.update_with_version(&playlist, 1).await,
        "second playlist update should succeed",
    );
    assert_eq!(playlist.version, 2);
    assert_eq!(playlist.name, "Update 2");

    // Third update
    playlist.name = "Update 3".to_string();
    playlist = ok(
        playlist_repo.update_with_version(&playlist, 2).await,
        "third playlist update should succeed",
    );
    assert_eq!(playlist.version, 3);
    assert_eq!(playlist.name, "Update 3");
}

/// This test simulates two concurrent updaters where one "wins" and the other
/// gets an optimistic lock conflict.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_concurrent_conflict() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = ok(
        user_repo.create(&make_user("lock_owner_4")).await,
        "lock owner should be created",
    );
    let room = ok(
        room_repo.create(&make_room("Lock Room 4", &owner.id)).await,
        "lock room should be created",
    );

    let root = ok(
        playlist_repo
            .create(&make_playlist(&room.id, "", None, 0))
            .await,
        "root playlist should be created",
    );
    let playlist = ok(
        playlist_repo
            .create(&make_playlist(
                &room.id,
                "Concurrent Test",
                Some(&root.id),
                0,
            ))
            .await,
        "concurrent test playlist should be created",
    );

    // Both "threads" get the same version of the playlist
    let mut updater1_copy = playlist.clone();
    let mut updater2_copy = playlist.clone();

    // Updater 1 wins - updates first with version 0
    updater1_copy.name = "Updater 1 Wins".to_string();
    let result1 = playlist_repo
        .update_with_version(&updater1_copy, playlist.version)
        .await;
    assert!(result1.is_ok(), "First update should succeed");
    let updated1 = ok(
        result1,
        "first concurrent-style playlist update should succeed",
    );
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
    let current = some(
        ok(
            playlist_repo.get_by_id(&playlist.id).await,
            "current playlist should be fetched",
        ),
        "current playlist should exist",
    );
    assert_eq!(current.name, "Updater 1 Wins");
    assert_eq!(current.version, 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_new_playlist_version_zero() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = ok(
        user_repo.create(&make_user("lock_owner_5")).await,
        "lock owner should be created",
    );
    let room = ok(
        room_repo.create(&make_room("Lock Room 5", &owner.id)).await,
        "lock room should be created",
    );

    let root = ok(
        playlist_repo
            .create(&make_playlist(&room.id, "", None, 0))
            .await,
        "root playlist should be created",
    );
    assert_eq!(
        root.version, 0,
        "New top-level playlist should have version 0"
    );

    let child = ok(
        playlist_repo
            .create(&make_playlist(&room.id, "Child", Some(&root.id), 0))
            .await,
        "child playlist should be created",
    );
    assert_eq!(child.version, 0, "New child playlist should have version 0");
}
