//! `RoomPlaybackStateRepository` integration tests
//!
//! Tests `create_or_get` idempotency and update optimistic locking.

use chrono::Utc;
use synctv_core::models::{
    DeletionSource, FromProviderParams, Media, MediaId, Playlist, PlaylistId, ProviderTarget,
    RoomMember, RoomRole, SourceProvider,
};
use synctv_core::{
    models::{Room, RoomId, RoomStatus, User, UserId, UserRole, UserStatus},
    repository::{
        MediaRepository, PlaylistRepository, RoomMemberRepository, RoomPlaybackStateRepository,
        RoomRepository, UserRepository,
    },
    service::DeleteEntriesRequest,
    Error,
};
use synctv_core_testing::{create_test_pool, create_test_room_service, err, ok, some};

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
        description: "test".to_string(),
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

async fn attach_test_media(
    pool: &sqlx::PgPool,
    playback_repo: &RoomPlaybackStateRepository,
    mut state: synctv_core::models::RoomPlaybackState,
    owner_id: UserId,
) -> synctv_core::models::RoomPlaybackState {
    let media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: None,
        room_id: state.room_id,
        creator_id: Some(owner_id),
        name: "Playback Repository Test Video".to_string(),
        description: String::new(),
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/video.mp4",
        ),
        source_provider: SourceProvider::DirectUrl,
        provider_instance_name: None,
        position: 0.0,
    });
    let media = ok(
        MediaRepository::new(pool.clone()).create(&media).await,
        "test media should be created",
    );
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    state.target = None;
    state.position = 0.0;
    ok(
        playback_repo.update(&state).await,
        "playback state should attach test media",
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_or_get_idempotent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = ok(
        user_repo.create(&make_user("owner_pb_idem")).await,
        "owner should be created",
    );
    let room = ok(
        room_repo
            .create(&make_room("Room PB Idem", &owner.id))
            .await,
        "room should be created",
    );

    // First call creates the state
    let state1 = ok(
        playback_repo.create_or_get(&room.id).await,
        "playback state should be created",
    );
    assert_eq!(state1.room_id, room.id);
    assert_f64_eq(state1.position, 0.0);
    assert_f64_eq(state1.speed, 1.0);
    assert!(!state1.is_playing);

    // Second call returns the same version (no new insert or update)
    let state2 = ok(
        playback_repo.create_or_get(&room.id).await,
        "playback state should be loaded",
    );
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

    let owner = ok(
        user_repo.create(&make_user("owner_pb_lock")).await,
        "owner should be created",
    );
    let room = ok(
        room_repo
            .create(&make_room("Room PB Lock", &owner.id))
            .await,
        "room should be created",
    );

    let state = ok(
        playback_repo.create_or_get(&room.id).await,
        "playback state should be created",
    );
    let state = attach_test_media(&pool, &playback_repo, state, owner.id).await;

    // Task 1 updates with correct version
    let mut state_t1 = state.clone();
    state_t1.position = 42.0;

    // Task 2 also has the same version (stale read)
    let mut state_t2 = state.clone();
    state_t2.position = 99.0;

    // Task 1 succeeds
    let updated = ok(
        playback_repo.update(&state_t1).await,
        "fresh playback state update should succeed",
    );
    assert_f64_eq(updated.position, 42.0);
    assert_eq!(updated.version, state.version + 1);

    // Task 2 uses stale version -> OptimisticLockConflict
    let err = err(
        playback_repo.update(&state_t2).await,
        "stale playback state update should fail",
    );
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

    let owner = ok(
        user_repo.create(&make_user("owner_pb_conc")).await,
        "owner should be created",
    );
    let room = ok(
        room_repo
            .create(&make_room("Room PB Conc", &owner.id))
            .await,
        "room should be created",
    );

    let state = ok(
        playback_repo.create_or_get(&room.id).await,
        "playback state should be created",
    );

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

    let r1 = ok(
        handle1.await,
        "first playback state update task should complete",
    );
    let r2 = ok(
        handle2.await,
        "second playback state update task should complete",
    );

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
    let Some(err) = r1.as_ref().err().or_else(|| r2.as_ref().err()) else {
        std::panic::panic_any("one update should fail");
    };
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

    let owner = ok(
        user_repo.create(&make_user("owner_pb_creator_lock")).await,
        "owner should be created",
    );
    let room = ok(
        room_repo
            .create(&make_room("Room PB Creator Lock", &owner.id))
            .await,
        "room should be created",
    );
    let playlist = ok(
        playlist_repo
            .create(&Playlist {
                id: PlaylistId::new(),
                room_id: room.id,
                creator_id: Some(owner.id),
                name: "Creator Lock Playlist".to_string(),
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
            })
            .await,
        "playlist should be created",
    );
    let media = ok(
        media_repo
            .create(&Media {
                id: MediaId::new(),
                playlist_id: Some(playlist.id),
                room_id: room.id,
                creator_id: Some(owner.id),
                name: "Creator Lock Media".to_string(),
                description: String::new(),
                position: 0.0,
                source_provider: SourceProvider::DirectUrl,
                source_config: synctv_core_testing::direct_url_media_source_config(
                    "https://example.com/creator-lock.mp4",
                ),
                provider_instance_name: None,
                cover_file_reference_id: None,
                thumbnail_file_reference_id: None,
                added_at: Utc::now(),
                updated_at: Utc::now(),
                version: 0,
            })
            .await,
        "media should be created",
    );

    let mut state = ok(
        playback_repo.create_or_get(&room.id).await,
        "playback state should be created",
    );
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    let state = ok(
        playback_repo.update(&state).await,
        "playback state should update",
    );

    let mut tx = ok(
        playback_repo.pool().begin().await,
        "transaction should begin",
    );
    let locked = ok(
        playback_repo
            .find_playback_for_creator_with_executor(&owner.id, &mut *tx)
            .await,
        "creator playback rows should be locked",
    );
    assert_eq!(locked.len(), 1);
    assert_eq!(locked[0].room_id, room.id);

    let repo_clone = playback_repo.clone();
    let mut concurrent = state.clone();
    concurrent.position = 99.0;
    let update_handle = tokio::spawn(async move { repo_clone.update(&concurrent).await });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !update_handle.is_finished(),
        "FOR UPDATE lock should block concurrent playback state updates until commit"
    );

    ok(tx.commit().await, "transaction should commit");
    let updated = ok(
        ok(
            update_handle.await,
            "concurrent playback state update task should complete",
        ),
        "concurrent playback state update should succeed",
    );
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

    let owner = ok(
        user_repo.create(&make_user("owner_pb_cross_room")).await,
        "owner should be created",
    );
    let room_a = ok(
        room_repo
            .create(&make_room("Room PB Cross A", &owner.id))
            .await,
        "first room should be created",
    );
    let room_b = ok(
        room_repo
            .create(&make_room("Room PB Cross B", &owner.id))
            .await,
        "second room should be created",
    );

    let playlist_b = ok(
        playlist_repo
            .create(&Playlist {
                id: PlaylistId::new(),
                room_id: room_b.id,
                creator_id: Some(owner.id),
                name: "Room B Playlist".to_string(),
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
            })
            .await,
        "second room playlist should be created",
    );

    let media_b = ok(
        media_repo
            .create(&Media {
                id: MediaId::new(),
                playlist_id: Some(playlist_b.id),
                room_id: room_b.id,
                creator_id: Some(owner.id),
                name: "Room B Media".to_string(),
                description: String::new(),
                position: 0.0,
                source_provider: SourceProvider::DirectUrl,
                source_config: synctv_core_testing::direct_url_media_source_config(
                    "https://example.com/room-b.mp4",
                ),
                provider_instance_name: None,
                cover_file_reference_id: None,
                thumbnail_file_reference_id: None,
                added_at: Utc::now(),
                updated_at: Utc::now(),
                version: 0,
            })
            .await,
        "second room media should be created",
    );

    let mut state = ok(
        playback_repo.create_or_get(&room_a.id).await,
        "playback state should be created",
    );
    state.playing_media_id = Some(media_b.id);
    state.playing_playlist_id = None;
    state.target = None;

    let result = playback_repo.update(&state).await;
    assert!(
        result.is_err(),
        "room playback state must not reference media or playlists from another room"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_soft_deleting_playing_media_preserves_playback_reference_for_service_cleanup() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = ok(
        user_repo
            .create(&make_user("owner_pb_delete_media_fk"))
            .await,
        "owner should be created",
    );
    let room = ok(
        room_repo
            .create(&make_room("Room PB Delete Media FK", &owner.id))
            .await,
        "room should be created",
    );

    let playlist = ok(
        playlist_repo
            .create(&Playlist {
                id: PlaylistId::new(),
                room_id: room.id,
                creator_id: Some(owner.id),
                name: "Room Delete Media Playlist".to_string(),
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
            })
            .await,
        "playlist should be created",
    );

    let media = ok(
        media_repo
            .create(&Media {
                id: MediaId::new(),
                playlist_id: Some(playlist.id),
                room_id: room.id,
                creator_id: Some(owner.id),
                name: "Room Delete Media".to_string(),
                description: String::new(),
                position: 0.0,
                source_provider: SourceProvider::DirectUrl,
                source_config: synctv_core_testing::direct_url_media_source_config(
                    "https://example.com/delete-media.mp4",
                ),
                provider_instance_name: None,
                cover_file_reference_id: None,
                thumbnail_file_reference_id: None,
                added_at: Utc::now(),
                updated_at: Utc::now(),
                version: 0,
            })
            .await,
        "media should be created",
    );

    let mut state = ok(
        playback_repo.create_or_get(&room.id).await,
        "playback state should be created",
    );
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    state.target = None;
    let _updated = ok(
        playback_repo.update(&state).await,
        "playback state should update",
    );

    let deleted = ok(
        media_repo.delete(&media.id).await,
        "repository soft-delete should succeed",
    );
    assert!(
        deleted,
        "repository soft-delete should update the active media row"
    );

    let visible_media = ok(
        media_repo.get_by_id(&media.id).await,
        "media visibility lookup should succeed",
    );
    assert!(
        visible_media.is_none(),
        "soft-deleted media should disappear from normal repository reads"
    );

    let (deleted_at, deletion_source): (Option<chrono::DateTime<Utc>>, Option<DeletionSource>) = ok(
        sqlx::query_as("SELECT deleted_at, deletion_source FROM media WHERE id = $1")
            .bind(media.id.as_i64())
            .fetch_one(&pool)
            .await,
        "media lifecycle metadata should remain queryable",
    );
    assert!(deleted_at.is_some());
    assert_eq!(deletion_source, Some(DeletionSource::User));

    let state_after_delete = some(
        ok(
            playback_repo.get(&room.id).await,
            "playback state should be fetched after repository soft-delete",
        ),
        "playback state should remain for the service cleanup boundary",
    );
    assert!(state_after_delete.playing_playlist_id.is_none());
    assert_eq!(state_after_delete.playing_media_id, Some(media.id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleting_playing_playlist_is_rejected_while_playback_references_it() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = ok(
        user_repo
            .create(&make_user("owner_pb_delete_playlist_fk"))
            .await,
        "owner should be created",
    );
    let room = ok(
        room_repo
            .create(&make_room("Room PB Delete Playlist FK", &owner.id))
            .await,
        "room should be created",
    );
    ok(
        member_repo
            .add(&RoomMember::new(room.id, owner.id, RoomRole::Creator))
            .await,
        "owner membership should be created",
    );

    let playlist = ok(
        playlist_repo
            .create(&Playlist {
                id: PlaylistId::new(),
                room_id: room.id,
                creator_id: Some(owner.id),
                name: "Room Delete Playlist".to_string(),
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
            })
            .await,
        "playlist should be created",
    );

    let _media = ok(
        media_repo
            .create(&Media {
                id: MediaId::new(),
                playlist_id: Some(playlist.id),
                room_id: room.id,
                creator_id: Some(owner.id),
                name: "Room Playlist Media".to_string(),
                description: String::new(),
                position: 0.0,
                source_provider: SourceProvider::DirectUrl,
                source_config: synctv_core_testing::direct_url_media_source_config(
                    "https://example.com/delete-playlist.mp4",
                ),
                provider_instance_name: None,
                cover_file_reference_id: None,
                thumbnail_file_reference_id: None,
                added_at: Utc::now(),
                updated_at: Utc::now(),
                version: 0,
            })
            .await,
        "media should be created",
    );

    let mut state = ok(
        playback_repo.create_or_get(&room.id).await,
        "playback state should be created",
    );
    state.playing_media_id = None;
    state.playing_playlist_id = Some(playlist.id);
    state.target = Some(ProviderTarget::alist("/currently-playing.mp4".to_string()));
    let _updated = ok(
        playback_repo.update(&state).await,
        "playback state should update",
    );

    let room_service = create_test_room_service(pool.clone());
    let delete_result = room_service
        .delete_entries(
            room.id,
            owner.id,
            DeleteEntriesRequest {
                playlist_ids: vec![playlist.id],
                media_ids: Vec::new(),
                force: false,
            },
        )
        .await;
    assert!(
        delete_result.is_err(),
        "deleting current playlist must be rejected until playback state is explicitly cleared"
    );

    let state_after_delete = some(
        ok(
            playback_repo.get(&room.id).await,
            "playback state should be fetched after delete attempt",
        ),
        "playback state should exist after delete attempt",
    );
    assert_eq!(state_after_delete.playing_playlist_id, Some(playlist.id));
    assert!(state_after_delete.playing_media_id.is_none());
}
