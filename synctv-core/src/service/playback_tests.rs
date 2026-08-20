use super::*;
use crate::cache::{
    CacheInvalidationService, CacheL2Backend, InvalidationMessage, KeyBuilder, UsernameCache,
};
use crate::models::{ProviderTarget, RoomId, SignupMethod, User, UserRole, UserStatus};
use crate::repository::{
    MediaRepository, PlaylistRepository, ProviderInstanceRepository, RoomPlaybackStateRepository,
    RoomRepository,
};
use crate::service::PermissionService;
use crate::service::{
    BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, MediaService,
    NotificationService, ProvidersManager, RemoteProviderManager, UserService,
};
use async_trait::async_trait;
use sqlx::PgPool;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
    match result {
        Ok(_) => std::panic::panic_any(context.to_string()),
        Err(error) => error,
    }
}

fn some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => std::panic::panic_any(context.to_string()),
    }
}

fn joined<T>(result: std::result::Result<T, tokio::task::JoinError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

#[test]
fn client_time_converts_positive_transport_delay_to_elapsed_seconds() {
    let received_at = crate::clock::utc_from_millis(10_000).expect("valid timestamp");
    let elapsed = ok(
        PlaybackService::client_elapsed_seconds(received_at, Some(9_500)),
        "client time should be accepted",
    );

    assert!((elapsed - 0.5).abs() < f64::EPSILON);
    assert!(
        (PlaybackService::compensate_client_position(42.0, true, 2.0, elapsed) - 43.0).abs()
            < f64::EPSILON
    );
    assert_eq!(
        PlaybackService::compensate_client_position(42.0, false, 2.0, elapsed),
        42.0
    );
}

#[test]
fn client_time_rejects_excessive_clock_difference() {
    let received_at = crate::clock::utc_from_millis(100_000).expect("valid timestamp");
    let error = err(
        PlaybackService::client_elapsed_seconds(received_at, Some(69_999)),
        "client time beyond the allowed skew should fail",
    );

    assert!(matches!(error, Error::InvalidInput(_)));
}

#[test]
fn missing_client_time_has_zero_transport_compensation() {
    let received_at = crate::clock::utc_from_millis(10_000).expect("valid timestamp");
    let elapsed = ok(
        PlaybackService::client_elapsed_seconds(received_at, None),
        "missing optional client time should be accepted",
    );

    assert_eq!(elapsed, 0.0);
}

#[test]
fn provider_targets_expose_deterministic_live_status_for_chat_metadata() {
    assert_eq!(
        live_status_for_target(&ProviderTarget::bilibili_live(123)),
        Some(true)
    );
    assert_eq!(
        live_status_for_target(&ProviderTarget::twitch(
            TwitchTargetKind::Live,
            "channel".to_string(),
        )),
        Some(true)
    );
    assert_eq!(
        live_status_for_target(&ProviderTarget::twitch(
            TwitchTargetKind::Video,
            "123".to_string(),
        )),
        Some(false)
    );
    assert_eq!(
        live_status_for_target(&ProviderTarget::youtube("video".to_string())),
        None
    );
}

#[test]
fn dynamic_playlist_configs_expose_known_live_status_for_chat_metadata() {
    let youtube_live = crate::models::PlaylistSourceConfig::Youtube(
        crate::models::YoutubePlaylistSourceConfig::Channel {
            channel_id: "channel".to_string(),
            content: crate::models::YoutubeChannelContent::Live,
            shared: false,
        },
    );
    let youtube_videos = crate::models::PlaylistSourceConfig::Youtube(
        crate::models::YoutubePlaylistSourceConfig::Channel {
            channel_id: "channel".to_string(),
            content: crate::models::YoutubeChannelContent::Videos,
            shared: false,
        },
    );
    let douyin_videos =
        crate::models::PlaylistSourceConfig::Douyin(crate::models::DouyinPlaylistSourceConfig {
            sec_uid: "creator".to_string(),
            shared: false,
        });

    assert_eq!(
        live_status_for_playlist_source(SourceProvider::Youtube, &youtube_live),
        Some(true)
    );
    assert_eq!(
        live_status_for_playlist_source(SourceProvider::Youtube, &youtube_videos),
        Some(false)
    );
    assert_eq!(
        live_status_for_playlist_source(SourceProvider::Douyin, &douyin_videos),
        Some(false)
    );
    assert_eq!(
        live_status_for_playlist_source(SourceProvider::TikTok, &douyin_videos),
        None
    );
    assert_eq!(
        ChatPlaybackMetadata::position_for_source(
            42.0,
            live_status_for_playlist_source(SourceProvider::Youtube, &youtube_live),
            None,
        ),
        None
    );
}

#[derive(Default)]
struct CountingL2Backend {
    delete_calls: AtomicUsize,
}

#[async_trait]
impl CacheL2Backend for CountingL2Backend {
    async fn get(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn set(&self, _key: &str, _json: &str, _ttl_secs: u64) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, _key: &str) -> Result<()> {
        self.delete_calls.fetch_add(1, AtomicOrdering::Relaxed);
        Ok(())
    }

    async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>> {
        Ok(vec![None; keys.len()])
    }

    async fn set_if_newer(
        &self,
        _key: &str,
        _json: &str,
        _ttl_secs: u64,
        _new_ts_millis: i64,
    ) -> Result<bool> {
        Ok(true)
    }

    async fn set_if_version_at_least(
        &self,
        _key: &str,
        _json: &str,
        _ttl_secs: u64,
        _version: i64,
    ) -> Result<bool> {
        Ok(true)
    }

    async fn delete_by_prefix(&self, _prefix: &str) -> Result<()> {
        Ok(())
    }

    fn is_active(&self) -> bool {
        true
    }
}

fn make_user_service(pool: &PgPool) -> UserService {
    let jwt_service = ok(
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!"),
        "JWT service should build",
    );
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

fn make_user(username: &str) -> User {
    let now = crate::SystemClock.now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
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

type PlaybackInvalidationRuntimeParts = (
    Arc<PlaybackInvalidationRuntime>,
    Arc<moka::future::Cache<String, RoomPlaybackState>>,
    Arc<parking_lot::RwLock<Option<PlaybackStateCache>>>,
    Arc<CacheInvalidationService>,
);

fn make_playback_invalidation_runtime_for_lifecycle_tests(
    l2_cache: Option<PlaybackStateCache>,
) -> PlaybackInvalidationRuntimeParts {
    let invalidation_service = Arc::new(CacheInvalidationService::new(
        "node-test".to_string(),
        "synctv:test:cache:invalidate".to_string(),
    ));
    let playback_cache = Arc::new(
        moka::future::CacheBuilder::new(PlaybackService::DEFAULT_CACHE_SIZE)
            .time_to_live(std::time::Duration::from_secs(
                PlaybackService::DEFAULT_CACHE_TTL_SECS,
            ))
            .build(),
    );
    (
        Arc::new(PlaybackInvalidationRuntime::new()),
        playback_cache,
        Arc::new(parking_lot::RwLock::new(l2_cache)),
        invalidation_service,
    )
}

#[tokio::test]
async fn standalone_playback_runtime_uses_local_authoritative_fence() {
    let runtime = crate::service::PlaybackServiceRuntime::local_only();
    let consistency = ConsistencyCoordinator::new(runtime.version_fence);

    assert!(
        consistency.is_authoritative(),
        "standalone playback runtime should use a local authoritative fence"
    );
}

#[tokio::test]
async fn write_playback_cache_refreshes_l1_when_l2_is_configured() {
    let playback_cache = moka::future::CacheBuilder::new(PlaybackService::DEFAULT_CACHE_SIZE)
        .time_to_live(std::time::Duration::from_secs(
            PlaybackService::DEFAULT_CACHE_TTL_SECS,
        ))
        .build();
    let l2_backend = Arc::new(CountingL2Backend::default());
    let l2_cache = PlaybackStateCache::new(
        l2_backend,
        100,
        PlaybackService::DEFAULT_CACHE_TTL_SECS,
        60,
        "test:playback:l1-refresh:".to_string(),
    );

    let room_id = RoomId::expect_positive(10_004);
    let cache_key = room_id.to_string();
    let stale_state = RoomPlaybackState {
        room_id,
        playing_media_id: None,
        playing_playlist_id: None,
        target: None,
        current_progress_id: None,
        history_cursor_id: None,
        position: 10.0,
        speed: 1.0,
        is_playing: false,
        playback_generation: 0,
        updated_at: crate::SystemClock.now(),
        version: 3,
    };
    playback_cache.insert(cache_key.clone(), stale_state).await;

    let fresh_state = RoomPlaybackState {
        room_id,
        playing_media_id: None,
        playing_playlist_id: None,
        target: None,
        current_progress_id: None,
        history_cursor_id: None,
        position: 42.0,
        speed: 1.0,
        is_playing: true,
        playback_generation: 0,
        updated_at: crate::SystemClock.now(),
        version: 4,
    };
    PlaybackService::write_playback_cache_entry(&playback_cache, Some(l2_cache), &fresh_state)
        .await;

    let cached = some(
        playback_cache.get(&cache_key).await,
        "local L1 cache should be refreshed by local write",
    );
    assert_eq!(cached.version, fresh_state.version);
    assert!((cached.position - fresh_state.position).abs() < f64::EPSILON);
    assert!(cached.is_playing);
}

#[test]
fn test_speed_validation_bounds() {
    // Valid boundary values
    assert!(validate_playback_speed_value(0.25).is_ok());
    assert!(validate_playback_speed_value(0.5).is_ok());
    assert!(validate_playback_speed_value(1.0).is_ok());
    assert!(validate_playback_speed_value(2.0).is_ok());
    assert!(validate_playback_speed_value(4.0).is_ok());

    // Invalid boundary values (below minimum)
    assert!(validate_playback_speed_value(0.0).is_err());
    assert!(validate_playback_speed_value(0.1).is_err());
    assert!(validate_playback_speed_value(0.24).is_err());
    assert!(validate_playback_speed_value(-1.0).is_err());

    // Invalid boundary values (above maximum)
    assert!(validate_playback_speed_value(4.1).is_err());
    assert!(validate_playback_speed_value(8.0).is_err());
    assert!(validate_playback_speed_value(16.0).is_err());
    assert!(validate_playback_speed_value(f64::NAN).is_err());
    assert!(validate_playback_speed_value(f64::INFINITY).is_err());
}

#[test]
fn test_update_multiple_speed_uses_standard_validation_bounds() {
    assert!(validate_playback_speed_value(0.25).is_ok());
    assert!(validate_playback_speed_value(1.0).is_ok());
    assert!(validate_playback_speed_value(4.0).is_ok());

    assert!(validate_playback_speed_value(0.0).is_err());
    assert!(validate_playback_speed_value(-1.0).is_err());
    assert!(validate_playback_speed_value(4.1).is_err());
    assert!(validate_playback_speed_value(8.0).is_err());
    assert!(validate_playback_speed_value(f64::NAN).is_err());
    assert!(validate_playback_speed_value(f64::INFINITY).is_err());
}

#[test]
fn test_seek_negative_position() {
    assert!(validate_seek_position(-1.0).is_err());
    assert!(validate_seek_position(0.0).is_ok());
    assert!(validate_seek_position(42.5).is_ok());
    assert!(validate_seek_position(MAX_PLAYBACK_POSITION_SECONDS).is_ok());
    assert!(validate_seek_position(MAX_PLAYBACK_POSITION_SECONDS + 0.1).is_err());
    assert!(validate_seek_position(f64::NAN).is_err());
    assert!(validate_seek_position(f64::INFINITY).is_err());
}

#[test]
fn test_position_update_requires_current_playback_source() {
    let mut state = RoomPlaybackState {
        room_id: RoomId::expect_positive(20_001),
        playing_media_id: None,
        playing_playlist_id: None,
        target: None,
        current_progress_id: None,
        history_cursor_id: None,
        position: 0.0,
        speed: 1.0,
        is_playing: false,
        playback_generation: 0,
        updated_at: crate::SystemClock.now(),
        version: 1,
    };

    let err = err(
        validate_position_update_source(&state),
        "position update without source should fail",
    );
    assert!(matches!(err, Error::InvalidInput(_)));

    state.playing_media_id = Some(MediaId::expect_positive(30_001));
    assert!(validate_position_update_source(&state).is_ok());

    state.playing_media_id = None;
    state.playing_playlist_id = Some(PlaylistId::expect_positive(40_001));
    state.target = Some(ProviderTarget::alist("dynamic-target".to_string()));
    assert!(validate_position_update_source(&state).is_ok());
}

#[test]
fn test_switch_target_source_shape_matches_progress_schema() {
    assert!(validate_switch_target(&SwitchPlaybackTarget {
        media_id: Some(MediaId::expect_positive(30_010)),
        playlist_id: None,
        target: None,
    })
    .is_ok());
    assert!(validate_switch_target(&SwitchPlaybackTarget {
        media_id: Some(MediaId::expect_positive(30_012)),
        playlist_id: Some(PlaylistId::expect_positive(40_012)),
        target: None,
    })
    .is_ok());
    assert!(validate_switch_target(&SwitchPlaybackTarget {
        media_id: None,
        playlist_id: Some(PlaylistId::expect_positive(40_010)),
        target: Some(ProviderTarget::alist("dynamic-target".to_string())),
    })
    .is_ok());
    assert!(validate_switch_target(&SwitchPlaybackTarget {
        media_id: Some(MediaId::expect_positive(30_011)),
        playlist_id: None,
        target: Some(ProviderTarget::alist(
            "static-media-must-have-no-target".to_string(),
        )),
    })
    .is_err());
    assert!(validate_switch_target(&SwitchPlaybackTarget {
        media_id: None,
        playlist_id: Some(PlaylistId::expect_positive(40_011)),
        target: None,
    })
    .is_err());
}

#[tokio::test]
async fn test_invalidation_listener_stops_after_cache_invalidation_service_stop() {
    let (runtime, playback_cache, l2_cache, invalidation_service) =
        make_playback_invalidation_runtime_for_lifecycle_tests(None);
    let room_id = RoomId::expect_positive(10_001);
    let cache_key = room_id.to_string();

    ok(
        runtime
            .start(
                invalidation_service.clone(),
                playback_cache.clone(),
                l2_cache.clone(),
            )
            .await,
        "playback invalidation listener should start",
    );

    assert!(
        runtime.is_started(),
        "start() must mark playback invalidation runtime as running"
    );

    runtime.shutdown().await;

    let updated_state = RoomPlaybackState {
        room_id,
        playing_media_id: None,
        playing_playlist_id: None,
        target: None,
        current_progress_id: None,
        history_cursor_id: None,
        position: 42.0,
        speed: 1.0,
        is_playing: true,
        playback_generation: 0,
        updated_at: crate::SystemClock.now(),
        version: 7,
    };

    ok(
        invalidation_service
            .broadcast_all(InvalidationMessage::PlaybackStateUpdate {
                room_id: cache_key.clone(),
                state: updated_state,
            })
            .await,
        "local invalidation broadcast should succeed",
    );
    tokio::task::yield_now().await;

    assert!(
        playback_cache.get(&cache_key).await.is_none(),
        "playback invalidation listener must stop processing local broadcasts once shutdown starts"
    );
}

#[tokio::test]
async fn test_start_can_restart_playback_invalidation_listener_after_shutdown() {
    let (runtime, playback_cache, l2_cache, invalidation_service) =
        make_playback_invalidation_runtime_for_lifecycle_tests(None);
    let room_id = RoomId::expect_positive(10_002);
    let cache_key = room_id.to_string();

    ok(
        runtime
            .start(
                invalidation_service.clone(),
                playback_cache.clone(),
                l2_cache.clone(),
            )
            .await,
        "initial playback invalidation start should succeed",
    );
    runtime.shutdown().await;

    let updated_state = RoomPlaybackState {
        room_id,
        playing_media_id: None,
        playing_playlist_id: None,
        target: None,
        current_progress_id: None,
        history_cursor_id: None,
        position: 64.0,
        speed: 1.0,
        is_playing: true,
        playback_generation: 0,
        updated_at: crate::SystemClock.now(),
        version: 9,
    };

    ok(
        runtime
            .start(
                invalidation_service.clone(),
                playback_cache.clone(),
                l2_cache.clone(),
            )
            .await,
        "restart after playback invalidation shutdown should succeed",
    );

    ok(
        invalidation_service
            .broadcast_all(InvalidationMessage::PlaybackStateUpdate {
                room_id: cache_key.clone(),
                state: updated_state,
            })
            .await,
        "local invalidation broadcast should succeed after restart",
    );
    tokio::task::yield_now().await;

    let cached = some(
        playback_cache.get(&cache_key).await,
        "restarted listener should populate cache from invalidation broadcast",
    );
    assert_eq!(cached.version, 9);

    runtime.shutdown().await;
}

#[tokio::test]
async fn test_start_activates_invalidation_listener_after_wiring_service() {
    let (runtime, playback_cache, l2_cache, invalidation_service) =
        make_playback_invalidation_runtime_for_lifecycle_tests(None);
    let room_id = RoomId::expect_positive(10_003);
    let cache_key = room_id.to_string();

    ok(
        runtime
            .start(
                invalidation_service.clone(),
                playback_cache.clone(),
                l2_cache.clone(),
            )
            .await,
        "explicit start should activate playback invalidation listener",
    );

    ok(
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !runtime.is_started() {
                tokio::task::yield_now().await;
            }
        })
        .await,
        "start() should mark playback invalidation listener as running",
    );

    let updated_state = RoomPlaybackState {
        room_id,
        playing_media_id: None,
        playing_playlist_id: None,
        target: None,
        current_progress_id: None,
        history_cursor_id: None,
        position: 88.0,
        speed: 1.0,
        is_playing: true,
        playback_generation: 0,
        updated_at: crate::SystemClock.now(),
        version: 11,
    };

    ok(
        invalidation_service
            .broadcast_all(InvalidationMessage::PlaybackStateUpdate {
                room_id: cache_key.clone(),
                state: updated_state.clone(),
            })
            .await,
        "local invalidation broadcast should succeed",
    );

    let cached = ok(
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(cached) = playback_cache.get(&cache_key).await {
                    break cached;
                }
                tokio::task::yield_now().await;
            }
        })
        .await,
        "started playback invalidation listener should process broadcasts",
    );

    assert_eq!(cached.version, updated_state.version);

    runtime.shutdown().await;
}

#[tokio::test]
async fn test_started_invalidation_listener_uses_configured_l2_cache() {
    let backend = Arc::new(CountingL2Backend::default());
    let l2_cache = PlaybackStateCache::new(
        backend.clone(),
        16,
        5,
        60,
        "synctv:test:playback:".to_string(),
    );
    let (runtime, playback_cache, l2_cache, invalidation_service) =
        make_playback_invalidation_runtime_for_lifecycle_tests(Some(l2_cache));
    let room_id = RoomId::expect_positive(10_004);

    ok(
        runtime
            .start(
                invalidation_service.clone(),
                playback_cache.clone(),
                l2_cache.clone(),
            )
            .await,
        "explicit start should activate playback invalidation listener",
    );

    ok(
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !runtime.is_started() {
                tokio::task::yield_now().await;
            }
        })
        .await,
        "start() should mark playback invalidation listener as running",
    );

    ok(
        invalidation_service
            .broadcast_all(InvalidationMessage::PlaybackState {
                room_id: room_id.to_string(),
            })
            .await,
        "playback invalidation should broadcast locally",
    );

    ok(
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend.delete_calls.load(AtomicOrdering::Relaxed) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await,
        "listener should invalidate configured L2 cache",
    );

    runtime.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_db_reload_seeds_missing_local_playback_fence() {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = crate::repository::UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let owner = ok(
        user_repo
            .create(&make_user("playback_seed_fence_owner"))
            .await,
        "owner should be created",
    );
    let room = ok(
        room_repo
            .create(&crate::models::Room::new(
                "Playback Seed Fence".to_string(),
                owner.id,
            ))
            .await,
        "room should be created",
    );
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = ok(
        playback_repo.create_or_get(&room.id).await,
        "playback state should exist",
    );
    state.position = 42.0;
    let state = ok(
        playback_repo.update_with_exact_version(&state, 5).await,
        "playback state should have a nonzero version",
    );

    let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
    let member_repo = crate::repository::RoomMemberRepository::new(pool.clone());
    let permission_service = ok(
        PermissionService::without_cache(member_repo, room_repo, None),
        "permission service should build",
    );
    let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
        provider_repo,
        None,
    ));
    let providers_manager = Arc::new(ok(
        ProvidersManager::new(provider_instance_manager),
        "providers manager should build",
    ));
    let media_service = MediaService::new(
        MediaRepository::new(pool.clone()),
        PlaylistRepository::new(pool.clone()),
        permission_service.clone(),
        providers_manager,
        NotificationService::default(),
    );
    let playback_service = PlaybackService::new_with_runtime(
        playback_repo,
        permission_service,
        media_service,
        make_user_service(&pool),
        crate::service::PlaybackServiceRuntime {
            clock: Arc::new(crate::SystemClock),
            version_fence: fence.clone(),
            invalidation_service: None,
            l2_cache: None,
            realtime_outbox: None,
            source_metadata_repo: None,
            notification_service: None,
        },
    );
    let domain = CacheDomain::Playback { room_id: room.id };
    assert_eq!(
        ok(
            fence.current_version(&domain).await,
            "local fence should be readable"
        ),
        None
    );

    let loaded = ok(
        playback_service.get_state(&room.id).await,
        "strong playback read should fall back to DB",
    );

    assert_eq!(loaded.version, state.version);
    assert_eq!(
        ok(
            fence.current_version(&domain).await,
            "local fence should be readable"
        ),
        Some(state.version)
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn unavailable_creator_reset_does_not_overwrite_a_newer_source() {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = crate::repository::UserRepository::new(pool.clone());
    let owner = ok(
        user_repo
            .create(&make_user("stale_creator_reset_owner"))
            .await,
        "owner should be created",
    );
    let room_service = ok(
        crate::service::RoomService::new_for_tests(pool.clone(), make_user_service(&pool)),
        "room service should build",
    );
    let room = ok(
        room_service
            .create_room(
                "Stale Creator Reset".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    )
    .0;
    let media_repo = MediaRepository::new(pool);
    let first_media = ok(
        media_repo
            .create(&crate::models::Media::from_provider_with_params(
                crate::models::FromProviderParams {
                    playlist_id: None,
                    room_id: room.id,
                    creator_id: Some(owner.id),
                    name: "first source".to_string(),
                    description: String::new(),
                    source_provider: crate::models::SourceProvider::DirectUrl,
                    source_config: crate::models::MediaSourceConfig::DirectUrl(
                        crate::models::DirectUrlMediaSourceConfig::single(
                            "https://example.com/first.mp4".to_string(),
                            std::collections::HashMap::new(),
                        ),
                    ),
                    provider_instance_name: None,
                    position: 0.0,
                },
            ))
            .await,
        "first media should be created",
    );
    let second_media = ok(
        media_repo
            .create(&crate::models::Media::from_provider_with_params(
                crate::models::FromProviderParams {
                    playlist_id: None,
                    room_id: room.id,
                    creator_id: Some(owner.id),
                    name: "second source".to_string(),
                    description: String::new(),
                    source_provider: crate::models::SourceProvider::DirectUrl,
                    source_config: crate::models::MediaSourceConfig::DirectUrl(
                        crate::models::DirectUrlMediaSourceConfig::single(
                            "https://example.com/second.mp4".to_string(),
                            std::collections::HashMap::new(),
                        ),
                    ),
                    provider_instance_name: None,
                    position: 1.0,
                },
            ))
            .await,
        "second media should be created",
    );
    let playback = room_service.playback_service();
    let stale_state = ok(
        playback
            .switch(room.id, owner.id, Some(first_media.id), None, None)
            .await,
        "first source should start",
    );
    let newer_state = ok(
        playback
            .switch(room.id, owner.id, Some(second_media.id), None, None)
            .await,
        "second source should start",
    );

    let error = err(
        playback
            .stop_playback_for_unavailable_creator(&stale_state, "media", None)
            .await,
        "a reset based on stale state must conflict",
    );
    assert!(matches!(error, Error::OptimisticLockConflict));

    let current = ok(
        playback.get_state(&room.id).await,
        "current playback state should remain readable",
    );
    assert_eq!(current.playing_media_id, Some(second_media.id));
    assert_eq!(current.version, newer_state.version);
    assert!(current.is_playing);
}

/// Tests for optimistic lock retry mechanism
mod optimistic_retry_tests {
    use super::*;

    #[test]
    fn test_retry_succeeds_within_max_attempts() {
        // Playback writes run under bursty contention and need a slightly
        // larger budget than the generic optimistic-lock default.
        let conflicts = 4;
        let attempts_needed = conflicts + 1; // 5 attempts
        assert!(
            attempts_needed <= PlaybackService::MAX_RETRIES,
            "Need {} attempts but MAX_RETRIES is {}",
            attempts_needed,
            PlaybackService::MAX_RETRIES
        );
    }
}

/// Tests for the CAS (compare-and-swap) version check in
/// `update_multiple_with_version`. These replicate the pre-check logic
/// without requiring a database.
mod cas_version_pre_check_tests {
    use super::err;
    use crate::Error;

    /// Replicates the CAS pre-check from `update_multiple_with_version`:
    /// when `expected_version` is provided, the current DB version must match.
    fn check_cas_version(current_version: i64, expected_version: Option<i64>) -> crate::Result<()> {
        if let Some(expected) = expected_version {
            if current_version != expected {
                return Err(Error::OptimisticLockConflict);
            }
        }
        Ok(())
    }

    #[test]
    fn test_cas_correct_version_succeeds() {
        let result = check_cas_version(5, Some(5));
        assert!(result.is_ok(), "Matching version should succeed");
    }

    #[test]
    fn test_cas_wrong_version_returns_conflict() {
        let result = check_cas_version(5, Some(3));
        assert!(result.is_err(), "Stale version should return conflict");
        match err(result, "stale CAS version should fail") {
            Error::OptimisticLockConflict => {} // expected
            other => {
                std::panic::panic_any(format!("Expected OptimisticLockConflict, got: {other:?}"))
            }
        }
    }

    #[test]
    fn test_cas_no_version_skips_check() {
        // When no expected version is provided, the check is skipped
        let result = check_cas_version(999, None);
        assert!(
            result.is_ok(),
            "No expected version should skip CAS check (last-writer-wins)"
        );
    }

    #[test]
    fn test_cas_version_zero_matches_initial_state() {
        // Initial playback state has version=0
        let result = check_cas_version(0, Some(0));
        assert!(result.is_ok(), "Version 0 should match initial state");
    }

    #[test]
    fn test_cas_version_zero_expected_but_updated() {
        // Caller expects version 0 but state was already updated to version 1
        let result = check_cas_version(1, Some(0));
        assert!(
            result.is_err(),
            "Stale version 0 when current is 1 should conflict"
        );
    }
}

/// Tests for playback state cache version checking (CAS semantics)
mod version_check_tests {
    use super::*;

    /// Helper to create a playback state with a specific version
    fn make_state(room_id: i64, version: i64, position: f64) -> RoomPlaybackState {
        RoomPlaybackState {
            room_id: RoomId::expect_positive(room_id),
            playing_media_id: None,
            playing_playlist_id: None,
            target: None,
            current_progress_id: None,
            history_cursor_id: None,
            position,
            speed: 1.0,
            is_playing: false,
            playback_generation: 0,
            updated_at: crate::SystemClock.now(),
            version,
        }
    }

    fn version_to_position(version: i64) -> f64 {
        match version {
            0 => 0.0,
            1 => 10.0,
            2 => 20.0,
            3 => 30.0,
            4 => 40.0,
            5 => 50.0,
            6 => 60.0,
            7 => 70.0,
            8 => 80.0,
            9 => 90.0,
            10 => 100.0,
            _ => std::panic::panic_any(format!(
                "test version {version} must be added to version_to_position"
            )),
        }
    }

    /// Test: When cache is empty, incoming state should be inserted
    #[tokio::test]
    async fn test_cache_insert_when_empty() {
        let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
            Arc::new(moka::future::Cache::new(100));

        let room_id = 31_001;
        let cache_key = room_id.to_string();
        let new_state = make_state(room_id, 5, 100.0);

        // Simulate the CAS logic from the invalidation handler
        cache
            .entry(cache_key.clone())
            .and_upsert_with(|maybe_entry| {
                let result = match maybe_entry {
                    Some(entry) => {
                        let current = entry.into_value();
                        if new_state.version > current.version {
                            new_state.clone()
                        } else {
                            current
                        }
                    }
                    None => new_state.clone(),
                };
                std::future::ready(result)
            })
            .await;

        let cached = some(cache.get(&cache_key).await, "should have entry");
        assert_eq!(cached.version, 5);
        assert!((cached.position - 100.0).abs() < f64::EPSILON);
    }

    /// Test: When incoming version is higher, cache should be updated
    #[tokio::test]
    async fn test_cache_update_when_version_higher() {
        let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
            Arc::new(moka::future::Cache::new(100));

        let room_id = 31_002;
        let cache_key = room_id.to_string();

        // Insert initial state with version 3
        let initial_state = make_state(room_id, 3, 50.0);
        cache.insert(cache_key.clone(), initial_state).await;

        // Verify initial state
        let cached = some(cache.get(&cache_key).await, "should have entry");
        assert_eq!(cached.version, 3);

        // Try to update with version 7 (higher)
        let new_state = make_state(room_id, 7, 150.0);
        cache
            .entry(cache_key.clone())
            .and_upsert_with(|maybe_entry| {
                let result = match maybe_entry {
                    Some(entry) => {
                        let current = entry.into_value();
                        if new_state.version > current.version {
                            new_state.clone()
                        } else {
                            current
                        }
                    }
                    None => new_state.clone(),
                };
                std::future::ready(result)
            })
            .await;

        // Cache should now have version 7
        let cached = some(cache.get(&cache_key).await, "should have entry");
        assert_eq!(cached.version, 7);
        assert!((cached.position - 150.0).abs() < f64::EPSILON);
    }

    /// Test: When incoming version is lower, cache should NOT be updated
    #[tokio::test]
    async fn test_cache_not_updated_when_version_lower() {
        let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
            Arc::new(moka::future::Cache::new(100));

        let room_id = 31_003;
        let cache_key = room_id.to_string();

        // Insert initial state with version 10
        let initial_state = make_state(room_id, 10, 200.0);
        cache.insert(cache_key.clone(), initial_state).await;

        // Try to update with version 5 (lower - simulates delayed/out-of-order message)
        let old_state = make_state(room_id, 5, 100.0);
        cache
            .entry(cache_key.clone())
            .and_upsert_with(|maybe_entry| {
                let result = match maybe_entry {
                    Some(entry) => {
                        let current = entry.into_value();
                        if old_state.version > current.version {
                            old_state.clone()
                        } else {
                            current
                        }
                    }
                    None => old_state.clone(),
                };
                std::future::ready(result)
            })
            .await;

        // Cache should still have version 10 (not downgraded to 5)
        let cached = some(cache.get(&cache_key).await, "should have entry");
        assert_eq!(cached.version, 10);
        assert!((cached.position - 200.0).abs() < f64::EPSILON);
    }

    /// Test: When versions are equal, cache should NOT be updated (idempotent)
    #[tokio::test]
    async fn test_cache_not_updated_when_version_equal() {
        let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
            Arc::new(moka::future::Cache::new(100));

        let room_id = 31_004;
        let cache_key = room_id.to_string();

        // Insert initial state with version 5
        let initial_state = make_state(room_id, 5, 200.0);
        cache.insert(cache_key.clone(), initial_state).await;

        // Try to update with same version 5 but different content
        let duplicate_state = make_state(room_id, 5, 999.0);
        cache
            .entry(cache_key.clone())
            .and_upsert_with(|maybe_entry| {
                let result = match maybe_entry {
                    Some(entry) => {
                        let current = entry.into_value();
                        if duplicate_state.version > current.version {
                            duplicate_state.clone()
                        } else {
                            current
                        }
                    }
                    None => duplicate_state.clone(),
                };
                std::future::ready(result)
            })
            .await;

        // Cache should still have original content (not overwritten)
        let cached = some(cache.get(&cache_key).await, "should have entry");
        assert_eq!(cached.version, 5);
        assert!((cached.position - 200.0).abs() < f64::EPSILON);
    }

    /// Test: Sequential updates should only keep the highest version
    #[tokio::test]
    async fn test_sequential_updates_keep_highest_version() {
        let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
            Arc::new(moka::future::Cache::new(100));

        let room_id = 31_005;
        let cache_key = room_id.to_string();

        // Apply updates in non-monotonic order: v1, v5, v3, v7, v2
        let versions = [1i64, 5, 3, 7, 2];
        for v in versions {
            let state = make_state(room_id, v, version_to_position(v));
            cache
                .entry(cache_key.clone())
                .and_upsert_with(|maybe_entry| {
                    let result = match maybe_entry {
                        Some(entry) => {
                            let current = entry.into_value();
                            if state.version > current.version {
                                state.clone()
                            } else {
                                current
                            }
                        }
                        None => state.clone(),
                    };
                    std::future::ready(result)
                })
                .await;
        }

        // Cache should have version 7 (the highest)
        let cached = some(cache.get(&cache_key).await, "should have entry");
        assert_eq!(cached.version, 7);
        assert!((cached.position - 70.0).abs() < f64::EPSILON);
    }

    /// Test: Concurrent updates should be serialized and result in highest version
    #[tokio::test]
    async fn test_concurrent_updates_serialized() {
        let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
            Arc::new(moka::future::Cache::new(100));

        let room_id = 31_006;
        let cache_key = Arc::new(room_id.to_string());
        let cache_clone = cache.clone();

        // Spawn 10 concurrent tasks, each trying to insert a different version
        let handles: Vec<_> = (1..=10)
            .map(|v| {
                let cache_key = cache_key.clone();
                let cache = cache_clone.clone();
                tokio::spawn(async move {
                    let state = make_state(room_id, v, version_to_position(v));
                    cache
                        .entry(cache_key.to_string())
                        .and_upsert_with(|maybe_entry| {
                            let result = match maybe_entry {
                                Some(entry) => {
                                    let current = entry.into_value();
                                    if state.version > current.version {
                                        state.clone()
                                    } else {
                                        current
                                    }
                                }
                                None => state.clone(),
                            };
                            std::future::ready(result)
                        })
                        .await
                })
            })
            .collect();

        // Wait for all tasks to complete
        for handle in handles {
            joined(handle.await, "task should complete");
        }

        // Cache should have version 10 (the highest)
        let cached = some(cache.get(&*cache_key).await, "should have entry");
        assert_eq!(cached.version, 10);
        assert!((cached.position - 100.0).abs() < f64::EPSILON);
    }
}
