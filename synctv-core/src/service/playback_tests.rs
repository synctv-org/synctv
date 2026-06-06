use super::*;
use crate::cache::{CacheInvalidationService, CacheL2Backend, KeyBuilder, UsernameCache};
use crate::models::{RoomId, SignupMethod, User, UserRole, UserStatus};
use crate::repository::{
    MediaRepository, PlaylistRepository, ProviderInstanceRepository, RoomPlaybackStateRepository,
    RoomRepository,
};
use crate::service::permission::PermissionService;
use crate::service::{
    auth::{BruteForceProtection, JwtService},
    InMemoryTokenBlacklistStore, MediaService, NotificationService, ProvidersManager,
    RemoteProviderManager, UserService,
};
use async_trait::async_trait;
use sqlx::PgPool;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

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

    async fn delete_with_retry(
        &self,
        _key: &str,
        _max_retries: u32,
        _cache_type: &str,
    ) -> Result<()> {
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
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
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
    let now = chrono::Utc::now();
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

fn make_playback_service_for_lifecycle_tests() -> (PlaybackService, Arc<CacheInvalidationService>) {
    make_playback_service_for_lifecycle_tests_with_l2(None)
}

fn make_playback_service_for_lifecycle_tests_with_l2(
    l2_cache: Option<PlaybackStateCache>,
) -> (PlaybackService, Arc<CacheInvalidationService>) {
    let pool = PgPool::connect_lazy("postgres://localhost/test")
        .expect("lazy postgres pool for unit tests should build");
    let member_repo = crate::repository::RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let permission_service = PermissionService::without_cache(member_repo, room_repo, None)
        .expect("permission service should build");
    let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
        provider_repo,
        None,
    ));
    let providers_manager = Arc::new(
        ProvidersManager::new(provider_instance_manager).expect("providers manager should build"),
    );
    let media_service = MediaService::new(
        MediaRepository::new(pool.clone()),
        PlaylistRepository::new(pool.clone()),
        permission_service.clone(),
        providers_manager,
        NotificationService::default(),
    );
    let user_service = make_user_service(&pool);
    let invalidation_service = Arc::new(CacheInvalidationService::new(
        "node-test".to_string(),
        "synctv:test:cache:invalidate".to_string(),
    ));
    let playback_service = PlaybackService::new_with_runtime(
        RoomPlaybackStateRepository::new(pool.clone()),
        permission_service,
        media_service,
        user_service,
        Some(invalidation_service.clone()),
        l2_cache,
        None,
        None,
    );
    (playback_service, invalidation_service)
}

#[tokio::test]
async fn standalone_playback_service_uses_non_authoritative_fence_by_default() {
    let pool = PgPool::connect_lazy("postgres://localhost/test")
        .expect("lazy postgres pool for unit tests should build");
    let member_repo = crate::repository::RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let permission_service = PermissionService::without_cache(member_repo, room_repo, None)
        .expect("permission service should build");
    let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
        provider_repo,
        None,
    ));
    let providers_manager = Arc::new(
        ProvidersManager::new(provider_instance_manager).expect("providers manager should build"),
    );
    let media_service = MediaService::new(
        MediaRepository::new(pool.clone()),
        PlaylistRepository::new(pool.clone()),
        permission_service.clone(),
        providers_manager,
        NotificationService::default(),
    );
    let service = PlaybackService::new(
        RoomPlaybackStateRepository::new(pool.clone()),
        permission_service,
        media_service,
        make_user_service(&pool),
    );

    assert!(
        !service.consistency.is_authoritative(),
        "standalone playback constructors must not create private authoritative fences"
    );
}

#[tokio::test]
async fn write_playback_cache_refreshes_l1_when_l2_is_configured() {
    let pool = PgPool::connect_lazy("postgres://localhost/test")
        .expect("lazy postgres pool for unit tests should build");
    let member_repo = crate::repository::RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let permission_service = PermissionService::without_cache(member_repo, room_repo, None)
        .expect("permission service should build");
    let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
        provider_repo,
        None,
    ));
    let providers_manager = Arc::new(
        ProvidersManager::new(provider_instance_manager).expect("providers manager should build"),
    );
    let media_service = MediaService::new(
        MediaRepository::new(pool.clone()),
        PlaylistRepository::new(pool.clone()),
        permission_service.clone(),
        providers_manager,
        NotificationService::default(),
    );
    let l2_backend = Arc::new(CountingL2Backend::default());
    let l2_cache = PlaybackStateCache::new(
        l2_backend,
        100,
        PlaybackService::DEFAULT_CACHE_TTL_SECS,
        60,
        "test:playback:l1-refresh:".to_string(),
    );
    let service = PlaybackService::new_with_runtime(
        RoomPlaybackStateRepository::new(pool.clone()),
        permission_service,
        media_service,
        make_user_service(&pool),
        None,
        Some(l2_cache),
        None,
        None,
    );

    let room_id = RoomId::expect_positive(10_004);
    let cache_key = room_id.to_string();
    let stale_state = RoomPlaybackState {
        room_id,
        playing_media_id: None,
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 10.0,
        speed: 1.0,
        is_playing: false,
        updated_at: chrono::Utc::now(),
        version: 3,
    };
    service
        .playback_cache
        .insert(cache_key.clone(), stale_state)
        .await;

    let fresh_state = RoomPlaybackState {
        room_id,
        playing_media_id: None,
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 42.0,
        speed: 1.0,
        is_playing: true,
        updated_at: chrono::Utc::now(),
        version: 4,
    };
    service.write_playback_cache(&fresh_state).await;

    let cached = service
        .playback_cache
        .get(&cache_key)
        .await
        .expect("local L1 cache should be refreshed by local write");
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
        target: Vec::new(),
        current_progress_id: None,
        position: 0.0,
        speed: 1.0,
        is_playing: false,
        updated_at: chrono::Utc::now(),
        version: 1,
    };

    let err = validate_position_update_source(&state).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)));

    state.playing_media_id = Some(MediaId::expect_positive(30_001));
    assert!(validate_position_update_source(&state).is_ok());

    state.playing_media_id = None;
    state.playing_playlist_id = Some(PlaylistId::expect_positive(40_001));
    state.target = b"dynamic-target".to_vec();
    assert!(validate_position_update_source(&state).is_ok());
}

#[test]
fn test_switch_target_source_shape_matches_progress_schema() {
    assert!(validate_switch_target(&SwitchPlaybackTarget {
        media_id: Some(MediaId::expect_positive(30_010)),
        playlist_id: None,
        target: Vec::new(),
    })
    .is_ok());
    assert!(validate_switch_target(&SwitchPlaybackTarget {
        media_id: None,
        playlist_id: Some(PlaylistId::expect_positive(40_010)),
        target: b"dynamic-target".to_vec(),
    })
    .is_ok());
    assert!(validate_switch_target(&SwitchPlaybackTarget {
        media_id: Some(MediaId::expect_positive(30_011)),
        playlist_id: None,
        target: b"static-media-must-not-have-target".to_vec(),
    })
    .is_err());
    assert!(validate_switch_target(&SwitchPlaybackTarget {
        media_id: None,
        playlist_id: Some(PlaylistId::expect_positive(40_011)),
        target: Vec::new(),
    })
    .is_err());
}

#[tokio::test]
async fn test_invalidation_listener_stops_after_cache_invalidation_service_stop() {
    let (playback_service, invalidation_service) = make_playback_service_for_lifecycle_tests();
    let room_id = RoomId::expect_positive(10_001);
    let cache_key = room_id.to_string();

    playback_service
        .start()
        .await
        .expect("playback invalidation listener should start");

    assert!(
        playback_service.invalidation_task_started(),
        "start() must mark playback invalidation runtime as running"
    );

    playback_service.shutdown().await;

    let updated_state = RoomPlaybackState {
        room_id,
        playing_media_id: None,
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 42.0,
        speed: 1.0,
        is_playing: true,
        updated_at: chrono::Utc::now(),
        version: 7,
    };

    invalidation_service
        .broadcast_all(InvalidationMessage::PlaybackStateUpdate {
            room_id: cache_key.clone(),
            state: updated_state,
        })
        .await
        .expect("local invalidation broadcast should succeed");
    tokio::task::yield_now().await;

    assert!(
        playback_service
            .playback_cache
            .get(&cache_key)
            .await
            .is_none(),
        "playback invalidation listener must stop processing local broadcasts once shutdown starts"
    );
}

#[tokio::test]
async fn test_start_can_restart_playback_invalidation_listener_after_shutdown() {
    let (playback_service, invalidation_service) = make_playback_service_for_lifecycle_tests();
    let room_id = RoomId::expect_positive(10_002);
    let cache_key = room_id.to_string();

    playback_service
        .start()
        .await
        .expect("initial playback invalidation start should succeed");
    playback_service.shutdown().await;

    let updated_state = RoomPlaybackState {
        room_id,
        playing_media_id: None,
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 64.0,
        speed: 1.0,
        is_playing: true,
        updated_at: chrono::Utc::now(),
        version: 9,
    };

    playback_service
        .start()
        .await
        .expect("restart after playback invalidation shutdown should succeed");

    invalidation_service
        .broadcast_all(InvalidationMessage::PlaybackStateUpdate {
            room_id: cache_key.clone(),
            state: updated_state,
        })
        .await
        .expect("local invalidation broadcast should succeed after restart");
    tokio::task::yield_now().await;

    let cached = playback_service
        .playback_cache
        .get(&cache_key)
        .await
        .expect("restarted listener should populate cache from invalidation broadcast");
    assert_eq!(cached.version, 9);

    playback_service.shutdown().await;
}

#[tokio::test]
async fn test_start_activates_invalidation_listener_after_wiring_service() {
    let (playback_service, invalidation_service) = make_playback_service_for_lifecycle_tests();
    let room_id = RoomId::expect_positive(10_003);
    let cache_key = room_id.to_string();

    playback_service
        .start()
        .await
        .expect("explicit start should activate playback invalidation listener");

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !playback_service.invalidation_task_started() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("start() should mark playback invalidation listener as running");

    let updated_state = RoomPlaybackState {
        room_id,
        playing_media_id: None,
        playing_playlist_id: None,
        target: Vec::new(),
        current_progress_id: None,
        position: 88.0,
        speed: 1.0,
        is_playing: true,
        updated_at: chrono::Utc::now(),
        version: 11,
    };

    invalidation_service
        .broadcast_all(InvalidationMessage::PlaybackStateUpdate {
            room_id: cache_key.clone(),
            state: updated_state.clone(),
        })
        .await
        .expect("local invalidation broadcast should succeed");

    let cached = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let Some(cached) = playback_service.playback_cache.get(&cache_key).await {
                break cached;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("started playback invalidation listener should process broadcasts");

    assert_eq!(cached.version, updated_state.version);

    playback_service.shutdown().await;
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
    let (playback_service, invalidation_service) =
        make_playback_service_for_lifecycle_tests_with_l2(Some(l2_cache));
    let room_id = RoomId::expect_positive(10_004);

    playback_service
        .start()
        .await
        .expect("explicit start should activate playback invalidation listener");

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !playback_service.invalidation_task_started() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("start() should mark playback invalidation listener as running");

    invalidation_service
        .broadcast_all(InvalidationMessage::PlaybackState {
            room_id: room_id.to_string(),
        })
        .await
        .expect("playback invalidation should broadcast locally");

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while backend.delete_calls.load(AtomicOrdering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("listener should invalidate configured L2 cache");

    playback_service.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_db_reload_seeds_missing_local_playback_fence() {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = crate::repository::UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let owner = user_repo
        .create(&make_user("playback_seed_fence_owner"))
        .await
        .expect("owner should be created");
    let room = room_repo
        .create(&crate::models::Room::new(
            "Playback Seed Fence".to_string(),
            owner.id,
        ))
        .await
        .expect("room should be created");
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = playback_repo
        .create_or_get(&room.id)
        .await
        .expect("playback state should exist");
    state.position = 42.0;
    let state = playback_repo
        .update_with_exact_version(&state, 5)
        .await
        .expect("playback state should have a nonzero version");

    let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
    let member_repo = crate::repository::RoomMemberRepository::new(pool.clone());
    let permission_service = PermissionService::without_cache(member_repo, room_repo, None)
        .expect("permission service should build");
    let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
        provider_repo,
        None,
    ));
    let providers_manager = Arc::new(
        ProvidersManager::new(provider_instance_manager).expect("providers manager should build"),
    );
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
        None,
        None,
        Some(fence.clone()),
        None,
    );
    let domain = CacheDomain::Playback { room_id: room.id };
    assert_eq!(
        fence
            .current_version(&domain)
            .await
            .expect("local fence should be readable"),
        None
    );

    let loaded = playback_service
        .get_state(&room.id)
        .await
        .expect("strong playback read should fall back to DB");

    assert_eq!(loaded.version, state.version);
    assert_eq!(
        fence
            .current_version(&domain)
            .await
            .expect("local fence should be readable"),
        Some(state.version)
    );
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
        match result.unwrap_err() {
            Error::OptimisticLockConflict => {} // expected
            other => panic!("Expected OptimisticLockConflict, got: {other:?}"),
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
            target: Vec::new(),
            current_progress_id: None,
            position,
            speed: 1.0,
            is_playing: false,
            updated_at: chrono::Utc::now(),
            version,
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn version_to_position(version: i64) -> f64 {
        version as f64 * 10.0
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

        let cached = cache.get(&cache_key).await.expect("should have entry");
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
        let cached = cache.get(&cache_key).await.expect("should have entry");
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
        let cached = cache.get(&cache_key).await.expect("should have entry");
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
        let cached = cache.get(&cache_key).await.expect("should have entry");
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
        let cached = cache.get(&cache_key).await.expect("should have entry");
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
        let cached = cache.get(&cache_key).await.expect("should have entry");
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
            handle.await.expect("task should complete");
        }

        // Cache should have version 10 (the highest)
        let cached = cache.get(&*cache_key).await.expect("should have entry");
        assert_eq!(cached.version, 10);
        assert!((cached.position - 100.0).abs() < f64::EPSILON);
    }
}
