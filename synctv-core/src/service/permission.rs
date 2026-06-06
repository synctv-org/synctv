//! Permission management service
//!
//! Centralized permission checking and management with Allow/Deny pattern and caching.
//! Supports multi-replica cache invalidation via Redis Pub/Sub.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    cache::{
        CacheDomain, CacheInvalidationRuntime, CacheL2Backend, CachedMemberPermissionSource,
        ConsistencyCoordinator, FenceReadResult, InvalidationMessage, MemberPermissionCache,
        MemberPermissionKey, NoopCacheL2, RoomSettingsCache, RoomSettingsSnapshot,
        VersionFenceReservation, VersionFenceStore,
    },
    models::{
        RoomId, RoomMember, RoomMemberWithUser, RoomPermission, RoomPermissionSet, RoomRole,
        RoomSettings, UserId,
    },
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

/// Runtime permission defaults captured at the composition boundary of a check.
///
/// Keeping this as plain data lets transaction helpers, response builders, and
/// `PermissionService` all feed the same pure calculator without depending on
/// cache state or repository access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimePermissionDefaults {
    pub admin: RoomPermissionSet,
    pub member: RoomPermissionSet,
    pub guest: RoomPermissionSet,
}

impl RuntimePermissionDefaults {
    #[must_use]
    pub const fn compiled() -> Self {
        Self {
            admin: RoomPermissionSet::default_admin(),
            member: RoomPermissionSet::default_member(),
            guest: RoomPermissionSet::default_guest(),
        }
    }

    #[must_use]
    pub const fn for_role(self, role: &RoomRole) -> RoomPermissionSet {
        match role {
            RoomRole::Creator => RoomPermissionSet::all(),
            RoomRole::Admin => self.admin,
            RoomRole::Member => self.member,
            RoomRole::Guest => self.guest,
        }
    }
}

/// Pure effective permission calculator.
///
/// Inputs are deliberately explicit: runtime defaults, room settings, and the
/// member row. This keeps every permission snapshot path on the same semantics.
#[derive(Debug, Clone, Copy)]
pub struct EffectivePermissionCalculator {
    defaults: RuntimePermissionDefaults,
}

impl EffectivePermissionCalculator {
    #[must_use]
    pub const fn new(defaults: RuntimePermissionDefaults) -> Self {
        Self { defaults }
    }

    #[must_use]
    pub const fn compiled_defaults() -> Self {
        Self::new(RuntimePermissionDefaults::compiled())
    }

    #[must_use]
    pub const fn role_default(
        &self,
        role: &RoomRole,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        match role {
            RoomRole::Creator => RoomPermissionSet::all(),
            RoomRole::Admin => room_settings.admin_permissions(self.defaults.admin),
            RoomRole::Member => room_settings.member_permissions(self.defaults.member),
            RoomRole::Guest => room_settings.guest_permissions(self.defaults.guest),
        }
    }

    #[must_use]
    pub const fn effective_for_member(
        &self,
        member: &RoomMember,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        member.effective_permissions(self.role_default(&member.role, room_settings))
    }

    #[must_use]
    pub fn effective_for_member_with_user(
        &self,
        member: &RoomMemberWithUser,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        member.effective_permissions(self.role_default(&member.role, room_settings))
    }

    #[must_use]
    pub const fn has_permission(
        &self,
        member: &RoomMember,
        room_settings: &RoomSettings,
        permission: RoomPermission,
    ) -> bool {
        if !member.has_permission(permission, RoomPermissionSet::all()) {
            return false;
        }

        self.effective_for_member(member, room_settings)
            .has(permission)
    }
}

/// Permission management service
///
/// Handles permission checking with Allow/Deny pattern, optional caching and role inheritance.
/// When `CacheInvalidationService` is provided, it listens for cross-replica invalidation messages.
#[derive(Debug)]
struct PermissionInvalidationRuntime {
    started: AtomicBool,
    cancel: tokio::sync::Mutex<CancellationToken>,
    listener_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    recovery_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl PermissionInvalidationRuntime {
    fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            cancel: tokio::sync::Mutex::new(CancellationToken::new()),
            listener_handle: tokio::sync::Mutex::new(None),
            recovery_handle: tokio::sync::Mutex::new(None),
        }
    }
}

#[derive(Default)]
struct SharedInvalidationService {
    service: parking_lot::RwLock<Option<Arc<dyn CacheInvalidationRuntime>>>,
}

impl std::fmt::Debug for SharedInvalidationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedInvalidationService")
            .field("configured", &self.service.read().is_some())
            .finish()
    }
}

#[derive(Clone)]
pub struct PermissionService {
    member_repo: RoomMemberRepository,
    room_repo: RoomRepository,
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
    pub version_fence: Option<Arc<dyn VersionFenceStore>>,
    pub member_permission_l2_cache: Option<Arc<dyn CacheL2Backend>>,
    pub member_permission_cache_key_prefix: String,
    pub room_settings_l2_cache: Option<Arc<dyn CacheL2Backend>>,
    pub room_settings_cache_key_prefix: String,
}

impl Default for PermissionServiceRuntime {
    fn default() -> Self {
        Self {
            settings_registry: None,
            cache_size: PermissionService::DEFAULT_CACHE_SIZE,
            cache_ttl_secs: PermissionService::DEFAULT_CACHE_TTL_SECS,
            room_settings_repo: None,
            invalidation_service: None,
            version_fence: None,
            member_permission_l2_cache: None,
            member_permission_cache_key_prefix: "member_permission:".to_string(),
            room_settings_l2_cache: None,
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
    fn build_member_permission_cache(runtime: &PermissionServiceRuntime) -> MemberPermissionCache {
        MemberPermissionCache::new(
            runtime
                .member_permission_l2_cache
                .clone()
                .unwrap_or_else(|| Arc::new(NoopCacheL2)),
            runtime.cache_size,
            runtime.cache_ttl_secs,
            runtime.cache_ttl_secs,
            runtime.member_permission_cache_key_prefix.clone(),
        )
    }

    fn build_room_settings_cache(runtime: &PermissionServiceRuntime) -> RoomSettingsCache {
        RoomSettingsCache::new(
            runtime
                .room_settings_l2_cache
                .clone()
                .unwrap_or_else(|| Arc::new(NoopCacheL2)),
            runtime.cache_size,
            runtime.cache_ttl_secs,
            runtime.cache_ttl_secs,
            runtime.room_settings_cache_key_prefix.clone(),
        )
    }

    /// Create a new permission service with caching
    pub fn new(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        settings_registry: Option<Arc<SettingsRegistry>>,
        cache_size: u64,
        cache_ttl_secs: u64,
    ) -> Result<Self> {
        let room_settings_repo = RoomSettingsRepository::new(room_repo.pool().clone());
        Self::new_with_runtime(
            member_repo,
            room_repo,
            PermissionServiceRuntime {
                settings_registry,
                cache_size,
                cache_ttl_secs,
                room_settings_repo: Some(room_settings_repo),
                ..PermissionServiceRuntime::default()
            },
        )
    }

    /// Create a permission service with all optional runtime collaborators wired
    /// at construction time.
    pub fn new_with_runtime(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        runtime: PermissionServiceRuntime,
    ) -> Result<Self> {
        let version_fence = runtime
            .version_fence
            .clone()
            .unwrap_or_else(|| Arc::new(crate::cache::NoopVersionFenceStore));
        let member_permission_cache = Self::build_member_permission_cache(&runtime);
        let room_settings_cache = Self::build_room_settings_cache(&runtime);

        Ok(Self {
            member_repo,
            room_repo,
            room_settings_repo: runtime.room_settings_repo,
            member_permission_cache,
            room_settings_cache,
            settings_registry: runtime.settings_registry,
            invalidation_service: Arc::new(SharedInvalidationService {
                service: parking_lot::RwLock::new(runtime.invalidation_service),
            }),
            cache_degraded: Arc::new(AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(
                Instant::now()
                    .checked_sub(Duration::from_secs(Self::FLUSH_RATE_LIMIT_SECS))
                    .unwrap_or(Instant::now()),
            )),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
            invalidation_runtime: Arc::new(PermissionInvalidationRuntime::new()),
            consistency: ConsistencyCoordinator::new(version_fence),
        })
    }

    /// Create a permission service without caching
    pub fn without_cache(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        settings_registry: Option<Arc<SettingsRegistry>>,
    ) -> Result<Self> {
        let room_settings_repo = RoomSettingsRepository::new(room_repo.pool().clone());
        Self::new_with_runtime(
            member_repo,
            room_repo,
            PermissionServiceRuntime {
                settings_registry,
                cache_size: 1,
                cache_ttl_secs: 1,
                room_settings_repo: Some(room_settings_repo),
                ..PermissionServiceRuntime::default()
            },
        )
    }

    async fn invalidate_cache_local_only(&self, room_id: &RoomId, user_id: &UserId) {
        let cache_key = MemberPermissionKey::new(*room_id, *user_id);
        if let Err(error) = self.member_permission_cache.invalidate(&cache_key).await {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                error = %error,
                "Failed to invalidate member permission source cache"
            );
        }
    }

    pub(crate) async fn invalidate_room_cache_local_only(&self, room_id: &RoomId) {
        self.member_permission_cache.clear().await;
        if let Err(error) = self.room_settings_cache.invalidate(room_id).await {
            tracing::warn!(
                room_id = %room_id,
                error = %error,
                "Failed to invalidate permission room settings source cache"
            );
        }
    }

    pub(crate) async fn clear_cache_local_only(&self) {
        self.member_permission_cache.clear().await;
        self.room_settings_cache.clear().await;
    }

    fn invalidation_service(&self) -> Option<Arc<dyn CacheInvalidationRuntime>> {
        self.invalidation_service.service.read().clone()
    }

    fn permission_domain(room_id: &RoomId, user_id: &UserId) -> CacheDomain {
        CacheDomain::Permission {
            room_id: *room_id,
            user_id: *user_id,
        }
    }

    fn room_settings_domain(room_id: &RoomId) -> CacheDomain {
        CacheDomain::RoomSettings { room_id: *room_id }
    }

    async fn current_permission_cache_fence(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<PermissionCacheFence>> {
        if !self.consistency.is_authoritative() {
            return Ok(None);
        }

        let user_domain = Self::permission_domain(room_id, user_id);
        let room_settings_domain = Self::room_settings_domain(room_id);
        let versions = self
            .consistency
            .current_versions(&[user_domain.clone(), room_settings_domain.clone()])
            .await?;
        let Some(user_version) = versions.first().and_then(|version| *version) else {
            return Ok(None);
        };
        let Some(room_settings_version) = versions.get(1).and_then(|version| *version) else {
            return Ok(None);
        };

        Ok(Some(PermissionCacheFence {
            user_version,
            room_settings_version,
            user_fence_key: self.consistency.fence_key(&user_domain),
            room_settings_fence_key: self.consistency.fence_key(&room_settings_domain),
        }))
    }

    pub(crate) async fn begin_permission_write(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        db_version: i64,
    ) -> Result<PermissionWriteFence> {
        let domain = Self::permission_domain(room_id, user_id);
        let reservation = self
            .consistency
            .begin_observed_write(&domain, db_version)
            .await?;
        let version = reservation
            .as_ref()
            .map_or(0, |reservation| reservation.version);
        Ok(PermissionWriteFence {
            domain,
            reservation,
            version,
        })
    }

    pub(crate) async fn commit_permission_write(
        &self,
        fence: &PermissionWriteFence,
        version: i64,
    ) -> Result<()> {
        self.consistency
            .commit_reserved_write(&fence.domain, fence.reservation.as_ref(), version)
            .await?;
        Ok(())
    }

    pub(crate) async fn abort_permission_write(&self, fence: &PermissionWriteFence) {
        self.consistency
            .abort_reserved_write(&fence.domain, fence.reservation.as_ref())
            .await;
    }

    async fn advance_permission_fence_to_current_member_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<i64> {
        if !self.consistency.is_authoritative() {
            return Ok(0);
        }

        if let Some(member) = self.member_repo.get(room_id, user_id).await? {
            self.consistency
                .set_version_at_least(&Self::permission_domain(room_id, user_id), member.version)
                .await
        } else {
            let version = self.member_repo.lifecycle_version(room_id, user_id).await?;
            self.consistency
                .set_version_at_least(&Self::permission_domain(room_id, user_id), version)
                .await
        }
    }

    pub(crate) async fn seed_permission_fence_to_member_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        member_version: i64,
    ) -> Result<i64> {
        if !self.consistency.is_authoritative() {
            return Ok(0);
        }

        self.consistency
            .set_version_at_least(&Self::permission_domain(room_id, user_id), member_version)
            .await
    }

    async fn seed_permission_fences_after_strong_read(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        member_version: i64,
        settings_version: i64,
    ) -> Result<()> {
        if !self.consistency.is_authoritative() {
            return Ok(());
        }

        self.consistency
            .set_version_at_least(&Self::permission_domain(room_id, user_id), member_version)
            .await?;
        self.consistency
            .set_version_at_least(&Self::room_settings_domain(room_id), settings_version)
            .await?;
        Ok(())
    }

    pub fn has_invalidation_service(&self) -> bool {
        self.invalidation_service().is_some()
    }

    #[cfg(test)]
    pub(crate) const fn has_settings_registry(&self) -> bool {
        self.settings_registry.is_some()
    }

    #[cfg(test)]
    fn invalidation_tasks_started(&self) -> bool {
        self.invalidation_runtime.started.load(Ordering::Acquire)
    }

    pub async fn start(&self) -> Result<()> {
        let Some(invalidation_service) = self.invalidation_service() else {
            return Ok(());
        };

        if self
            .invalidation_runtime
            .started
            .swap(true, Ordering::AcqRel)
        {
            return Ok(());
        }

        if tokio::runtime::Handle::try_current().is_err() {
            self.invalidation_runtime
                .started
                .store(false, Ordering::Release);
            return Err(Error::Internal(
                "PermissionService::start requires a Tokio runtime".to_string(),
            ));
        }

        let mut receiver = invalidation_service.subscribe();
        let member_permission_cache = self.member_permission_cache.clone();
        let room_settings_cache = self.room_settings_cache.clone();
        let cache_degraded = self.cache_degraded.clone();
        let last_flush_time = self.last_flush_time.clone();
        let degradation_started = self.degradation_started.clone();
        let listener_cancel = self.invalidation_runtime.cancel.lock().await.child_token();

        let listener_handle = crate::spawn::spawn_monitored(
            "permission_invalidation_listener",
            async move {
                loop {
                    tokio::select! {
                        () = listener_cancel.cancelled() => {
                            tracing::info!("Permission invalidation listener shutting down");
                            break;
                        }
                        result = receiver.recv() => {
                            match result {
                                Ok(msg) => {
                                    if cache_degraded.swap(false, Ordering::Release) {
                                        tracing::info!("Permission cache recovered from degraded state");
                                    }
                                    *degradation_started.lock() = None;

                                    match msg {
                                        InvalidationMessage::UserPermission { room_id, user_id } => {
                                            match (room_id.parse::<RoomId>(), user_id.parse::<UserId>()) {
                                                (Ok(room_id), Ok(user_id)) => {
                                                    let cache_key = MemberPermissionKey::new(room_id, user_id);
                                                    if let Err(error) = member_permission_cache.invalidate(&cache_key).await {
                                                        tracing::warn!(
                                                            room_id = %room_id,
                                                            user_id = %user_id,
                                                            error = %error,
                                                            "Failed to invalidate member permission source cache"
                                                        );
                                                    }
                                                }
                                                _ => {
                                                    tracing::warn!(
                                                        room_id = %room_id,
                                                        user_id = %user_id,
                                                        "Ignoring invalid member permission invalidation key"
                                                    );
                                                }
                                            }
                                            tracing::debug!(
                                                room_id = %room_id,
                                                user_id = %user_id,
                                                "Member permission source cache invalidated (cross-replica)"
                                            );
                                        }
                                        InvalidationMessage::RoomPermission { room_id } => {
                                            member_permission_cache.clear().await;
                                            match room_id.parse::<RoomId>() {
                                                Ok(parsed_room_id) => {
                                                    if let Err(error) = room_settings_cache.invalidate(&parsed_room_id).await {
                                                        tracing::warn!(
                                                            room_id = %parsed_room_id,
                                                            error = %error,
                                                            "Failed to invalidate permission room settings source cache"
                                                        );
                                                    }
                                                }
                                                Err(_) => {
                                                    tracing::warn!(
                                                        room_id = %room_id,
                                                        "Ignoring invalid room permission invalidation key"
                                                    );
                                                }
                                            }
                                            tracing::debug!(
                                                room_id = %room_id,
                                                "Room permission source caches invalidated (cross-replica)"
                                            );
                                        }
                                        InvalidationMessage::All => {
                                            member_permission_cache.clear().await;
                                            room_settings_cache.clear().await;
                                            tracing::debug!("All permission source caches invalidated (cross-replica)");
                                        }
                                        _ => {}
                                    }
                                }
                                Err(broadcast::error::RecvError::Closed) => {
                                    tracing::debug!("Invalidation channel closed, stopping listener");
                                    break;
                                }
                                Err(broadcast::error::RecvError::Lagged(n)) => {
                                    let was_degraded = cache_degraded.swap(true, Ordering::Release);
                                    if !was_degraded {
                                        *degradation_started.lock() = Some(Instant::now());
                                        tracing::warn!(
                                            lagged_messages = n,
                                            "Permission cache entered degraded state due to Pub/Sub lag"
                                        );
                                    }

                                    std::sync::atomic::fence(Ordering::SeqCst);

                                    let should_flush = {
                                        let mut last = last_flush_time.lock();
                                        if last.elapsed() >= Duration::from_secs(Self::FLUSH_RATE_LIMIT_SECS) {
                                            *last = Instant::now();
                                            true
                                        } else {
                                            false
                                        }
                                    };

                                    if should_flush {
                                        tracing::warn!(
                                            lagged_messages = n,
                                        "Invalidation listener lagged, flushing all permission source caches"
                                    );
                                        member_permission_cache.clear().await;
                                        room_settings_cache.clear().await;
                                    } else {
                                        tracing::debug!(
                                            lagged_messages = n,
                                            "Invalidation listener lagged, cache flush rate-limited (already degraded)"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            },
        );

        let member_cache_for_recovery = self.member_permission_cache.clone();
        let room_settings_cache_for_recovery = self.room_settings_cache.clone();
        let cache_degraded_for_recovery = self.cache_degraded.clone();
        let degradation_started_for_recovery = self.degradation_started.clone();
        let recovery_cancel = self.invalidation_runtime.cancel.lock().await.child_token();
        let recovery_handle = crate::spawn::spawn_monitored(
            "permission_cache_recovery",
            async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(10));
                loop {
                    tokio::select! {
                        () = recovery_cancel.cancelled() => {
                            tracing::info!("Permission cache recovery task shutting down");
                            break;
                        }
                        _ = ticker.tick() => {
                            if !cache_degraded_for_recovery.load(Ordering::Acquire) {
                                continue;
                            }

                            let should_recover = {
                                let started = degradation_started_for_recovery.lock();
                                started.is_some_and(|start_time| {
                                    start_time.elapsed()
                                        >= Duration::from_secs(Self::MAX_DEGRADATION_DURATION_SECS)
                                })
                            };

                            if !should_recover {
                                continue;
                            }

                            tracing::warn!(
                                "Permission cache degraded for {} seconds, forcing cache refresh before recovery",
                                Self::MAX_DEGRADATION_DURATION_SECS
                            );
                            member_cache_for_recovery.clear().await;
                            room_settings_cache_for_recovery.clear().await;
                            cache_degraded_for_recovery.store(false, Ordering::Release);
                            *degradation_started_for_recovery.lock() = None;
                            tracing::info!(
                                "Permission cache auto-recovered after full cache refresh"
                            );
                        }
                    }
                }
            },
        );

        *self.invalidation_runtime.listener_handle.lock().await = Some(listener_handle);
        *self.invalidation_runtime.recovery_handle.lock().await = Some(recovery_handle);

        Ok(())
    }

    pub async fn shutdown(&self) {
        let cancel = {
            let mut runtime_cancel = self.invalidation_runtime.cancel.lock().await;
            std::mem::replace(&mut *runtime_cancel, CancellationToken::new())
        };
        cancel.cancel();

        let listener_handle = self
            .invalidation_runtime
            .listener_handle
            .lock()
            .await
            .take();
        if let Some(handle) = listener_handle {
            Self::await_invalidation_task_shutdown("permission invalidation listener", handle)
                .await;
        }

        let recovery_handle = self
            .invalidation_runtime
            .recovery_handle
            .lock()
            .await
            .take();
        if let Some(handle) = recovery_handle {
            Self::await_invalidation_task_shutdown("permission cache recovery", handle).await;
        }
        self.invalidation_runtime
            .started
            .store(false, Ordering::Release);
    }

    async fn await_invalidation_task_shutdown(name: &'static str, mut handle: JoinHandle<()>) {
        match tokio::time::timeout(Self::INVALIDATION_TASK_SHUTDOWN_TIMEOUT, &mut handle).await {
            Ok(Ok(())) => info!("{name} stopped"),
            Ok(Err(error)) => warn!(%error, "{name} panicked during shutdown"),
            Err(_) => {
                warn!(
                    timeout_secs = Self::INVALIDATION_TASK_SHUTDOWN_TIMEOUT.as_secs(),
                    "{name} did not stop before timeout; aborting task"
                );
                handle.abort();
                match handle.await {
                    Ok(()) => info!("{name} aborted cleanly"),
                    Err(error) if error.is_cancelled() => info!("{name} aborted"),
                    Err(error) => warn!(%error, "{name} failed after abort"),
                }
            }
        }
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

    /// Get global default permissions for a role from `SettingsRegistry`
    fn get_global_default_permissions(&self, role: &crate::models::RoomRole) -> RoomPermissionSet {
        if let Some(registry) = &self.settings_registry {
            match role {
                crate::models::RoomRole::Admin => registry
                    .admin_default_permissions
                    .get()
                    .map_or(RoomPermissionSet::default_admin(), |permissions| {
                        permissions.bits()
                    }),
                crate::models::RoomRole::Member => registry
                    .member_default_permissions
                    .get()
                    .map_or(RoomPermissionSet::default_member(), |permissions| {
                        permissions.bits()
                    }),
                crate::models::RoomRole::Guest => registry
                    .guest_default_permissions
                    .get()
                    .map_or(RoomPermissionSet::default_guest(), |permissions| {
                        permissions.bits()
                    }),
                crate::models::RoomRole::Creator => RoomPermissionSet::all(),
            }
        } else {
            match role {
                crate::models::RoomRole::Admin => RoomPermissionSet::default_admin(),
                crate::models::RoomRole::Member => RoomPermissionSet::default_member(),
                crate::models::RoomRole::Guest => RoomPermissionSet::default_guest(),
                crate::models::RoomRole::Creator => RoomPermissionSet::all(),
            }
        }
    }

    #[must_use]
    pub fn runtime_permission_defaults(&self) -> RuntimePermissionDefaults {
        RuntimePermissionDefaults {
            admin: self.get_global_default_permissions(&RoomRole::Admin),
            member: self.get_global_default_permissions(&RoomRole::Member),
            guest: self.get_global_default_permissions(&RoomRole::Guest),
        }
    }

    #[must_use]
    pub fn effective_permission_calculator(&self) -> EffectivePermissionCalculator {
        EffectivePermissionCalculator::new(self.runtime_permission_defaults())
    }

    #[must_use]
    pub fn effective_member_permissions(
        &self,
        member: &RoomMember,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        self.effective_permission_calculator()
            .effective_for_member(member, room_settings)
    }

    fn runtime_permission_defaults_strong(&self) -> Result<RuntimePermissionDefaults> {
        let Some(registry) = &self.settings_registry else {
            return Ok(RuntimePermissionDefaults::compiled());
        };

        Ok(RuntimePermissionDefaults {
            admin: registry.admin_default_permissions.get()?.bits(),
            member: registry.member_default_permissions.get()?.bits(),
            guest: registry.guest_default_permissions.get()?.bits(),
        })
    }

    fn effective_member_permissions_strong(
        &self,
        member: &RoomMember,
        room_settings: &RoomSettings,
    ) -> Result<RoomPermissionSet> {
        Ok(
            EffectivePermissionCalculator::new(self.runtime_permission_defaults_strong()?)
                .effective_for_member(member, room_settings),
        )
    }

    #[must_use]
    pub fn effective_member_with_user_permissions(
        &self,
        member: &RoomMemberWithUser,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        self.effective_permission_calculator()
            .effective_for_member_with_user(member, room_settings)
    }

    /// Calculate role default permissions with room-level overrides applied
    ///
    /// This combines:
    /// 1. Global default permissions (from `SettingsRegistry`)
    /// 2. Room-level overrides: (global | `room_added`) & ~`room_removed`
    #[must_use]
    pub fn calculate_role_default_permissions(
        &self,
        role: &crate::models::RoomRole,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        self.effective_permission_calculator()
            .role_default(role, room_settings)
    }

    #[must_use]
    pub fn calculate_role_default_permissions_from_base(
        role: &crate::models::RoomRole,
        room_settings: &RoomSettings,
        global_default: RoomPermissionSet,
    ) -> RoomPermissionSet {
        let defaults = RuntimePermissionDefaults {
            admin: global_default,
            member: global_default,
            guest: global_default,
        };
        EffectivePermissionCalculator::new(defaults).role_default(role, room_settings)
    }

    async fn ensure_room_accepts_member_actions(&self, room_id: &RoomId) -> Result<()> {
        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.is_banned {
            return Err(Error::Authorization("Room is banned".to_string()));
        }

        if !room.status.is_active() {
            return Err(Error::Authorization("Room is not active".to_string()));
        }

        Ok(())
    }

    /// Check if a user has a specific permission in a room
    ///
    /// When the cache is degraded (e.g., due to Pub/Sub lag), uses a short-TTL
    /// fallback cache instead of hitting the database for every request.
    /// This balances correctness (not serving stale data for too long) with
    /// database protection (avoiding cache stampedes during degraded periods).
    pub async fn check_permission(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: RoomPermission,
    ) -> Result<()> {
        self.ensure_room_accepts_member_actions(room_id).await?;

        let permissions = self.get_user_permissions_strong(room_id, user_id).await?;

        if !permissions.has(permission) {
            return Err(Error::Authorization(
                synctv_common::messages::PERMISSION_DENIED.to_string(),
            ));
        }

        Ok(())
    }

    /// Check permission without using cache (for critical operations).
    ///
    /// Use this for security-sensitive operations where permission changes
    /// must be immediately reflected, such as:
    /// - Deleting a room
    /// - Kicking members and changing room/user ban state
    /// - Changing user roles or permissions
    ///
    /// This bypasses the cache and always fetches fresh permissions from the database.
    pub async fn check_permission_no_cache(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: RoomPermission,
    ) -> Result<()> {
        self.ensure_room_accepts_member_actions(room_id).await?;

        let permissions = self.get_user_permissions_no_cache(room_id, user_id).await?;

        if !permissions.has(permission) {
            return Err(Error::Authorization(
                synctv_common::messages::PERMISSION_DENIED.to_string(),
            ));
        }

        Ok(())
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
            .member_repo
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
            .member_repo
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

    /// Check multiple permissions at once
    pub async fn check_permissions(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permissions: &[RoomPermission],
    ) -> Result<()> {
        let user_permissions = self.get_user_permissions_strong(room_id, user_id).await?;

        for &permission in permissions {
            if !user_permissions.has(permission) {
                return Err(Error::Authorization(
                    synctv_common::messages::PERMISSION_DENIED.to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Check if user has a specific role in room
    pub async fn check_role(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        expected_role: crate::models::RoomRole,
    ) -> Result<()> {
        let member = self
            .member_repo
            .get(room_id, user_id)
            .await?
            .ok_or_else(|| {
                Error::Authorization(synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string())
            })?;

        if member.role != expected_role {
            return Err(Error::Authorization("Insufficient permissions".to_string()));
        }

        Ok(())
    }

    /// Check if user is room creator
    pub async fn is_creator(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let member = self.member_repo.get(room_id, user_id).await?;

        Ok(member.is_some_and(|m| m.role == crate::models::RoomRole::Creator))
    }

    /// Check if user is room admin or creator
    pub async fn is_admin_or_creator(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let member = self.member_repo.get(room_id, user_id).await?;

        Ok(member.is_some_and(|m| {
            matches!(
                m.role,
                crate::models::RoomRole::Admin | crate::models::RoomRole::Creator
            )
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheKey;
    use crate::models::permission::Role as RoomRole;
    use crate::models::{
        room_settings::*, RoomAdminPermissionBits, RoomGuestPermissionBits, RoomMember,
        RoomMemberPermissionBits,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;

    #[derive(Default)]
    struct RecordingVersionFenceStore {
        versions: parking_lot::Mutex<HashMap<CacheDomain, i64>>,
    }

    #[async_trait]
    impl VersionFenceStore for RecordingVersionFenceStore {
        async fn current_version(&self, domain: &CacheDomain) -> Result<Option<i64>> {
            Ok(self.versions.lock().get(domain).copied())
        }

        async fn current_versions(&self, domains: &[CacheDomain]) -> Result<Vec<Option<i64>>> {
            let versions = self.versions.lock();
            Ok(domains
                .iter()
                .map(|domain| versions.get(domain).copied())
                .collect())
        }

        async fn bump_version(&self, domain: &CacheDomain) -> Result<i64> {
            let mut versions = self.versions.lock();
            let version = versions.entry(domain.clone()).or_insert(0);
            *version += 1;
            Ok(*version)
        }

        async fn set_version_at_least(&self, domain: &CacheDomain, version: i64) -> Result<i64> {
            let mut versions = self.versions.lock();
            let current = versions.entry(domain.clone()).or_insert(0);
            if version > *current {
                *current = version;
            }
            Ok(*current)
        }

        async fn reserve_next_after_observed_version(
            &self,
            domain: &CacheDomain,
            observed_version: i64,
        ) -> Result<i64> {
            let mut versions = self.versions.lock();
            let current = versions.entry(domain.clone()).or_insert(0);
            if *current > observed_version {
                return Err(Error::OptimisticLockConflict);
            }

            *current = observed_version + 1;
            Ok(*current)
        }

        fn is_authoritative(&self) -> bool {
            true
        }
    }

    fn make_service_from_pool(
        pool: sqlx::PgPool,
        runtime: PermissionServiceRuntime,
    ) -> PermissionService {
        PermissionService::new_with_runtime(
            RoomMemberRepository::new(pool.clone()),
            RoomRepository::new(pool),
            PermissionServiceRuntime {
                cache_size: 10,
                cache_ttl_secs: 60,
                member_permission_cache_key_prefix: "member_permission:".to_string(),
                room_settings_cache_key_prefix: "room_settings:".to_string(),
                ..runtime
            },
        )
        .expect("permission service should build")
    }

    fn make_service_with_runtime(runtime: PermissionServiceRuntime) -> PermissionService {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });
        make_service_from_pool(pool, runtime)
    }

    fn make_service() -> PermissionService {
        make_service_with_runtime(PermissionServiceRuntime::default())
    }

    fn make_service_async_with_runtime(runtime: PermissionServiceRuntime) -> PermissionService {
        let pool = sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap();
        make_service_from_pool(pool, runtime)
    }

    fn make_service_async() -> PermissionService {
        make_service_async_with_runtime(PermissionServiceRuntime::default())
    }

    fn make_member(role: RoomRole) -> RoomMember {
        RoomMember::new(RoomId::expect_positive(1), UserId::expect_positive(1), role)
    }

    #[test]
    fn test_member_permission_cache_key_generation() {
        let room_id = RoomId::expect_positive(123);
        let user_id = UserId::expect_positive(456);
        let key = MemberPermissionKey::new(room_id, user_id);
        assert_eq!(key.cache_key(), "123:456");
    }

    #[tokio::test]
    async fn standalone_permission_constructors_use_non_authoritative_fences_by_default() {
        let pool = sqlx::PgPool::connect_lazy("postgres://unused:5432/unused")
            .expect("lazy postgres pool for unit tests should build");

        let service = PermissionService::new(
            RoomMemberRepository::new(pool.clone()),
            RoomRepository::new(pool.clone()),
            None,
            PermissionService::DEFAULT_CACHE_SIZE,
            PermissionService::DEFAULT_CACHE_TTL_SECS,
        )
        .expect("permission service should build");
        assert!(
            !service.consistency.is_authoritative(),
            "standalone PermissionService::new must not create a private authoritative fence"
        );

        let service = PermissionService::new_with_runtime(
            RoomMemberRepository::new(pool.clone()),
            RoomRepository::new(pool),
            PermissionServiceRuntime::default(),
        )
        .expect("permission service should build");
        assert!(
            !service.consistency.is_authoritative(),
            "new_with_runtime without an explicit shared fence must remain non-authoritative"
        );
    }

    #[test]
    fn test_member_permission_cache_key_different_for_different_users() {
        let room = RoomId::expect_positive(1);
        let u1 = UserId::expect_positive(1);
        let u2 = UserId::expect_positive(2);
        assert_ne!(
            MemberPermissionKey::new(room, u1).cache_key(),
            MemberPermissionKey::new(room, u2).cache_key(),
        );
    }

    #[tokio::test]
    async fn test_removed_member_seed_uses_lifecycle_version_and_invalidation_does_not_advance() {
        let fence = Arc::new(RecordingVersionFenceStore::default());
        let service = make_service_async_with_runtime(PermissionServiceRuntime {
            version_fence: Some(fence.clone()),
            ..PermissionServiceRuntime::default()
        });
        let room_id = RoomId::expect_positive(1);
        let user_id = UserId::expect_positive(2);
        let domain = PermissionService::permission_domain(&room_id, &user_id);

        service
            .seed_permission_fence_to_member_version(&room_id, &user_id, 7)
            .await
            .expect("membership removal fence should seed to lifecycle version");
        service
            .invalidate_removed_member_cache(&room_id, &user_id)
            .await;

        assert_eq!(
            fence
                .current_version(&domain)
                .await
                .expect("fence should be readable"),
            Some(7),
            "post-delete invalidation must not advance beyond the DB lifecycle version"
        );
    }

    #[tokio::test]
    async fn test_added_member_seed_does_not_advance_permission_fence() {
        let fence = Arc::new(RecordingVersionFenceStore::default());
        let service = make_service_async_with_runtime(PermissionServiceRuntime {
            version_fence: Some(fence.clone()),
            ..PermissionServiceRuntime::default()
        });
        let room_id = RoomId::expect_positive(1);
        let user_id = UserId::expect_positive(2);
        let domain = PermissionService::permission_domain(&room_id, &user_id);

        service.seed_added_member_cache(&room_id, &user_id, 0).await;

        assert_eq!(
            fence
                .current_version(&domain)
                .await
                .expect("fence should be readable"),
            Some(0),
            "newly inserted version-0 members must not get an unsatisfiable version-1 fence"
        );
    }

    #[test]
    fn test_member_permission_cache_key_different_for_different_rooms() {
        let r1 = RoomId::expect_positive(1);
        let r2 = RoomId::expect_positive(2);
        let user = UserId::expect_positive(1);
        assert_ne!(
            MemberPermissionKey::new(r1, user).cache_key(),
            MemberPermissionKey::new(r2, user).cache_key(),
        );
    }

    #[test]
    fn test_creator_always_gets_all_permissions() {
        let service = make_service();
        let settings = RoomSettings::default();
        let perms = service.calculate_role_default_permissions(&RoomRole::Creator, &settings);
        assert_eq!(perms.0, RoomPermissionSet::all().0);
    }

    #[test]
    fn test_room_level_add_permissions_for_member() {
        let settings = RoomSettings {
            member_added_permissions: MemberAddedPermissions(RoomMemberPermissionBits::CHAT),
            ..RoomSettings::default()
        };
        let perms = PermissionService::calculate_role_default_permissions_from_base(
            &RoomRole::Member,
            &settings,
            RoomPermissionSet::empty(),
        );
        assert!(perms.has(crate::models::RoomPermission::CHAT));
    }

    #[test]
    fn test_room_level_remove_permissions_for_member() {
        let service = make_service();
        let settings = RoomSettings {
            member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::CHAT),
            ..RoomSettings::default()
        };
        let perms = service.calculate_role_default_permissions(&RoomRole::Member, &settings);
        assert!(!perms.has(crate::models::RoomPermission::CHAT));
        assert!(perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_room_level_add_and_remove_for_admin() {
        let service = make_service();
        let settings = RoomSettings {
            admin_added_permissions: AdminAddedPermissions(RoomAdminPermissionBits::PLAY_CONTROL),
            admin_removed_permissions: AdminRemovedPermissions(
                RoomAdminPermissionBits::KICK_MEMBER,
            ),
            ..RoomSettings::default()
        };
        let perms = service.calculate_role_default_permissions(&RoomRole::Admin, &settings);
        assert!(perms.has(crate::models::RoomPermission::PLAY_CONTROL));
        assert!(!perms.has(crate::models::RoomPermission::KICK_MEMBER));
    }

    #[test]
    fn test_room_overrides_do_not_affect_creator() {
        let service = make_service();
        let settings = RoomSettings {
            admin_removed_permissions: AdminRemovedPermissions(RoomAdminPermissionBits::ALL),
            ..RoomSettings::default()
        };
        let perms = service.calculate_role_default_permissions(&RoomRole::Creator, &settings);
        assert_eq!(perms.0, RoomPermissionSet::all().0);
    }

    #[test]
    fn test_member_allow_pattern() {
        let mut member = make_member(RoomRole::Member);
        member.added_permissions = RoomMemberPermissionBits::CHAT;
        let role_default = RoomPermissionSet::empty();
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(crate::models::RoomPermission::CHAT));
        assert!(!effective.has(crate::models::RoomPermission::KICK_MEMBER));
    }

    #[test]
    fn test_member_deny_pattern() {
        let mut member = make_member(RoomRole::Member);
        member.removed_permissions = RoomMemberPermissionBits::CHAT;
        let role_default = RoomPermissionSet::default_member();
        let effective = member.effective_permissions(role_default);
        assert!(!effective.has(crate::models::RoomPermission::CHAT));
        assert!(effective.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_admin_uses_admin_overrides() {
        let mut member = make_member(RoomRole::Admin);
        member.admin_added_permissions = RoomAdminPermissionBits::PLAY_CONTROL;
        member.admin_removed_permissions = RoomAdminPermissionBits::KICK_MEMBER;
        member.added_permissions = RoomMemberPermissionBits::USE_WEBRTC;

        let role_default = RoomPermissionSet::default_admin();
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(crate::models::RoomPermission::PLAY_CONTROL));
        assert!(!effective.has(crate::models::RoomPermission::KICK_MEMBER));
    }

    #[test]
    fn test_creator_ignores_all_overrides() {
        let mut member = make_member(RoomRole::Creator);
        member.removed_permissions = RoomMemberPermissionBits::ALL;
        member.admin_removed_permissions = RoomAdminPermissionBits::ALL;
        let role_default = RoomPermissionSet::empty();
        let effective = member.effective_permissions(role_default);
        assert_eq!(effective.0, RoomPermissionSet::all().0);
    }

    #[test]
    fn test_guest_allow_deny_pattern() {
        let mut member = make_member(RoomRole::Guest);
        member.added_permissions = RoomGuestPermissionBits::USE_WEBRTC;
        let role_default = RoomPermissionSet::default_guest();
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(crate::models::RoomPermission::USE_WEBRTC));
        assert!(!effective.has(crate::models::RoomPermission::CHAT));
        assert!(!effective.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn test_three_layer_permission_chain() {
        // Layer 2: Room adds USE_WEBRTC, removes CHAT
        let settings = RoomSettings {
            member_added_permissions: MemberAddedPermissions(RoomMemberPermissionBits::USE_WEBRTC),
            member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::CHAT),
            ..RoomSettings::default()
        };
        let role_default = PermissionService::calculate_role_default_permissions_from_base(
            &RoomRole::Member,
            &settings,
            RoomPermissionSet(
                RoomAdminPermissionBits::CHAT | RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE,
            ),
        );
        assert!(role_default.has(crate::models::RoomPermission::USE_WEBRTC));
        assert!(!role_default.has(crate::models::RoomPermission::CHAT));

        // Layer 3: Member re-adds CHAT, removes CREATE_MEDIA_RESOURCE
        let mut member = make_member(RoomRole::Member);
        member.added_permissions = RoomMemberPermissionBits::CHAT;
        member.removed_permissions = RoomMemberPermissionBits::CREATE_MEDIA_RESOURCE;

        let effective = member.effective_permissions(role_default);
        assert!(effective.has(crate::models::RoomPermission::CHAT));
        assert!(!effective.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
        assert!(effective.has(crate::models::RoomPermission::USE_WEBRTC));
        assert!(!effective.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn test_cache_degraded_flag_toggling() {
        let degraded = AtomicBool::new(false);
        degraded.store(true, Ordering::Release);
        assert!(degraded.load(Ordering::Acquire));
        degraded.store(false, Ordering::Release);
        assert!(!degraded.load(Ordering::Acquire));
    }

    #[test]
    fn test_flush_rate_limit_allows_after_interval() {
        let last_flush =
            parking_lot::Mutex::new(Instant::now().checked_sub(Duration::from_secs(20)).unwrap());
        let elapsed = last_flush.lock().elapsed();
        assert!(elapsed >= Duration::from_secs(PermissionService::FLUSH_RATE_LIMIT_SECS));
    }

    #[test]
    fn test_flush_rate_limit_blocks_within_interval() {
        let last_flush = parking_lot::Mutex::new(Instant::now());
        let elapsed = last_flush.lock().elapsed();
        assert!(elapsed < Duration::from_secs(PermissionService::FLUSH_RATE_LIMIT_SECS));
    }

    #[test]
    fn test_has_all_requires_all_bits() {
        let perms = RoomPermissionSet(
            crate::models::RoomAdminPermissionBits::CHAT
                | crate::models::RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE,
        );
        assert!(perms.has_all(RoomPermissionSet(
            crate::models::RoomAdminPermissionBits::CHAT
                | crate::models::RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
        )));
        assert!(!perms.has_all(RoomPermissionSet(
            crate::models::RoomAdminPermissionBits::CHAT
                | crate::models::RoomAdminPermissionBits::KICK_MEMBER
        )));
    }

    #[test]
    fn test_has_any_requires_any_bit() {
        let perms = RoomPermissionSet(crate::models::RoomAdminPermissionBits::CHAT);
        assert!(perms.has_any(RoomPermissionSet(
            crate::models::RoomAdminPermissionBits::CHAT
                | crate::models::RoomAdminPermissionBits::KICK_MEMBER
        )));
        assert!(!perms.has_any(RoomPermissionSet(
            crate::models::RoomAdminPermissionBits::KICK_MEMBER
                | crate::models::RoomAdminPermissionBits::SET_ROOM_SETTINGS
        )));
    }

    #[test]
    fn test_room_rejects_chat_for_guest() {
        let service = make_service();
        let settings = RoomSettings {
            guest_added_permissions: GuestAddedPermissions(1 << 21),
            ..RoomSettings::default()
        };
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(!perms.has(crate::models::RoomPermission::CHAT));
        assert!(!perms.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn test_room_adds_webrtc_for_guest() {
        let service = make_service();
        let settings = RoomSettings {
            guest_added_permissions: GuestAddedPermissions(RoomGuestPermissionBits::USE_WEBRTC),
            ..RoomSettings::default()
        };
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(perms.has(crate::models::RoomPermission::USE_WEBRTC));
        assert!(!perms.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn test_room_removes_view_media_resources_for_guest() {
        let service = make_service();
        let settings = RoomSettings {
            guest_removed_permissions: GuestRemovedPermissions(1 << 21),
            ..RoomSettings::default()
        };
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(!perms.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn test_empty_permissions_has_nothing() {
        let perms = RoomPermissionSet::empty();
        assert!(!perms.has(crate::models::RoomPermission::CHAT));
        assert!(!perms.has_any(RoomPermissionSet::all()));
        assert!(perms.has_all(RoomPermissionSet::empty())); // vacuously true
    }

    #[test]
    fn test_three_layer_guest_chain() {
        let service = make_service();

        // Layer 1: Global defaults for Guest (no media resource permissions)
        // Layer 2: Room adds WebRTC for guests
        let settings = RoomSettings {
            guest_added_permissions: GuestAddedPermissions(RoomGuestPermissionBits::USE_WEBRTC),
            ..RoomSettings::default()
        };
        let role_default = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(!role_default.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
        assert!(role_default.has(crate::models::RoomPermission::USE_WEBRTC));

        // Layer 3: Per-actor removal can still remove guest-level permissions.
        let mut member = make_member(RoomRole::Guest);
        member.removed_permissions = crate::models::RoomGuestPermissionBits::USE_WEBRTC;
        let effective = member.effective_permissions(role_default);
        assert!(!effective.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
        assert!(!effective.has(crate::models::RoomPermission::USE_WEBRTC));
    }

    #[test]
    fn test_three_layer_admin_chain() {
        let service = make_service();

        // Layer 2: Room removes KICK_MEMBER for admins
        let settings = RoomSettings {
            admin_removed_permissions: AdminRemovedPermissions(
                RoomAdminPermissionBits::KICK_MEMBER,
            ),
            ..RoomSettings::default()
        };
        let role_default = service.calculate_role_default_permissions(&RoomRole::Admin, &settings);
        assert!(!role_default.has(crate::models::RoomPermission::KICK_MEMBER));
        assert!(role_default.has(crate::models::RoomPermission::SET_MEMBER_PERMISSIONS));

        // Layer 3: Admin-level re-adds KICK_MEMBER (specific admin override)
        let mut member = make_member(RoomRole::Admin);
        member.admin_added_permissions = RoomAdminPermissionBits::KICK_MEMBER;
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(crate::models::RoomPermission::KICK_MEMBER));
        assert!(effective.has(crate::models::RoomPermission::SET_MEMBER_PERMISSIONS));
    }

    #[test]
    fn test_creator_ignores_member_level_deny() {
        let mut member = make_member(RoomRole::Creator);
        member.removed_permissions = RoomMemberPermissionBits::ALL;
        member.admin_removed_permissions = RoomAdminPermissionBits::ALL;
        member.added_permissions = 0;
        member.admin_added_permissions = 0;

        // Even with everything denied, Creator still has ALL
        let role_default = RoomPermissionSet::empty();
        let effective = member.effective_permissions(role_default);
        assert_eq!(effective.0, RoomPermissionSet::all().0);
    }

    #[test]
    fn test_creator_always_all_regardless_of_room_settings() {
        let service = make_service();
        let settings = RoomSettings {
            admin_removed_permissions: AdminRemovedPermissions(RoomAdminPermissionBits::ALL),
            member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::ALL),
            ..RoomSettings::default()
        };
        let perms = service.calculate_role_default_permissions(&RoomRole::Creator, &settings);
        assert_eq!(perms.0, RoomPermissionSet::all().0);
    }

    #[test]
    fn test_admin_ignores_member_level_added_permissions() {
        let mut member = make_member(RoomRole::Admin);
        // Set member-level overrides (should be ignored for Admin role)
        member.added_permissions = RoomMemberPermissionBits::USE_WEBRTC;
        member.removed_permissions = RoomMemberPermissionBits::CHAT;
        // Admin-level overrides: these should apply
        member.admin_added_permissions = 0;
        member.admin_removed_permissions = 0;

        let role_default = RoomPermissionSet::default_admin();
        let effective = member.effective_permissions(role_default);
        // DEFAULT_MEMBER already includes USE_WEBRTC; member-level grant is redundant and ignored.
        assert!(effective.has(crate::models::RoomPermission::USE_WEBRTC));
        // member-level CHAT deny should NOT apply to admin
        assert!(effective.has(crate::models::RoomPermission::CHAT));
    }

    #[test]
    fn test_member_ignores_admin_level_permissions() {
        let mut member = make_member(RoomRole::Member);
        // Set admin-level overrides (should be ignored for Member role)
        member.admin_added_permissions = 1 << 21;
        member.admin_removed_permissions = RoomAdminPermissionBits::CHAT;
        // Member-level overrides: these should apply
        member.added_permissions = 0;
        member.removed_permissions = 0;

        let role_default = RoomPermissionSet::default_member();
        let effective = member.effective_permissions(role_default);
        // admin-level overrides should NOT apply
        assert!(!effective.has(crate::models::RoomPermission::KICK_MEMBER));
        // admin-level CHAT deny should NOT apply
        assert!(effective.has(crate::models::RoomPermission::CHAT));
    }

    #[test]
    fn test_room_level_add_and_remove_same_permission_for_member() {
        let service = make_service();
        let settings = RoomSettings {
            member_added_permissions: MemberAddedPermissions(RoomMemberPermissionBits::CHAT),
            member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::CHAT),
            ..RoomSettings::default()
        };
        let perms = service.calculate_role_default_permissions(&RoomRole::Member, &settings);
        // Remove is applied after add, so CHAT should be absent
        assert!(!perms.has(crate::models::RoomPermission::CHAT));
    }

    #[test]
    fn test_room_level_add_and_remove_same_permission_for_guest() {
        let service = make_service();
        let settings = RoomSettings {
            guest_added_permissions: GuestAddedPermissions(RoomGuestPermissionBits::USE_WEBRTC),
            guest_removed_permissions: GuestRemovedPermissions(RoomGuestPermissionBits::USE_WEBRTC),
            ..RoomSettings::default()
        };
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        // Remove wins over add
        assert!(!perms.has(crate::models::RoomPermission::USE_WEBRTC));
    }

    #[test]
    fn test_permission_bits_grant_revoke() {
        let mut perms = RoomPermissionSet(0);
        perms.grant(crate::models::RoomPermission::CHAT);
        assert!(perms.has(crate::models::RoomPermission::CHAT));

        perms.grant(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE);
        assert!(perms.has(crate::models::RoomPermission::CHAT));
        assert!(perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));

        perms.revoke(crate::models::RoomPermission::CHAT);
        assert!(!perms.has(crate::models::RoomPermission::CHAT));
        assert!(perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_permission_bits_all_contains_every_named_permission() {
        let all = RoomPermissionSet::all();
        assert!(all.has(crate::models::RoomPermission::CHAT));
        assert!(all.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
        assert!(all.has(crate::models::RoomPermission::KICK_MEMBER));
        assert!(all.has(crate::models::RoomPermission::USE_WEBRTC));
        assert!(all.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
        assert!(all.has(crate::models::RoomPermission::PLAY_CONTROL));
    }

    #[test]
    fn test_has_room_settings_repo_returns_true_when_configured() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let member_pool = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });
        let room_pool = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });
        let settings_pool = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });
        let member_repo = RoomMemberRepository::new(member_pool);
        let room_repo = RoomRepository::new(room_pool);
        let settings_repo = RoomSettingsRepository::new(settings_pool);

        let service = PermissionService::new_with_runtime(
            member_repo,
            room_repo,
            PermissionServiceRuntime {
                room_settings_repo: Some(settings_repo),
                ..PermissionServiceRuntime::default()
            },
        )
        .expect("permission service should build");

        assert!(service.has_room_settings_repo());
    }

    #[test]
    fn test_without_cache_builds_room_settings_repo_from_room_pool() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });
        let member_repo = RoomMemberRepository::new(pool);
        let room_repo = RoomRepository::new(rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        }));

        let service = PermissionService::without_cache(member_repo, room_repo, None)
            .expect("permission service should build");
        assert!(service.has_room_settings_repo());
    }

    #[test]
    fn test_invalidation_service_configured_at_construction_propagates_to_clones() {
        let invalidation_service = Arc::new(crate::cache::CacheInvalidationService::new(
            "permission-clone-node".to_string(),
            "permission-clone-stream".to_string(),
        ));
        let service = make_service_with_runtime(PermissionServiceRuntime {
            invalidation_service: Some(invalidation_service),
            ..PermissionServiceRuntime::default()
        });
        let cloned = service.clone();

        assert!(
            service.has_invalidation_service(),
            "original service must observe the injected invalidation service"
        );
        assert!(
            cloned.has_invalidation_service(),
            "cloned permission services must share the injected invalidation service"
        );
    }

    #[test]
    fn test_first_flush_allowed_immediately_after_startup() {
        let service = make_service();

        // Immediately after construction, a flush should be allowed
        // (last_flush_time is initialized to the past, not Instant::now())
        let elapsed = service.last_flush_time.lock().elapsed();
        assert!(
            elapsed >= Duration::from_secs(PermissionService::FLUSH_RATE_LIMIT_SECS),
            "First flush should be allowed immediately after startup, \
             but elapsed={elapsed:?} < FLUSH_RATE_LIMIT_SECS={}s",
            PermissionService::FLUSH_RATE_LIMIT_SECS
        );
    }

    #[test]
    fn test_flush_rate_limit_blocks_rapid_second_flush() {
        let service = make_service();

        // Simulate a flush happening "now"
        *service.last_flush_time.lock() = Instant::now();

        // Immediately after, a second flush should be blocked
        let elapsed = service.last_flush_time.lock().elapsed();
        assert!(
            elapsed < Duration::from_secs(PermissionService::FLUSH_RATE_LIMIT_SECS),
            "Second flush immediately after first should be blocked"
        );
    }

    fn make_service_with_invalidation() -> (
        PermissionService,
        Arc<crate::cache::CacheInvalidationService>,
    ) {
        use crate::cache::CacheInvalidationService;

        // PgPool::connect_lazy is actually sync, despite its name
        let pool = sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap();
        let member_repo = RoomMemberRepository::new(pool);
        let room_repo = RoomRepository::new(
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap(),
        );

        let invalidation_service = Arc::new(CacheInvalidationService::new(
            // No Redis - local only
            "test-node".to_string(),
            "test-stream".to_string(),
        ));

        let service = PermissionService::new_with_runtime(
            member_repo,
            room_repo,
            PermissionServiceRuntime {
                cache_size: 10,
                cache_ttl_secs: 60,
                invalidation_service: Some(invalidation_service.clone()),
                ..PermissionServiceRuntime::default()
            },
        )
        .expect("permission service should build");

        (service, invalidation_service)
    }

    fn permission_service_with_invalidation(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        invalidation_service: Arc<dyn CacheInvalidationRuntime>,
    ) -> PermissionService {
        PermissionService::new_with_runtime(
            member_repo,
            room_repo,
            PermissionServiceRuntime {
                cache_size: 10,
                cache_ttl_secs: 60,
                invalidation_service: Some(invalidation_service),
                ..PermissionServiceRuntime::default()
            },
        )
        .expect("permission service should build")
    }

    #[tokio::test]
    async fn test_runtime_invalidation_does_not_start_tasks_until_explicit_start() {
        let (service, _invalidation_service) = make_service_with_invalidation();

        assert!(
            !service.invalidation_tasks_started(),
            "permission service construction must not spawn background tasks"
        );

        service.start().await.expect("start should succeed");

        assert!(
            service.invalidation_tasks_started(),
            "start() must mark invalidation tasks as running"
        );

        service.shutdown().await;

        assert!(
            !service.invalidation_tasks_started(),
            "shutdown() must reset invalidation runtime state"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_degraded_mode_auto_recovers_after_timeout_and_flushes_caches() {
        let (service, _invalidation_service) = make_service_with_invalidation();

        service.cache_degraded.store(true, Ordering::Release);
        *service.degradation_started.lock() = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(11))
                .expect("backdating degradation start should succeed"),
        );

        service.start().await.expect("start should succeed");

        tokio::task::yield_now().await;

        assert!(
            !service.cache_degraded.load(Ordering::Acquire),
            "permission source cache should leave degraded mode after the bounded recovery timeout"
        );
        assert!(
            service.degradation_started.lock().is_none(),
            "auto-recovery must clear degradation start time"
        );

        service.shutdown().await;
    }

    #[tokio::test]
    async fn test_degraded_mode_recovers_on_invalidation_message() {
        let (service, invalidation_service) = make_service_with_invalidation();

        service.cache_degraded.store(true, Ordering::Release);
        *service.degradation_started.lock() = Some(Instant::now());

        service.start().await.expect("start should succeed");

        invalidation_service
            .broadcast_all(InvalidationMessage::All)
            .await
            .expect("local invalidation broadcast should succeed");
        tokio::task::yield_now().await;

        assert!(
            !service.cache_degraded.load(Ordering::Acquire),
            "permission cache must leave degraded mode after a real invalidation message arrives"
        );
        assert!(
            service.degradation_started.lock().is_none(),
            "recovery on a real invalidation message must clear degradation start time"
        );

        service.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn test_shutdown_aborts_stuck_invalidation_tasks() {
        let service = make_service_async();
        service
            .invalidation_runtime
            .started
            .store(true, Ordering::Release);

        *service.invalidation_runtime.listener_handle.lock().await = Some(tokio::spawn(async {
            std::future::pending::<()>().await;
        }));

        let shutdown = tokio::spawn({
            let service = service.clone();
            async move {
                service.shutdown().await;
            }
        });

        tokio::task::yield_now().await;
        tokio::time::advance(PermissionService::INVALIDATION_TASK_SHUTDOWN_TIMEOUT).await;
        tokio::task::yield_now().await;

        shutdown
            .await
            .expect("shutdown task should finish after aborting the stuck listener");

        assert!(
            !service.invalidation_runtime.started.load(Ordering::Acquire),
            "shutdown must reset runtime state even when it had to abort a task"
        );
        assert!(
            service
                .invalidation_runtime
                .listener_handle
                .lock()
                .await
                .is_none(),
            "shutdown must drain the stuck listener handle after aborting it"
        );
    }

    #[tokio::test]
    async fn test_start_can_restart_after_shutdown() {
        let (service, invalidation_service) = make_service_with_invalidation();

        service.start().await.expect("initial start should succeed");
        service.shutdown().await;

        service.cache_degraded.store(true, Ordering::Release);
        *service.degradation_started.lock() = Some(Instant::now());

        service
            .start()
            .await
            .expect("restart after shutdown should succeed");

        invalidation_service
            .broadcast_all(InvalidationMessage::All)
            .await
            .expect("local invalidation broadcast should succeed after restart");
        tokio::task::yield_now().await;

        assert!(
            !service.cache_degraded.load(Ordering::Acquire),
            "restart must install fresh listener tasks that can recover degraded mode"
        );

        service.shutdown().await;
    }

    #[test]
    fn test_invalidate_cache_local_clear_works() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (service, _invalidation_service) = make_service_with_invalidation();
            let room_id = RoomId::expect_positive(1);
            let user_id = UserId::expect_positive(1);
            let cache_key = MemberPermissionKey::new(room_id, user_id);

            service
                .member_permission_cache
                .set_if_version_at_least(
                    &cache_key,
                    CachedMemberPermissionSource {
                        room_id,
                        user_id,
                        role: RoomRole::Member,
                        added_permissions: 0,
                        removed_permissions: 0,
                        admin_added_permissions: 0,
                        admin_removed_permissions: 0,
                        version: 1,
                    },
                )
                .await
                .unwrap();
            assert!(service
                .member_permission_cache
                .get_l1(&cache_key)
                .await
                .is_some());

            service.invalidate_cache(&room_id, &user_id).await;

            assert!(service
                .member_permission_cache
                .get_l1(&cache_key)
                .await
                .is_none());
        });
    }

    #[test]
    fn test_invalidate_room_cache_local_clear_works() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (service, invalidation_service) = make_service_with_invalidation();
            let mut receiver = invalidation_service.subscribe();
            let room_id = RoomId::expect_positive(1);

            service.invalidate_room_cache(&room_id).await;

            let result =
                tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver.recv())
                    .await;

            match result {
                Ok(Ok(InvalidationMessage::RoomPermission { room_id: rid })) => {
                    assert_eq!(rid, "1");
                }
                Ok(Ok(other)) => {
                    panic!("Expected RoomPermission message, got {other:?}");
                }
                Ok(Err(e)) => {
                    panic!("Receiver error: {e:?}");
                }
                Err(timeout_error) => {
                    panic!("Timeout waiting for broadcast: {timeout_error:?}");
                }
            }
        });
    }

    #[test]
    fn test_clear_cache_local_clear_works() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (service, _invalidation_service) = make_service_with_invalidation();
            let room_id = RoomId::expect_positive(1);
            let user_id = UserId::expect_positive(1);
            let cache_key = MemberPermissionKey::new(room_id, user_id);

            service
                .member_permission_cache
                .set_if_version_at_least(
                    &cache_key,
                    CachedMemberPermissionSource {
                        room_id,
                        user_id,
                        role: RoomRole::Member,
                        added_permissions: 0,
                        removed_permissions: 0,
                        admin_added_permissions: 0,
                        admin_removed_permissions: 0,
                        version: 1,
                    },
                )
                .await
                .unwrap();
            assert!(service
                .member_permission_cache
                .get_l1(&cache_key)
                .await
                .is_some());

            service.clear_cache().await;

            assert!(service
                .member_permission_cache
                .get_l1(&cache_key)
                .await
                .is_none());
        });
    }

    #[test]
    fn test_invalidate_cache_no_panic_without_invalidation_service() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let service = make_service_async();
            let room_id = RoomId::expect_positive(1);
            let user_id = UserId::expect_positive(1);
            service.invalidate_cache(&room_id, &user_id).await;
        });
    }

    #[test]
    fn test_invalidate_cache_receives_broadcast_after_fix() {
        use crate::cache::{CacheInvalidationService, InvalidationMessage};

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Create a CacheInvalidationService without Redis
            let invalidation_service = Arc::new(CacheInvalidationService::new(
                // No Redis
                "test-node".to_string(),
                "test-stream".to_string(),
            ));

            // Subscribe to receive invalidation messages
            let mut receiver = invalidation_service.subscribe();

            // Create a PermissionService with the invalidation service
            let pool = sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap();
            let member_repo = RoomMemberRepository::new(pool);
            let room_repo = RoomRepository::new(
                sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap(),
            );

            let service = permission_service_with_invalidation(
                member_repo,
                room_repo,
                invalidation_service.clone(),
            );

            let room_id = RoomId::expect_positive(1);
            let user_id = UserId::expect_positive(1);

            // Invalidate the cache - this should broadcast via invalidation_service
            service.invalidate_cache(&room_id, &user_id).await;

            // Try to receive the broadcast message
            // invalidate_cache broadcasts both locally and to Redis. Since there's
            // no Redis here, only local broadcast happens and should be received.
            let result =
                tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver.recv())
                    .await;

            // After the fix, this should receive the message
            match result {
                Ok(Ok(InvalidationMessage::UserPermission {
                    room_id: rid,
                    user_id: uid,
                })) => {
                    assert_eq!(rid, "1");
                    assert_eq!(uid, "1");
                    // Success! The broadcast was received.
                }
                Ok(Ok(other)) => {
                    panic!("Expected UserPermission message, got {other:?}");
                }
                Ok(Err(e)) => {
                    panic!("Receiver error: {e:?}");
                }
                Err(timeout_error) => {
                    panic!(
                        "Timeout waiting for broadcast ({timeout_error:?}) - this indicates \
                         invalidate_cache is not broadcasting locally. It should use \
                         invalidate_and_broadcast_user_permission."
                    );
                }
            }
        });
    }

    #[test]
    fn test_invalidate_room_cache_receives_broadcast_after_fix() {
        use crate::cache::{CacheInvalidationService, InvalidationMessage};

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Create a CacheInvalidationService without Redis
            let invalidation_service = Arc::new(CacheInvalidationService::new(
                // No Redis
                "test-node".to_string(),
                "test-stream".to_string(),
            ));

            // Subscribe to receive invalidation messages
            let mut receiver = invalidation_service.subscribe();

            // Create a PermissionService with the invalidation service
            let pool = sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap();
            let member_repo = RoomMemberRepository::new(pool);
            let room_repo = RoomRepository::new(
                sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap(),
            );

            let service = permission_service_with_invalidation(
                member_repo,
                room_repo,
                invalidation_service.clone(),
            );

            // Invalidate the room cache - this should broadcast via invalidation_service
            let room_id = RoomId::expect_positive(1);
            service.invalidate_room_cache(&room_id).await;

            // Try to receive the broadcast message
            let result =
                tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver.recv())
                    .await;

            // After the fix, this should receive the message
            match result {
                Ok(Ok(InvalidationMessage::RoomPermission { room_id: rid })) => {
                    assert_eq!(rid, "1");
                    // Success! The broadcast was received.
                }
                Ok(Ok(other)) => {
                    panic!("Expected RoomPermission message, got {other:?}");
                }
                Ok(Err(e)) => {
                    panic!("Receiver error: {e:?}");
                }
                Err(timeout_error) => {
                    panic!(
                        "Timeout waiting for broadcast ({timeout_error:?}) - this indicates \
                         invalidate_room_cache is not broadcasting locally. It should use \
                         invalidate_and_broadcast_room_permission."
                    );
                }
            }
        });
    }

    #[test]
    fn test_clear_cache_receives_broadcast_after_fix() {
        use crate::cache::{CacheInvalidationService, InvalidationMessage};

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Create a CacheInvalidationService without Redis
            let invalidation_service = Arc::new(CacheInvalidationService::new(
                // No Redis
                "test-node".to_string(),
                "test-stream".to_string(),
            ));

            // Subscribe to receive invalidation messages
            let mut receiver = invalidation_service.subscribe();

            // Create a PermissionService with the invalidation service
            let pool = sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap();
            let member_repo = RoomMemberRepository::new(pool);
            let room_repo = RoomRepository::new(
                sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap(),
            );

            let service = permission_service_with_invalidation(
                member_repo,
                room_repo,
                invalidation_service.clone(),
            );

            // Clear the cache - this should broadcast via invalidation_service
            service.clear_cache().await;

            // Try to receive the broadcast message
            let result =
                tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver.recv())
                    .await;

            // After the fix, this should receive the message
            match result {
                Ok(Ok(InvalidationMessage::All)) => {
                    // Success! The broadcast was received.
                }
                Ok(Ok(other)) => {
                    panic!("Expected All message, got {other:?}");
                }
                Ok(Err(e)) => {
                    panic!("Receiver error: {e:?}");
                }
                Err(timeout_error) => {
                    panic!(
                        "Timeout waiting for broadcast ({timeout_error:?}) - this indicates \
                         clear_cache is not broadcasting locally. It should use broadcast_all."
                    );
                }
            }
        });
    }
}
