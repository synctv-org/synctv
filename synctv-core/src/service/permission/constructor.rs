use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{
    cache::{ConsistencyCoordinator, MemberPermissionCache, RoomSettingsCache},
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository},
    service::permission::{
        PermissionInvalidationRuntime, PermissionService, PermissionServiceRuntime,
        SharedInvalidationService,
    },
    Result,
};

impl PermissionService {
    fn build_member_permission_cache(runtime: &PermissionServiceRuntime) -> MemberPermissionCache {
        MemberPermissionCache::new(
            runtime.member_permission_l2_cache.clone(),
            runtime.cache_size,
            runtime.cache_ttl_secs,
            runtime.cache_ttl_secs,
            runtime.member_permission_cache_key_prefix.clone(),
        )
    }

    fn build_room_settings_cache(runtime: &PermissionServiceRuntime) -> RoomSettingsCache {
        RoomSettingsCache::new(
            runtime.room_settings_l2_cache.clone(),
            runtime.cache_size,
            runtime.cache_ttl_secs,
            runtime.cache_ttl_secs,
            runtime.room_settings_cache_key_prefix.clone(),
        )
    }

    pub fn new(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        runtime_settings_store: Option<Arc<crate::service::RuntimeSettingsStore>>,
        cache_size: u64,
        cache_ttl_secs: u64,
    ) -> Result<Self> {
        let room_settings_repo = RoomSettingsRepository::new(room_repo.pool().clone());
        Self::new_with_runtime(
            member_repo,
            room_repo,
            PermissionServiceRuntime {
                runtime_settings_store,
                cache_size,
                cache_ttl_secs,
                room_settings_repo: Some(room_settings_repo),
                ..PermissionServiceRuntime::local_only()
            },
        )
    }

    pub fn new_with_runtime(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        runtime: PermissionServiceRuntime,
    ) -> Result<Self> {
        let member_permission_cache = Self::build_member_permission_cache(&runtime);
        let room_settings_cache = Self::build_room_settings_cache(&runtime);

        Ok(Self {
            member_repo: Some(member_repo),
            room_repo: Some(room_repo),
            room_settings_repo: runtime.room_settings_repo,
            member_permission_cache,
            room_settings_cache,
            runtime_settings_store: runtime.runtime_settings_store,
            invalidation_service: Arc::new(SharedInvalidationService {
                service: parking_lot::RwLock::new(runtime.invalidation_service),
            }),
            cache_degraded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(
                Instant::now()
                    .checked_sub(Duration::from_secs(Self::FLUSH_RATE_LIMIT_SECS))
                    .unwrap_or(Instant::now()),
            )),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
            invalidation_runtime: Arc::new(PermissionInvalidationRuntime::new()),
            consistency: ConsistencyCoordinator::new(runtime.version_fence),
        })
    }

    #[cfg(test)]
    pub(crate) fn new_without_repositories_for_tests(
        runtime: PermissionServiceRuntime,
    ) -> Result<Self> {
        let member_permission_cache = Self::build_member_permission_cache(&runtime);
        let room_settings_cache = Self::build_room_settings_cache(&runtime);

        Ok(Self {
            member_repo: None,
            room_repo: None,
            room_settings_repo: runtime.room_settings_repo,
            member_permission_cache,
            room_settings_cache,
            runtime_settings_store: runtime.runtime_settings_store,
            invalidation_service: Arc::new(SharedInvalidationService {
                service: parking_lot::RwLock::new(runtime.invalidation_service),
            }),
            cache_degraded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(
                Instant::now()
                    .checked_sub(Duration::from_secs(Self::FLUSH_RATE_LIMIT_SECS))
                    .unwrap_or(Instant::now()),
            )),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
            invalidation_runtime: Arc::new(PermissionInvalidationRuntime::new()),
            consistency: ConsistencyCoordinator::new(runtime.version_fence),
        })
    }

    pub fn without_cache(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        runtime_settings_store: Option<Arc<crate::service::RuntimeSettingsStore>>,
    ) -> Result<Self> {
        let room_settings_repo = RoomSettingsRepository::new(room_repo.pool().clone());
        Self::new_with_runtime(
            member_repo,
            room_repo,
            PermissionServiceRuntime {
                runtime_settings_store,
                cache_size: 1,
                cache_ttl_secs: 1,
                room_settings_repo: Some(room_settings_repo),
                ..PermissionServiceRuntime::local_only()
            },
        )
    }
}
