use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        Media, MediaId, PlaybackDurationStatus, PlaybackSourceIdentity, Playlist, PlaylistId,
        ProviderTarget, Room, RoomId, SourceProvider, User, UserId, UserRole, UserStatus,
    },
    repository::{
        MediaRepository, PlaybackSourceMetadataRepository, PlaylistRepository,
        RoomPlaybackStateRepository, UserRepository,
    },
    service::{
        ActivePlaybackRoomSource, BruteForceProtection, InMemoryTokenBlacklistStore, JwtService,
        PlaybackDurationProbeService, RoomService, UserService,
    },
};
use synctv_core_testing::{create_test_pool, TestOptionExt, TestResultExt};

#[derive(Clone)]
struct StaticActiveRoomSource {
    room_ids: Vec<RoomId>,
}

#[async_trait::async_trait]
impl ActivePlaybackRoomSource for StaticActiveRoomSource {
    async fn active_room_ids(&self) -> synctv_core::Result<Vec<RoomId>> {
        Ok(self.room_ids.clone())
    }
}

fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).checked("JWT service should be created");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);
    RoomService::new_for_tests(pool, user_service).checked("room service should build")
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

async fn create_media(pool: &PgPool, room_id: RoomId, owner_id: UserId, name: &str) -> Media {
    create_media_with_source_config(
        pool,
        room_id,
        owner_id,
        name,
        synctv_core_testing::direct_url_media_source_config("https://example.com/video.mp4"),
    )
    .await
}

async fn create_media_with_source_config(
    pool: &PgPool,
    room_id: RoomId,
    owner_id: UserId,
    name: &str,
    source_config: synctv_core::models::MediaSourceConfig,
) -> Media {
    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id,
        creator_id: Some(owner_id),
        name: name.to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: SourceProvider::DirectUrl,
        source_config,
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    MediaRepository::new(pool.clone())
        .create(&media)
        .await
        .checked("test media should be created")
}

async fn create_room_with_media_source_config(
    pool: &PgPool,
    room_service: &RoomService,
    owner_id: UserId,
    room_name: &str,
    media_name: &str,
    source_config: synctv_core::models::MediaSourceConfig,
) -> (Room, Media, synctv_core::models::RoomPlaybackState) {
    let room = room_service
        .create_room(room_name.to_string(), String::new(), owner_id, None, None)
        .await
        .checked("test room should be created");
    let media =
        create_media_with_source_config(pool, room.id, owner_id, media_name, source_config).await;
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("playback state should exist");
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    state.target = None;
    state.position = 0.0;
    state.is_playing = true;
    let state = playback_repo
        .update(&state)
        .await
        .checked("playback state should update");

    (room, media, state)
}

async fn create_room_with_media(
    pool: &PgPool,
    room_service: &RoomService,
    owner_id: UserId,
    room_name: &str,
    media_name: &str,
) -> (Room, Media, synctv_core::models::RoomPlaybackState) {
    let room = room_service
        .create_room(room_name.to_string(), String::new(), owner_id, None, None)
        .await
        .checked("test room should be created");
    let media = create_media(pool, room.id, owner_id, media_name).await;
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("playback state should exist");
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    state.target = None;
    state.position = 0.0;
    state.is_playing = true;
    let state = playback_repo
        .update(&state)
        .await
        .checked("playback state should update");

    (room, media, state)
}

fn alist_target(relative_path: &str) -> ProviderTarget {
    ProviderTarget::alist(relative_path.to_string())
}

async fn create_dynamic_playlist(
    pool: &PgPool,
    room_id: RoomId,
    owner_id: UserId,
    name: &str,
) -> Playlist {
    let playlist = Playlist {
        id: PlaylistId::new(),
        room_id,
        creator_id: Some(owner_id),
        browse_access_mode: synctv_core::models::PlaylistBrowseAccessMode::Default,
        name: name.to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: 0.0,
        source_provider: Some(SourceProvider::Alist),
        source_config: Some(synctv_core_testing::alist_directory_playlist_source_config(
            "alist", "/",
        )),
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };

    PlaylistRepository::new(pool.clone())
        .create(&playlist)
        .await
        .checked("dynamic playlist should be created")
}

async fn create_room_with_dynamic_playlist(
    pool: &PgPool,
    room_service: &RoomService,
    owner_id: UserId,
    room_name: &str,
    playlist_name: &str,
    target_path: &str,
) -> (Room, Playlist, synctv_core::models::RoomPlaybackState) {
    let room = room_service
        .create_room(room_name.to_string(), String::new(), owner_id, None, None)
        .await
        .checked("test room should be created");
    let playlist = create_dynamic_playlist(pool, room.id, owner_id, playlist_name).await;
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("playback state should exist");
    state.playing_media_id = None;
    state.playing_playlist_id = Some(playlist.id);
    state.target = Some(alist_target(target_path));
    state.position = 0.0;
    state.is_playing = true;
    let state = playback_repo
        .update(&state)
        .await
        .checked("playback state should update");

    (room, playlist, state)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn claim_duration_probe_for_active_source_claims_only_current_source() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let metadata_repo = PlaybackSourceMetadataRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("duration_probe_owner"))
        .await
        .checked("test owner should be created");
    let room = room_service
        .create_room(
            "Duration Probe Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test room should be created");
    let active_media = create_media(&pool, room.id, owner.id, "Active Media").await;
    let inactive_media = create_media(&pool, room.id, owner.id, "Inactive Media").await;

    let mut state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("playback state should exist");
    state.playing_media_id = Some(active_media.id);
    state.playing_playlist_id = None;
    state.target = None;
    state.position = 0.0;
    state.is_playing = true;
    let state = playback_repo
        .update(&state)
        .await
        .checked("playback state should update");
    let active_identity = PlaybackSourceIdentity::from_state(&state)
        .checked("source identity hash should compute")
        .checked("active source identity should exist");
    let inactive_identity = PlaybackSourceIdentity::static_media(room.id, inactive_media.id);
    metadata_repo
        .mark_probeable_unknown_if_absent(&active_identity)
        .await
        .checked("active metadata should be inserted");
    metadata_repo
        .mark_probeable_unknown_if_absent(&inactive_identity)
        .await
        .checked("inactive metadata should be inserted");

    let skipped = metadata_repo
        .claim_duration_probe_for_active_source(&inactive_identity)
        .await
        .checked("inactive source claim should run");
    assert!(skipped.is_none());

    let claimed = metadata_repo
        .claim_duration_probe_for_active_source(&active_identity)
        .await
        .checked("active source claim should run")
        .checked("active source should be claimed");
    assert_eq!(claimed.metadata.room_id, room.id);
    assert_eq!(claimed.metadata.media_id, Some(active_media.id));
    assert_eq!(
        claimed.metadata.duration_status,
        PlaybackDurationStatus::Pending
    );
    assert!(claimed.metadata.next_retry_at.is_some());
    assert_eq!(claimed.state.room_id, room.id);
    assert_eq!(claimed.state.playing_media_id, Some(active_media.id));

    let duplicate = metadata_repo
        .claim_duration_probe_for_active_source(&active_identity)
        .await
        .checked("duplicate claim should run");
    assert!(duplicate.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn room_scoped_duration_probe_claims_only_active_rooms() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let metadata_repo = PlaybackSourceMetadataRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_scoped_duration_probe_owner"))
        .await
        .checked("test owner should be created");
    let (active_room, active_media, active_state) = Box::pin(create_room_with_media(
        &pool,
        &room_service,
        owner.id,
        "Active Probe Room",
        "Active Probe Media",
    ))
    .await;
    let (inactive_room, inactive_media, inactive_state) = Box::pin(create_room_with_media(
        &pool,
        &room_service,
        owner.id,
        "Inactive Probe Room",
        "Inactive Probe Media",
    ))
    .await;

    let active_identity = PlaybackSourceIdentity::from_state(&active_state)
        .checked("source identity hash should compute")
        .checked("active source identity should exist");
    let inactive_identity = PlaybackSourceIdentity::from_state(&inactive_state)
        .checked("source identity hash should compute")
        .checked("inactive source identity should exist");
    metadata_repo
        .mark_probeable_unknown_if_absent(&active_identity)
        .await
        .checked("active metadata should be inserted");
    metadata_repo
        .mark_probeable_unknown_if_absent(&inactive_identity)
        .await
        .checked("inactive metadata should be inserted");

    let empty_claims = metadata_repo
        .claim_duration_probe_batch_for_rooms(&[], 10)
        .await
        .checked("empty room scoped claim should run");
    assert!(empty_claims.is_empty());

    let claims = metadata_repo
        .claim_duration_probe_batch_for_rooms(&[active_room.id], 10)
        .await
        .checked("room scoped claim should run");
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].metadata.room_id, active_room.id);
    assert_eq!(claims[0].metadata.media_id, Some(active_media.id));
    assert_eq!(claims[0].state.room_id, active_room.id);

    let inactive_metadata = metadata_repo
        .get(&inactive_identity)
        .await
        .checked("inactive metadata should fetch")
        .checked("inactive metadata should exist");
    assert_eq!(inactive_metadata.room_id, inactive_room.id);
    assert_eq!(inactive_metadata.media_id, Some(inactive_media.id));
    assert_eq!(
        inactive_metadata.duration_status,
        PlaybackDurationStatus::Unknown
    );

    let duplicate_claims = metadata_repo
        .claim_duration_probe_batch_for_rooms(&[active_room.id], 10)
        .await
        .checked("duplicate room scoped claim should run");
    assert!(duplicate_claims.is_empty());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn room_scoped_duration_probe_skips_live_sources() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let metadata_repo = PlaybackSourceMetadataRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_scoped_live_duration_probe_owner"))
        .await
        .checked("test owner should be created");
    let (room, live_media, live_state) = Box::pin(create_room_with_media(
        &pool,
        &room_service,
        owner.id,
        "Live Probe Room",
        "Live Probe Media",
    ))
    .await;
    let live_identity = PlaybackSourceIdentity::from_state(&live_state)
        .checked("source identity hash should compute")
        .checked("live source identity should exist");

    metadata_repo
        .upsert_provider_source_metadata(
            &live_identity,
            synctv_core::models::PlaybackKind::Live,
            None,
            None,
            None,
        )
        .await
        .checked("live metadata should be inserted");

    let claims = metadata_repo
        .claim_duration_probe_batch_for_rooms(&[room.id], 10)
        .await
        .checked("room scoped live claim should run");
    assert!(claims.is_empty());

    let direct_claim = metadata_repo
        .claim_duration_probe_for_active_source(&live_identity)
        .await
        .checked("active live source claim should run");
    assert!(direct_claim.is_none());

    let live_metadata = metadata_repo
        .get(&live_identity)
        .await
        .checked("live metadata should fetch")
        .checked("live metadata should exist");
    assert_eq!(live_metadata.room_id, room.id);
    assert_eq!(live_metadata.media_id, Some(live_media.id));
    assert_eq!(
        live_metadata.playback_kind,
        synctv_core::models::PlaybackKind::Live
    );
    assert_eq!(live_metadata.duration_seconds, None);
    assert_eq!(
        live_metadata.duration_status,
        PlaybackDurationStatus::Unavailable
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn duration_probe_initializes_plain_direct_url_as_probeable() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let metadata_repo = PlaybackSourceMetadataRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("duration_probe_direct_url_owner"))
        .await
        .checked("test owner should be created");
    let (room, _media, state) = Box::pin(create_room_with_media_source_config(
        &pool,
        &room_service,
        owner.id,
        "DirectUrl Probe Room",
        "DirectUrl Probe Media",
        synctv_core_testing::direct_url_media_source_config("http://127.0.0.1/video.mp4"),
    ))
    .await;
    let identity = PlaybackSourceIdentity::from_state(&state)
        .checked("source identity hash should compute")
        .checked("source identity should exist");

    let probe_service = PlaybackDurationProbeService::new(
        room_service.playback_service().clone(),
        synctv_common::ssrf::SsrfGuard::strict_policy(),
    )
    .with_active_room_source(Arc::new(StaticActiveRoomSource {
        room_ids: vec![room.id],
    }));

    let probed = probe_service
        .run_once()
        .await
        .checked("duration probe should run");
    assert_eq!(probed, 0);

    let metadata = metadata_repo
        .get(&identity)
        .await
        .checked("metadata lookup should run")
        .checked("plain direct url should get metadata initialized");
    assert_eq!(
        metadata.playback_kind,
        synctv_core::models::PlaybackKind::Regular
    );
    assert_eq!(metadata.duration_seconds, None);
    assert!(
        matches!(
            metadata.duration_status,
            PlaybackDurationStatus::Pending | PlaybackDurationStatus::Failed
        ),
        "metadata should be claimable or record the attempted probe, got {:?}",
        metadata.duration_status
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn room_scoped_duration_probe_claims_dynamic_playlist_current_targets_only() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let metadata_repo = PlaybackSourceMetadataRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_scoped_dynamic_duration_probe_owner"))
        .await
        .checked("test owner should be created");
    let (active_room, active_playlist, active_state) = Box::pin(create_room_with_dynamic_playlist(
        &pool,
        &room_service,
        owner.id,
        "Active Dynamic Probe Room",
        "Active Dynamic Probe Playlist",
        "/episode-1.mp4",
    ))
    .await;
    let (inactive_room, inactive_playlist, inactive_state) =
        Box::pin(create_room_with_dynamic_playlist(
            &pool,
            &room_service,
            owner.id,
            "Inactive Dynamic Probe Room",
            "Inactive Dynamic Probe Playlist",
            "/episode-1.mp4",
        ))
        .await;

    let active_identity = PlaybackSourceIdentity::from_state(&active_state)
        .checked("source identity hash should compute")
        .checked("active dynamic source identity should exist");
    let inactive_identity = PlaybackSourceIdentity::from_state(&inactive_state)
        .checked("source identity hash should compute")
        .checked("inactive dynamic source identity should exist");
    let stale_identity = PlaybackSourceIdentity::dynamic_playlist(
        active_room.id,
        active_playlist.id,
        &alist_target("/episode-0.mp4"),
    )
    .checked("stale dynamic source identity hash should compute");

    metadata_repo
        .mark_probeable_unknown_if_absent(&active_identity)
        .await
        .checked("active dynamic metadata should be inserted");
    metadata_repo
        .mark_probeable_unknown_if_absent(&inactive_identity)
        .await
        .checked("inactive dynamic metadata should be inserted");
    metadata_repo
        .mark_probeable_unknown_if_absent(&stale_identity)
        .await
        .checked("stale dynamic metadata should be inserted");

    let claims = metadata_repo
        .claim_duration_probe_batch_for_rooms(&[active_room.id], 10)
        .await
        .checked("room scoped dynamic claim should run");
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].metadata.room_id, active_room.id);
    assert_eq!(claims[0].metadata.media_id, None);
    assert_eq!(claims[0].metadata.playlist_id, Some(active_playlist.id));
    assert_eq!(claims[0].metadata.target_hash, active_identity.target_hash);
    assert_eq!(claims[0].state.playing_media_id, None);
    assert_eq!(
        claims[0].state.playing_playlist_id,
        Some(active_playlist.id)
    );
    assert_eq!(claims[0].state.target, Some(alist_target("/episode-1.mp4")));

    let inactive_metadata = metadata_repo
        .get(&inactive_identity)
        .await
        .checked("inactive dynamic metadata should fetch")
        .checked("inactive dynamic metadata should exist");
    assert_eq!(inactive_metadata.room_id, inactive_room.id);
    assert_eq!(inactive_metadata.media_id, None);
    assert_eq!(inactive_metadata.playlist_id, Some(inactive_playlist.id));
    assert_eq!(
        inactive_metadata.duration_status,
        PlaybackDurationStatus::Unknown
    );

    let stale_metadata = metadata_repo
        .get(&stale_identity)
        .await
        .checked("stale dynamic metadata should fetch")
        .checked("stale dynamic metadata should exist");
    assert_eq!(stale_metadata.room_id, active_room.id);
    assert_eq!(stale_metadata.media_id, None);
    assert_eq!(stale_metadata.playlist_id, Some(active_playlist.id));
    assert_eq!(
        stale_metadata.duration_status,
        PlaybackDurationStatus::Unknown
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn room_scoped_auto_advance_candidates_only_include_active_rooms() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let metadata_repo = PlaybackSourceMetadataRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_scoped_auto_advance_owner"))
        .await
        .checked("test owner should be created");
    let (active_room, active_media, active_state) = Box::pin(create_room_with_media(
        &pool,
        &room_service,
        owner.id,
        "Active Auto Advance Room",
        "Active Auto Advance Media",
    ))
    .await;
    let (inactive_room, inactive_media, inactive_state) = Box::pin(create_room_with_media(
        &pool,
        &room_service,
        owner.id,
        "Inactive Auto Advance Room",
        "Inactive Auto Advance Media",
    ))
    .await;

    let active_identity = PlaybackSourceIdentity::from_state(&active_state)
        .checked("source identity hash should compute")
        .checked("active source identity should exist");
    let inactive_identity = PlaybackSourceIdentity::from_state(&inactive_state)
        .checked("source identity hash should compute")
        .checked("inactive source identity should exist");
    metadata_repo
        .upsert_provider_source_metadata(
            &active_identity,
            synctv_core::models::PlaybackKind::Regular,
            Some(30.0),
            None,
            None,
        )
        .await
        .checked("active duration should be inserted");
    metadata_repo
        .upsert_provider_source_metadata(
            &inactive_identity,
            synctv_core::models::PlaybackKind::Regular,
            Some(30.0),
            None,
            None,
        )
        .await
        .checked("inactive duration should be inserted");

    let empty_candidates = metadata_repo
        .list_active_finite_sources_for_rooms(&[], 10)
        .await
        .checked("empty room scoped candidate query should run");
    assert!(empty_candidates.is_empty());

    let candidates = metadata_repo
        .list_active_finite_sources_for_rooms(&[active_room.id], 10)
        .await
        .checked("room scoped candidate query should run");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].0.room_id, active_room.id);
    assert_eq!(candidates[0].0.media_id, Some(active_media.id));
    assert_eq!(candidates[0].1.room_id, active_room.id);

    let inactive_metadata = metadata_repo
        .get(&inactive_identity)
        .await
        .checked("inactive metadata should fetch")
        .checked("inactive metadata should exist");
    assert_eq!(inactive_metadata.room_id, inactive_room.id);
    assert_eq!(inactive_metadata.media_id, Some(inactive_media.id));
    assert_eq!(
        inactive_metadata.duration_status,
        PlaybackDurationStatus::Available
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn room_scoped_auto_advance_candidates_skip_paused_sources() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let metadata_repo = PlaybackSourceMetadataRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("paused_auto_advance_owner"))
        .await
        .checked("test owner should be created");
    let (room, _media, mut state) = Box::pin(create_room_with_media(
        &pool,
        &room_service,
        owner.id,
        "Paused Auto Advance Room",
        "Paused Auto Advance Media",
    ))
    .await;

    let identity = PlaybackSourceIdentity::from_state(&state)
        .checked("source identity hash should compute")
        .checked("source identity should exist");
    metadata_repo
        .upsert_provider_source_metadata(
            &identity,
            synctv_core::models::PlaybackKind::Regular,
            Some(30.0),
            None,
            None,
        )
        .await
        .checked("duration should be inserted");

    let playing_candidates = metadata_repo
        .list_active_finite_sources_for_rooms(&[room.id], 10)
        .await
        .checked("playing candidate query should run");
    assert_eq!(playing_candidates.len(), 1);

    state.is_playing = false;
    state.position = 30.0;
    playback_repo
        .update(&state)
        .await
        .checked("playback state should pause");

    let paused_candidates = metadata_repo
        .list_active_finite_sources_for_rooms(&[room.id], 10)
        .await
        .checked("paused candidate query should run");
    assert!(
        paused_candidates.is_empty(),
        "paused finite sources should stay out of auto-advance scans"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn room_scoped_auto_advance_candidates_include_dynamic_playlist_current_targets_only() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let metadata_repo = PlaybackSourceMetadataRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_scoped_dynamic_auto_advance_owner"))
        .await
        .checked("test owner should be created");
    let (active_room, active_playlist, active_state) = Box::pin(create_room_with_dynamic_playlist(
        &pool,
        &room_service,
        owner.id,
        "Active Dynamic Auto Advance Room",
        "Active Dynamic Auto Advance Playlist",
        "/episode-1.mp4",
    ))
    .await;
    let (inactive_room, inactive_playlist, inactive_state) =
        Box::pin(create_room_with_dynamic_playlist(
            &pool,
            &room_service,
            owner.id,
            "Inactive Dynamic Auto Advance Room",
            "Inactive Dynamic Auto Advance Playlist",
            "/episode-1.mp4",
        ))
        .await;

    let active_identity = PlaybackSourceIdentity::from_state(&active_state)
        .checked("source identity hash should compute")
        .checked("active dynamic source identity should exist");
    let inactive_identity = PlaybackSourceIdentity::from_state(&inactive_state)
        .checked("source identity hash should compute")
        .checked("inactive dynamic source identity should exist");
    let stale_identity = PlaybackSourceIdentity::dynamic_playlist(
        active_room.id,
        active_playlist.id,
        &alist_target("/episode-0.mp4"),
    )
    .checked("stale dynamic source identity hash should compute");

    metadata_repo
        .upsert_provider_source_metadata(
            &active_identity,
            synctv_core::models::PlaybackKind::Regular,
            Some(30.0),
            None,
            None,
        )
        .await
        .checked("active dynamic duration should be inserted");
    metadata_repo
        .upsert_provider_source_metadata(
            &inactive_identity,
            synctv_core::models::PlaybackKind::Regular,
            Some(30.0),
            None,
            None,
        )
        .await
        .checked("inactive dynamic duration should be inserted");
    metadata_repo
        .upsert_provider_source_metadata(
            &stale_identity,
            synctv_core::models::PlaybackKind::Regular,
            Some(30.0),
            None,
            None,
        )
        .await
        .checked("stale dynamic duration should be inserted");

    let candidates = metadata_repo
        .list_active_finite_sources_for_rooms(&[active_room.id], 10)
        .await
        .checked("room scoped dynamic candidate query should run");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].0.room_id, active_room.id);
    assert_eq!(candidates[0].0.media_id, None);
    assert_eq!(candidates[0].0.playlist_id, Some(active_playlist.id));
    assert_eq!(candidates[0].0.target_hash, active_identity.target_hash);
    assert_eq!(candidates[0].1.playing_media_id, None);
    assert_eq!(
        candidates[0].1.playing_playlist_id,
        Some(active_playlist.id)
    );
    assert_eq!(candidates[0].1.target, Some(alist_target("/episode-1.mp4")));

    let inactive_metadata = metadata_repo
        .get(&inactive_identity)
        .await
        .checked("inactive dynamic metadata should fetch")
        .checked("inactive dynamic metadata should exist");
    assert_eq!(inactive_metadata.room_id, inactive_room.id);
    assert_eq!(inactive_metadata.media_id, None);
    assert_eq!(inactive_metadata.playlist_id, Some(inactive_playlist.id));
    assert_eq!(
        inactive_metadata.duration_status,
        PlaybackDurationStatus::Available
    );

    let stale_metadata = metadata_repo
        .get(&stale_identity)
        .await
        .checked("stale dynamic metadata should fetch")
        .checked("stale dynamic metadata should exist");
    assert_eq!(stale_metadata.room_id, active_room.id);
    assert_eq!(stale_metadata.media_id, None);
    assert_eq!(stale_metadata.playlist_id, Some(active_playlist.id));
    assert_eq!(
        stale_metadata.duration_status,
        PlaybackDurationStatus::Available
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn duration_probe_service_skips_initialization_without_active_rooms() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let metadata_repo = PlaybackSourceMetadataRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("duration_probe_empty_active_owner"))
        .await
        .checked("test owner should be created");
    let (_room, _media, state) = Box::pin(create_room_with_media(
        &pool,
        &room_service,
        owner.id,
        "Empty Active Probe Room",
        "Empty Active Probe Media",
    ))
    .await;
    let identity = PlaybackSourceIdentity::from_state(&state)
        .checked("source identity hash should compute")
        .checked("source identity should exist");

    let probe_service = PlaybackDurationProbeService::new(
        room_service.playback_service().clone(),
        synctv_common::ssrf::SsrfGuard::strict_policy(),
    )
    .with_active_room_source(Arc::new(StaticActiveRoomSource {
        room_ids: Vec::new(),
    }));

    let probed = probe_service
        .run_once()
        .await
        .checked("duration probe should run");
    assert_eq!(probed, 0);
    assert!(
        metadata_repo
            .get(&identity)
            .await
            .checked("metadata lookup should run")
            .is_none(),
        "inactive rooms should not get duration metadata initialized"
    );
}
