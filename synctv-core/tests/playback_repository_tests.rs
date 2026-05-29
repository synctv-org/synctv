//! `RoomPlaybackStateRepository` integration tests
//!
//! Tests `create_or_get` idempotency and update optimistic locking.
//!
//! Run with: cargo test --test `playback_repository_tests`
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use synctv_core::models::{Media, MediaId, Playlist, PlaylistId};
use synctv_core::{
    models::{Room, RoomId, RoomStatus, User, UserId, UserRole, UserStatus},
    repository::{
        MediaRepository, PlaylistRepository, RoomPlaybackStateRepository, RoomRepository,
        UserRepository,
    },
    Error,
};
use synctv_core_testing::create_test_pool;

fn assert_f64_eq(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < f64::EPSILON,
        "expected {expected}, got {actual}"
    );
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

fn make_room(name: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: "test".to_string(),
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_or_get_idempotent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_pb_idem")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room PB Idem", &owner.id))
        .await
        .unwrap();

    // First call creates the state
    let state1 = playback_repo.create_or_get(&room.id).await.unwrap();
    assert_eq!(state1.room_id, room.id);
    assert_f64_eq(state1.position, 0.0);
    assert_f64_eq(state1.speed, 1.0);
    assert!(!state1.is_playing);

    // Second call returns the same version (no new insert or update)
    let state2 = playback_repo.create_or_get(&room.id).await.unwrap();
    assert_eq!(state2.version, state1.version);
    assert_eq!(state2.room_id, state1.room_id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_optimistic_lock_conflict() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("owner_pb_lock")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room PB Lock", &owner.id))
        .await
        .unwrap();

    let state = playback_repo.create_or_get(&room.id).await.unwrap();

    // Task 1 updates with correct version
    let mut state_t1 = state.clone();
    state_t1.position = 42.0;

    // Task 2 also has the same version (stale read)
    let mut state_t2 = state.clone();
    state_t2.position = 99.0;

    // Task 1 succeeds
    let updated = playback_repo.update(&state_t1).await.unwrap();
    assert_f64_eq(updated.position, 42.0);
    assert_eq!(updated.version, state.version + 1);

    // Task 2 uses stale version -> OptimisticLockConflict
    let err = playback_repo.update(&state_t2).await.unwrap_err();
    assert!(matches!(err, Error::OptimisticLockConflict));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_concurrent_tasks_one_gets_conflict() {
    use std::sync::Arc;
    use tokio::sync::Barrier;

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = Arc::new(RoomPlaybackStateRepository::new(pool.clone()));

    let owner = user_repo.create(&make_user("owner_pb_conc")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room PB Conc", &owner.id))
        .await
        .unwrap();

    let state = playback_repo.create_or_get(&room.id).await.unwrap();

    let barrier = Arc::new(Barrier::new(2));

    // Spawn two concurrent tasks both trying to update with the same version
    let repo1 = playback_repo.clone();
    let state1 = state.clone();
    let barrier1 = barrier.clone();
    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;
        let mut s = state1;
        s.position = 10.0;
        repo1.update(&s).await
    });

    let repo2 = playback_repo.clone();
    let state2 = state.clone();
    let barrier2 = barrier.clone();
    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;
        let mut s = state2;
        s.position = 20.0;
        repo2.update(&s).await
    });

    let r1 = handle1.await.unwrap();
    let r2 = handle2.await.unwrap();

    // Exactly one should succeed and one should fail
    let (successes, failures) = match (&r1, &r2) {
        (Ok(_), Ok(_)) => (2, 0),
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => (1, 1),
        (Err(_), Err(_)) => (0, 2),
    };

    assert_eq!(successes, 1, "Exactly one update should succeed");
    assert_eq!(
        failures, 1,
        "Exactly one update should fail with OptimisticLockConflict"
    );

    // Verify the failure is OptimisticLockConflict
    let err = r1
        .as_ref()
        .err()
        .or_else(|| r2.as_ref().err())
        .expect("One should be Err");
    assert!(matches!(err, Error::OptimisticLockConflict));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_find_playback_for_creator_locks_rows_until_transaction_commit() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_pb_creator_lock"))
        .await
        .unwrap();
    let room = room_repo
        .create(&make_room("Room PB Creator Lock", &owner.id))
        .await
        .unwrap();
    let playlist = playlist_repo
        .create(&Playlist {
            id: PlaylistId::new(),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: "Creator Lock Playlist".to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();
    let media = media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: "Creator Lock Media".to_string(),
            position: 0.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({"url": "https://example.com/creator-lock.mp4"}),
            provider_instance_name: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let mut state = playback_repo.create_or_get(&room.id).await.unwrap();
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    let state = playback_repo.update(&state).await.unwrap();

    let mut tx = playback_repo.pool().begin().await.unwrap();
    let locked = playback_repo
        .find_playback_for_creator_with_executor(&owner.id, &mut *tx)
        .await
        .unwrap();
    assert_eq!(locked.len(), 1);
    assert_eq!(locked[0].room_id, room.id);

    let repo_clone = playback_repo.clone();
    let mut concurrent = state.clone();
    concurrent.position = 99.0;
    let update_handle = tokio::spawn(async move { repo_clone.update(&concurrent).await });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !update_handle.is_finished(),
        "FOR UPDATE lock should block concurrent playback updates until commit"
    );

    tx.commit().await.unwrap();
    let updated = update_handle.await.unwrap().unwrap();
    assert_f64_eq(updated.position, 99.0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_rejects_cross_room_media_and_playlist_references() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_pb_cross_room"))
        .await
        .unwrap();
    let room_a = room_repo
        .create(&make_room("Room PB Cross A", &owner.id))
        .await
        .unwrap();
    let room_b = room_repo
        .create(&make_room("Room PB Cross B", &owner.id))
        .await
        .unwrap();

    let playlist_b = playlist_repo
        .create(&Playlist {
            id: PlaylistId::new(),
            room_id: room_b.id,
            creator_id: Some(owner.id),
            name: "Room B Playlist".to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let media_b = media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: Some(playlist_b.id),
            room_id: room_b.id,
            creator_id: Some(owner.id),
            name: "Room B Media".to_string(),
            position: 0.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({"url": "https://example.com/room-b.mp4"}),
            provider_instance_name: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let mut state = playback_repo.create_or_get(&room_a.id).await.unwrap();
    state.playing_media_id = Some(media_b.id);
    state.playing_playlist_id = None;
    state.target = Vec::new();

    let result = playback_repo.update(&state).await;
    assert!(
        result.is_err(),
        "room playback state must not reference media or playlists from another room"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleting_playing_media_is_rejected_while_playback_references_it() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_pb_delete_media_fk"))
        .await
        .unwrap();
    let room = room_repo
        .create(&make_room("Room PB Delete Media FK", &owner.id))
        .await
        .unwrap();

    let playlist = playlist_repo
        .create(&Playlist {
            id: PlaylistId::new(),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: "Room Delete Media Playlist".to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let media = media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: "Room Delete Media".to_string(),
            position: 0.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({"url": "https://example.com/delete-media.mp4"}),
            provider_instance_name: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let mut state = playback_repo.create_or_get(&room.id).await.unwrap();
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    state.target = Vec::new();
    let _updated = playback_repo.update(&state).await.unwrap();

    let delete_result = media_repo.delete(&media.id).await;
    assert!(
        delete_result.is_err(),
        "deleting current media must be rejected until playback state is explicitly cleared"
    );

    let state_after_delete = playback_repo.get(&room.id).await.unwrap().unwrap();
    assert!(state_after_delete.playing_playlist_id.is_none());
    assert_eq!(state_after_delete.playing_media_id, Some(media.id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleting_playing_playlist_is_rejected_while_playback_references_it() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_pb_delete_playlist_fk"))
        .await
        .unwrap();
    let room = room_repo
        .create(&make_room("Room PB Delete Playlist FK", &owner.id))
        .await
        .unwrap();

    let playlist = playlist_repo
        .create(&Playlist {
            id: PlaylistId::new(),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: "Room Delete Playlist".to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let _media = media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: "Room Playlist Media".to_string(),
            position: 0.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({"url": "https://example.com/delete-playlist.mp4"}),
            provider_instance_name: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let mut state = playback_repo.create_or_get(&room.id).await.unwrap();
    state.playing_media_id = None;
    state.playing_playlist_id = Some(playlist.id);
    state.target = br#"{"relative_path":"/currently-playing.mp4"}"#.to_vec();
    let _updated = playback_repo.update(&state).await.unwrap();

    let delete_result = playlist_repo.delete(&playlist.id).await;
    assert!(
        delete_result.is_err(),
        "deleting current playlist must be rejected until playback state is explicitly cleared"
    );

    let state_after_delete = playback_repo.get(&room.id).await.unwrap().unwrap();
    assert_eq!(state_after_delete.playing_playlist_id, Some(playlist.id));
    assert!(state_after_delete.playing_media_id.is_none());
}
