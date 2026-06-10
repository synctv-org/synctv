use super::*;
use crate::cache::{CacheInvalidationService, CacheL2Backend, InvalidationMessage, NoopCacheL2};
use crate::cache::{KeyBuilder, UsernameCache};
use crate::models::{SignupMethod, User, UserId, UserRole, UserStatus};
use crate::repository::RoomSettingsRepository;
use crate::repository::UserRepository;
use crate::service::auth::BruteForceProtection;
use crate::service::notification::NotificationService;
use crate::service::{auth::JwtService, InMemoryTokenBlacklistStore, UserService};
use chrono::Utc;
use sqlx::PgPool;
use synctv_core_testing::create_test_pool;

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => std::panic::panic_any(context.to_string()),
    }
}

struct FailingRoomSettingsL2;

#[async_trait::async_trait]
impl CacheL2Backend for FailingRoomSettingsL2 {
    async fn get(&self, _key: &str) -> Result<Option<String>> {
        Err(Error::Internal(
            "simulated room settings L2 failure".to_string(),
        ))
    }

    async fn set(&self, _key: &str, _json: &str, _ttl_secs: u64) -> Result<()> {
        Err(Error::Internal(
            "simulated room settings L2 failure".to_string(),
        ))
    }

    async fn delete(&self, _key: &str) -> Result<()> {
        Err(Error::Internal(
            "simulated room settings L2 failure".to_string(),
        ))
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
        Err(Error::Internal(
            "simulated room settings L2 failure".to_string(),
        ))
    }

    async fn set_if_version_at_least(
        &self,
        _key: &str,
        _json: &str,
        _ttl_secs: u64,
        _version: i64,
    ) -> Result<bool> {
        Err(Error::Internal(
            "simulated room settings L2 failure".to_string(),
        ))
    }

    async fn delete_by_prefix(&self, _prefix: &str) -> Result<()> {
        Err(Error::Internal(
            "simulated room settings L2 failure".to_string(),
        ))
    }

    fn is_active(&self) -> bool {
        true
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_strong_read_uses_l1_when_version_satisfies_fence() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = ok(
        crate::service::RoomService::new_for_tests(pool.clone(), make_user_service(&pool)),
        "room service should build",
    );
    let owner = ok(
        user_repo
            .create(&make_user("room_settings_fence_l1_owner"))
            .await,
        "owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Room Settings Fence L1".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let (_redis_container, redis_conn) = synctv_core_testing::start_redis().await;
    let fence = Arc::new(crate::cache::RedisVersionFenceStore::new(
        crate::direct_runtime(redis_conn),
        "test:fence-l1:",
    ));
    let service = RoomSettingsService::new_with_version_fence(
        RoomSettingsRepository::new(pool.clone()),
        None,
        Arc::new(NotificationService::default()),
        RoomSettingsRuntime {
            version_fence: fence.clone(),
            cache_key_prefix: "test:room_settings:l1:".to_string(),
            ..RoomSettingsRuntime::local_only()
        },
    );

    let cached_settings = RoomSettings {
        allow_auto_join: crate::models::room_settings::AllowAutoJoin(false),
        ..RoomSettings::default()
    };
    ok(
        service
            .cache
            .set(
                &room.id,
                RoomSettingsSnapshot {
                    settings: cached_settings,
                    version: 7,
                },
            )
            .await,
        "cache write should succeed",
    );
    ok(
        fence
            .set_version_at_least(&CacheDomain::RoomSettings { room_id: room.id }, 7)
            .await,
        "fence should be written",
    );

    let snapshot = ok(
        service.get_with_version(&room.id).await,
        "strong read should use cache that satisfies fence",
    );
    assert_eq!(snapshot.version, 7);
    assert!(!snapshot.settings.allow_auto_join.0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_strong_read_uses_l1_with_local_version_fence() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = ok(
        crate::service::RoomService::new_for_tests(pool.clone(), make_user_service(&pool)),
        "room service should build",
    );
    let owner = ok(
        user_repo
            .create(&make_user("room_settings_local_fence_owner"))
            .await,
        "owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Room Settings Local Fence".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
    let service = RoomSettingsService::new_with_version_fence(
        RoomSettingsRepository::new(pool.clone()),
        None,
        Arc::new(NotificationService::default()),
        RoomSettingsRuntime {
            version_fence: fence.clone(),
            cache_key_prefix: "test:room_settings:local:".to_string(),
            ..RoomSettingsRuntime::local_only()
        },
    );

    let cached_settings = RoomSettings {
        allow_auto_join: crate::models::room_settings::AllowAutoJoin(false),
        ..RoomSettings::default()
    };
    ok(
        service
            .cache
            .set(
                &room.id,
                RoomSettingsSnapshot {
                    settings: cached_settings,
                    version: 3,
                },
            )
            .await,
        "cache write should succeed",
    );
    ok(
        fence
            .set_version_at_least(&CacheDomain::RoomSettings { room_id: room.id }, 3)
            .await,
        "local fence should be written",
    );

    let snapshot = ok(
        service.get_with_version(&room.id).await,
        "strong read should use local-fenced L1",
    );
    assert_eq!(snapshot.version, 3);
    assert!(!snapshot.settings.allow_auto_join.0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_write_uses_redis_allocated_version() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = ok(
        crate::service::RoomService::new_for_tests(pool.clone(), make_user_service(&pool)),
        "room service should build",
    );
    let owner = ok(
        user_repo
            .create(&make_user("room_settings_allocator_owner"))
            .await,
        "owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Room Settings Allocator".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let (_redis_container, redis_conn) = synctv_core_testing::start_redis().await;
    let fence = Arc::new(crate::cache::RedisVersionFenceStore::new(
        crate::direct_runtime(redis_conn),
        "test:fence-allocator:",
    ));
    let service = RoomSettingsService::new_with_version_fence(
        RoomSettingsRepository::new(pool.clone()),
        None,
        Arc::new(NotificationService::default()),
        RoomSettingsRuntime {
            version_fence: fence.clone(),
            cache_key_prefix: "test:room_settings:allocator:".to_string(),
            ..RoomSettingsRuntime::local_only()
        },
    );

    let settings = RoomSettings {
        allow_auto_join: crate::models::room_settings::AllowAutoJoin(false),
        ..RoomSettings::default()
    };
    ok(
        service.set(&room.id, &settings).await,
        "settings write should use Redis allocated version",
    );

    let domain = CacheDomain::RoomSettings { room_id: room.id };
    let fence_version = some(
        ok(
            fence.current_version(&domain).await,
            "fence should be readable",
        ),
        "fence should exist",
    );
    let snapshot = ok(
        service.get_refresh_with_version(&room.id).await,
        "DB settings should be readable",
    );
    assert_eq!(snapshot.version, fence_version);

    let updated = RoomSettings {
        chat_enabled: crate::models::room_settings::ChatEnabled(false),
        ..settings
    };
    ok(
        service.set(&room.id, &updated).await,
        "second settings write should use next Redis version",
    );
    let next_fence = some(
        ok(
            fence.current_version(&domain).await,
            "fence should be readable",
        ),
        "fence should exist",
    );
    let next_snapshot = ok(
        service.get_refresh_with_version(&room.id).await,
        "DB settings should be readable",
    );
    assert!(next_fence > fence_version);
    assert_eq!(next_snapshot.version, next_fence);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_reserve_rejects_stale_snapshot_without_advancing_fence() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = ok(
        crate::service::RoomService::new_for_tests(pool.clone(), make_user_service(&pool)),
        "room service should build",
    );
    let owner = ok(
        user_repo
            .create(&make_user("room_settings_stale_reserve_owner"))
            .await,
        "owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Room Settings Stale Reserve".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
    let service = RoomSettingsService::new_with_version_fence(
        RoomSettingsRepository::new(pool.clone()),
        None,
        Arc::new(NotificationService::default()),
        RoomSettingsRuntime {
            version_fence: fence.clone(),
            cache_key_prefix: "test:room_settings:stale-reserve:".to_string(),
            ..RoomSettingsRuntime::local_only()
        },
    );
    let domain = CacheDomain::RoomSettings { room_id: room.id };

    let stale_observed_version = 1;
    ok(
        fence
            .set_version_at_least(&domain, stale_observed_version + 1)
            .await,
        "concurrent writer should advance fence",
    );

    let result = service.begin_write(&room.id, stale_observed_version).await;
    assert!(
        matches!(result, Err(Error::OptimisticLockConflict)),
        "stale settings snapshots must retry before reserving a fence version; got {result:?}"
    );
    assert_eq!(
        ok(
            fence.current_version(&domain).await,
            "fence should be readable"
        ),
        Some(stale_observed_version + 1),
        "failed reservations must not burn additional fence versions"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_write_does_not_retry_committed_update_after_l2_failure() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = ok(
        crate::service::RoomService::new_for_tests(pool.clone(), make_user_service(&pool)),
        "room service should build",
    );
    let owner = ok(
        user_repo
            .create(&make_user("room_settings_l2_failure_owner"))
            .await,
        "owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Room Settings L2 Failure".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let service = RoomSettingsService::new_with_version_fence(
        RoomSettingsRepository::new(pool.clone()),
        None,
        Arc::new(NotificationService::default()),
        RoomSettingsRuntime {
            l2_cache: Arc::new(FailingRoomSettingsL2),
            cache_key_prefix: "test:room_settings:l2-failure:".to_string(),
            ..RoomSettingsRuntime::local_only()
        },
    );

    let settings = RoomSettings {
        chat_enabled: crate::models::room_settings::ChatEnabled(false),
        ..RoomSettings::default()
    };
    ok(
        service.set(&room.id, &settings).await,
        "committed settings write must not fail because cache refresh failed",
    );

    let snapshot = ok(
        service.get_refresh_with_version(&room.id).await,
        "committed settings should remain readable",
    );
    assert_eq!(snapshot.version, 2);
    assert!(!snapshot.settings.chat_enabled.0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_db_refresh_seeds_missing_local_version_fence() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = ok(
        crate::service::RoomService::new_for_tests(pool.clone(), make_user_service(&pool)),
        "room service should build",
    );
    let owner = ok(
        user_repo
            .create(&make_user("room_settings_seed_fence_owner"))
            .await,
        "owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Room Settings Seed Fence".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let settings = RoomSettings {
        allow_auto_join: crate::models::room_settings::AllowAutoJoin(false),
        ..RoomSettings::default()
    };
    let repo = RoomSettingsRepository::new(pool.clone());
    let (_current_settings, current_version) = ok(
        repo.get_with_version(&room.id).await,
        "current settings should be readable",
    );
    let target_version = current_version + 3;
    let db_version = ok(
        repo.set_settings_with_exact_version(&room.id, &settings, current_version, target_version)
            .await,
        "settings row should be written",
    );

    let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
    let service = RoomSettingsService::new_with_version_fence(
        repo,
        None,
        Arc::new(NotificationService::default()),
        RoomSettingsRuntime {
            version_fence: fence.clone(),
            cache_key_prefix: "test:room_settings:seed-fence:".to_string(),
            ..RoomSettingsRuntime::local_only()
        },
    );
    let domain = CacheDomain::RoomSettings { room_id: room.id };
    assert_eq!(
        ok(
            fence.current_version(&domain).await,
            "local fence should be readable"
        ),
        None
    );

    let snapshot = ok(
        service.get_with_version(&room.id).await,
        "strong read should fall back to DB",
    );

    assert_eq!(snapshot.version, db_version);
    assert_eq!(
        ok(
            fence.current_version(&domain).await,
            "local fence should be readable"
        ),
        Some(db_version)
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_writes_versioned_default_settings() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = ok(
        crate::service::RoomService::new_for_tests(pool.clone(), make_user_service(&pool)),
        "room service should build",
    );
    let owner = ok(
        user_repo
            .create(&make_user("room_settings_delete_default_owner"))
            .await,
        "owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Room Settings Delete Default".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
    let repo = RoomSettingsRepository::new(pool.clone());
    let service = RoomSettingsService::new_with_version_fence(
        repo.clone(),
        None,
        Arc::new(NotificationService::default()),
        RoomSettingsRuntime {
            version_fence: fence.clone(),
            cache_key_prefix: "test:room_settings:delete-default:".to_string(),
            ..RoomSettingsRuntime::local_only()
        },
    );

    let changed = RoomSettings {
        allow_guest_join: crate::models::room_settings::AllowGuestJoin(true),
        ..RoomSettings::default()
    };
    ok(
        service.set(&room.id, &changed).await,
        "custom settings should be written",
    );
    ok(
        repo.set(&room.id, "password", "stale-password-hash").await,
        "password row should be written",
    );
    let before_delete = ok(
        service.get_refresh_with_version(&room.id).await,
        "settings should be readable before delete",
    );

    ok(
        service.delete(&room.id).await,
        "delete should write versioned default settings",
    );

    let after_delete = ok(
        repo.get_with_version(&room.id).await,
        "default settings row should remain readable",
    );
    assert!(!after_delete.0.allow_guest_join.0);
    assert!(
        after_delete.1 > before_delete.version,
        "delete must keep a monotonic DB version"
    );
    let domain = CacheDomain::RoomSettings { room_id: room.id };
    assert_eq!(
        ok(
            fence.current_version(&domain).await,
            "local fence should be readable"
        ),
        Some(after_delete.1)
    );
}

fn make_room_settings_invalidation_runtime_for_lifecycle_tests() -> (
    Arc<RoomSettingsInvalidationRuntime>,
    RoomSettingsCache,
    Arc<CacheInvalidationService>,
    RoomId,
) {
    let room_id = RoomId::expect_positive(20_001);
    let invalidation_service = Arc::new(CacheInvalidationService::new(
        "test-node".to_string(),
        "synctv:test:room-settings".to_string(),
    ));
    let cache = RoomSettingsCache::new(
        Arc::new(NoopCacheL2),
        128,
        60,
        60,
        "test:room_settings:".to_string(),
    );
    (
        Arc::new(RoomSettingsInvalidationRuntime::new()),
        cache,
        invalidation_service,
        room_id,
    )
}

#[tokio::test]
async fn standalone_room_settings_runtime_uses_local_authoritative_fence() {
    let runtime = RoomSettingsRuntime::local_only();
    let consistency = ConsistencyCoordinator::new(runtime.version_fence);

    assert!(
        consistency.is_authoritative(),
        "standalone room settings runtime must use an explicit local version fence"
    );
}

#[tokio::test]
async fn test_invalidation_via_streams() {
    let inv_service = Arc::new(CacheInvalidationService::new(
        "test-node".to_string(),
        "synctv:cache:invalidate:stream".to_string(),
    ));

    let mut receiver = inv_service.subscribe();

    ok(
        inv_service
            .broadcast_all(InvalidationMessage::RoomSettings {
                room_id: "room1".to_string(),
            })
            .await,
        "room settings invalidation should broadcast",
    );

    let msg = ok(
        receiver.recv().await,
        "room settings invalidation should receive",
    );
    match msg {
        InvalidationMessage::RoomSettings { ref room_id } => {
            assert_eq!(room_id, "room1");
        }
        _ => std::panic::panic_any("Expected RoomSettings invalidation message"),
    }
}

#[test]
fn test_room_settings_invalidation_message_serialization() {
    let msg = InvalidationMessage::RoomSettings {
        room_id: "room123".to_string(),
    };

    let json = ok(
        serde_json::to_string(&msg),
        "room settings invalidation message should serialize",
    );
    assert!(json.contains("room_settings"));
    assert!(json.contains("room123"));

    let decoded: InvalidationMessage = ok(
        serde_json::from_str(&json),
        "room settings invalidation message should deserialize",
    );
    assert_eq!(msg, decoded);
}

#[tokio::test]
async fn test_local_invalidation_broadcast_accepts_all_target_message() {
    let inv_service = Arc::new(CacheInvalidationService::new(
        "test-node".to_string(),
        "synctv:cache:invalidate:stream".to_string(),
    ));

    ok(
        inv_service.broadcast_all(InvalidationMessage::All).await,
        "all-target invalidation should broadcast",
    );
}

#[tokio::test]
async fn test_invalidation_listener_stops_after_shutdown() {
    let (runtime, cache, invalidation_service, room_id) =
        make_room_settings_invalidation_runtime_for_lifecycle_tests();

    ok(
        runtime
            .start(invalidation_service.clone(), cache.clone())
            .await,
        "room settings invalidation listener should start",
    );
    assert!(
        runtime.is_started(),
        "start() must mark room settings invalidation runtime as running"
    );

    ok(
        cache
            .set(
                &room_id,
                RoomSettingsSnapshot {
                    settings: RoomSettings::default(),
                    version: 0,
                },
            )
            .await,
        "cache fixture write should succeed",
    );

    runtime.shutdown().await;

    ok(
        invalidation_service
            .broadcast_all(InvalidationMessage::RoomSettings {
                room_id: room_id.to_string(),
            })
            .await,
        "local invalidation broadcast should succeed",
    );
    tokio::task::yield_now().await;

    assert!(
        ok(cache.get(&room_id).await, "cache entry should be readable").is_some(),
        "room settings listener should have stopped once invalidation service shutdown begins"
    );
}

#[tokio::test]
async fn test_start_can_restart_room_settings_invalidation_listener_after_shutdown() {
    let (runtime, cache, invalidation_service, room_id) =
        make_room_settings_invalidation_runtime_for_lifecycle_tests();

    ok(
        runtime
            .start(invalidation_service.clone(), cache.clone())
            .await,
        "initial room settings invalidation start should succeed",
    );
    runtime.shutdown().await;

    ok(
        cache
            .set(
                &room_id,
                RoomSettingsSnapshot {
                    settings: RoomSettings::default(),
                    version: 0,
                },
            )
            .await,
        "cache fixture write should succeed",
    );

    ok(
        runtime
            .start(invalidation_service.clone(), cache.clone())
            .await,
        "restart after room settings invalidation shutdown should succeed",
    );

    ok(
        invalidation_service
            .broadcast_all(InvalidationMessage::RoomSettings {
                room_id: room_id.to_string(),
            })
            .await,
        "local invalidation broadcast should succeed after restart",
    );
    tokio::task::yield_now().await;

    assert!(
        ok(cache.get(&room_id).await, "cache entry should be readable").is_none(),
        "restarted room settings listener should invalidate cache entries again"
    );

    runtime.shutdown().await;
}

fn make_user_service(pool: &PgPool) -> UserService {
    let jwt_service = ok(
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!"),
        "jwt service should build",
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
    let now = Utc::now();
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_with_version_returns_current_snapshot_version() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = make_user_service(&pool);
    let room_service = ok(
        crate::service::RoomService::new_for_tests(pool.clone(), user_service),
        "room service should build",
    );
    let owner = ok(
        user_repo
            .create(&make_user("room_settings_version_owner"))
            .await,
        "owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Room Settings Version".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let service = RoomSettingsService::new(
        RoomSettingsRepository::new(pool),
        None,
        Arc::new(NotificationService::default()),
        None,
        None,
    );

    let updated = RoomSettings {
        chat_enabled: crate::models::room_settings::ChatEnabled(false),
        ..RoomSettings::default()
    };
    ok(
        service.set(&room.id, &updated).await,
        "room settings should be persisted",
    );

    let cached = ok(
        service
            .get_eventually_consistent_with_version(&room.id)
            .await,
        "cached room settings should be readable",
    );
    assert!(
        !cached.settings.chat_enabled.0,
        "sanity check: cache should contain the updated settings value"
    );

    let snapshot = ok(
        service.get_with_version(&room.id).await,
        "strong room settings snapshot should include version",
    );
    assert_eq!(snapshot.version, 2);
    assert!(
        !snapshot.settings.chat_enabled.0,
        "snapshot should include the updated settings value"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_local_fence_rejects_stale_l1_after_service_settings_change() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = make_user_service(&pool);
    let room_service = ok(
        crate::service::RoomService::new_for_tests(pool.clone(), user_service),
        "room service should build",
    );
    let owner = ok(
        user_repo
            .create(&make_user("room_settings_strong_owner"))
            .await,
        "owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Room Settings Strong".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
    let repo = RoomSettingsRepository::new(pool.clone());
    let service = RoomSettingsService::new_with_version_fence(
        repo.clone(),
        None,
        Arc::new(NotificationService::default()),
        RoomSettingsRuntime {
            version_fence: fence.clone(),
            cache_key_prefix: "test:room_settings:stale-l1:".to_string(),
            ..RoomSettingsRuntime::local_only()
        },
    );

    let original = RoomSettings {
        allow_auto_join: crate::models::room_settings::AllowAutoJoin(true),
        ..RoomSettings::default()
    };
    ok(
        service.set(&room.id, &original).await,
        "initial settings should be persisted",
    );
    let cached = ok(
        service
            .get_eventually_consistent_with_version(&room.id)
            .await,
        "cache should be populated",
    );
    assert!(cached.settings.allow_auto_join.0);

    let changed = RoomSettings {
        allow_auto_join: crate::models::room_settings::AllowAutoJoin(false),
        ..RoomSettings::default()
    };
    let (_current, cached_version) = ok(
        repo.get_with_version(&room.id).await,
        "settings version should be readable",
    );
    let newer_version = cached_version + 1;
    ok(
        repo.set_settings_with_exact_version(&room.id, &changed, cached_version, newer_version)
            .await,
        "DB settings update should succeed",
    );
    ok(
        fence
            .set_version_at_least(
                &CacheDomain::RoomSettings { room_id: room.id },
                newer_version,
            )
            .await,
        "local fence should be advanced",
    );

    let stale_snapshot = ok(
        service
            .get_eventually_consistent_with_version(&room.id)
            .await,
        "eventual settings read should still expose stale cache fixture",
    );
    assert!(
        stale_snapshot.settings.allow_auto_join.0,
        "cache-first settings snapshot should demonstrate stale L1 is present"
    );

    let strong_settings = ok(
        service.get(&room.id).await,
        "strong settings read should succeed",
    );
    assert!(
        !strong_settings.allow_auto_join.0,
        "default settings get must bypass stale L1 and read DB"
    );

    let strong_snapshot = ok(
        service.get_with_version(&room.id).await,
        "strong versioned settings read should succeed",
    );
    assert!(
        !strong_snapshot.settings.allow_auto_join.0,
        "versioned settings get must bypass stale L1 and read DB"
    );
    assert!(
        strong_snapshot.version >= newer_version,
        "versioned settings get must return a snapshot satisfying the local fence"
    );
}
