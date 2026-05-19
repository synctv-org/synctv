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
    cache::{CacheInvalidationRuntime, InvalidationMessage},
    models::{
        PermissionBits, RoomId, RoomMember, RoomMemberWithUser, RoomRole, RoomSettings, UserId,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository},
    service::SettingsRegistry,
    Error, Result,
};

/// Runtime permission defaults captured at the composition boundary of a check.
///
/// Keeping this as plain data lets transaction helpers, response builders, and
/// `PermissionService` all feed the same pure calculator without depending on
/// cache state or repository access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimePermissionDefaults {
    pub admin: PermissionBits,
    pub member: PermissionBits,
    pub guest: PermissionBits,
}

impl RuntimePermissionDefaults {
    #[must_use]
    pub const fn compiled() -> Self {
        Self {
            admin: PermissionBits(PermissionBits::DEFAULT_ADMIN),
            member: PermissionBits(PermissionBits::DEFAULT_MEMBER),
            guest: PermissionBits(PermissionBits::DEFAULT_GUEST),
        }
    }

    #[must_use]
    pub const fn for_role(self, role: &RoomRole) -> PermissionBits {
        match role {
            RoomRole::Creator => PermissionBits(PermissionBits::ALL),
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
    ) -> PermissionBits {
        match role {
            RoomRole::Creator => PermissionBits(PermissionBits::ALL),
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
    ) -> PermissionBits {
        member.effective_permissions(self.role_default(&member.role, room_settings))
    }

    #[must_use]
    pub fn effective_for_member_with_user(
        &self,
        member: &RoomMemberWithUser,
        room_settings: &RoomSettings,
    ) -> PermissionBits {
        member.effective_permissions(self.role_default(&member.role, room_settings))
    }

    #[must_use]
    pub const fn has_permission(
        &self,
        member: &RoomMember,
        room_settings: &RoomSettings,
        permission: u64,
    ) -> bool {
        if !member.has_permission(permission, PermissionBits(PermissionBits::ALL)) {
            return false;
        }

        self.effective_for_member(member, room_settings)
            .has_all(permission)
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
    cache: Arc<moka::future::Cache<String, PermissionBits>>,
    /// Short-term fallback cache used during degraded mode (Pub/Sub lag).
    /// Has a much shorter TTL (30s) than the main cache to balance:
    /// - Reducing database load during degraded periods
    /// - Not serving stale data for too long when invalidation is unreliable
    degraded_cache: Arc<moka::future::Cache<String, PermissionBits>>,
    settings_registry: Option<Arc<SettingsRegistry>>,
    /// Optional invalidation service for cross-replica cache sync
    invalidation_service: Arc<SharedInvalidationService>,
    /// When true, cache is considered unreliable due to Pub/Sub lag;
    /// all permission checks use `degraded_cache` with short TTL.
    cache_degraded: Arc<AtomicBool>,
    /// Tracks last `invalidate_all()` time to rate-limit flushes
    last_flush_time: Arc<parking_lot::Mutex<Instant>>,
    /// Tracks when cache degradation started for diagnostics and tests
    degradation_started: Arc<parking_lot::Mutex<Option<Instant>>>,
    /// Shared lifecycle state for invalidation listener tasks.
    invalidation_runtime: Arc<PermissionInvalidationRuntime>,
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
    /// TTL for the degraded cache (seconds)
    /// Short enough to not serve stale data for too long, but long enough
    /// to significantly reduce database load during degraded periods.
    const DEGRADED_CACHE_TTL_SECS: u64 = 30;

    /// Create a new permission service with caching
    #[must_use]
    pub fn new(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        settings_registry: Option<Arc<SettingsRegistry>>,
        cache_size: u64,
        cache_ttl_secs: u64,
    ) -> Self {
        Self {
            member_repo,
            room_repo,
            room_settings_repo: None, // Will be set later if needed
            cache: Arc::new(
                moka::future::CacheBuilder::new(cache_size)
                    .time_to_live(Duration::from_secs(cache_ttl_secs))
                    .build(),
            ),
            degraded_cache: Arc::new(
                moka::future::CacheBuilder::new(cache_size)
                    .time_to_live(Duration::from_secs(Self::DEGRADED_CACHE_TTL_SECS))
                    .build(),
            ),
            settings_registry,
            invalidation_service: Arc::new(SharedInvalidationService::default()),
            cache_degraded: Arc::new(AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(
                Instant::now()
                    .checked_sub(Duration::from_secs(Self::FLUSH_RATE_LIMIT_SECS))
                    .unwrap_or(Instant::now()),
            )),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
            invalidation_runtime: Arc::new(PermissionInvalidationRuntime::new()),
        }
    }

    /// Create a permission service with all optional runtime collaborators wired
    /// at construction time.
    #[must_use]
    pub fn new_with_runtime(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        settings_registry: Option<Arc<SettingsRegistry>>,
        cache_size: u64,
        cache_ttl_secs: u64,
        room_settings_repo: Option<RoomSettingsRepository>,
        invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
    ) -> Self {
        Self {
            member_repo,
            room_repo,
            room_settings_repo,
            cache: Arc::new(
                moka::future::CacheBuilder::new(cache_size)
                    .time_to_live(Duration::from_secs(cache_ttl_secs))
                    .build(),
            ),
            degraded_cache: Arc::new(
                moka::future::CacheBuilder::new(cache_size)
                    .time_to_live(Duration::from_secs(Self::DEGRADED_CACHE_TTL_SECS))
                    .build(),
            ),
            settings_registry,
            invalidation_service: Arc::new(SharedInvalidationService {
                service: parking_lot::RwLock::new(invalidation_service),
            }),
            cache_degraded: Arc::new(AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(
                Instant::now()
                    .checked_sub(Duration::from_secs(Self::FLUSH_RATE_LIMIT_SECS))
                    .unwrap_or(Instant::now()),
            )),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
            invalidation_runtime: Arc::new(PermissionInvalidationRuntime::new()),
        }
    }

    /// Create a new permission service with cache invalidation support
    ///
    /// This enables cross-replica cache invalidation via Redis Pub/Sub.
    /// When one node invalidates a permission cache, all other nodes are notified.
    ///
    /// On Pub/Sub lag, `invalidate_all()` is rate-limited to at most once per
    /// `FLUSH_RATE_LIMIT_SECS` seconds. Between flushes, the service falls back
    /// to `check_permission_no_cache` for all requests to avoid cache storms.
    #[must_use]
    pub fn with_invalidation(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        settings_registry: Option<Arc<SettingsRegistry>>,
        cache_size: u64,
        cache_ttl_secs: u64,
        invalidation_service: Arc<dyn CacheInvalidationRuntime>,
    ) -> Self {
        Self::new_with_runtime(
            member_repo,
            room_repo,
            settings_registry,
            cache_size,
            cache_ttl_secs,
            None,
            Some(invalidation_service),
        )
    }

    /// Create a permission service without caching
    #[must_use]
    pub fn without_cache(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        settings_registry: Option<Arc<SettingsRegistry>>,
    ) -> Self {
        Self {
            member_repo,
            room_repo,
            room_settings_repo: None,
            cache: Arc::new(
                moka::future::CacheBuilder::new(1)
                    .time_to_live(Duration::from_secs(1))
                    .build(),
            ),
            degraded_cache: Arc::new(
                moka::future::CacheBuilder::new(1)
                    .time_to_live(Duration::from_secs(1))
                    .build(),
            ),
            settings_registry,
            invalidation_service: Arc::new(SharedInvalidationService::default()),
            cache_degraded: Arc::new(AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(
                Instant::now()
                    .checked_sub(Duration::from_secs(Self::FLUSH_RATE_LIMIT_SECS))
                    .unwrap_or(Instant::now()),
            )),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
            invalidation_runtime: Arc::new(PermissionInvalidationRuntime::new()),
        }
    }

    async fn invalidate_cache_local_only(&self, room_id: &RoomId, user_id: &UserId) {
        let cache_key = Self::cache_key(room_id, user_id);
        self.cache.invalidate(&cache_key).await;
        self.degraded_cache.invalidate(&cache_key).await;
    }

    pub(crate) fn invalidate_room_cache_local_only(&self, room_id: &RoomId) {
        let prefix = format!("perm:room:{room_id}:user:");
        let _ = self.cache.invalidate_entries_if({
            let prefix = prefix.clone();
            move |key, _| key.starts_with(&prefix)
        });
        let _ = self
            .degraded_cache
            .invalidate_entries_if(move |key, _| key.starts_with(&prefix));
    }

    pub(crate) fn clear_cache_local_only(&self) {
        self.cache.invalidate_all();
        self.degraded_cache.invalidate_all();
    }

    fn invalidation_service(&self) -> Option<Arc<dyn CacheInvalidationRuntime>> {
        self.invalidation_service.service.read().clone()
    }

    pub fn set_invalidation_service(&mut self, service: Arc<dyn CacheInvalidationRuntime>) {
        *self.invalidation_service.service.write() = Some(service);
    }

    pub fn has_invalidation_service(&self) -> bool {
        self.invalidation_service().is_some()
    }

    /// Replace the global settings registry used for role defaults.
    pub fn set_settings_registry(&mut self, registry: Arc<SettingsRegistry>) {
        self.settings_registry = Some(registry);
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
        let cache = self.cache.clone();
        let degraded_cache = self.degraded_cache.clone();
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
                                            let cache_key = format!("perm:room:{room_id}:user:{user_id}");
                                            cache.invalidate(&cache_key).await;
                                            degraded_cache.invalidate(&cache_key).await;
                                            tracing::debug!(
                                                room_id = %room_id,
                                                user_id = %user_id,
                                                "Permission cache invalidated (cross-replica)"
                                            );
                                        }
                                        InvalidationMessage::RoomPermission { room_id } => {
                                            let prefix = format!("perm:room:{room_id}:user:");
                                            let _ = cache.invalidate_entries_if({
                                                let prefix = prefix.clone();
                                                move |key, _| key.starts_with(&prefix)
                                            });
                                            let _ = degraded_cache.invalidate_entries_if(move |key, _| key.starts_with(&prefix));
                                            tracing::debug!(
                                                room_id = %room_id,
                                                "Room permission cache invalidated (cross-replica)"
                                            );
                                        }
                                        InvalidationMessage::All => {
                                            cache.invalidate_all();
                                            degraded_cache.invalidate_all();
                                            tracing::debug!("All permission cache invalidated (cross-replica)");
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
                                            "Invalidation listener lagged, flushing all cached permissions"
                                        );
                                        cache.invalidate_all();
                                        degraded_cache.invalidate_all();
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

        let cache_for_recovery = self.cache.clone();
        let degraded_cache_for_recovery = self.degraded_cache.clone();
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
                            cache_for_recovery.invalidate_all();
                            degraded_cache_for_recovery.invalidate_all();
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

    /// Set the room settings repository
    pub fn set_room_settings_repo(&mut self, repo: RoomSettingsRepository) {
        self.room_settings_repo = Some(repo);
    }

    /// Check if room settings repository is configured
    ///
    /// Returns `true` if a room settings repository has been set via
    /// `set_room_settings_repo()`, `false` otherwise.
    ///
    /// When `false`, permission checks will fall back to default `RoomSettings`,
    /// which may ignore room-specific permission customizations.
    #[must_use]
    pub const fn has_room_settings_repo(&self) -> bool {
        self.room_settings_repo.is_some()
    }

    /// Log a warning if `room_settings_repo` is not configured
    ///
    /// Call this during application startup to ensure operators are aware
    /// that room-specific permission settings will be ignored.
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
                 all rooms will use default permission settings. \
                 Call set_room_settings_repo() to enable room-specific permissions."
            );
        }
    }

    /// Get global default permissions for a role from `SettingsRegistry`
    fn get_global_default_permissions(&self, role: &crate::models::RoomRole) -> PermissionBits {
        if let Some(registry) = &self.settings_registry {
            match role {
                crate::models::RoomRole::Admin => PermissionBits(
                    registry
                        .admin_default_permissions
                        .get()
                        .map_or(PermissionBits::DEFAULT_ADMIN, |permissions| {
                            permissions.bits().0
                        }),
                ),
                crate::models::RoomRole::Member => PermissionBits(
                    registry
                        .member_default_permissions
                        .get()
                        .map_or(PermissionBits::DEFAULT_MEMBER, |permissions| {
                            permissions.bits().0
                        }),
                ),
                crate::models::RoomRole::Guest => PermissionBits(
                    registry
                        .guest_default_permissions
                        .get()
                        .map_or(PermissionBits::DEFAULT_GUEST, |permissions| {
                            permissions.bits().0
                        }),
                ),
                crate::models::RoomRole::Creator => {
                    PermissionBits(crate::models::PermissionBits::ALL)
                }
            }
        } else {
            // Fallback to PermissionBits::DEFAULT_* constants if SettingsRegistry not available
            match role {
                crate::models::RoomRole::Admin => PermissionBits(PermissionBits::DEFAULT_ADMIN),
                crate::models::RoomRole::Member => PermissionBits(PermissionBits::DEFAULT_MEMBER),
                crate::models::RoomRole::Guest => PermissionBits(PermissionBits::DEFAULT_GUEST),
                crate::models::RoomRole::Creator => PermissionBits(PermissionBits::ALL),
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
    ) -> PermissionBits {
        self.effective_permission_calculator()
            .effective_for_member(member, room_settings)
    }

    #[must_use]
    pub fn effective_member_with_user_permissions(
        &self,
        member: &RoomMemberWithUser,
        room_settings: &RoomSettings,
    ) -> PermissionBits {
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
    ) -> PermissionBits {
        self.effective_permission_calculator()
            .role_default(role, room_settings)
    }

    #[must_use]
    pub fn calculate_role_default_permissions_from_base(
        role: &crate::models::RoomRole,
        room_settings: &RoomSettings,
        global_default: PermissionBits,
    ) -> PermissionBits {
        let defaults = RuntimePermissionDefaults {
            admin: global_default,
            member: global_default,
            guest: global_default,
        };
        EffectivePermissionCalculator::new(defaults).role_default(role, room_settings)
    }

    /// Generate cache key for room + user with namespace prefix
    ///
    /// Format: `perm:room:<room_id>:user:<user_id>`
    /// The namespace prefix prevents collisions with other cache types and
    /// ensures room/user ID pairs are always unique even if IDs overlap.
    fn cache_key(room_id: &RoomId, user_id: &UserId) -> String {
        format!("perm:room:{room_id}:user:{user_id}")
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
        permission: u64,
    ) -> Result<()> {
        self.ensure_room_accepts_member_actions(room_id).await?;

        let permissions = if self.cache_degraded.load(Ordering::Acquire) {
            // Use degraded cache with short TTL instead of no cache at all
            self.get_user_permissions_degraded(room_id, user_id).await?
        } else {
            self.get_user_permissions(room_id, user_id).await?
        };

        if !permissions.has_all(permission) {
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
        permission: u64,
    ) -> Result<()> {
        self.ensure_room_accepts_member_actions(room_id).await?;

        let permissions = self.get_user_permissions_no_cache(room_id, user_id).await?;

        if !permissions.has_all(permission) {
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
    ) -> Result<PermissionBits> {
        // Fetch from database directly, bypassing cache
        let member = self
            .member_repo
            .get(room_id, user_id)
            .await?
            .ok_or_else(|| {
                Error::Authorization(synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string())
            })?;

        // Get room settings for role defaults
        let room_settings = if let Some(ref settings_repo) = self.room_settings_repo {
            settings_repo.get(room_id).await?
        } else {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                "room_settings_repo not configured, using default RoomSettings; \
                 room-specific permission settings will be ignored"
            );
            RoomSettings::default()
        };

        Ok(self.effective_member_permissions(&member, &room_settings))
    }

    /// Get user's effective permissions in a room (with caching)
    ///
    /// This implements the Allow/Deny permission pattern:
    /// `effective_permissions` = (`role_default` | added) & ~removed
    pub async fn get_user_permissions(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<PermissionBits> {
        let cache_key = Self::cache_key(room_id, user_id);

        // Check cache first
        if let Some(permissions) = self.cache.get(&cache_key).await {
            return Ok(permissions);
        }

        // Fetch from database
        let member = self
            .member_repo
            .get(room_id, user_id)
            .await?
            .ok_or_else(|| {
                Error::Authorization(synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string())
            })?;

        // Get room settings for role defaults
        let room_settings = if let Some(ref settings_repo) = self.room_settings_repo {
            settings_repo.get(room_id).await?
        } else {
            tracing::warn!(
                room_id = %room_id,
                user_id = %user_id,
                "room_settings_repo not configured, using default RoomSettings; \
                 room-specific permission settings will be ignored"
            );
            RoomSettings::default()
        };

        let permissions = self.effective_member_permissions(&member, &room_settings);

        // Update cache
        self.cache.insert(cache_key, permissions).await;

        Ok(permissions)
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
    ) -> Result<PermissionBits> {
        let cache_key = Self::cache_key(room_id, user_id);

        // Check degraded cache first
        if let Some(permissions) = self.degraded_cache.get(&cache_key).await {
            return Ok(permissions);
        }

        // Fetch from database
        let member = self
            .member_repo
            .get(room_id, user_id)
            .await?
            .ok_or_else(|| {
                Error::Authorization(synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string())
            })?;

        // Get room settings for role defaults
        let room_settings = if let Some(ref settings_repo) = self.room_settings_repo {
            settings_repo.get(room_id).await?
        } else {
            RoomSettings::default()
        };

        let permissions = self.effective_member_permissions(&member, &room_settings);

        // Update degraded cache with short TTL
        self.degraded_cache.insert(cache_key, permissions).await;

        Ok(permissions)
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
        self.invalidate_cache_local_only(room_id, user_id).await;

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
    ///
    /// If cache invalidation service is configured, this also broadcasts the
    /// invalidation to other replicas via Redis Pub/Sub.
    pub async fn invalidate_room_cache(&self, room_id: &RoomId) {
        self.invalidate_room_cache_local_only(room_id);

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
        self.clear_cache_local_only();

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
        permissions: &[u64],
    ) -> Result<()> {
        let user_permissions = self.get_user_permissions(room_id, user_id).await?;

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
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::models::permission::Role as RoomRole;
    use crate::models::{room_settings::*, RoomMember};

    // Helper to create a PermissionService using tokio runtime for PgPool
    // Note: This function should NOT be called from within an async context
    // (e.g., inside rt.block_on()). Use make_service_async() instead.
    fn make_service() -> PermissionService {
        // PgPool::connect_lazy requires a tokio runtime context
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });
        PermissionService {
            member_repo: RoomMemberRepository::new(pool.clone()),
            room_repo: RoomRepository::new(pool),
            room_settings_repo: None,
            cache: Arc::new(
                moka::future::CacheBuilder::new(10)
                    .time_to_live(Duration::from_mins(1))
                    .build(),
            ),
            degraded_cache: Arc::new(
                moka::future::CacheBuilder::new(10)
                    .time_to_live(Duration::from_secs(
                        PermissionService::DEGRADED_CACHE_TTL_SECS,
                    ))
                    .build(),
            ),
            settings_registry: None,
            invalidation_service: Arc::new(SharedInvalidationService::default()),
            cache_degraded: Arc::new(AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(
                Instant::now()
                    .checked_sub(Duration::from_secs(
                        PermissionService::FLUSH_RATE_LIMIT_SECS,
                    ))
                    .unwrap_or(Instant::now()),
            )),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
            invalidation_runtime: Arc::new(PermissionInvalidationRuntime::new()),
        }
    }

    // Helper to create a PermissionService within an async context
    // This should be called from inside rt.block_on() or async tests
    fn make_service_async() -> PermissionService {
        // PgPool::connect_lazy requires a tokio runtime context
        // When called from within an async context, we can use the current context
        let pool = sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap();
        PermissionService {
            member_repo: RoomMemberRepository::new(pool.clone()),
            room_repo: RoomRepository::new(pool),
            room_settings_repo: None,
            cache: Arc::new(
                moka::future::CacheBuilder::new(10)
                    .time_to_live(Duration::from_mins(1))
                    .build(),
            ),
            degraded_cache: Arc::new(
                moka::future::CacheBuilder::new(10)
                    .time_to_live(Duration::from_secs(
                        PermissionService::DEGRADED_CACHE_TTL_SECS,
                    ))
                    .build(),
            ),
            settings_registry: None,
            invalidation_service: Arc::new(SharedInvalidationService::default()),
            cache_degraded: Arc::new(AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(
                Instant::now()
                    .checked_sub(Duration::from_secs(
                        PermissionService::FLUSH_RATE_LIMIT_SECS,
                    ))
                    .unwrap_or(Instant::now()),
            )),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
            invalidation_runtime: Arc::new(PermissionInvalidationRuntime::new()),
        }
    }

    fn make_member(role: RoomRole) -> RoomMember {
        RoomMember::new(RoomId::expect_positive(1), UserId::expect_positive(1), role)
    }

    #[test]
    fn test_cache_key_generation() {
        let room_id = RoomId::expect_positive(123);
        let user_id = UserId::expect_positive(456);
        let key = PermissionService::cache_key(&room_id, &user_id);
        assert_eq!(key, "perm:room:123:user:456");
    }

    #[test]
    fn test_cache_key_different_for_different_users() {
        let room = RoomId::expect_positive(1);
        let u1 = UserId::expect_positive(1);
        let u2 = UserId::expect_positive(2);
        assert_ne!(
            PermissionService::cache_key(&room, &u1),
            PermissionService::cache_key(&room, &u2),
        );
    }

    #[test]
    fn test_cache_key_different_for_different_rooms() {
        let r1 = RoomId::expect_positive(1);
        let r2 = RoomId::expect_positive(2);
        let user = UserId::expect_positive(1);
        assert_ne!(
            PermissionService::cache_key(&r1, &user),
            PermissionService::cache_key(&r2, &user),
        );
    }

    #[test]
    fn test_creator_always_gets_all_permissions() {
        let service = make_service();
        let settings = RoomSettings::default();
        let perms = service.calculate_role_default_permissions(&RoomRole::Creator, &settings);
        assert_eq!(perms.0, PermissionBits::ALL);
    }

    #[test]
    fn test_room_level_add_permissions_for_member() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.member_added_permissions = MemberAddedPermissions(PermissionBits::PLAY_CONTROL);
        let perms = service.calculate_role_default_permissions(&RoomRole::Member, &settings);
        assert!(perms.has(PermissionBits::PLAY_CONTROL));
        assert!(perms.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_room_level_remove_permissions_for_member() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.member_removed_permissions = MemberRemovedPermissions(PermissionBits::SEND_CHAT);
        let perms = service.calculate_role_default_permissions(&RoomRole::Member, &settings);
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_room_level_add_and_remove_for_admin() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.admin_added_permissions = AdminAddedPermissions(PermissionBits::PLAY_CONTROL);
        settings.admin_removed_permissions = AdminRemovedPermissions(PermissionBits::KICK_MEMBER);
        let perms = service.calculate_role_default_permissions(&RoomRole::Admin, &settings);
        assert!(perms.has(PermissionBits::PLAY_CONTROL));
        assert!(!perms.has(PermissionBits::KICK_MEMBER));
    }

    #[test]
    fn test_room_overrides_do_not_affect_creator() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.admin_removed_permissions = AdminRemovedPermissions(PermissionBits::ALL);
        let perms = service.calculate_role_default_permissions(&RoomRole::Creator, &settings);
        assert_eq!(perms.0, PermissionBits::ALL);
    }

    #[test]
    fn test_member_allow_pattern() {
        let mut member = make_member(RoomRole::Member);
        member.added_permissions = PermissionBits::KICK_MEMBER;
        let role_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::KICK_MEMBER));
        assert!(effective.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_member_deny_pattern() {
        let mut member = make_member(RoomRole::Member);
        member.removed_permissions = PermissionBits::SEND_CHAT;
        let role_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        let effective = member.effective_permissions(role_default);
        assert!(!effective.has(PermissionBits::SEND_CHAT));
        assert!(effective.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_admin_uses_admin_overrides() {
        let mut member = make_member(RoomRole::Admin);
        member.admin_added_permissions = PermissionBits::PLAY_CONTROL;
        member.admin_removed_permissions = PermissionBits::KICK_MEMBER;
        member.added_permissions = PermissionBits::USE_WEBRTC;

        let role_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::PLAY_CONTROL));
        assert!(!effective.has(PermissionBits::KICK_MEMBER));
    }

    #[test]
    fn test_creator_ignores_all_overrides() {
        let mut member = make_member(RoomRole::Creator);
        member.removed_permissions = PermissionBits::ALL;
        member.admin_removed_permissions = PermissionBits::ALL;
        let role_default = PermissionBits::empty();
        let effective = member.effective_permissions(role_default);
        assert_eq!(effective.0, PermissionBits::ALL);
    }

    #[test]
    fn test_guest_allow_deny_pattern() {
        let mut member = make_member(RoomRole::Guest);
        member.added_permissions = PermissionBits::USE_WEBRTC;
        let role_default = PermissionBits(PermissionBits::DEFAULT_GUEST);
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::USE_WEBRTC));
        assert!(!effective.has(PermissionBits::SEND_CHAT));
        assert!(!effective.has(PermissionBits::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn test_three_layer_permission_chain() {
        let service = make_service();

        // Layer 2: Room adds PLAY_CONTROL, removes SEND_CHAT
        let mut settings = RoomSettings::default();
        settings.member_added_permissions = MemberAddedPermissions(PermissionBits::PLAY_CONTROL);
        settings.member_removed_permissions = MemberRemovedPermissions(PermissionBits::SEND_CHAT);
        let role_default = service.calculate_role_default_permissions(&RoomRole::Member, &settings);
        assert!(role_default.has(PermissionBits::PLAY_CONTROL));
        assert!(!role_default.has(PermissionBits::SEND_CHAT));

        // Layer 3: Member re-adds SEND_CHAT, removes CREATE_MEDIA_RESOURCE
        let mut member = make_member(RoomRole::Member);
        member.added_permissions = PermissionBits::SEND_CHAT;
        member.removed_permissions = PermissionBits::CREATE_MEDIA_RESOURCE;

        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::SEND_CHAT));
        assert!(!effective.has(PermissionBits::CREATE_MEDIA_RESOURCE));
        assert!(effective.has(PermissionBits::PLAY_CONTROL));
        assert!(effective.has(PermissionBits::VIEW_MEDIA_RESOURCES));
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
        let perms =
            PermissionBits(PermissionBits::SEND_CHAT | PermissionBits::CREATE_MEDIA_RESOURCE);
        assert!(perms.has_all(PermissionBits::SEND_CHAT | PermissionBits::CREATE_MEDIA_RESOURCE));
        assert!(!perms.has_all(PermissionBits::SEND_CHAT | PermissionBits::KICK_MEMBER));
    }

    #[test]
    fn test_has_any_requires_any_bit() {
        let perms = PermissionBits(PermissionBits::SEND_CHAT);
        assert!(perms.has_any(PermissionBits::SEND_CHAT | PermissionBits::KICK_MEMBER));
        assert!(!perms.has_any(PermissionBits::KICK_MEMBER | PermissionBits::DELETE_ROOM));
    }

    #[test]
    fn test_room_rejects_send_chat_for_guest() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.guest_added_permissions = GuestAddedPermissions(PermissionBits::SEND_CHAT);
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert!(!perms.has(PermissionBits::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn test_room_adds_webrtc_for_guest() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.guest_added_permissions = GuestAddedPermissions(PermissionBits::USE_WEBRTC);
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(perms.has(PermissionBits::USE_WEBRTC));
        assert!(!perms.has(PermissionBits::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn test_room_removes_view_media_resources_for_guest() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.guest_removed_permissions =
            GuestRemovedPermissions(PermissionBits::VIEW_MEDIA_RESOURCES);
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(!perms.has(PermissionBits::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn test_empty_permissions_has_nothing() {
        let perms = PermissionBits::empty();
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert!(!perms.has_any(PermissionBits::ALL));
        assert!(perms.has_all(0)); // vacuously true
    }

    #[test]
    fn test_three_layer_guest_chain() {
        let service = make_service();

        // Layer 1: Global defaults for Guest (no media resource permissions)
        // Layer 2: Room adds WebRTC for guests
        let mut settings = RoomSettings::default();
        settings.guest_added_permissions = GuestAddedPermissions(PermissionBits::USE_WEBRTC);
        let role_default = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(!role_default.has(PermissionBits::VIEW_MEDIA_RESOURCES));
        assert!(role_default.has(PermissionBits::USE_WEBRTC));

        // Layer 3: Per-actor removal can still remove guest-level permissions.
        let mut member = make_member(RoomRole::Guest);
        member.removed_permissions = PermissionBits::USE_WEBRTC;
        let effective = member.effective_permissions(role_default);
        assert!(!effective.has(PermissionBits::VIEW_MEDIA_RESOURCES));
        assert!(!effective.has(PermissionBits::USE_WEBRTC));
    }

    #[test]
    fn test_three_layer_admin_chain() {
        let service = make_service();

        // Layer 2: Room removes KICK_MEMBER for admins
        let mut settings = RoomSettings::default();
        settings.admin_removed_permissions = AdminRemovedPermissions(PermissionBits::KICK_MEMBER);
        let role_default = service.calculate_role_default_permissions(&RoomRole::Admin, &settings);
        assert!(!role_default.has(PermissionBits::KICK_MEMBER));
        assert!(role_default.has(PermissionBits::SET_MEMBER_PERMISSIONS));

        // Layer 3: Admin-level re-adds KICK_MEMBER (specific admin override)
        let mut member = make_member(RoomRole::Admin);
        member.admin_added_permissions = PermissionBits::KICK_MEMBER;
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::KICK_MEMBER));
        assert!(effective.has(PermissionBits::SET_MEMBER_PERMISSIONS));
    }

    #[test]
    fn test_creator_ignores_member_level_deny() {
        let mut member = make_member(RoomRole::Creator);
        member.removed_permissions = PermissionBits::ALL;
        member.admin_removed_permissions = PermissionBits::ALL;
        member.added_permissions = 0;
        member.admin_added_permissions = 0;

        // Even with everything denied, Creator still has ALL
        let role_default = PermissionBits::empty();
        let effective = member.effective_permissions(role_default);
        assert_eq!(effective.0, PermissionBits::ALL);
    }

    #[test]
    fn test_creator_always_all_regardless_of_room_settings() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        // Try to restrict creator via room settings
        settings.admin_removed_permissions = AdminRemovedPermissions(PermissionBits::ALL);
        settings.member_removed_permissions = MemberRemovedPermissions(PermissionBits::ALL);
        let perms = service.calculate_role_default_permissions(&RoomRole::Creator, &settings);
        assert_eq!(perms.0, PermissionBits::ALL);
    }

    #[test]
    fn test_admin_ignores_member_level_added_permissions() {
        let mut member = make_member(RoomRole::Admin);
        // Set member-level overrides (should be ignored for Admin role)
        member.added_permissions = PermissionBits::USE_WEBRTC;
        member.removed_permissions = PermissionBits::SEND_CHAT;
        // Admin-level overrides: these should apply
        member.admin_added_permissions = 0;
        member.admin_removed_permissions = 0;

        let role_default = PermissionBits(PermissionBits::DEFAULT_ADMIN);
        let effective = member.effective_permissions(role_default);
        // DEFAULT_MEMBER already includes USE_WEBRTC; member-level grant is redundant and ignored.
        assert!(effective.has(PermissionBits::USE_WEBRTC));
        // member-level SEND_CHAT deny should NOT apply to admin
        assert!(effective.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_member_ignores_admin_level_permissions() {
        let mut member = make_member(RoomRole::Member);
        // Set admin-level overrides (should be ignored for Member role)
        member.admin_added_permissions = PermissionBits::DELETE_ROOM;
        member.admin_removed_permissions = PermissionBits::SEND_CHAT;
        // Member-level overrides: these should apply
        member.added_permissions = 0;
        member.removed_permissions = 0;

        let role_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        let effective = member.effective_permissions(role_default);
        // admin-level DELETE_ROOM grant should NOT apply
        assert!(!effective.has(PermissionBits::DELETE_ROOM));
        // admin-level SEND_CHAT deny should NOT apply
        assert!(effective.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_room_level_add_and_remove_same_permission_for_member() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        // Both add and remove SEND_CHAT at room level:
        // Result: (default | add) & ~remove => SEND_CHAT is removed
        settings.member_added_permissions = MemberAddedPermissions(PermissionBits::SEND_CHAT);
        settings.member_removed_permissions = MemberRemovedPermissions(PermissionBits::SEND_CHAT);
        let perms = service.calculate_role_default_permissions(&RoomRole::Member, &settings);
        // Remove is applied after add, so SEND_CHAT should be absent
        assert!(!perms.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_room_level_add_and_remove_same_permission_for_guest() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.guest_added_permissions = GuestAddedPermissions(PermissionBits::USE_WEBRTC);
        settings.guest_removed_permissions = GuestRemovedPermissions(PermissionBits::USE_WEBRTC);
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        // Remove wins over add
        assert!(!perms.has(PermissionBits::USE_WEBRTC));
    }

    #[test]
    fn test_permission_bits_grant_revoke() {
        let mut perms = PermissionBits(0);
        perms.grant(PermissionBits::SEND_CHAT);
        assert!(perms.has(PermissionBits::SEND_CHAT));

        perms.grant(PermissionBits::CREATE_MEDIA_RESOURCE);
        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::CREATE_MEDIA_RESOURCE));

        perms.revoke(PermissionBits::SEND_CHAT);
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_permission_bits_all_contains_every_named_permission() {
        let all = PermissionBits(PermissionBits::ALL);
        assert!(all.has(PermissionBits::SEND_CHAT));
        assert!(all.has(PermissionBits::CREATE_MEDIA_RESOURCE));
        assert!(all.has(PermissionBits::KICK_MEMBER));
        assert!(all.has(PermissionBits::DELETE_ROOM));
        assert!(all.has(PermissionBits::USE_WEBRTC));
        assert!(all.has(PermissionBits::VIEW_MEDIA_RESOURCES));
        assert!(all.has(PermissionBits::PLAY_CONTROL));
    }

    #[test]
    fn test_has_room_settings_repo_returns_true_after_set() {
        let mut service = make_service();

        // Create a RoomSettingsRepository
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });
        let settings_repo = RoomSettingsRepository::new(pool);

        // Initially false
        assert!(!service.has_room_settings_repo());

        // Set the repository
        service.set_room_settings_repo(settings_repo);

        // Now returns true
        assert!(service.has_room_settings_repo());
    }

    #[test]
    fn test_without_cache_has_no_room_settings_repo() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });
        let member_repo = RoomMemberRepository::new(pool);
        let room_repo = RoomRepository::new(rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        }));

        let service = PermissionService::without_cache(member_repo, room_repo, None);
        assert!(!service.has_room_settings_repo());
    }

    #[test]
    fn test_set_invalidation_service_propagates_to_clones() {
        let mut service = make_service();
        let cloned = service.clone();
        let invalidation_service = Arc::new(crate::cache::CacheInvalidationService::new(
            "permission-clone-node".to_string(),
            "permission-clone-stream".to_string(),
        ));

        service.set_invalidation_service(invalidation_service);

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
    fn test_set_room_settings_repo_can_be_called_multiple_times() {
        let mut service = make_service();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool1 = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });
        let pool2 = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });

        let settings_repo1 = RoomSettingsRepository::new(pool1);
        let settings_repo2 = RoomSettingsRepository::new(pool2);

        // Set first repo
        service.set_room_settings_repo(settings_repo1);
        assert!(service.has_room_settings_repo());

        // Replace with second repo - should work without panicking
        service.set_room_settings_repo(settings_repo2);
        assert!(service.has_room_settings_repo());
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

    /// Helper to create a PermissionService with invalidation service for tests
    /// This version creates services without needing a nested runtime.
    fn make_service_with_invalidation_no_rt() -> (
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

        let service = PermissionService::with_invalidation(
            member_repo,
            room_repo,
            None,
            10,
            60,
            invalidation_service.clone(),
        );

        (service, invalidation_service)
    }

    #[tokio::test]
    async fn test_with_invalidation_does_not_start_tasks_until_explicit_start() {
        let (service, _invalidation_service) = make_service_with_invalidation_no_rt();

        assert!(
            !service.invalidation_tasks_started(),
            "with_invalidation must not spawn background tasks during construction"
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
        let (service, _invalidation_service) = make_service_with_invalidation_no_rt();
        let room_id = RoomId::expect_positive(1);
        let user_id = UserId::expect_positive(1);
        let cache_key = PermissionService::cache_key(&room_id, &user_id);

        service
            .cache
            .insert(cache_key.clone(), PermissionBits(PermissionBits::ALL))
            .await;
        service
            .degraded_cache
            .insert(cache_key.clone(), PermissionBits(PermissionBits::ALL))
            .await;

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
            "permission cache should leave degraded mode after the bounded recovery timeout"
        );
        assert!(
            service.cache.get(&cache_key).await.is_none(),
            "auto-recovery must clear the primary cache before re-enabling it"
        );
        assert!(
            service.degraded_cache.get(&cache_key).await.is_none(),
            "auto-recovery must clear degraded cache entries once recovery completes"
        );
        assert!(
            service.degradation_started.lock().is_none(),
            "auto-recovery must clear degradation start time"
        );

        service.shutdown().await;
    }

    #[tokio::test]
    async fn test_degraded_mode_recovers_on_invalidation_message() {
        let (service, invalidation_service) = make_service_with_invalidation_no_rt();

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
        let (service, invalidation_service) = make_service_with_invalidation_no_rt();

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
        // This test verifies that when a PermissionService has an invalidation_service,
        // calling invalidate_cache clears the local cache.
        // The key behavior being tested is that invalidate_cache works correctly.

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (service, _invalidation_service) = make_service_with_invalidation_no_rt();

            // Insert a value into the cache
            let room_id = RoomId::expect_positive(1);
            let user_id = UserId::expect_positive(1);
            let cache_key = PermissionService::cache_key(&room_id, &user_id);
            service
                .cache
                .insert(cache_key.clone(), PermissionBits(PermissionBits::ALL))
                .await;

            // Verify the cache has the value
            assert!(service.cache.get(&cache_key).await.is_some());

            // Invalidate the cache
            service.invalidate_cache(&room_id, &user_id).await;

            // Verify the local cache is invalidated
            assert!(service.cache.get(&cache_key).await.is_none());
        });
    }

    #[test]
    fn test_invalidate_room_cache_local_clear_works() {
        // This test verifies that invalidate_room_cache correctly invalidates
        // cache entries for all users in a room.
        // Note: moka's invalidate_entries_if is a background operation that may not
        // be immediate. For local cache, verify the method doesn't panic and the
        // broadcast is sent.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (service, invalidation_service) = make_service_with_invalidation_no_rt();

            // Subscribe to receive invalidation messages
            let mut receiver = invalidation_service.subscribe();

            // Insert values into the cache for multiple users in the same room
            let room_id = RoomId::expect_positive(1);
            let user1_id = UserId::expect_positive(1);
            let user2_id = UserId::expect_positive(2);

            service
                .cache
                .insert(
                    PermissionService::cache_key(&room_id, &user1_id),
                    PermissionBits(PermissionBits::ALL),
                )
                .await;
            service
                .cache
                .insert(
                    PermissionService::cache_key(&room_id, &user2_id),
                    PermissionBits(PermissionBits::ALL),
                )
                .await;

            // Verify the cache has the values
            assert!(service
                .cache
                .get(&PermissionService::cache_key(&room_id, &user1_id))
                .await
                .is_some());
            assert!(service
                .cache
                .get(&PermissionService::cache_key(&room_id, &user2_id))
                .await
                .is_some());

            // Invalidate the room cache
            service.invalidate_room_cache(&room_id).await;

            // Verify the broadcast was sent (this is the main fix)
            let result =
                tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver.recv())
                    .await;

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
                    panic!("Timeout waiting for broadcast: {timeout_error:?}");
                }
            }
        });
    }

    #[test]
    fn test_clear_cache_local_clear_works() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (service, _invalidation_service) = make_service_with_invalidation_no_rt();

            // Insert values into the cache
            let room_id = RoomId::expect_positive(1);
            let user_id = UserId::expect_positive(1);
            service
                .cache
                .insert(
                    PermissionService::cache_key(&room_id, &user_id),
                    PermissionBits(PermissionBits::ALL),
                )
                .await;

            // Verify the cache has the value
            assert!(service
                .cache
                .get(&PermissionService::cache_key(&room_id, &user_id))
                .await
                .is_some());

            // Clear the cache
            service.clear_cache().await;

            // Verify the local cache is cleared
            assert!(service
                .cache
                .get(&PermissionService::cache_key(&room_id, &user_id))
                .await
                .is_none());
        });
    }

    #[test]
    fn test_invalidate_cache_no_panic_without_invalidation_service() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Create a PermissionService without invalidation service
            // Use make_service_async() because we're inside an async context
            let service = make_service_async();

            // Insert a value into the cache
            let room_id = RoomId::expect_positive(1);
            let user_id = UserId::expect_positive(1);
            let cache_key = PermissionService::cache_key(&room_id, &user_id);
            service
                .cache
                .insert(cache_key.clone(), PermissionBits(PermissionBits::ALL))
                .await;

            // Verify the cache has the value
            assert!(service.cache.get(&cache_key).await.is_some());

            // Invalidate the cache - should not panic even without invalidation service
            service.invalidate_cache(&room_id, &user_id).await;

            // Verify the local cache is invalidated
            assert!(service.cache.get(&cache_key).await.is_none());
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

            let service = PermissionService::with_invalidation(
                member_repo,
                room_repo,
                None,
                10,
                60,
                invalidation_service.clone(),
            );

            // Insert a value into the cache
            let room_id = RoomId::expect_positive(1);
            let user_id = UserId::expect_positive(1);
            let cache_key = PermissionService::cache_key(&room_id, &user_id);
            service
                .cache
                .insert(cache_key.clone(), PermissionBits(PermissionBits::ALL))
                .await;

            // Invalidate the cache - this should broadcast via invalidation_service
            service.invalidate_cache(&room_id, &user_id).await;

            // Verify the local cache is invalidated
            assert!(service.cache.get(&cache_key).await.is_none());

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

            let service = PermissionService::with_invalidation(
                member_repo,
                room_repo,
                None,
                10,
                60,
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

            let service = PermissionService::with_invalidation(
                member_repo,
                room_repo,
                None,
                10,
                60,
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
