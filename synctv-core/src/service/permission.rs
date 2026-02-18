//! Permission management service
//!
//! Centralized permission checking and management with Allow/Deny pattern and caching.
//! Supports multi-replica cache invalidation via Redis Pub/Sub.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::{
    cache::{CacheInvalidationService, InvalidationMessage},
    models::{RoomId, UserId, PermissionBits, RoomSettings},
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository},
    service::SettingsRegistry,
    Error, Result,
};

/// Permission management service
///
/// Handles permission checking with Allow/Deny pattern, optional caching and role inheritance.
/// When `CacheInvalidationService` is provided, it listens for cross-replica invalidation messages.
#[derive(Clone)]
pub struct PermissionService {
    member_repo: RoomMemberRepository,
    room_settings_repo: Option<RoomSettingsRepository>,
    cache: Arc<moka::future::Cache<String, PermissionBits>>,
    settings_registry: Option<Arc<SettingsRegistry>>,
    /// Optional invalidation service for cross-replica cache sync
    invalidation_service: Option<Arc<CacheInvalidationService>>,
    /// When true, cache is considered unreliable due to Pub/Sub lag;
    /// all permission checks fall back to no-cache until recovery.
    cache_degraded: Arc<AtomicBool>,
    /// Tracks last `invalidate_all()` time to rate-limit flushes
    last_flush_time: Arc<parking_lot::Mutex<Instant>>,
    /// Tracks when cache degradation started for automatic recovery
    degradation_started: Arc<parking_lot::Mutex<Option<Instant>>>,
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

    /// Minimum interval between `invalidate_all()` calls (seconds)
    const FLUSH_RATE_LIMIT_SECS: u64 = 10;
    /// Maximum duration to remain in degraded mode before automatic recovery (seconds)
    /// After this time, the cache is re-enabled assuming Pub/Sub lag has resolved.
    const MAX_DEGRADATION_DURATION_SECS: u64 = 60;

    /// Create a new permission service with caching
    #[must_use]
    pub fn new(
        member_repo: RoomMemberRepository,
        _room_repo: RoomRepository,
        settings_registry: Option<Arc<SettingsRegistry>>,
        cache_size: u64,
        cache_ttl_secs: u64,
    ) -> Self {
        Self {
            member_repo,
            room_settings_repo: None, // Will be set later if needed
            cache: Arc::new(
                moka::future::CacheBuilder::new(cache_size)
                    .time_to_live(Duration::from_secs(cache_ttl_secs))
                    .build(),
            ),
            settings_registry,
            invalidation_service: None,
            cache_degraded: Arc::new(AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(Instant::now())),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
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
        invalidation_service: Arc<CacheInvalidationService>,
    ) -> Self {
        let service = Self::new(member_repo, room_repo, settings_registry, cache_size, cache_ttl_secs);

        // Subscribe to invalidation messages
        let cache = service.cache.clone();
        let cache_degraded = service.cache_degraded.clone();
        let last_flush_time = service.last_flush_time.clone();
        let degradation_started = service.degradation_started.clone();
        let mut receiver = invalidation_service.subscribe();

        crate::spawn::spawn_monitored("permission_invalidation_listener", async move {
            loop {
                match receiver.recv().await {
                    Ok(msg) => {
                        // Any successful message means Pub/Sub is healthy;
                        // clear degraded flag and reset degradation timer.
                        if cache_degraded.swap(false, Ordering::Release) {
                            tracing::info!("Permission cache recovered from degraded state");
                        }
                        *degradation_started.lock() = None;

                        match msg {
                            InvalidationMessage::UserPermission { room_id, user_id } => {
                                let cache_key = format!("perm:room:{room_id}:user:{user_id}");
                                cache.invalidate(&cache_key).await;
                                tracing::debug!(
                                    room_id = %room_id,
                                    user_id = %user_id,
                                    "Permission cache invalidated (cross-replica)"
                                );
                            }
                            InvalidationMessage::RoomPermission { room_id } => {
                                let prefix = format!("perm:room:{room_id}:user:");
                                let _ = cache.invalidate_entries_if(move |key, _| key.starts_with(&prefix));
                                tracing::debug!(
                                    room_id = %room_id,
                                    "Room permission cache invalidated (cross-replica)"
                                );
                            }
                            InvalidationMessage::All => {
                                cache.invalidate_all();
                                tracing::debug!("All permission cache invalidated (cross-replica)");
                            }
                            _ => {
                                // Other message types not relevant to permission cache
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::debug!("Invalidation channel closed, stopping listener");
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // CRITICAL FIX: Set degraded flag BEFORE flushing cache to prevent race condition
                        // Race condition scenario without this fix:
                        // 1. Thread A: flush cache (cache is empty)
                        // 2. Thread B: check cache_degraded=false, read cache (gets nothing)
                        // 3. Thread A: set cache_degraded=true
                        // Result: Thread B got stale/missing data
                        //
                        // With this fix:
                        // 1. Thread A: set cache_degraded=true
                        // 2. Thread B: check cache_degraded=true, skip cache, query DB
                        // 3. Thread A: flush cache
                        // Result: Thread B correctly bypassed cache

                        // Step 1: Mark cache as degraded FIRST
                        let was_degraded = cache_degraded.swap(true, Ordering::Release);
                        if !was_degraded {
                            *degradation_started.lock() = Some(Instant::now());
                            tracing::warn!(
                                lagged_messages = n,
                                "Permission cache entered degraded state due to Pub/Sub lag"
                            );
                        }

                        // Step 2: Memory barrier to ensure degraded flag is visible to all threads
                        std::sync::atomic::fence(Ordering::SeqCst);

                        // Step 3: Now safe to flush cache (other threads will see degraded=true)
                        // Rate-limit invalidate_all() to prevent cache storms
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
                        } else {
                            tracing::debug!(
                                lagged_messages = n,
                                "Invalidation listener lagged, cache flush rate-limited (already degraded)"
                            );
                        }
                    }
                }
            }
        });

        // Spawn a recovery task that periodically checks if we've been degraded too long
        let cache_degraded_for_recovery = service.cache_degraded.clone();
        let degradation_started_for_recovery = service.degradation_started.clone();
        crate::spawn::spawn_monitored("permission_cache_recovery", async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(10));
            loop {
                ticker.tick().await;

                // Check if we should recover from degraded state
                if cache_degraded_for_recovery.load(Ordering::Acquire) {
                    let should_recover = {
                        let started = degradation_started_for_recovery.lock();
                        started.is_some_and(|start_time| {
                            start_time.elapsed() >= Duration::from_secs(Self::MAX_DEGRADATION_DURATION_SECS)
                        })
                    };

                    if should_recover {
                        tracing::warn!(
                            "Permission cache degraded for {} seconds, forcing recovery",
                            Self::MAX_DEGRADATION_DURATION_SECS
                        );
                        cache_degraded_for_recovery.store(false, Ordering::Release);
                        *degradation_started_for_recovery.lock() = None;
                        tracing::info!("Permission cache auto-recovered after max degradation duration");
                    }
                }
            }
        });

        Self {
            invalidation_service: Some(invalidation_service),
            ..service
        }
    }

    /// Create a permission service without caching
    #[must_use]
    pub fn without_cache(
        member_repo: RoomMemberRepository,
        _room_repo: RoomRepository,
        settings_registry: Option<Arc<SettingsRegistry>>,
    ) -> Self {
        Self {
            member_repo,
            room_settings_repo: None,
            cache: Arc::new(
                moka::future::CacheBuilder::new(1)
                    .time_to_live(Duration::from_secs(1))
                    .build(),
            ),
            settings_registry,
            invalidation_service: None,
            cache_degraded: Arc::new(AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(Instant::now())),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// Set the room settings repository
    pub fn set_room_settings_repo(&mut self, repo: RoomSettingsRepository) {
        self.room_settings_repo = Some(repo);
    }

    /// Get global default permissions for a role from `SettingsRegistry`
    fn get_global_default_permissions(&self, role: &crate::models::RoomRole) -> PermissionBits {
        if let Some(registry) = &self.settings_registry {
            match role {
                crate::models::RoomRole::Admin => {
                    PermissionBits(registry.admin_default_permissions.get().unwrap_or(PermissionBits::DEFAULT_ADMIN))
                }
                crate::models::RoomRole::Member => {
                    PermissionBits(registry.member_default_permissions.get().unwrap_or(PermissionBits::DEFAULT_MEMBER))
                }
                crate::models::RoomRole::Guest => {
                    PermissionBits(registry.guest_default_permissions.get().unwrap_or(PermissionBits::DEFAULT_GUEST))
                }
                crate::models::RoomRole::Creator => PermissionBits(crate::models::PermissionBits::ALL),
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
        let global_default = self.get_global_default_permissions(role);

        match role {
            crate::models::RoomRole::Creator => PermissionBits(crate::models::PermissionBits::ALL),
            crate::models::RoomRole::Admin => {
                room_settings.admin_permissions(global_default)
            }
            crate::models::RoomRole::Member => {
                room_settings.member_permissions(global_default)
            }
            crate::models::RoomRole::Guest => {
                room_settings.guest_permissions(global_default)
            }
        }
    }

    /// Generate cache key for room + user with namespace prefix
    ///
    /// Format: `perm:room:<room_id>:user:<user_id>`
    /// The namespace prefix prevents collisions with other cache types and
    /// ensures room/user ID pairs are always unique even if IDs overlap.
    fn cache_key(room_id: &RoomId, user_id: &UserId) -> String {
        format!("perm:room:{}:user:{}", room_id.0, user_id.0)
    }

    /// Check if a user has a specific permission in a room
    ///
    /// Falls back to `check_permission_no_cache` when the cache is degraded
    /// (e.g., due to Pub/Sub lag), ensuring correct permission data is always used.
    pub async fn check_permission(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<()> {
        // Fall back to no-cache when degraded (Pub/Sub lag)
        if self.cache_degraded.load(Ordering::Acquire) {
            return self.check_permission_no_cache(room_id, user_id, permission).await;
        }

        let permissions = self.get_user_permissions(room_id, user_id).await?;

        if !permissions.has_all(permission) {
            return Err(Error::Authorization("Permission denied".to_string()));
        }

        Ok(())
    }

    /// Check permission without using cache (for critical operations).
    ///
    /// Use this for security-sensitive operations where permission changes
    /// must be immediately reflected, such as:
    /// - Deleting a room
    /// - Banning/kicking users
    /// - Changing user roles or permissions
    ///
    /// This bypasses the cache and always fetches fresh permissions from the database.
    pub async fn check_permission_no_cache(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<()> {
        let permissions = self.get_user_permissions_no_cache(room_id, user_id).await?;

        if !permissions.has_all(permission) {
            return Err(Error::Authorization("Permission denied".to_string()));
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
            .ok_or_else(|| Error::Authorization("Not a member of this room".to_string()))?;

        // Get room settings for role defaults
        let room_settings = if let Some(ref settings_repo) = self.room_settings_repo {
            settings_repo.get(room_id).await?
        } else {
            RoomSettings::default()
        };

        // Calculate role default permissions (global + room-level overrides)
        let role_default = self.calculate_role_default_permissions(&member.role, &room_settings);

        // Apply member-level overrides
        Ok(member.effective_permissions(role_default))
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
            .ok_or_else(|| Error::Authorization("Not a member of this room".to_string()))?;

        // Get room settings for role defaults
        let room_settings = if let Some(ref settings_repo) = self.room_settings_repo {
            settings_repo.get(room_id).await?
        } else {
            RoomSettings::default()
        };

        // Calculate role default permissions (global + room-level overrides)
        let role_default = self.calculate_role_default_permissions(&member.role, &room_settings);

        // Apply member-level overrides
        let permissions = member.effective_permissions(role_default);

        // Update cache
        self.cache.insert(cache_key, permissions).await;

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
    /// 1. Broadcast succeeds → other replicas invalidate
    /// 2. Before local invalidation → cached reads on this node return stale data
    /// 3. After local invalidation → window closes
    ///
    /// By invalidating locally first, we ensure this node never serves stale
    /// data after the mutation completes, even if the broadcast fails.
    pub async fn invalidate_cache(&self, room_id: &RoomId, user_id: &UserId) {
        // Invalidate local cache first to close the stale read window immediately
        let cache_key = Self::cache_key(room_id, user_id);
        self.cache.invalidate(&cache_key).await;

        // Broadcast to other replicas (best effort)
        if let Some(ref service) = self.invalidation_service {
            if let Err(e) = service.invalidate_user_permission(room_id, user_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
                    user_id = %user_id.as_str(),
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
        // Invalidate local cache first to close the stale read window immediately
        // Use namespace prefix to match all permission cache entries for this room
        let prefix = format!("perm:room:{}:user:", room_id.0);
        let _ = self.cache.invalidate_entries_if(move |key, _| key.starts_with(&prefix));

        // Broadcast to other replicas (best effort)
        if let Some(ref service) = self.invalidation_service {
            if let Err(e) = service.invalidate_room_permission(room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
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
        // Clear local cache first to close the stale read window immediately
        self.cache.invalidate_all();

        // Broadcast to other replicas (best effort)
        if let Some(ref service) = self.invalidation_service {
            if let Err(e) = service.invalidate_all().await {
                tracing::warn!(
                    error = %e,
                    "Failed to broadcast full permission cache invalidation to other replicas"
                );
            }
        }
    }

    /// Check if user can perform an action (alias for `check_permission`)
    pub async fn can(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<bool> {
        match self.check_permission(room_id, user_id, permission).await {
            Ok(()) => Ok(true),
            Err(Error::Authorization(_)) => Ok(false),
            Err(e) => Err(e),
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
                return Err(Error::Authorization("Permission denied".to_string()));
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
            .ok_or_else(|| Error::Authorization("Not a member of this room".to_string()))?;

        if member.role != expected_role {
            return Err(Error::Authorization("Insufficient permissions".to_string()));
        }

        Ok(())
    }

    /// Check if user is room creator
    pub async fn is_creator(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<bool> {
        let member = self
            .member_repo
            .get(room_id, user_id)
            .await?;

        Ok(member.is_some_and(|m| m.role == crate::models::RoomRole::Creator))
    }

    /// Check if user is room admin or creator
    pub async fn is_admin_or_creator(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<bool> {
        let member = self
            .member_repo
            .get(room_id, user_id)
            .await?;

        Ok(member.is_some_and(|m| matches!(m.role, crate::models::RoomRole::Admin | crate::models::RoomRole::Creator)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        RoomMember, MemberStatus,
        room_settings::*,
    };
    use crate::models::permission::Role as RoomRole;

    // Helper to create a PermissionService using tokio runtime for PgPool
    fn make_service() -> PermissionService {
        // PgPool::connect_lazy requires a tokio runtime, so use Runtime::new
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://unused:5432/unused").unwrap()
        });
        PermissionService {
            member_repo: RoomMemberRepository::new(pool),
            room_settings_repo: None,
            cache: Arc::new(
                moka::future::CacheBuilder::new(10)
                    .time_to_live(Duration::from_secs(60))
                    .build(),
            ),
            settings_registry: None,
            invalidation_service: None,
            cache_degraded: Arc::new(AtomicBool::new(false)),
            last_flush_time: Arc::new(parking_lot::Mutex::new(Instant::now())),
            degradation_started: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    fn make_member(role: RoomRole) -> RoomMember {
        RoomMember::new(
            RoomId("room1".to_string()),
            UserId("user1".to_string()),
            role,
        )
    }

    // ========== Cache Key Tests ==========

    #[test]
    fn test_cache_key_generation() {
        let room_id = RoomId("room123".to_string());
        let user_id = UserId("user456".to_string());
        let key = PermissionService::cache_key(&room_id, &user_id);
        assert_eq!(key, "perm:room:room123:user:user456");
    }

    #[test]
    fn test_cache_key_different_for_different_users() {
        let room = RoomId("r1".to_string());
        let u1 = UserId("u1".to_string());
        let u2 = UserId("u2".to_string());
        assert_ne!(
            PermissionService::cache_key(&room, &u1),
            PermissionService::cache_key(&room, &u2),
        );
    }

    #[test]
    fn test_cache_key_different_for_different_rooms() {
        let r1 = RoomId("r1".to_string());
        let r2 = RoomId("r2".to_string());
        let user = UserId("u1".to_string());
        assert_ne!(
            PermissionService::cache_key(&r1, &user),
            PermissionService::cache_key(&r2, &user),
        );
    }

    // ========== Role Default Permission Tests ==========

    #[test]
    fn test_creator_always_gets_all_permissions() {
        let service = make_service();
        let settings = RoomSettings::default();
        let perms = service.calculate_role_default_permissions(&RoomRole::Creator, &settings);
        assert_eq!(perms.0, PermissionBits::ALL);
    }

    #[test]
    fn test_admin_gets_default_admin_permissions() {
        let service = make_service();
        let settings = RoomSettings::default();
        let perms = service.calculate_role_default_permissions(&RoomRole::Admin, &settings);
        assert!(perms.has(PermissionBits::BAN_MEMBER));
        assert!(perms.has(PermissionBits::KICK_USER));
        assert!(perms.has(PermissionBits::SET_ROOM_SETTINGS));
        assert!(perms.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_member_gets_default_member_permissions() {
        let service = make_service();
        let settings = RoomSettings::default();
        let perms = service.calculate_role_default_permissions(&RoomRole::Member, &settings);
        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::ADD_MOVIE));
        assert!(perms.has(PermissionBits::VIEW_PLAYLIST));
        assert!(!perms.has(PermissionBits::BAN_MEMBER));
        assert!(!perms.has(PermissionBits::DELETE_ROOM));
    }

    #[test]
    fn test_guest_gets_default_guest_permissions() {
        let service = make_service();
        let settings = RoomSettings::default();
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(perms.has(PermissionBits::VIEW_PLAYLIST));
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert!(!perms.has(PermissionBits::ADD_MOVIE));
    }

    // ========== Room-Level Override Tests ==========

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
        assert!(perms.has(PermissionBits::ADD_MOVIE));
    }

    #[test]
    fn test_room_level_add_and_remove_for_admin() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.admin_added_permissions = AdminAddedPermissions(PermissionBits::DELETE_ROOM);
        settings.admin_removed_permissions = AdminRemovedPermissions(PermissionBits::BAN_MEMBER);
        let perms = service.calculate_role_default_permissions(&RoomRole::Admin, &settings);
        assert!(perms.has(PermissionBits::DELETE_ROOM));
        assert!(!perms.has(PermissionBits::BAN_MEMBER));
    }

    #[test]
    fn test_room_overrides_do_not_affect_creator() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.admin_removed_permissions = AdminRemovedPermissions(PermissionBits::ALL);
        let perms = service.calculate_role_default_permissions(&RoomRole::Creator, &settings);
        assert_eq!(perms.0, PermissionBits::ALL);
    }

    // ========== Member-Level Override Tests (effective_permissions) ==========

    #[test]
    fn test_member_allow_pattern() {
        let mut member = make_member(RoomRole::Member);
        member.added_permissions = PermissionBits::BAN_MEMBER;
        let role_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::BAN_MEMBER));
        assert!(effective.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_member_deny_pattern() {
        let mut member = make_member(RoomRole::Member);
        member.removed_permissions = PermissionBits::SEND_CHAT;
        let role_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        let effective = member.effective_permissions(role_default);
        assert!(!effective.has(PermissionBits::SEND_CHAT));
        assert!(effective.has(PermissionBits::ADD_MOVIE));
    }

    #[test]
    fn test_admin_uses_admin_overrides() {
        let mut member = make_member(RoomRole::Admin);
        member.admin_added_permissions = PermissionBits::DELETE_ROOM;
        member.admin_removed_permissions = PermissionBits::BAN_MEMBER;
        member.added_permissions = PermissionBits::EXPORT_DATA;

        let role_default = PermissionBits(PermissionBits::DEFAULT_ADMIN);
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::DELETE_ROOM));
        assert!(!effective.has(PermissionBits::BAN_MEMBER));
        assert!(!effective.has(PermissionBits::EXPORT_DATA));
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
        member.added_permissions = PermissionBits::SEND_CHAT;
        let role_default = PermissionBits(PermissionBits::DEFAULT_GUEST);
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::SEND_CHAT));
        assert!(effective.has(PermissionBits::VIEW_PLAYLIST));
    }

    // ========== Three-Layer Override Chain Tests ==========

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

        // Layer 3: Member re-adds SEND_CHAT, removes ADD_MOVIE
        let mut member = make_member(RoomRole::Member);
        member.added_permissions = PermissionBits::SEND_CHAT;
        member.removed_permissions = PermissionBits::ADD_MOVIE;

        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::SEND_CHAT));
        assert!(!effective.has(PermissionBits::ADD_MOVIE));
        assert!(effective.has(PermissionBits::PLAY_CONTROL));
        assert!(effective.has(PermissionBits::VIEW_PLAYLIST));
    }

    // ========== Banned/Pending Member Tests ==========

    #[test]
    fn test_banned_member_has_no_permissions() {
        let mut member = make_member(RoomRole::Admin);
        member.status = MemberStatus::Banned;
        let role_default = PermissionBits(PermissionBits::DEFAULT_ADMIN);
        assert!(!member.has_permission(PermissionBits::SEND_CHAT, role_default));
        assert!(!member.has_permission(PermissionBits::DELETE_ROOM, role_default));
    }

    #[test]
    fn test_pending_member_has_no_permissions() {
        let mut member = make_member(RoomRole::Member);
        member.status = MemberStatus::Pending;
        let role_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        assert!(!member.has_permission(PermissionBits::SEND_CHAT, role_default));
    }

    // ========== Cache Degradation Tests ==========

    #[test]
    fn test_cache_degraded_flag_default_false() {
        let degraded = AtomicBool::new(false);
        assert!(!degraded.load(Ordering::Acquire));
    }

    #[test]
    fn test_cache_degraded_flag_toggling() {
        let degraded = AtomicBool::new(false);
        degraded.store(true, Ordering::Release);
        assert!(degraded.load(Ordering::Acquire));
        degraded.store(false, Ordering::Release);
        assert!(!degraded.load(Ordering::Acquire));
    }

    // ========== Flush Rate Limit Tests ==========

    #[test]
    fn test_flush_rate_limit_allows_after_interval() {
        let last_flush = parking_lot::Mutex::new(Instant::now() - Duration::from_secs(20));
        let elapsed = last_flush.lock().elapsed();
        assert!(elapsed >= Duration::from_secs(PermissionService::FLUSH_RATE_LIMIT_SECS));
    }

    #[test]
    fn test_flush_rate_limit_blocks_within_interval() {
        let last_flush = parking_lot::Mutex::new(Instant::now());
        let elapsed = last_flush.lock().elapsed();
        assert!(elapsed < Duration::from_secs(PermissionService::FLUSH_RATE_LIMIT_SECS));
    }

    // ========== has_all / has_any Tests ==========

    #[test]
    fn test_has_all_requires_all_bits() {
        let perms = PermissionBits(PermissionBits::SEND_CHAT | PermissionBits::ADD_MOVIE);
        assert!(perms.has_all(PermissionBits::SEND_CHAT | PermissionBits::ADD_MOVIE));
        assert!(!perms.has_all(PermissionBits::SEND_CHAT | PermissionBits::BAN_MEMBER));
    }

    #[test]
    fn test_has_any_requires_any_bit() {
        let perms = PermissionBits(PermissionBits::SEND_CHAT);
        assert!(perms.has_any(PermissionBits::SEND_CHAT | PermissionBits::BAN_MEMBER));
        assert!(!perms.has_any(PermissionBits::BAN_MEMBER | PermissionBits::DELETE_ROOM));
    }

    // ========== Room-Level Guest Override Tests ==========

    #[test]
    fn test_room_adds_send_chat_for_guest() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.guest_added_permissions = GuestAddedPermissions(PermissionBits::SEND_CHAT);
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::VIEW_PLAYLIST));
    }

    #[test]
    fn test_room_removes_view_playlist_for_guest() {
        let service = make_service();
        let mut settings = RoomSettings::default();
        settings.guest_removed_permissions = GuestRemovedPermissions(PermissionBits::VIEW_PLAYLIST);
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(!perms.has(PermissionBits::VIEW_PLAYLIST));
    }

    // ========== Edge Case: Empty Permissions ==========

    #[test]
    fn test_empty_permissions_has_nothing() {
        let perms = PermissionBits::empty();
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert!(!perms.has_any(PermissionBits::ALL));
        assert!(perms.has_all(0)); // vacuously true
    }

    // ========== Full Three-Layer Override Chain: Guest ==========

    #[test]
    fn test_three_layer_guest_chain() {
        let service = make_service();

        // Layer 1: Global defaults for Guest (VIEW_PLAYLIST only)
        // Layer 2: Room adds SEND_CHAT for guests
        let mut settings = RoomSettings::default();
        settings.guest_added_permissions = GuestAddedPermissions(PermissionBits::SEND_CHAT);
        let role_default = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        assert!(role_default.has(PermissionBits::VIEW_PLAYLIST));
        assert!(role_default.has(PermissionBits::SEND_CHAT));

        // Layer 3: Member-level removes SEND_CHAT (e.g. muted guest)
        let mut member = make_member(RoomRole::Guest);
        member.removed_permissions = PermissionBits::SEND_CHAT;
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::VIEW_PLAYLIST));
        assert!(!effective.has(PermissionBits::SEND_CHAT));
    }

    // ========== Full Three-Layer Override Chain: Admin ==========

    #[test]
    fn test_three_layer_admin_chain() {
        let service = make_service();

        // Layer 2: Room removes BAN_MEMBER for admins
        let mut settings = RoomSettings::default();
        settings.admin_removed_permissions = AdminRemovedPermissions(PermissionBits::BAN_MEMBER);
        let role_default = service.calculate_role_default_permissions(&RoomRole::Admin, &settings);
        assert!(!role_default.has(PermissionBits::BAN_MEMBER));
        assert!(role_default.has(PermissionBits::KICK_USER));

        // Layer 3: Admin-level re-adds BAN_MEMBER (specific admin override)
        let mut member = make_member(RoomRole::Admin);
        member.admin_added_permissions = PermissionBits::BAN_MEMBER;
        let effective = member.effective_permissions(role_default);
        assert!(effective.has(PermissionBits::BAN_MEMBER));
        assert!(effective.has(PermissionBits::KICK_USER));
    }

    // ========== Creator Immunity ==========

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

    // ========== Member-Level: Admin Uses admin_* Fields, Member Uses Regular Fields ==========

    #[test]
    fn test_admin_ignores_member_level_added_permissions() {
        let mut member = make_member(RoomRole::Admin);
        // Set member-level overrides (should be ignored for Admin role)
        member.added_permissions = PermissionBits::EXPORT_DATA;
        member.removed_permissions = PermissionBits::SEND_CHAT;
        // Admin-level overrides: these should apply
        member.admin_added_permissions = 0;
        member.admin_removed_permissions = 0;

        let role_default = PermissionBits(PermissionBits::DEFAULT_ADMIN);
        let effective = member.effective_permissions(role_default);
        // member-level EXPORT_DATA grant should NOT apply to admin
        assert!(!effective.has(PermissionBits::EXPORT_DATA));
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

    // ========== Banned Member: Various Roles ==========

    #[test]
    fn test_banned_creator_has_no_permissions_via_has_permission() {
        // RoomMember::has_permission checks status first
        let mut member = make_member(RoomRole::Creator);
        member.status = MemberStatus::Banned;
        let role_default = PermissionBits(PermissionBits::ALL);
        assert!(!member.has_permission(PermissionBits::SEND_CHAT, role_default));
    }

    #[test]
    fn test_banned_guest_has_no_permissions() {
        let mut member = make_member(RoomRole::Guest);
        member.status = MemberStatus::Banned;
        let role_default = PermissionBits(PermissionBits::DEFAULT_GUEST);
        assert!(!member.has_permission(PermissionBits::VIEW_PLAYLIST, role_default));
    }

    // ========== Room-Level: Combined Add and Remove ==========

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
        settings.guest_added_permissions = GuestAddedPermissions(PermissionBits::VIEW_PLAYLIST);
        settings.guest_removed_permissions = GuestRemovedPermissions(PermissionBits::VIEW_PLAYLIST);
        let perms = service.calculate_role_default_permissions(&RoomRole::Guest, &settings);
        // Remove wins over add
        assert!(!perms.has(PermissionBits::VIEW_PLAYLIST));
    }

    // ========== Member Leave Behavior ==========

    #[test]
    fn test_member_leave_sets_left_at() {
        let mut member = make_member(RoomRole::Member);
        assert!(member.left_at.is_none());
        assert!(member.is_active());

        member.leave();
        assert!(member.left_at.is_some());
        assert!(!member.is_active());
    }

    // ========== PermissionBits: Grant and Revoke ==========

    #[test]
    fn test_permission_bits_grant_revoke() {
        let mut perms = PermissionBits(0);
        perms.grant(PermissionBits::SEND_CHAT);
        assert!(perms.has(PermissionBits::SEND_CHAT));

        perms.grant(PermissionBits::ADD_MOVIE);
        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::ADD_MOVIE));

        perms.revoke(PermissionBits::SEND_CHAT);
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::ADD_MOVIE));
    }

    #[test]
    fn test_permission_bits_all_contains_every_named_permission() {
        let all = PermissionBits(PermissionBits::ALL);
        assert!(all.has(PermissionBits::SEND_CHAT));
        assert!(all.has(PermissionBits::ADD_MOVIE));
        assert!(all.has(PermissionBits::BAN_MEMBER));
        assert!(all.has(PermissionBits::DELETE_ROOM));
        assert!(all.has(PermissionBits::EXPORT_DATA));
        assert!(all.has(PermissionBits::VIEW_PLAYLIST));
        assert!(all.has(PermissionBits::PLAY_CONTROL));
        assert!(all.has(PermissionBits::MANAGE_ADMIN));
    }
}
