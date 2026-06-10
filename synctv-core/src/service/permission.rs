//! Permission management service
//!
//! Centralized permission checking and management with Allow/Deny pattern and caching.
//! Supports multi-replica cache invalidation via Redis Pub/Sub.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{
    cache::{
        CacheDomain, CacheInvalidationRuntime, CacheL2Backend, CachedMemberPermissionSource,
        ConsistencyCoordinator, FenceReadResult, InvalidationMessage, MemberPermissionCache,
        MemberPermissionKey, RoomSettingsCache, RoomSettingsSnapshot, VersionFenceReservation,
        VersionFenceStore,
    },
    models::{RoomId, RoomPermissionSet, UserId},
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository},
    service::SettingsRegistry,
    Error, Result,
};

#[derive(Debug, Clone)]
struct PermissionCacheFence {
    user_version: i64,
    room_settings_version: i64,
    user_fence_key: Option<String>,
    room_settings_fence_key: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PermissionWriteFence {
    domain: CacheDomain,
    reservation: Option<VersionFenceReservation>,
    version: i64,
}

impl PermissionWriteFence {
    #[must_use]
    pub(crate) const fn version(&self) -> i64 {
        self.version
    }
}

mod cache_fence;
mod calculator;
mod checks;
mod constructor;
mod effective;
mod invalidation_runtime;
pub use calculator::{EffectivePermissionCalculator, RuntimePermissionDefaults};
use invalidation_runtime::{PermissionInvalidationRuntime, SharedInvalidationService};

#[derive(Clone)]
pub struct PermissionService {
    member_repo: Option<RoomMemberRepository>,
    room_repo: Option<RoomRepository>,
    room_settings_repo: Option<RoomSettingsRepository>,
    member_permission_cache: MemberPermissionCache,
    room_settings_cache: RoomSettingsCache,
    settings_registry: Option<Arc<SettingsRegistry>>,
    /// Optional invalidation service for cross-replica cache sync
    invalidation_service: Arc<SharedInvalidationService>,
    /// When true, source caches are considered unreliable due to Pub/Sub lag.
    cache_degraded: Arc<AtomicBool>,
    /// Tracks last `invalidate_all()` time to rate-limit flushes
    last_flush_time: Arc<parking_lot::Mutex<Instant>>,
    /// Tracks when cache degradation started for diagnostics and tests
    degradation_started: Arc<parking_lot::Mutex<Option<Instant>>>,
    /// Shared lifecycle state for invalidation listener tasks.
    invalidation_runtime: Arc<PermissionInvalidationRuntime>,
    consistency: ConsistencyCoordinator,
}

#[derive(Clone)]
pub struct PermissionServiceRuntime {
    pub settings_registry: Option<Arc<SettingsRegistry>>,
    pub cache_size: u64,
    pub cache_ttl_secs: u64,
    pub room_settings_repo: Option<RoomSettingsRepository>,
    pub invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
    pub version_fence: Arc<dyn VersionFenceStore>,
    pub member_permission_l2_cache: Arc<dyn CacheL2Backend>,
    pub member_permission_cache_key_prefix: String,
    pub room_settings_l2_cache: Arc<dyn CacheL2Backend>,
    pub room_settings_cache_key_prefix: String,
}

impl PermissionServiceRuntime {
    #[must_use]
    pub fn local_only() -> Self {
        Self {
            settings_registry: None,
            cache_size: PermissionService::DEFAULT_CACHE_SIZE,
            cache_ttl_secs: PermissionService::DEFAULT_CACHE_TTL_SECS,
            room_settings_repo: None,
            invalidation_service: None,
            version_fence: Arc::new(crate::cache::LocalVersionFenceStore::new()),
            member_permission_l2_cache: Arc::new(crate::cache::NoopCacheL2),
            member_permission_cache_key_prefix: "member_permission:".to_string(),
            room_settings_l2_cache: Arc::new(crate::cache::NoopCacheL2),
            room_settings_cache_key_prefix: "room_settings:".to_string(),
        }
    }
}

impl std::fmt::Debug for PermissionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionService").finish()
    }
}

impl PermissionService {
    /// Default permission cache capacity (max entries)
    pub const DEFAULT_CACHE_SIZE: u64 = 10_000;
    /// Default permission cache TTL in seconds (5 minutes)
    pub const DEFAULT_CACHE_TTL_SECS: u64 = 300;
    /// Maximum time to wait for an invalidation background task to stop.
    const INVALIDATION_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

    /// Minimum interval between `invalidate_all()` calls (seconds)
    const FLUSH_RATE_LIMIT_SECS: u64 = 10;
    /// Maximum duration to remain in degraded mode before forcing a full cache refresh.
    /// After this timeout, both caches are flushed and the primary cache is re-enabled.
    const MAX_DEGRADATION_DURATION_SECS: u64 = 10;

    fn member_repo(&self) -> Result<&RoomMemberRepository> {
        self.member_repo
            .as_ref()
            .ok_or_else(|| Error::Internal("PermissionService has no member repository".into()))
    }

    fn room_repo(&self) -> Result<&RoomRepository> {
        self.room_repo
            .as_ref()
            .ok_or_else(|| Error::Internal("PermissionService has no room repository".into()))
    }

    #[cfg(test)]
    pub(crate) const fn has_settings_registry(&self) -> bool {
        self.settings_registry.is_some()
    }

    /// Check if room settings repository is configured
    ///
    /// Returns `true` if a room settings repository was provided through
    /// `PermissionServiceRuntime`, `false` otherwise.
    ///
    /// When `false`, strong permission checks fail because room settings are
    /// part of the authoritative permission model.
    #[must_use]
    pub const fn has_room_settings_repo(&self) -> bool {
        self.room_settings_repo.is_some()
    }

    /// Log a warning if `room_settings_repo` is not configured
    ///
    /// Call this during application startup to surface invalid service wiring
    /// before authorization requests start failing.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // This example is ignored because PermissionService requires multiple dependencies.
    /// // In practice, use your dependency injection framework to construct the service.
    /// use synctv_core::service::PermissionService;
    ///
    /// // Assuming you have a properly constructed PermissionService:
    /// // permission_service.warn_if_missing_settings_repo();
    /// ```
    pub fn warn_if_missing_settings_repo(&self) {
        if !self.has_room_settings_repo() {
            tracing::warn!(
                "PermissionService started without room_settings_repo; \
                 strong permission checks will fail. \
                 Provide room_settings_repo through PermissionServiceRuntime."
            );
        }
    }

    /// Get user's effective permissions without cache (for critical operations).
    ///
    /// This always fetches from the database to ensure fresh permission state.
    pub async fn get_user_permissions_no_cache(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<RoomPermissionSet> {
        let member = self
            .member_repo()?
            .get(room_id, user_id)
            .await?
            .ok_or_else(|| {
                Error::Authorization(synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string())
            })?;

        // Get room settings for role defaults
        let settings_repo = self.room_settings_repo.as_ref().ok_or_else(|| {
            Error::Internal(
                "PermissionService is missing room_settings_repo for strong permission checks"
                    .to_string(),
            )
        })?;
        let room_settings = settings_repo.get(room_id).await?;

        self.effective_member_permissions_strong(&member, &room_settings)
    }

    async fn load_member_permission_source(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<CachedMemberPermissionSource> {
        let member = self
            .member_repo()?
            .get(room_id, user_id)
            .await?
            .ok_or_else(|| {
                Error::Authorization(synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string())
            })?;
        Ok(CachedMemberPermissionSource::from(&member))
    }

    async fn refresh_member_permission_source(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<CachedMemberPermissionSource> {
        let source = self.load_member_permission_source(room_id, user_id).await?;
        self.consistency
            .repair_after_db_read(&Self::permission_domain(room_id, user_id), source.version)
            .await;
        if let Err(error) = self
            .seed_permission_fence_to_member_version(room_id, user_id, source.version)
            .await
        {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                version = source.version,
                error = %error,
                "Failed to seed permission fence after member source refresh"
            );
        }
        let cache_key = MemberPermissionKey::new(*room_id, *user_id);
        if let Err(error) = self
            .member_permission_cache
            .set_if_version_at_least(&cache_key, source.clone())
            .await
        {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                version = source.version,
                error = %error,
                "Failed to refresh member permission source cache"
            );
        }
        Ok(source)
    }

    async fn get_member_permission_source_by_fence(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        fence_version: i64,
        fence_key: Option<&str>,
    ) -> Result<CachedMemberPermissionSource> {
        let cache_key = MemberPermissionKey::new(*room_id, *user_id);
        let domain = Self::permission_domain(room_id, user_id);
        if let Some(fence_key) = fence_key {
            match self
                .member_permission_cache
                .get_by_fence_key(&cache_key, fence_key)
                .await
            {
                Ok(FenceReadResult::Hit(source)) => return Ok(source),
                Ok(FenceReadResult::DbFallback) => {
                    ConsistencyCoordinator::record_db_fallback(
                        &domain,
                        "stale_member_source_cache",
                    );
                    return self
                        .refresh_member_permission_source(room_id, user_id)
                        .await;
                }
                Ok(FenceReadResult::Unsupported) => {}
                Err(error) => {
                    tracing::warn!(
                        room_id = %room_id,
                        user_id = %user_id,
                        error = %error,
                        "Member permission source fence-key cache read failed; falling back to version read"
                    );
                    ConsistencyCoordinator::record_db_fallback(
                        &domain,
                        "member_source_fence_key_read_error",
                    );
                }
            }
        }

        if let Some(source) = self.member_permission_cache.get_l1(&cache_key).await {
            if source.version >= fence_version {
                return Ok(source);
            }
        }

        match self.member_permission_cache.get_l2(&cache_key).await {
            Ok(Some(source)) if source.version >= fence_version => Ok(source),
            Ok(_) => {
                ConsistencyCoordinator::record_db_fallback(&domain, "stale_member_source_cache");
                self.refresh_member_permission_source(room_id, user_id)
                    .await
            }
            Err(error) => {
                tracing::warn!(
                    room_id = %room_id,
                    user_id = %user_id,
                    error = %error,
                    "Member permission source L2 read failed; bypassing cache"
                );
                ConsistencyCoordinator::record_db_fallback(&domain, "member_source_l2_error");
                self.refresh_member_permission_source(room_id, user_id)
                    .await
            }
        }
    }

    async fn refresh_room_settings_source(&self, room_id: &RoomId) -> Result<RoomSettingsSnapshot> {
        let settings_repo = self.room_settings_repo.as_ref().ok_or_else(|| {
            Error::Internal(
                "PermissionService is missing room_settings_repo for room settings refresh"
                    .to_string(),
            )
        })?;
        let (settings, version) = settings_repo.get_with_version(room_id).await?;
        let snapshot = RoomSettingsSnapshot { settings, version };
        self.consistency
            .repair_after_db_read(&Self::room_settings_domain(room_id), snapshot.version)
            .await;
        if let Err(error) = self
            .room_settings_cache
            .set_if_version_at_least(room_id, snapshot.clone())
            .await
        {
            tracing::warn!(
                room_id = %room_id,
                version = snapshot.version,
                error = %error,
                "Failed to refresh permission room settings source cache"
            );
        }
        Ok(snapshot)
    }

    async fn get_room_settings_source_by_fence(
        &self,
        room_id: &RoomId,
        fence_version: i64,
        fence_key: Option<&str>,
    ) -> Result<RoomSettingsSnapshot> {
        let domain = Self::room_settings_domain(room_id);
        if let Some(fence_key) = fence_key {
            match self
                .room_settings_cache
                .get_by_fence_key(room_id, fence_key)
                .await
            {
                Ok(FenceReadResult::Hit(snapshot)) => return Ok(snapshot),
                Ok(FenceReadResult::DbFallback) => {
                    ConsistencyCoordinator::record_db_fallback(
                        &domain,
                        "stale_room_settings_source_cache",
                    );
                    return self.refresh_room_settings_source(room_id).await;
                }
                Ok(FenceReadResult::Unsupported) => {}
                Err(error) => {
                    tracing::warn!(
                        room_id = %room_id,
                        error = %error,
                        "Permission room settings source fence-key cache read failed; falling back to version read"
                    );
                    ConsistencyCoordinator::record_db_fallback(
                        &domain,
                        "room_settings_source_fence_key_read_error",
                    );
                }
            }
        }

        if let Some(snapshot) = self.room_settings_cache.get_l1(room_id).await {
            if snapshot.version >= fence_version {
                return Ok(snapshot);
            }
        }

        match self.room_settings_cache.get_l2(room_id).await {
            Ok(Some(snapshot)) if snapshot.version >= fence_version => Ok(snapshot),
            Ok(_) => {
                ConsistencyCoordinator::record_db_fallback(
                    &domain,
                    "stale_room_settings_source_cache",
                );
                self.refresh_room_settings_source(room_id).await
            }
            Err(error) => {
                tracing::warn!(
                    room_id = %room_id,
                    error = %error,
                    "Permission room settings source L2 read failed; bypassing cache"
                );
                ConsistencyCoordinator::record_db_fallback(
                    &domain,
                    "room_settings_source_l2_error",
                );
                self.refresh_room_settings_source(room_id).await
            }
        }
    }

    /// Get user's effective permissions in a room with cache-first eventual consistency.
    ///
    /// This is reserved for non-authorization reads and tests that intentionally
    /// need cache-first behavior. Authorization paths must use
    /// [`get_user_permissions_strong`](Self::get_user_permissions_strong) or
    /// [`check_permission`](Self::check_permission).
    pub async fn get_user_permissions_eventually_consistent(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<RoomPermissionSet> {
        let source = self
            .refresh_member_permission_source(room_id, user_id)
            .await?;
        let settings = self.refresh_room_settings_source(room_id).await?;
        self.effective_member_permissions_strong(&source.to_room_member(), &settings.settings)
    }

    /// Get user's effective permissions with strong-read semantics.
    ///
    /// Authorization uses the database as the authoritative source and then
    /// refreshes local cache for eventually-consistent callers.
    pub async fn get_user_permissions_strong(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<RoomPermissionSet> {
        match self.current_permission_cache_fence(room_id, user_id).await {
            Ok(Some(fence)) => {
                let source = self
                    .get_member_permission_source_by_fence(
                        room_id,
                        user_id,
                        fence.user_version,
                        fence.user_fence_key.as_deref(),
                    )
                    .await?;
                let settings = self
                    .get_room_settings_source_by_fence(
                        room_id,
                        fence.room_settings_version,
                        fence.room_settings_fence_key.as_deref(),
                    )
                    .await?;
                return self.effective_member_permissions_strong(
                    &source.to_room_member(),
                    &settings.settings,
                );
            }
            Ok(None) => {
                ConsistencyCoordinator::record_db_fallback(
                    &Self::permission_domain(room_id, user_id),
                    "missing_fence",
                );
            }
            Err(error) => {
                tracing::warn!(
                    room_id = %room_id,
                    user_id = %user_id,
                    error = %error,
                    "Permission version fence unavailable; bypassing cache"
                );
            }
        }

        ConsistencyCoordinator::record_db_fallback(
            &Self::permission_domain(room_id, user_id),
            "stale_or_missing_fence",
        );
        let source = self
            .refresh_member_permission_source(room_id, user_id)
            .await?;
        let settings = self.refresh_room_settings_source(room_id).await?;
        if let Err(error) = self
            .seed_permission_fences_after_strong_read(
                room_id,
                user_id,
                source.version,
                settings.version,
            )
            .await
        {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                error = %error,
                "Failed to seed permission version fences after DB strong read"
            );
        }
        self.effective_member_permissions_strong(&source.to_room_member(), &settings.settings)
    }

    /// Get user's permissions during degraded mode (Pub/Sub lag)
    ///
    /// Uses a separate cache with a much shorter TTL (30 seconds) to balance:
    /// - **Database protection**: Avoid cache stampede during degraded periods
    /// - **Freshness**: Don't serve stale data for too long when invalidation is unreliable
    ///
    /// When the main cache's Pub/Sub is lagging, cross-replica invalidation messages
    /// may be delayed or lost. Using a short TTL ensures that even if invalidation
    /// doesn't work, stale data won't be served for more than 30 seconds.
    pub async fn get_user_permissions_degraded(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<RoomPermissionSet> {
        self.get_user_permissions_strong(room_id, user_id).await
    }

    /// Invalidate cache for a specific user in a room
    ///
    /// If cache invalidation service is configured, this also broadcasts the
    /// invalidation to other replicas via Redis Pub/Sub.
    ///
    /// # Multi-Replica Consistency
    /// The order is: invalidate local cache first, then broadcast to Redis.
    /// This prevents a stale cache window where:
    /// 1. Broadcast succeeds -> other replicas invalidate
    /// 2. Before local invalidation -> cached reads on this node return stale data
    /// 3. After local invalidation -> window closes
    ///
    /// By invalidating locally first, we ensure this node never serves stale
    /// data after the mutation completes, even if the broadcast fails.
    pub async fn invalidate_cache(&self, room_id: &RoomId, user_id: &UserId) {
        if let Err(error) = self
            .advance_permission_fence_to_current_member_version(room_id, user_id)
            .await
        {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                error = %error,
                "Failed to advance permission version fence"
            );
        }
        self.invalidate_cache_local_only(room_id, user_id).await;
        self.broadcast_permission_invalidation(room_id, user_id)
            .await;
    }

    /// Invalidate permission cache after a membership row has been removed.
    ///
    /// Removal paths reserve the deletion fence before committing the DB delete.
    /// Advancing it again after commit would create a fence version that no
    /// member-row snapshot can satisfy, because non-members are not cached as
    /// permission tombstones.
    pub async fn invalidate_removed_member_cache(&self, room_id: &RoomId, user_id: &UserId) {
        self.invalidate_cache_local_only(room_id, user_id).await;
        self.broadcast_permission_invalidation(room_id, user_id)
            .await;
    }

    /// Invalidate caches after a version-fenced member mutation has committed.
    ///
    /// The caller has already reserved and committed the exact permission fence
    /// version. Advancing the fence again here would make the cache require a
    /// version that no committed row/cache entry can satisfy.
    pub async fn invalidate_committed_member_write_cache(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) {
        self.invalidate_cache_local_only(room_id, user_id).await;
        self.broadcast_permission_invalidation(room_id, user_id)
            .await;
    }

    /// Invalidate permission cache after inserting a membership row.
    ///
    /// Inserted members already have a concrete row version. Seeding the fence to
    /// that version lets strong permission reads converge on cache immediately;
    /// bumping here would require a future member mutation before any cached
    /// snapshot could satisfy the fence.
    pub async fn seed_added_member_cache(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        member_version: i64,
    ) {
        if let Err(error) = self
            .seed_permission_fence_to_member_version(room_id, user_id, member_version)
            .await
        {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                member_version,
                error = %error,
                "Failed to seed permission version fence after member insert"
            );
        }
        self.invalidate_cache_local_only(room_id, user_id).await;
        self.broadcast_permission_invalidation(room_id, user_id)
            .await;
    }

    async fn broadcast_permission_invalidation(&self, room_id: &RoomId, user_id: &UserId) {
        // Broadcast to other replicas (best effort)
        // Use invalidate_and_broadcast_user_permission which broadcasts both locally
        // (for other local subscribers) AND to Redis (for remote replicas).
        // This is important for multi-replica scenarios where other replicas need
        // to invalidate their caches.
        let invalidation_service = self.invalidation_service();
        if let Some(service) = invalidation_service {
            if let Err(e) = service
                .invalidate_and_broadcast_user_permission(room_id, user_id)
                .await
            {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    user_id = %user_id,
                    "Failed to broadcast permission cache invalidation to other replicas"
                );
                // Local cache is already invalidated, so this node is consistent.
                // Other replicas may have a brief stale window until their TTL expires.
            }
        }
    }

    /// Invalidate permission cache for all users in a room.
    /// Called when room-level permission settings change (e.g., admin/member/guest
    /// added/removed permissions), since these affect all members' effective permissions.
    /// Correctness comes from the room settings version fence, which strong
    /// reads validate alongside the user-specific permission fence.
    ///
    /// If cache invalidation service is configured, this also broadcasts the
    /// invalidation to other replicas via Redis Pub/Sub.
    pub async fn invalidate_room_cache(&self, room_id: &RoomId) {
        self.invalidate_room_cache_local_only(room_id).await;

        // Broadcast to other replicas (best effort)
        // Use invalidate_and_broadcast_room_permission which broadcasts both locally
        // AND to Redis for remote replicas.
        let invalidation_service = self.invalidation_service();
        if let Some(service) = invalidation_service {
            if let Err(e) = service
                .invalidate_and_broadcast_room_permission(room_id)
                .await
            {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    "Failed to broadcast room permission cache invalidation to other replicas"
                );
            }
        }
    }

    /// Clear all permission cache
    ///
    /// If cache invalidation service is configured, this also broadcasts the
    /// invalidation to other replicas via Redis Pub/Sub.
    pub async fn clear_cache(&self) {
        self.clear_cache_local_only().await;

        // Broadcast to other replicas (best effort)
        // Use broadcast_all which broadcasts both locally AND to Redis.
        let invalidation_service = self.invalidation_service();
        if let Some(service) = invalidation_service {
            if let Err(e) = service.broadcast_all(InvalidationMessage::All).await {
                tracing::warn!(
                    error = %e,
                    "Failed to broadcast full permission cache invalidation to other replicas"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
