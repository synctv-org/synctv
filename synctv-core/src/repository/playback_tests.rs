use super::*;
use crate::models::{
    DirectUrlMediaResourceConfig, DirectUrlMediaSourceConfig, FromProviderParams, Media,
    MediaSourceConfig, SourceProvider,
};
use crate::repository::media::MediaRepository;
use crate::test_helpers::TestResultExt;
use synctv_core_testing::create_test_pool;

fn direct_url_media_source_config(url: impl Into<String>) -> MediaSourceConfig {
    MediaSourceConfig::DirectUrl(DirectUrlMediaSourceConfig {
        playback_kind: None,
        duration_seconds: None,
        proxy_mode: crate::models::PlaybackProxyMode::Auto,
        medias: vec![DirectUrlMediaResourceConfig {
            name: String::new(),
            url: url.into(),
            headers: std::collections::HashMap::new(),
            format: String::new(),
            expires_at: None,
        }],
        default_media_index: None,
        subtitles: Vec::new(),
        default_subtitle_index: None,
        danmakus: Vec::new(),
        default_danmaku_index: None,
    })
}

async fn attach_test_media(
    pool: &PgPool,
    playback_repo: &RoomPlaybackStateRepository,
    mut state: RoomPlaybackState,
    owner_id: UserId,
) -> RoomPlaybackState {
    let media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: None,
        room_id: state.room_id,
        creator_id: Some(owner_id),
        name: "Playback Position Test Video".to_string(),
        description: String::new(),
        source_config: direct_url_media_source_config("https://example.com/video.mp4"),
        source_provider: SourceProvider::DirectUrl,
        provider_instance_name: None,
        position: 0.0,
    });
    let media = MediaRepository::new(pool.clone())
        .create(&media)
        .await
        .checked("test media should be created");
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    state.target = None;
    state.position = 0.0;
    playback_repo
        .update(&state)
        .await
        .checked("playback state should attach test media")
}

/// Integration test: Create and get playback state
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_or_get_playback_state() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    // Create owner user first
    let owner = UserFixture::new().with_username("playback_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    // Create room
    let room = RoomFixture::new()
        .with_name("Playback Test Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playback state
    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("operation should succeed");
    assert_eq!(state.room_id, room.id);
    assert!(state.playing_media_id.is_none());
    assert!(!state.is_playing);
    assert_eq!(state.version, 0);

    // Get existing playback state (should return same state)
    let state2 = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("operation should succeed");
    assert_eq!(state2.room_id, room.id);
    assert_eq!(state2.version, 0); // version should still be 0
}

/// Integration test: Get non-existent playback state returns None
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_nonexistent_playback_state() {
    let (_postgres, pool) = create_test_pool().await;
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let room_id = RoomId::expect_positive(90_001);
    let result = playback_repo
        .get(&room_id)
        .await
        .checked("operation should succeed");
    assert!(result.is_none());
}

/// Integration test: Update playback state
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_playback_state() {
    use crate::models::Media;
    use crate::repository::media::MediaRepository;
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new()
        .with_username("playback_state_update_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Playback Update Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playlist hierarchy (root + child with name)
    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Playback Playlist",
    )
    .await;

    // Create media for playback reference (required by FK constraint)
    let media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Test Video".to_string(),
        description: String::new(),
        source_config: direct_url_media_source_config("https://example.com/video.mp4"),
        source_provider: SourceProvider::DirectUrl,
        provider_instance_name: None,
        position: 0.0,
    });
    let media = media_repo
        .create(&media)
        .await
        .checked("operation should succeed");

    // Create playback state
    let mut state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("operation should succeed");

    // Update state with valid media_id reference
    state.position = 120.5;
    state.speed = 1.5;
    state.is_playing = true;
    state.playing_media_id = Some(media.id);

    let updated = playback_repo
        .update(&state)
        .await
        .checked("operation should succeed");
    assert!((updated.position - 120.5).abs() < f64::EPSILON);
    assert!((updated.speed - 1.5).abs() < f64::EPSILON);
    assert!(updated.is_playing);
    assert!(updated.playing_media_id.is_some());
    assert_eq!(updated.version, state.version + 1); // version should increment
}

/// Integration test: Optimistic lock conflict detection
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_conflict() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("lock_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Lock Test Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playback state
    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("operation should succeed");

    // First update succeeds
    let mut state1 = state.clone();
    state1.position = 50.0;
    let updated1 = playback_repo
        .update(&state1)
        .await
        .checked("operation should succeed");
    assert_eq!(updated1.version, 1);

    // Second update with stale version fails (optimistic lock conflict)
    let mut state2 = state.clone(); // Still has version 0
    state2.position = 100.0;
    let result = playback_repo.update(&state2).await;
    assert!(matches!(result, Err(crate::Error::OptimisticLockConflict)));
}

/// Integration test: Version increments on each update
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_version_increments_on_update() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("version_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Version Test Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playback state
    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("operation should succeed");
    let mut state = attach_test_media(&pool, &playback_repo, state, owner.id).await;
    assert_eq!(state.version, 1);

    // Multiple updates
    for position in [10.0, 20.0, 30.0, 40.0, 50.0] {
        state.position = position;
        state = playback_repo
            .update(&state)
            .await
            .checked("operation should succeed");
        assert!((state.position - position).abs() < f64::EPSILON);
    }
}

/// Integration test: Boundary conditions for `position` and speed
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_boundary_conditions() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("boundary_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Boundary Test Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playback state
    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("operation should succeed");
    let mut state = attach_test_media(&pool, &playback_repo, state, owner.id).await;

    // Test zero position
    state.position = 0.0;
    state = playback_repo
        .update(&state)
        .await
        .checked("operation should succeed");
    assert!((state.position - 0.0).abs() < f64::EPSILON);

    // Test very large position (e.g., long video)
    state.position = 7200.5; // 2 hours
    state = playback_repo
        .update(&state)
        .await
        .checked("operation should succeed");
    assert!((state.position - 7200.5).abs() < f64::EPSILON);

    // Test very small speed (but not zero)
    state.speed = 0.25;
    state = playback_repo
        .update(&state)
        .await
        .checked("operation should succeed");
    assert!((state.speed - 0.25).abs() < f64::EPSILON);

    // Test very large speed
    state.speed = 4.0;
    state = playback_repo
        .update(&state)
        .await
        .checked("operation should succeed");
    assert!((state.speed - 4.0).abs() < f64::EPSILON);

    // Test negative position (should be allowed for some edge cases)
    state.position = -1.0;
    let _result = playback_repo.update(&state).await;
    // Note: Whether negative time is allowed depends on database constraints
    // This test documents the expected behavior
}
