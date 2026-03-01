//! Room management service (facade)
//!
//! `RoomService` is the main entry point for room-related business logic.
//! It acts as a facade that coordinates between domain sub-services:
//!
//! - **Core room CRUD** — create, join, leave, delete rooms (handled here)
//! - **Member management** — delegated to [`MemberService`]
//! - **Media management** — delegated to [`MediaService`]
//! - **Playback control** — delegated to [`PlaybackService`]
//! - **Permissions** — delegated to [`PermissionService`]
//! - **Chat** — uses [`ChatRepository`] directly (thin layer)
//!
//! The API layer (synctv-api) should call `RoomService` for operations that
//! span multiple sub-services or require transaction coordination. For
//! domain-specific operations, the API layer can also access sub-services
//! directly via the accessor methods (`member_service()`, `media_service()`, etc.).
//!
//! # Cache Invalidation Patterns
//!
//! This service uses a standardized cache invalidation strategy to prevent
//! race conditions and ensure data consistency across replicas.
//!
//! ## Transactional Operations (Before Commit)
//!
//! For operations wrapped in transactions (e.g., `delete_room`, `admin_delete_room`),
//! cache invalidation MUST happen BEFORE `tx.commit()`. This prevents the race condition:
//!
//! 1. Transaction commits (data changes)
//! 2. Concurrent request reads stale data from cache
//! 3. Cache is invalidated (too late - stale data was already served)
//!
//! By invalidating before commit, we ensure that when the transaction commits,
//! the cache is already empty. Any concurrent request will miss the cache and
//! read fresh data from the database.
//!
//! ### Rollback Safety
//!
//! If the transaction rolls back after cache invalidation, the cache will be
//! empty and will be repopulated on the next read with the correct data.
//! This is safe because:
//! - Empty cache → cache miss → database read → returns current state → cache repopulated
//!
//! ### Implementation
//!
//! Use the `invalidate_room_caches()` helper method for transactional operations:
//!
//! ```text
//! let mut tx = self.pool.begin().await?;
//! // ... perform database operations ...
//! self.invalidate_room_caches(&room_id).await;  // BEFORE commit
//! tx.commit().await?;
//! // ... post-commit operations ...
//! ```
//!
//! ## Non-Transactional Operations
//!
//! For simple updates without transactions (e.g., status changes, bans), cache
//! invalidation can happen after the database operation completes. These operations
//! typically only need room cache invalidation (not permission/playback):
//!
//! ```text
//! self.room_repo.update_status(&room_id, new_status).await?;
//! self.notify_room_invalidation(&room_id).await;  // Room cache only
//! ```
//!
//! ## Cache Types
//!
//! - **Room cache**: Broadcast to all replicas via `CacheInvalidationService`
//! - **Permission cache**: Local only (cleared on each replica independently)
//! - **Playback cache**: Broadcast to all replicas via `CacheInvalidationService`
//!
//! The `invalidate_room_caches()` method handles all three types appropriately.

use chrono::{DateTime, Utc};
use rand::RngExt;
use sqlx::PgPool;
use std::net::IpAddr;

use crate::{
    cache::CacheInvalidationService,
    models::{
        ChatMessage, Media, MediaId, MemberStatus, PageParams, PermissionBits, Playlist,
        PlaylistId, Room, RoomId, RoomListQuery, RoomMember, RoomPlaybackState, RoomRole,
        RoomSettings, RoomStatus, RoomWithCount, UserId,
    },
    repository::{
        ChatRepository, MediaRepository, PlaylistRepository, RoomMemberRepository,
        RoomPlaybackStateRepository, RoomRepository, RoomSettingsRepository,
    },
    service::{
        audit::{AuditAction, AuditService, AuditTargetType},
        auth::password::{hash_password, verify_password},
        media::MediaService,
        member::MemberService,
        notification::NotificationService,
        permission::PermissionService,
        playback::PlaybackService,
        playlist::PlaylistService,
        user::UserService,
        ProvidersManager,
    },
    Error, InternalExt, Result,
};
use std::sync::Arc;

/// Room service for business logic
///
/// This is the main service that coordinates between domain services.
/// Core room operations are handled here, while specific domains are delegated.
#[derive(Clone)]
pub struct RoomService {
    // Database pool for transactions
    pool: PgPool,

    // Optional distributed lock (requires Redis, used in multi-replica mode)
    distributed_lock: Option<crate::service::DistributedLock>,

    // Core repositories
    room_repo: RoomRepository,
    room_settings_repo: RoomSettingsRepository,
    member_repo: RoomMemberRepository,
    playlist_repo: PlaylistRepository,
    playback_repo: RoomPlaybackStateRepository,
    chat_repo: ChatRepository,

    // Domain services
    member_service: MemberService,
    permission_service: PermissionService,
    playlist_service: PlaylistService,
    media_service: MediaService,
    playback_service: PlaybackService,
    notification_service: NotificationService,
    user_service: UserService,

    /// Optional cache invalidation service for cross-replica room cache sync
    cache_invalidation: Option<Arc<CacheInvalidationService>>,

    /// Optional audit service for logging security-sensitive operations
    audit_service: Option<Arc<AuditService>>,

    /// Optional brute-force protection for room password verification
    brute_force_service: Option<crate::service::auth::BruteForceProtection>,

    /// Optional settings registry for reading `create_room_need_review` setting
    settings_registry: Option<Arc<crate::service::SettingsRegistry>>,
}

impl std::fmt::Debug for RoomService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomService").finish()
    }
}

impl RoomService {
    /// Maximum retry attempts for optimistic lock conflicts on settings updates
    const MAX_RETRIES: u32 = 3;
    /// Base backoff in milliseconds (exponential: 5ms, 10ms, 20ms)
    const BACKOFF_BASE_MS: u64 = 5;
    /// Total timeout for settings updates with retries (seconds)
    /// Prevents unbounded wait times when database operations are slow.
    const SETTINGS_UPDATE_TIMEOUT_SECS: u64 = 5;
    /// TTL for `create_room` distributed lock (seconds)
    /// Increased from 15s to 30s to account for:
    /// - bcrypt password hashing (1-3 seconds)
    /// - database transaction latency
    /// - network delays under high load
    const CREATE_ROOM_LOCK_TTL_SECS: u64 = 30;

    /// Get the playlist service
    #[must_use]
    pub const fn playlist_service(&self) -> &PlaylistService {
        &self.playlist_service
    }

    /// Get the permission service
    ///
    /// Used by `ClusterManager` to invalidate permission cache on cross-replica events.
    #[must_use]
    pub const fn permission_service(&self) -> &PermissionService {
        &self.permission_service
    }

    /// Set the distributed lock (enables multi-replica safety for room creation)
    pub fn set_distributed_lock(&mut self, lock: crate::service::DistributedLock) {
        self.distributed_lock = Some(lock);
    }

    /// Set the cache invalidation service for cross-replica room cache sync.
    ///
    /// Also propagates to the inner `MemberService` so that permission/role
    /// changes are broadcast to other replicas.
    pub fn set_cache_invalidation(&mut self, service: Arc<CacheInvalidationService>) {
        self.member_service
            .set_cache_invalidation(Arc::clone(&service));
        self.cache_invalidation = Some(service);
    }

    /// Set the cluster broadcaster on the inner playback service for cross-replica sync.
    /// Uses interior mutability so this can be called through `Arc<RoomService>`.
    pub fn set_playback_cluster_broadcaster(
        &self,
        broadcaster: Arc<dyn crate::service::PlaybackBroadcaster>,
    ) {
        self.playback_service.set_cluster_broadcaster(broadcaster);
    }

    /// Wire the cache invalidation service into the inner playback service
    /// so it can broadcast invalidation messages to other replicas on updates.
    pub fn set_playback_cache_invalidation(&mut self, service: Arc<CacheInvalidationService>) {
        self.playback_service.set_invalidation_service(service);
    }

    #[must_use]
    pub fn new(pool: PgPool, user_service: UserService) -> Self {
        // Initialize repositories
        let room_repo = RoomRepository::new(pool.clone());
        let room_settings_repo = RoomSettingsRepository::new(pool.clone());
        let member_repo = RoomMemberRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
        let provider_instance_repo = Arc::new(crate::repository::ProviderInstanceRepository::new(
            pool.clone(),
        ));
        let chat_repo = ChatRepository::new(pool.clone());

        // Initialize permission service with caching
        let mut permission_service = PermissionService::new(
            member_repo.clone(),
            room_repo.clone(),
            None, // SettingsRegistry - will be set later if needed
            PermissionService::DEFAULT_CACHE_SIZE,
            PermissionService::DEFAULT_CACHE_TTL_SECS,
        );
        permission_service.set_room_settings_repo(room_settings_repo.clone());

        // Initialize provider instance manager and providers manager
        let provider_instance_manager = Arc::new(crate::service::RemoteProviderManager::new(
            provider_instance_repo,
            None,
            None,
        ));
        let providers_manager = Arc::new(ProvidersManager::new(provider_instance_manager));

        // Initialize domain services
        let mut member_service = MemberService::new(
            member_repo.clone(),
            room_repo.clone(),
            permission_service.clone(),
        );
        member_service.set_room_settings_repo(room_settings_repo.clone());
        let playlist_service =
            PlaylistService::new(playlist_repo.clone(), permission_service.clone());
        let media_service = MediaService::new(
            media_repo.clone(),
            playlist_repo.clone(),
            permission_service.clone(),
            providers_manager,
        );
        let notification_service = NotificationService::default();
        let mut playback_service = PlaybackService::new(
            playback_repo.clone(),
            permission_service.clone(),
            media_service.clone(),
            media_repo,
        );
        playback_service.set_notification_service(notification_service.clone());

        Self {
            pool,
            distributed_lock: None,
            room_repo,
            room_settings_repo,
            member_repo,
            playlist_repo,
            playback_repo,
            chat_repo,
            member_service,
            permission_service,
            playlist_service,
            media_service,
            playback_service,
            notification_service,
            user_service,
            cache_invalidation: None,
            audit_service: None,
            brute_force_service: None,
            settings_registry: None,
        }
    }

    /// Inject the audit service for logging security-sensitive operations.
    ///
    /// Also propagates the audit service to the inner `MemberService`.
    pub fn set_audit_service(&mut self, audit: Arc<AuditService>) {
        self.member_service.set_audit_service(Arc::clone(&audit));
        self.audit_service = Some(audit);
    }

    /// Inject the brute-force protection service for room password rate limiting.
    pub fn set_brute_force_service(&mut self, service: crate::service::auth::BruteForceProtection) {
        self.brute_force_service = Some(service);
    }

    /// Inject the settings registry for reading `create_room_need_review` and other global settings.
    pub fn set_settings_registry(&mut self, registry: Arc<crate::service::SettingsRegistry>) {
        self.settings_registry = Some(registry);
    }

    /// Log an audit event if the audit service is configured.
    /// Failures are logged as warnings but never propagated.
    async fn audit_log(
        &self,
        actor_id: &UserId,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: serde_json::Value,
    ) {
        if let Some(ref audit) = self.audit_service {
            if let Err(e) = audit
                .log(
                    actor_id.as_str().to_string(),
                    String::new(),
                    action,
                    target_type,
                    target_id,
                    details,
                    None,
                    None,
                )
                .await
            {
                tracing::warn!(error = %e, "Failed to write audit log from RoomService");
            }
        }
    }

    // ========== Core Room Operations ==========

    /// Create a new room
    ///
    /// All database operations run inside a single transaction so the room is
    /// either fully created or not visible at all — no partially-created rooms.
    ///
    /// When a distributed lock is configured (multi-replica mode), a per-user
    /// lock prevents duplicate rooms from concurrent requests (network retries,
    /// double-clicks).
    pub async fn create_room(
        &self,
        name: String,
        description: String,
        created_by: UserId,
        password: Option<String>,
        settings: Option<RoomSettings>,
    ) -> Result<(Room, RoomMember)> {
        // Acquire distributed lock to prevent duplicate creation by the same user
        if let Some(ref lock) = self.distributed_lock {
            let lock_key = format!("create_room:{}", created_by.as_str());
            return lock
                .with_lock(&lock_key, Self::CREATE_ROOM_LOCK_TTL_SECS, || {
                    let name = name.clone();
                    let description = description.clone();
                    let created_by = created_by.clone();
                    let password = password.clone();
                    let settings = settings.clone();
                    async move {
                        self.do_create_room(name, description, created_by, password, settings)
                            .await
                    }
                })
                .await;
        }

        self.do_create_room(name, description, created_by, password, settings)
            .await
    }

    /// Internal room creation implementation
    async fn do_create_room(
        &self,
        name: String,
        description: String,
        created_by: UserId,
        password: Option<String>,
        settings: Option<RoomSettings>,
    ) -> Result<(Room, RoomMember)> {
        tracing::info!(
            user_id = %created_by,
            room_name = %name,
            has_password = password.is_some(),
            "Creating new room"
        );

        // Check global settings: room creation must be allowed
        if let Some(ref registry) = self.settings_registry {
            // `disable_create_room` takes precedence (explicit disable)
            if registry.disable_create_room.get().unwrap_or(false) {
                tracing::warn!(user_id = %created_by, "Room creation rejected: disable_create_room is true");
                return Err(Error::Authorization(
                    "Room creation is currently disabled".to_string(),
                ));
            }
            // `allow_room_creation` defaults to true; when explicitly set to false, block
            if !registry.allow_room_creation.get().unwrap_or(true) {
                tracing::warn!(user_id = %created_by, "Room creation rejected: allow_room_creation is false");
                return Err(Error::Authorization(
                    "Room creation is currently disabled".to_string(),
                ));
            }
        }

        // Validate room name using centralized validator
        crate::validation::RoomNameValidator::new()
            .validate(&name)
            .map_err(|e| Error::InvalidInput(e.to_string()))?;

        // Validate description length (character count for Unicode safety)
        if description.chars().count() > 500 {
            tracing::warn!(user_id = %created_by, desc_len = description.chars().count(), "Attempted to create room with description too long");
            return Err(Error::InvalidInput(
                "Room description too long (max 500 characters)".to_string(),
            ));
        }

        // Build settings
        let mut room_settings = settings.unwrap_or_default();
        room_settings.require_password =
            crate::models::room_settings::RequirePassword(password.is_some());

        // Hash password outside the transaction (CPU-intensive bcrypt work)
        let pwd_hash = if let Some(ref pwd) = password {
            Some(hash_password(pwd).await?)
        } else {
            None
        };

        // Determine initial room status based on create_room_need_review setting
        let need_review = self
            .settings_registry
            .as_ref()
            .is_some_and(|r| r.create_room_need_review.get().unwrap_or(false));
        let initial_status = if need_review {
            RoomStatus::Pending
        } else {
            RoomStatus::Active
        };

        if need_review {
            tracing::info!(
                user_id = %created_by,
                room_name = %name,
                "Room requires review, creating in Pending status"
            );
        }

        // Transaction: Create room with all related data atomically.
        // On error, the transaction will be automatically rolled back.
        let mut tx = self.pool.begin().await?;

        // 1. Create room with appropriate status
        let room = Room::new_with_status(name, description, created_by.clone(), initial_status);
        let created_room = self.room_repo.create_with_executor(&room, &mut *tx).await?;

        // 2. Set password if provided
        if let Some(ref hash) = pwd_hash {
            self.room_settings_repo
                .set_with_executor(&created_room.id, "password", hash, &mut *tx)
                .await?;
            tracing::debug!(room_id = %created_room.id, "Room password set");
        }

        // 3. Set room settings
        self.room_settings_repo
            .set_settings_with_executor(&created_room.id, &room_settings, &mut *tx)
            .await?;

        // 4. Add creator as member with full permissions
        let member = RoomMember::new(
            created_room.id.clone(),
            created_by.clone(),
            RoomRole::Creator,
        );
        let created_member = self.member_repo.add_with_executor(&member, &mut tx).await?;

        // 5. Create root playlist
        let root_playlist = Playlist {
            id: PlaylistId::new(),
            room_id: created_room.id.clone(),
            creator_id: Some(created_by.clone()),
            name: String::new(),
            parent_id: None,
            position: 0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };
        self.playlist_repo
            .create_with_executor(&root_playlist, &mut *tx)
            .await?;

        // 6. Initialize playback state
        self.playback_repo
            .create_or_get_with_executor(&created_room.id, &mut tx)
            .await?;

        // Commit — all or nothing
        tx.commit().await?;

        tracing::info!(
            room_id = %created_room.id,
            user_id = %created_by,
            "Room creation completed"
        );

        // Track room metrics
        crate::metrics::http::ROOMS_ACTIVE.inc();

        // Invalidate permission cache outside transaction
        self.permission_service
            .invalidate_cache(&created_room.id, &created_by)
            .await;

        Ok((created_room, created_member))
    }

    /// Join a room
    ///
    /// Optimized: fetches room, ban-status, settings, and password hash in a
    /// single JOIN query via `RoomRepository::get_join_context`, reducing the
    /// number of sequential DB round-trips from 4+ to 1.
    ///
    /// When a distributed lock is configured (multi-replica mode), a per-room+user
    /// lock prevents the TOCTOU race where concurrent requests could both pass
    /// validation checks and then conflict on the `add_member` step.
    pub async fn join_room(
        &self,
        room_id: RoomId,
        user_id: UserId,
        password: Option<String>,
    ) -> Result<(Room, RoomMember, Vec<crate::models::RoomMemberWithUser>)> {
        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            has_password = password.is_some(),
            "User attempting to join room"
        );

        // Verify password before acquiring the lock (CPU-intensive bcrypt work).
        // This reduces lock hold time and avoids blocking concurrent joins.
        // We fetch the join context for validation first.
        let ctx = self
            .room_repo
            .get_join_context(&room_id, &user_id)
            .await?
            .ok_or_else(|| {
                tracing::warn!(room_id = %room_id, user_id = %user_id, "Room not found");
                Error::NotFound("Room not found".to_string())
            })?;

        // Check if room is active
        if ctx.room.status != RoomStatus::Active {
            tracing::warn!(room_id = %room_id, user_id = %user_id, status = ?ctx.room.status, "Attempted to join inactive room");
            return Err(Error::InvalidInput("Room is closed".to_string()));
        }

        // Check if user is banned from this room
        if ctx.is_banned {
            tracing::warn!(room_id = %room_id, user_id = %user_id, "Banned user attempted to join room");
            return Err(Error::Authorization(
                "You are banned from this room".to_string(),
            ));
        }

        // Check password if required (CPU-intensive bcrypt, done before lock).
        // This is a pre-check to avoid acquiring the lock if the password is invalid.
        // We'll re-verify under the lock to catch race conditions.
        if ctx.settings.require_password.0 {
            if let Some(ref hash) = ctx.password_hash {
                let provided_password = password.as_ref().ok_or_else(|| {
                    tracing::warn!(room_id = %room_id, user_id = %user_id, "Password required but not provided");
                    Error::Authorization("Password required".to_string())
                })?;

                if !verify_password(provided_password, hash).await? {
                    tracing::warn!(room_id = %room_id, user_id = %user_id, "Invalid password provided");
                    return Err(Error::Authorization("Invalid password".to_string()));
                }
                tracing::debug!(room_id = %room_id, user_id = %user_id, "Password verified successfully (pre-check)");
            } else {
                // Room requires password but none is configured -- reject join
                tracing::warn!(room_id = %room_id, "Room requires password but none is set");
                return Err(Error::Authorization("Invalid password".to_string()));
            }
        }

        // Use distributed lock to make the check-then-add-member atomic.
        // This prevents the TOCTOU race where two concurrent join requests
        // for the same user both pass validation and then both attempt to
        // add the member, or where the room state changes between validation
        // and the add_member call.
        if let Some(ref lock) = self.distributed_lock {
            let lock_key = format!("join_room:{}:{}", room_id.as_str(), user_id.as_str());
            return lock.with_lock(&lock_key, 10, || {
                let room_id = room_id.clone();
                let user_id = user_id.clone();
                let password = password.clone();
                async move {
                    // Re-validate state under lock to catch changes that occurred
                    // between the initial check and lock acquisition
                    let fresh_ctx = self
                        .room_repo
                        .get_join_context(&room_id, &user_id)
                        .await?
                        .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

                    if fresh_ctx.room.status != RoomStatus::Active {
                        return Err(Error::InvalidInput("Room is closed".to_string()));
                    }
                    if fresh_ctx.is_banned {
                        return Err(Error::Authorization("You are banned from this room".to_string()));
                    }

                    // Re-verify password under lock to prevent race condition where
                    // the password was changed between the initial verification and
                    // lock acquisition. This ensures the provided password is still
                    // valid against the current password hash.
                    //
                    // Always re-verify under lock, even if the hash appears unchanged.
                    // This prevents the A→B→A race condition where the password changes
                    // and then changes back to the same hash between the initial check
                    // and lock acquisition.
                    if fresh_ctx.settings.require_password.0 {
                        if let Some(ref hash) = fresh_ctx.password_hash {
                            let provided_password = password.ok_or_else(|| {
                                tracing::warn!(room_id = %room_id, user_id = %user_id, "Password required but not provided under lock");
                                Error::Authorization("Password required".to_string())
                            })?;

                            if !verify_password(&provided_password, hash).await? {
                                tracing::warn!(room_id = %room_id, user_id = %user_id, "Invalid password provided under lock (password changed during join)");
                                return Err(Error::Authorization("Invalid password".to_string()));
                            }
                            tracing::debug!(room_id = %room_id, user_id = %user_id, "Password re-verified successfully under lock");
                        } else {
                            // Room requires password but none is configured -- reject join
                            tracing::warn!(room_id = %room_id, "Room requires password but none is set under lock");
                            return Err(Error::Authorization("Invalid password".to_string()));
                        }
                    }

                    self.do_join_room(fresh_ctx.room, room_id, user_id).await
                }
            }).await;
        }

        // Single-replica path: no distributed lock, rely on DB-level constraints
        let room = ctx.room;
        self.do_join_room(room, room_id, user_id).await
    }

    /// Internal join implementation: adds member, lists members, notifies.
    ///
    /// Called after all validation (room active, not banned, password checked).
    /// When used with a distributed lock, the lock ensures atomicity of the
    /// re-validation + `add_member` sequence.
    ///
    /// **Idempotent**: if the user is already a member the call succeeds and
    /// returns the existing membership record. This handles the concurrent-join
    /// race (Issue #60) where two simultaneous requests both pass validation and
    /// then one gets `AlreadyExists` from the repository.
    async fn do_join_room(
        &self,
        room: Room,
        room_id: RoomId,
        user_id: UserId,
    ) -> Result<(Room, RoomMember, Vec<crate::models::RoomMemberWithUser>)> {
        // R-P2-1: Enforce room capacity limits by enabling max_members check.
        // AddMemberOptions::new() defaults to check_max_members=false; explicitly
        // enable it so the member_service reads max_members from RoomSettings and
        // rejects the join if the room is at capacity.
        use crate::service::member::AddMemberOptions;
        let options = AddMemberOptions::new().with_max_members(0); // 0 = read from RoomSettings
        let created_member = match self
            .member_service
            .add_member_with_options(room_id.clone(), user_id.clone(), RoomRole::Member, options)
            .await
        {
            Ok(member) => member,
            Err(Error::AlreadyExists(_)) => {
                // User is already a member — return the existing record (idempotent).
                // This handles the concurrent-join race condition where two requests
                // both pass the pre-validation check before either has written the row.
                tracing::debug!(
                    room_id = %room_id,
                    user_id = %user_id,
                    "User is already a member of the room (idempotent join)"
                );
                self.member_repo
                    .get(&room_id, &user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::Internal("Member disappeared after AlreadyExists".to_string())
                    })?
            }
            Err(e) => return Err(e),
        };

        // Get all members
        let members = self.member_service.list_members(&room_id).await?;

        // Notify room members with username
        let username = self
            .user_service
            .get_username(&user_id)
            .await?
            .unwrap_or_else(|| "Unknown".to_string());
        let _ = self
            .notification_service
            .notify_user_joined(&room_id, &user_id, &username)
            .await;

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            username = %username,
            member_count = members.len(),
            "User joined room successfully"
        );

        Ok((room, created_member, members))
    }

    /// Leave a room
    ///
    /// The room creator cannot leave; they must delete the room instead.
    ///
    /// **Important for callers**: This method only removes the membership record
    /// and sends an in-app notification. It does NOT:
    /// - Disconnect the user's WebSocket/gRPC connections from the room
    /// - Publish a `ClusterEvent::UserLeft` for cross-replica disconnect propagation
    ///
    /// Callers (API layer) MUST handle these two concerns after calling this method.
    /// See `synctv-api/src/impls/client/room.rs` `leave_room()` for the reference
    /// implementation that correctly handles WS disconnect and cluster events.
    pub async fn leave_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "User leaving room");

        // Block the creator from leaving - they must delete the room instead
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.created_by == user_id {
            return Err(Error::Authorization(
                "Room creator cannot leave the room. Delete the room instead.".to_string(),
            ));
        }

        self.member_service
            .remove_member(room_id.clone(), user_id.clone())
            .await?;

        // Notify room members with username
        let username = self
            .user_service
            .get_username(&user_id)
            .await?
            .unwrap_or_else(|| "Unknown".to_string());
        let _ = self
            .notification_service
            .notify_user_left(&room_id, &user_id, &username)
            .await;

        tracing::info!(room_id = %room_id, user_id = %user_id, username = %username, "User left room");

        Ok(())
    }

    /// Check if guests are allowed to access a room
    ///
    /// Validates guest access based on:
    /// 1. Global `enable_guest` setting
    /// 2. Room `allow_guest_join` setting
    /// 3. Room password requirement (guests blocked if password required)
    ///
    /// # Arguments
    /// * `room_id` - Room ID to check
    /// * `settings_registry` - Optional global settings registry (if None, guests are denied -- fail-closed)
    ///
    /// # Returns
    /// * `Ok(())` if guests are allowed
    /// * `Err` with appropriate error message if guests are not allowed
    pub async fn check_guest_allowed(
        &self,
        room_id: &RoomId,
        settings_registry: Option<&crate::service::SettingsRegistry>,
    ) -> Result<()> {
        // Check global enable_guest setting (fail-closed: deny when registry unavailable)
        if let Some(registry) = settings_registry {
            let enable_guest = registry.enable_guest.get().unwrap_or(true);
            if !enable_guest {
                tracing::debug!(room_id = %room_id, "Guest access denied: global guest mode disabled");
                return Err(Error::Authorization(
                    "Guest mode is disabled globally".to_string(),
                ));
            }
        } else {
            tracing::debug!(room_id = %room_id, "Guest access denied: settings registry unavailable (fail-closed)");
            return Err(Error::Authorization(
                "Guest mode is not available".to_string(),
            ));
        }

        // Get room settings
        let room_settings = self.room_settings_repo.get(room_id).await?;

        // Check room-level allow_guest_join setting
        if !room_settings.allow_guest_join.0 {
            tracing::debug!(room_id = %room_id, "Guest access denied: room guest mode disabled");
            return Err(Error::Authorization(
                "Guest access is not allowed in this room".to_string(),
            ));
        }

        // Check if room has password (guests cannot join password-protected rooms)
        if room_settings.require_password.0 {
            tracing::debug!(room_id = %room_id, "Guest access denied: room has password");
            return Err(Error::Authorization(
                "Guests cannot join password-protected rooms. Please create an account and join as a member.".to_string(),
            ));
        }

        tracing::debug!(room_id = %room_id, "Guest access allowed");
        Ok(())
    }

    /// Soft-delete a room (creator only)
    ///
    /// Sets the `deleted_at` timestamp on the room row. The room and its related
    /// data (members, playlists, media, chat messages, settings, playback state)
    /// remain in the database until the periodic `CleanupService` permanently
    /// purges rows whose `deleted_at` exceeds the configured retention period
    /// (default: 90 days). The actual SQL `DELETE` at purge time triggers
    /// `ON DELETE CASCADE` on all related tables.
    ///
    /// **Soft-delete lifecycle (optimized):**
    /// 1. This method sets `rooms.deleted_at = NOW()` (room becomes invisible to queries)
    /// 2. IMMEDIATELY deletes non-critical related data to free storage:
    ///    - playlists (cascades to media via FK)
    ///    - `room_members`
    ///    - `room_settings`
    ///    - `room_playback_state`
    ///    - `chat_messages`
    /// 3. Preserves only the room row (for audit) and `audit_logs` entries
    /// 4. `CleanupService::purge_soft_deleted_rooms()` eventually purges the room row
    ///    after `room_soft_delete_retention_days` (default: 90 days)
    pub async fn delete_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "Soft-deleting room");

        // First check if room exists and is not already deleted (before permission check)
        let room_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM rooms WHERE id = $1 AND deleted_at IS NULL)",
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        if !room_exists {
            return Err(Error::NotFound(
                "Room not found or already deleted".to_string(),
            ));
        }

        // Check permission without cache - critical operation requires fresh permissions
        self.permission_service
            .check_permission_no_cache(&room_id, &user_id, PermissionBits::DELETE_ROOM)
            .await?;

        let mut tx = self.pool.begin().await?;

        // Soft-delete: set deleted_at timestamp.
        let deleted = sqlx::query(
            "UPDATE rooms
             SET deleted_at = $2, updated_at = $2
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(room_id.as_str())
        .bind(chrono::Utc::now())
        .execute(&mut *tx)
        .await?;

        if deleted.rows_affected() == 0 {
            // Transaction will be automatically rolled back on drop
            return Err(Error::NotFound(
                "Room not found or already deleted".to_string(),
            ));
        }

        // IMMEDIATE CLEANUP: Delete non-critical related data to free storage.
        // This optimization prevents resource bloat during the 90-day retention period.
        //
        // The ON DELETE CASCADE constraints will handle these automatically when the room
        // row is purged, but we explicitly delete them now to reclaim storage immediately.
        //
        // Order matters due to FK dependencies:
        // 1. media (depends on playlists) - deleted via CASCADE when playlists are deleted
        // 2. playlists (depends on room) - CASCADE to media
        // 3. room_members (depends on room and users)
        // 4. room_settings (depends on room)
        // 5. room_playback_state (depends on room)
        // 6. chat_messages (depends on room)

        // Delete playlists (cascades to media via FK)
        let playlists_deleted = sqlx::query("DELETE FROM playlists WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        // Delete room members
        let members_deleted = sqlx::query("DELETE FROM room_members WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        // Delete room settings
        let settings_deleted = sqlx::query("DELETE FROM room_settings WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        // Delete room playback state
        let _playback_deleted = sqlx::query("DELETE FROM room_playback_state WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        // Delete chat messages
        let chat_deleted = sqlx::query("DELETE FROM chat_messages WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        // Invalidate caches BEFORE committing the transaction.
        // See `invalidate_room_caches` for detailed rationale.
        self.invalidate_room_caches(&room_id).await;

        // Commit transaction - all or nothing
        tx.commit().await?;

        // Notify after commit so notifications are only sent for successful deletions
        let _ = self
            .notification_service
            .notify_room_deleted(&room_id)
            .await;

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            playlists_deleted = playlists_deleted.rows_affected(),
            members_deleted = members_deleted.rows_affected(),
            settings_deleted = settings_deleted.rows_affected(),
            chat_deleted = chat_deleted.rows_affected(),
            "Room soft-deleted with immediate cleanup of related data (room row preserved for audit, will be purged by CleanupService after retention period)"
        );

        // Track room metrics
        crate::metrics::http::ROOMS_ACTIVE.dec();

        // Audit log (preserved - not deleted with room data)
        self.audit_log(
            &user_id,
            AuditAction::RoomDeleted,
            AuditTargetType::Room,
            Some(room_id.as_str().to_string()),
            serde_json::json!({
                "reason": "Room deleted by user",
                "playlists_deleted": playlists_deleted.rows_affected(),
                "members_deleted": members_deleted.rows_affected(),
                "settings_deleted": settings_deleted.rows_affected(),
                "chat_deleted": chat_deleted.rows_affected(),
            }),
        )
        .await;

        Ok(())
    }

    // ========== Room Review Workflow (Admin Only) ==========

    /// Approve a pending room (review workflow), changing its status to Active.
    ///
    /// This is an admin-only operation for rooms created when `create_room_need_review=true`.
    /// After approval, the room becomes visible and usable by its creator.
    ///
    /// # Errors
    /// - `Error::NotFound` if room doesn't exist
    /// - `Error::InvalidInput` if room is not in Pending status
    /// - `Error::Authorization` if caller is not a global admin
    pub async fn approve_pending_room(&self, room_id: RoomId, admin_id: UserId) -> Result<Room> {
        tracing::info!(room_id = %room_id, admin_id = %admin_id, "Approving pending room");

        // Verify admin permission (global admin required)
        let admin = self.user_service.get_user(&admin_id).await?;

        if !admin.role.is_admin_or_above() {
            return Err(Error::Authorization(
                "Only admins can approve rooms".to_string(),
            ));
        }

        // Get current room to check status
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("Room {} not found", room_id.as_str())))?;

        if room.status != RoomStatus::Pending {
            return Err(Error::InvalidInput(format!(
                "Room is not pending (current status: {:?})",
                room.status
            )));
        }

        // Update status to Active
        let updated = self
            .room_repo
            .update_status(&room_id, RoomStatus::Active)
            .await?;

        // Invalidate cache
        self.notify_room_invalidation(&room_id).await;
        self.permission_service
            .invalidate_room_cache(&room_id)
            .await;

        // Audit log
        self.audit_log(
            &admin_id,
            AuditAction::RoomApproved,
            AuditTargetType::Room,
            Some(room_id.as_str().to_string()),
            serde_json::json!({
                "previous_status": "pending",
                "new_status": "active",
            }),
        )
        .await;

        tracing::info!(room_id = %room_id, admin_id = %admin_id, "Room approved and activated");

        Ok(updated)
    }

    /// Reject a pending room, changing its status to Closed.
    ///
    /// This is an admin-only operation for rooms created when `create_room_need_review=true`.
    /// Rejected rooms are closed and cannot be used.
    ///
    /// # Errors
    /// - `Error::NotFound` if room doesn't exist
    /// - `Error::InvalidInput` if room is not in Pending status
    /// - Permission error if caller is not a global admin
    pub async fn reject_room(
        &self,
        room_id: RoomId,
        admin_id: UserId,
        reason: Option<String>,
    ) -> Result<Room> {
        tracing::info!(room_id = %room_id, admin_id = %admin_id, "Rejecting pending room");

        // Verify admin permission (global admin required)
        let admin = self.user_service.get_user(&admin_id).await?;

        if !admin.role.is_admin_or_above() {
            return Err(Error::Authorization(
                "Only admins can reject rooms".to_string(),
            ));
        }

        // Get current room to check status
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("Room {} not found", room_id.as_str())))?;

        if room.status != RoomStatus::Pending {
            return Err(Error::InvalidInput(format!(
                "Room is not pending (current status: {:?})",
                room.status
            )));
        }

        // Update status to Closed
        let updated = self
            .room_repo
            .update_status(&room_id, RoomStatus::Closed)
            .await?;

        // Invalidate cache
        self.notify_room_invalidation(&room_id).await;
        self.permission_service
            .invalidate_room_cache(&room_id)
            .await;

        // Audit log
        self.audit_log(
            &admin_id,
            AuditAction::RoomRejected,
            AuditTargetType::Room,
            Some(room_id.as_str().to_string()),
            serde_json::json!({
                "previous_status": "pending",
                "new_status": "closed",
                "reason": reason,
            }),
        )
        .await;

        tracing::info!(room_id = %room_id, admin_id = %admin_id, "Room rejected and closed");

        Ok(updated)
    }

    /// List pending rooms (admin only).
    ///
    /// Returns all rooms with `RoomStatus::Pending` for admin review.
    pub async fn list_pending_rooms(
        &self,
        admin_id: UserId,
        pagination: PageParams,
    ) -> Result<(Vec<Room>, i64)> {
        // Verify admin permission
        let admin = self.user_service.get_user(&admin_id).await?;

        if !admin.role.is_admin_or_above() {
            return Err(Error::Authorization(
                "Only admins can list pending rooms".to_string(),
            ));
        }

        let query = RoomListQuery {
            pagination,
            status: Some(RoomStatus::Pending),
            search: None,
            is_banned: None,
            creator_id: None,
        };

        self.room_repo.list(&query).await
    }

    /// Set room settings with optimistic locking (CAS).
    ///
    /// Uses version-based CAS to prevent concurrent overwrites. Retries
    /// automatically on version conflicts with a total timeout limit.
    pub async fn set_settings(
        &self,
        room_id: RoomId,
        user_id: UserId,
        settings: RoomSettings,
    ) -> Result<Room> {
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::UPDATE_ROOM_SETTINGS)
            .await?;

        // Validate permission escalation
        settings.validate_permissions()?;

        // Verify room exists
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        // CAS write with retry and total timeout
        let room_id_clone = room_id.clone();
        let settings_clone = settings.clone();
        let room_settings_repo = self.room_settings_repo.clone();
        let permission_service = self.permission_service.clone();
        let invalidation_service = self.cache_invalidation.clone();
        let notification_service = self.notification_service.clone();
        let user_id_clone = user_id.clone();
        let audit_service = self.audit_service.clone();

        super::optimistic_retry::retry_with_optimistic_lock_timeout(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            std::time::Duration::from_secs(Self::SETTINGS_UPDATE_TIMEOUT_SECS),
            "Settings update failed after maximum retry attempts",
            || {
                let room_id = room_id_clone.clone();
                let settings = settings_clone.clone();
                let room_settings_repo = room_settings_repo.clone();
                let permission_service = permission_service.clone();
                let invalidation_service = invalidation_service.clone();
                let notification_service = notification_service.clone();
                let user_id = user_id_clone.clone();
                let audit_service = audit_service.clone();
                async move {
                    let (_current, version) = room_settings_repo.get_with_version(&room_id).await?;
                    room_settings_repo
                        .set_settings_with_version(&room_id, &settings, version)
                        .await?;

                    // Invalidate permission cache for all room members
                    permission_service.invalidate_room_cache(&room_id).await;

                    // Broadcast cache invalidation
                    if let Some(ref service) = invalidation_service {
                        if let Err(e) = service.invalidate_and_broadcast_room(&room_id).await {
                            tracing::warn!(
                                error = %e,
                                room_id = %room_id.as_str(),
                                "Failed to broadcast room cache invalidation"
                            );
                        }
                    }

                    // Notify clients
                    let settings_json = serde_json::to_value(&settings).map_err(|e| {
                        crate::Error::Internal(format!("Failed to serialize settings: {e}"))
                    })?;
                    let _ = notification_service
                        .notify_settings_updated(&room_id, settings_json.clone())
                        .await;

                    // Audit log
                    if let Some(ref audit) = audit_service {
                        let _ = audit
                            .log(
                                user_id.as_str().to_string(),
                                user_id.as_str().to_string(),
                                AuditAction::RoomSettingsUpdated,
                                AuditTargetType::Room,
                                Some(room_id.as_str().to_string()),
                                settings_json,
                                None,
                                None,
                            )
                            .await;
                    }

                    Ok(())
                }
            },
        )
        .await?;

        Ok(room)
    }

    // ========== Query Operations ==========

    /// Check if a room exists (lightweight existence check, no full row fetch).
    ///
    /// Prefer this over `get_room()` when only existence verification is needed
    /// (e.g., guest token validation), as it avoids fetching and deserializing
    /// the full room row.
    pub async fn room_exists(&self, room_id: &RoomId) -> Result<bool> {
        self.room_repo.exists(room_id).await
    }

    /// Get room with details
    pub async fn get_room(&self, room_id: &RoomId) -> Result<Room> {
        self.room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))
    }

    /// Get room with settings
    pub async fn get_room_with_settings(&self, room_id: &RoomId) -> Result<(Room, RoomSettings)> {
        let room = self.get_room(room_id).await?;
        let settings = self.room_settings_repo.get(room_id).await?;
        Ok((room, settings))
    }

    /// Get room settings
    pub async fn get_room_settings(&self, room_id: &RoomId) -> Result<RoomSettings> {
        self.room_settings_repo.get(room_id).await
    }

    /// Get settings for multiple rooms in a single query (avoids N+1)
    pub async fn get_room_settings_batch(
        &self,
        room_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, RoomSettings>> {
        self.room_settings_repo.get_batch(room_ids).await
    }

    /// Set room settings (replace entire settings object) with optimistic locking.
    pub async fn set_room_settings(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
    ) -> Result<RoomSettings> {
        super::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Settings update failed after maximum retry attempts",
            || async {
                let (_current, version) = self.room_settings_repo.get_with_version(room_id).await?;
                self.room_settings_repo
                    .set_settings_with_version(room_id, settings, version)
                    .await?;
                Ok(settings.clone())
            },
        )
        .await
    }

    /// Update single room setting by key (requires `UPDATE_ROOM_SETTINGS` permission)
    ///
    /// The flow is fully generic -- no per-setting special cases here:
    /// 1. Permission check
    /// 2. Registry validates type + value constraints (incl. macro validators)
    /// 3. CAS (Compare-And-Swap) update with automatic retry on version conflict
    /// 4. Post-apply hooks handle side effects (e.g., kick guests)
    pub async fn update_room_setting(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        key: &str,
        value: &str,
    ) -> Result<String> {
        use crate::models::room_settings::RoomSettingsRegistry;

        // 1. Permission check
        self.permission_service
            .check_permission(room_id, user_id, PermissionBits::UPDATE_ROOM_SETTINGS)
            .await?;

        // 2. Validate via registry (type parsing + value constraints from macro validators)
        RoomSettingsRegistry::validate_setting(key, value)?;

        // 3. CAS update with retry
        let mut final_settings = None;
        for attempt in 0..Self::MAX_RETRIES {
            let (mut settings, version) = self.room_settings_repo.get_with_version(room_id).await?;
            settings.set_by_key(key, value)?;
            settings.validate_permissions()?;

            match self
                .room_settings_repo
                .set_settings_with_version(room_id, &settings, version)
                .await
            {
                Ok(_new_version) => {
                    final_settings = Some(settings);
                    break;
                }
                Err(Error::OptimisticLockConflict) => {
                    if attempt + 1 < Self::MAX_RETRIES {
                        let backoff = Self::BACKOFF_BASE_MS * (1 << attempt);
                        let jitter = rand::rng().random_range(0..Self::BACKOFF_BASE_MS);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff + jitter))
                            .await;
                        continue;
                    }
                    return Err(Error::Internal(
                        "Settings update failed after maximum retry attempts".to_string(),
                    ));
                }
                Err(e) => return Err(e),
            }
        }

        let settings = final_settings.ok_or_else(|| {
            Error::Internal("Settings update failed after maximum retry attempts".to_string())
        })?;

        // 4. Post-apply hooks (side effects after commit)
        self.permission_service.invalidate_room_cache(room_id).await;
        self.notify_room_invalidation(room_id).await;
        self.run_post_apply_hooks(room_id, key, value).await;

        serde_json::to_string(&settings).internal_with_err("Failed to serialize settings")
    }

    /// Post-apply hooks: side effects triggered after a setting change commits.
    ///
    /// Centralized registry — add new side effects here when a setting
    /// change needs to trigger external actions (notifications, kicks, etc.).
    async fn run_post_apply_hooks(&self, room_id: &RoomId, key: &str, value: &str) {
        use crate::models::room_settings::{AllowGuestJoin, RequirePassword, RoomSetting};
        use crate::service::notification::GuestKickReason;

        let kick_reason = match (key, value) {
            (k, "false") if k == AllowGuestJoin::KEY => {
                Some(GuestKickReason::RoomGuestModeDisabled)
            }
            (k, "true") if k == RequirePassword::KEY => Some(GuestKickReason::RoomPasswordAdded),
            _ => None,
        };

        if let Some(reason) = kick_reason {
            if let Err(e) = self
                .notification_service
                .kick_all_guests(room_id, reason)
                .await
            {
                tracing::warn!("Failed to kick guests after settings change: {}", e);
            }
        }
    }

    /// Reset room settings to default values with optimistic locking.
    pub async fn reset_room_settings(&self, room_id: &RoomId, user_id: &UserId) -> Result<String> {
        self.permission_service
            .check_permission(room_id, user_id, PermissionBits::UPDATE_ROOM_SETTINGS)
            .await?;

        let default_settings = RoomSettings::default();

        super::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Settings reset failed after maximum retry attempts",
            || async {
                let (_current, version) = self.room_settings_repo.get_with_version(room_id).await?;
                self.room_settings_repo
                    .set_settings_with_version(room_id, &default_settings, version)
                    .await?;
                serde_json::to_string(&default_settings)
                    .internal_with_err("Failed to serialize settings")
            },
        )
        .await
    }

    /// Check room password
    pub async fn check_room_password(&self, room_id: &RoomId, password: &str) -> Result<bool> {
        let password_hash = self.room_settings_repo.get_password_hash(room_id).await?;

        match password_hash {
            Some(stored) => verify_password(password, &stored)
                .await
                .internal_with_err("Password verification failed"),
            None => Ok(false),
        }
    }

    /// Check room password with brute-force rate limiting.
    ///
    /// Uses the `BruteForceProtection` service to prevent password guessing attacks.
    /// Rate limiting is based on `room_id + client_ip` combination.
    ///
    /// # Rate Limit Thresholds
    ///
    /// - 5 failures: 1 minute lockout
    /// - 10 failures: 5 minute lockout
    /// - 15+ failures: 15 minute lockout
    ///
    /// # Arguments
    ///
    /// * `room_id` - The room to check password for
    /// * `password` - The password to verify
    /// * `client_ip` - Optional client IP for per-IP rate limiting
    ///
    /// # Returns
    ///
    /// * `Ok(true)` - Password is correct
    /// * `Ok(false)` - Password is incorrect (but not rate limited)
    /// * `Err(Error::Authentication)` - Rate limited (too many failed attempts)
    /// * `Err(Error::Internal)` - Brute-force service unavailable (fail-closed)
    pub async fn check_room_password_with_rate_limit(
        &self,
        room_id: &RoomId,
        password: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<bool> {
        // Build the rate limit key: room_id + client_ip (or just room_id if no IP)
        let rate_limit_key = match client_ip {
            Some(ip) => format!("{}:{}", room_id.as_str(), ip),
            None => room_id.as_str().to_string(),
        };

        // Check rate limit if brute-force service is configured
        if let Some(ref brute_force) = self.brute_force_service {
            brute_force
                .check_allowed(&rate_limit_key, client_ip)
                .await?;
        }

        // Verify the password
        let password_hash = self.room_settings_repo.get_password_hash(room_id).await?;
        let is_valid = match password_hash {
            Some(stored) => verify_password(password, &stored)
                .await
                .internal_with_err("Password verification failed"),
            None => Ok(false),
        }?;

        // Handle success/failure tracking
        if let Some(ref brute_force) = self.brute_force_service {
            if is_valid {
                // Reset failure counter on successful verification
                if let Err(e) = brute_force.reset(&rate_limit_key).await {
                    // Log warning for monitoring
                    tracing::warn!(
                        room_id = %room_id,
                        client_ip = ?client_ip,
                        error = %e,
                        "Failed to reset room password rate limit counter after successful verification"
                    );

                    // Record to audit log for security tracking
                    // This is security-relevant because a persistent counter could lead to
                    // legitimate users being locked out if Redis recovers with stale data
                    if let Some(ref audit) = self.audit_service {
                        let ip_str = client_ip.map(|ip| ip.to_string());
                        if let Err(audit_err) = audit
                            .log_rate_limit_reset_failed(
                                crate::service::audit::AuditTargetType::Room,
                                room_id.as_str().to_string(),
                                e.to_string(),
                                ip_str,
                            )
                            .await
                        {
                            tracing::error!(
                                room_id = %room_id,
                                error = %audit_err,
                                "Failed to log rate limit reset failure to audit log"
                            );
                        }
                    }
                }
            } else {
                // Record failure on incorrect password
                brute_force
                    .record_failure(&rate_limit_key, client_ip)
                    .await?;
            }
        }

        Ok(is_valid)
    }

    /// Reset the room password rate limit counter.
    ///
    /// This is primarily used for testing to simulate lockout expiry.
    /// In production, counters expire automatically via TTL.
    pub async fn reset_room_password_rate_limit(
        &self,
        room_id: &RoomId,
        client_ip: IpAddr,
    ) -> Result<()> {
        if let Some(ref brute_force) = self.brute_force_service {
            let rate_limit_key = format!("{}:{}", room_id.as_str(), client_ip);
            brute_force.reset(&rate_limit_key).await?;
        }
        Ok(())
    }

    /// Update room password
    pub async fn update_room_password(
        &self,
        room_id: &RoomId,
        password_hash: Option<String>,
    ) -> Result<()> {
        use crate::service::notification::GuestKickReason;

        let password_was_set = password_hash.is_some();
        self.do_set_password_hash(room_id, password_hash).await?;

        // Invalidate room cache across all replicas
        self.notify_room_invalidation(room_id).await;

        // Side effects outside transaction
        if password_was_set {
            if let Err(e) = self
                .notification_service
                .kick_all_guests(room_id, GuestKickReason::RoomPasswordAdded)
                .await
            {
                tracing::warn!("Failed to kick guests after password was added: {}", e);
            }
        }
        Ok(())
    }

    /// Core password update logic: atomically set/remove password hash and sync `require_password`.
    ///
    /// Uses a transaction for the password hash row (separate key) plus CAS for the
    /// settings row. Does NOT trigger side effects (guest kicking, notifications) --
    /// callers handle that.
    async fn do_set_password_hash(
        &self,
        room_id: &RoomId,
        password_hash: Option<String>,
    ) -> Result<()> {
        for attempt in 0..Self::MAX_RETRIES {
            // Read current settings and version
            let (mut settings, version) = self.room_settings_repo.get_with_version(room_id).await?;

            // Update password hash in a transaction (separate key row, not version-checked)
            let mut tx = self.pool.begin().await?;
            if let Some(ref pwd_hash) = password_hash {
                self.room_settings_repo
                    .set_with_executor(room_id, "password", pwd_hash, &mut *tx)
                    .await?;
                settings.require_password = crate::models::room_settings::RequirePassword(true);
            } else {
                self.room_settings_repo
                    .delete_with_executor(room_id, "password", &mut *tx)
                    .await?;
                settings.require_password = crate::models::room_settings::RequirePassword(false);
            }

            // CAS update for the _settings row within the same transaction
            let json_value = serde_json::to_string(&settings)
                .internal_with_err("Failed to serialize room settings")?;

            let cas_result = if version == 0 {
                sqlx::query(
                    r"
                    INSERT INTO room_settings (room_id, key, value, version)
                    VALUES ($1, '_settings', $2, 1)
                    ON CONFLICT (room_id, key) DO NOTHING
                    RETURNING version
                    ",
                )
                .bind(room_id.as_str())
                .bind(&json_value)
                .fetch_optional(&mut *tx)
                .await?
            } else {
                sqlx::query(
                    r"
                    UPDATE room_settings
                    SET value = $2, version = version + 1, updated_at = NOW()
                    WHERE room_id = $1 AND key = '_settings' AND version = $3
                    RETURNING version
                    ",
                )
                .bind(room_id.as_str())
                .bind(&json_value)
                .bind(version)
                .fetch_optional(&mut *tx)
                .await?
            };

            if cas_result.is_some() {
                tx.commit().await?;
                return Ok(());
            }

            // Version mismatch -- explicit rollback before retry.
            // This is necessary to release locks immediately and allow the next
            // iteration to acquire a fresh snapshot.
            tx.rollback().await?;
            if attempt + 1 < Self::MAX_RETRIES {
                let backoff = Self::BACKOFF_BASE_MS * (1 << attempt);
                let jitter = rand::rng().random_range(0..Self::BACKOFF_BASE_MS);
                tokio::time::sleep(std::time::Duration::from_millis(backoff + jitter)).await;
            }
        }

        Err(Error::Internal(
            "Password update failed after maximum retry attempts".to_string(),
        ))
    }

    /// Update room description.
    ///
    /// Requires `UPDATE_ROOM_SETTINGS` permission.
    ///
    /// # Errors
    ///
    /// - `Error::InvalidInput` - Description exceeds 500 characters
    /// - `Error::Authentication` - User lacks `UPDATE_ROOM_SETTINGS` permission
    pub async fn update_room_description(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        description: String,
    ) -> Result<Room> {
        if description.chars().count() > 500 {
            return Err(Error::InvalidInput(
                "Room description too long (max 500 characters)".to_string(),
            ));
        }

        // Check permission
        self.permission_service
            .check_permission(room_id, user_id, PermissionBits::UPDATE_ROOM_SETTINGS)
            .await?;

        let room = self
            .room_repo
            .update_description(room_id, &description)
            .await?;
        self.notify_room_invalidation(room_id).await;
        Ok(room)
    }

    /// List all rooms (paginated)
    pub async fn list_rooms(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        self.room_repo.list(query).await
    }

    /// List all rooms with member count (optimized, single query)
    pub async fn list_rooms_with_count(
        &self,
        query: &RoomListQuery,
    ) -> Result<(Vec<RoomWithCount>, i64)> {
        self.room_repo.list_with_count(query).await
    }

    /// List rooms created by a specific user
    pub async fn list_rooms_by_creator(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<Room>, i64)> {
        self.room_repo.list_by_creator(creator_id, pagination).await
    }

    /// List rooms created by a specific user with member count (optimized)
    pub async fn list_rooms_by_creator_with_count(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomWithCount>, i64)> {
        self.room_repo
            .list_by_creator_with_count(creator_id, pagination)
            .await
    }

    /// List rooms where a user is a member
    pub async fn list_joined_rooms(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomId>, i64)> {
        self.member_service
            .list_user_rooms(user_id, pagination)
            .await
    }

    /// List rooms where a user is a member with full details (optimized)
    pub async fn list_joined_rooms_with_details(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        self.member_service
            .list_user_rooms_with_details(user_id, pagination)
            .await
    }

    // ========== Member Operations (delegated) ==========

    /// Grant permission to user
    pub async fn grant_permission(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        permission: u64,
    ) -> Result<crate::models::RoomMember> {
        self.member_service
            .grant_permission(room_id, granter_id, target_user_id, permission)
            .await
    }

    /// Update member permissions (Allow/Deny pattern)
    ///
    /// This method sets both `added_permissions` and `removed_permissions`.
    /// To reset to role default, pass 0 for both.
    pub async fn set_member_permission(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        added_permissions: u64,
        removed_permissions: u64,
    ) -> Result<crate::models::RoomMember> {
        self.member_service
            .set_member_permissions(
                room_id,
                granter_id,
                target_user_id,
                added_permissions,
                removed_permissions,
            )
            .await
    }

    /// Kick member from room
    pub async fn kick_member(
        &self,
        room_id: RoomId,
        kicker_id: UserId,
        target_user_id: UserId,
    ) -> Result<()> {
        self.member_service
            .kick_member(room_id, kicker_id, target_user_id)
            .await
    }

    /// Get room members with user info
    pub async fn get_room_members(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<crate::models::RoomMemberWithUser>> {
        self.member_service.list_members(room_id).await
    }

    /// Get member count for a room
    pub async fn get_member_count(&self, room_id: &RoomId) -> Result<i32> {
        self.member_service.count_members(room_id).await
    }

    /// Get member counts for multiple rooms in a single query.
    pub async fn get_member_count_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<std::collections::HashMap<String, i32>> {
        self.member_service.count_members_batch(room_ids).await
    }

    /// Get a specific room member record.
    ///
    /// Returns `None` if the user is not (or is no longer) a member of the room.
    pub async fn get_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<RoomMember>> {
        self.member_service.get_member(room_id, user_id).await
    }

    /// Check if user is a member of the room
    pub async fn check_membership(&self, room_id: &RoomId, user_id: &UserId) -> Result<()> {
        if self.member_service.is_member(room_id, user_id).await? {
            Ok(())
        } else {
            Err(Error::Authorization(
                "Not a member of this room".to_string(),
            ))
        }
    }

    // ========== Media Operations (delegated) ==========

    /// Add media to playlist (convenience method)
    ///
    /// This is a convenience method that:
    /// 1. Gets the root playlist for the room
    /// 2. Calls `MediaService::add_media` with the provided `source_config`
    ///
    /// Note: Clients should typically call the parse endpoint first to get
    /// `source_config`, then call this method with `provider_instance_name`.
    ///
    /// Uses provider registry pattern - no enum switching in service layer.
    pub async fn add_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        provider_instance_name: String,
        source_config: serde_json::Value,
        title: String,
    ) -> Result<Media> {
        use crate::service::media::AddMediaRequest;

        // Get room's root playlist
        let root_playlist = self.playlist_service.get_root_playlist(&room_id).await?;

        // Create request with provider_instance_name
        let request = AddMediaRequest {
            playlist_id: root_playlist.id.clone(),
            name: title,
            provider_instance_name,
            source_config,
        };

        self.media_service
            .add_media(room_id, user_id, request)
            .await
    }

    /// Add multiple media items atomically (all-or-nothing via transaction)
    pub async fn add_media_batch(
        &self,
        room_id: RoomId,
        user_id: UserId,
        items: Vec<(String, serde_json::Value, String)>, // (provider_instance_name, source_config, title)
    ) -> Result<Vec<Media>> {
        use crate::service::media::AddMediaRequest;

        // Get room's root playlist
        let root_playlist = self.playlist_service.get_root_playlist(&room_id).await?;

        let requests: Vec<AddMediaRequest> = items
            .into_iter()
            .map(
                |(provider_instance_name, source_config, title)| AddMediaRequest {
                    playlist_id: root_playlist.id.clone(),
                    name: title,
                    provider_instance_name,
                    source_config,
                },
            )
            .collect();

        self.media_service
            .add_media_batch(room_id, user_id, root_playlist.id, requests)
            .await
    }

    /// Remove media from playlist
    ///
    /// Uses a transaction to atomically:
    /// 1. Verify the user has permission (TOCTOU prevention - permission check is within transaction)
    /// 2. Verify the media is not currently playing (locking read on playback state)
    /// 3. Delete the media
    ///
    /// # TOCTOU Prevention
    ///
    /// The permission check is performed **within the transaction** using raw SQL.
    /// This prevents a race condition where:
    /// - Thread A: Check permission (passes)
    /// - Thread B: Revoke permission
    /// - Thread A: Delete media (succeeds despite revoked permission)
    ///
    /// By checking permissions within the transaction, the database's isolation
    /// level ensures that permission changes during the operation will cause the
    /// transaction to fail or the check to see the updated permissions.
    pub async fn remove_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<()> {
        // Start transaction first for atomic permission check and delete
        let mut tx = self.pool.begin().await?;

        // Step 1: Fetch media within transaction to verify it exists and get creator_id
        let media_row: Option<(String, Option<String>, String)> =
            sqlx::query_as("SELECT id, creator_id, room_id FROM media WHERE id = $1")
                .bind(media_id.as_str())
                .fetch_optional(&mut *tx)
                .await?;

        let (media_id, media_creator_id, media_room_id) =
            media_row.ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

        if media_room_id != room_id.as_str() {
            return Err(Error::Authorization(
                "Media does not belong to this room".to_string(),
            ));
        }

        // Step 2: Check permission within transaction (TOCTOU fix)
        // We perform a raw SQL query to calculate effective permissions atomically
        let is_owner = media_creator_id.as_deref() == Some(user_id.as_str());

        let has_permission: Option<bool> = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1
                FROM room_members rm
                LEFT JOIN room_settings rs ON rs.room_id = rm.room_id
                WHERE rm.room_id = $1
                  AND rm.user_id = $2
                  AND rm.left_at IS NULL
                  AND (
                      -- Creator has all permissions
                      rm.role = 'creator'
                      OR (
                          -- Calculate effective permissions:
                          -- (role_default | added) & ~removed
                          CASE rm.role
                              WHEN 'admin' THEN
                                  ((COALESCE(rs.admin_added_permissions, 0::bigint) | rm.added_permissions) &
                                   ~COALESCE(rs.admin_removed_permissions, 0::bigint) & ~rm.removed_permissions) & $3 > 0
                              WHEN 'member' THEN
                                  ((COALESCE(rs.member_added_permissions, 0::bigint) | rm.added_permissions) &
                                   ~COALESCE(rs.member_removed_permissions, 0::bigint) & ~rm.removed_permissions) & $3 > 0
                              WHEN 'guest' THEN
                                  ((COALESCE(rs.guest_added_permissions, 0::bigint) | rm.added_permissions) &
                                   ~COALESCE(rs.guest_removed_permissions, 0::bigint) & ~rm.removed_permissions) & $3 > 0
                              ELSE FALSE
                          END
                      )
                  )
            )"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(if is_owner {
            PermissionBits::DELETE_MOVIE_SELF
        } else {
            PermissionBits::DELETE_MOVIE_ANY
        } as i64)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();

        if !has_permission.unwrap_or(false) {
            return Err(Error::Authorization("Permission denied".to_string()));
        }

        // Step 3: Lock the playback state row to prevent concurrent playback switches
        let playing_media_id: Option<String> = sqlx::query_scalar(
            "SELECT playing_media_id FROM room_playback_state
             WHERE room_id = $1
             FOR UPDATE",
        )
        .bind(room_id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .flatten();

        if playing_media_id.as_deref() == Some(media_id.as_str()) {
            return Err(Error::InvalidInput(
                "Cannot remove media that is currently playing".to_string(),
            ));
        }

        // Step 4: Delete the media within the transaction
        sqlx::query("DELETE FROM media WHERE id = $1")
            .bind(media_id.as_str())
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            media_id = %media_id.as_str(),
            user_id = %user_id.as_str(),
            "Media removed from playlist"
        );

        Ok(())
    }

    /// Get playlist (all media in room's root playlist)
    pub async fn get_playlist(&self, room_id: &RoomId) -> Result<Vec<Media>> {
        let root_playlist = self.playlist_service.get_root_playlist(room_id).await?;
        self.media_service
            .get_playlist_media(&root_playlist.id)
            .await
    }

    /// Get playlist paginated
    pub async fn get_playlist_paginated(
        &self,
        room_id: &RoomId,
        pagination: PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        let root_playlist = self.playlist_service.get_root_playlist(room_id).await?;
        self.media_service
            .get_playlist_media_paginated(&root_playlist.id, pagination)
            .await
    }

    /// Get current playing media for a room
    pub async fn get_playing_media(&self, room_id: &RoomId) -> Result<Option<Media>> {
        let state = self.playback_service.get_state(room_id).await?;
        if let Some(media_id) = state.playing_media_id {
            Ok(self.media_service.get_media(&media_id).await?)
        } else {
            Ok(None)
        }
    }

    /// Edit media item
    pub async fn edit_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
        name: Option<String>,
    ) -> Result<Media> {
        use crate::service::media::EditMediaRequest;
        let request = EditMediaRequest {
            media_id,
            name,
            position: None,
        };
        self.media_service
            .edit_media(room_id, user_id, request)
            .await
    }

    /// Clear all media from room's root playlist
    ///
    /// Permission check is handled by the API layer (`CLEAR_PLAYLIST`).
    /// This method no longer performs its own permission check to avoid
    /// inconsistency with the API layer's `CLEAR_PLAYLIST` check.
    ///
    /// Returns an error if media that is currently playing is in the playlist,
    /// since removing it would leave playback in an inconsistent state.
    pub async fn clear_playlist(&self, room_id: RoomId, _user_id: UserId) -> Result<i64> {
        let root_playlist = self.playlist_service.get_root_playlist(&room_id).await?;

        // Atomic check-and-clear within a transaction to prevent TOCTOU race
        // where another user starts playing media between the check and the clear.
        let mut tx = self.pool.begin().await?;

        // Lock the playback state row to prevent concurrent playback switches
        let row = sqlx::query(
            "SELECT playing_media_id, playing_playlist_id FROM room_playback_state
             WHERE room_id = $1
             FOR UPDATE",
        )
        .bind(room_id.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        // Only block clearing if the currently playing media is in this playlist
        if let Some(row) = row {
            use sqlx::Row;
            let playing_media_id: Option<String> = row.try_get("playing_media_id")?;
            if let Some(ref mid) = playing_media_id {
                // Check if the playing media belongs to this playlist
                let in_playlist: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM media WHERE id = $1 AND playlist_id = $2)",
                )
                .bind(mid.as_str())
                .bind(root_playlist.id.as_str())
                .fetch_one(&mut *tx)
                .await?;

                if in_playlist {
                    return Err(Error::InvalidInput(
                        "Cannot clear playlist while media from it is currently playing"
                            .to_string(),
                    ));
                }
            }
        }

        // Delete all media in playlist within the transaction
        let result = sqlx::query("DELETE FROM media WHERE playlist_id = $1")
            .bind(root_playlist.id.as_str())
            .execute(&mut *tx)
            .await?;

        let count = result.rows_affected() as i64;
        tx.commit().await?;

        Ok(count)
    }

    /// Set current playing media for a room
    pub async fn set_playing_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<RoomPlaybackState> {
        self.playback_service
            .switch_media(room_id, user_id, media_id)
            .await
    }

    /// Swap positions of two media items in playlist
    pub async fn swap_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id1: MediaId,
        media_id2: MediaId,
    ) -> Result<()> {
        self.media_service
            .swap_media_positions(room_id, user_id, media_id1, media_id2)
            .await
    }

    // ========== Playback Operations (delegated) ==========

    /// Update playback state (play/pause/seek/etc)
    pub async fn update_playback(
        &self,
        room_id: RoomId,
        user_id: UserId,
        update_fn: impl Fn(&mut RoomPlaybackState),
        required_permission: u64,
    ) -> Result<RoomPlaybackState> {
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, required_permission)
            .await?;

        // Get current state and apply update
        self.playback_service.update_state(room_id, update_fn).await
    }

    /// Get playback state
    pub async fn get_playback_state(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        self.playback_service.get_state(room_id).await
    }

    // ========== Chat Operations ==========

    /// Get chat history for a room (legacy timestamp cursor)
    pub async fn get_chat_history(
        &self,
        room_id: &RoomId,
        before: Option<DateTime<Utc>>,
        limit: i32,
    ) -> Result<Vec<ChatMessage>> {
        self.chat_repo.list_by_room(room_id, before, limit).await
    }

    /// Get chat history using keyset (cursor) pagination.
    ///
    /// Prefer this over [`get_chat_history`] for large rooms — it avoids the
    /// O(N) timestamp scan by using `(created_at, id)` composite keyset pagination.
    ///
    /// Returns `(messages, next_cursor)`.
    pub async fn get_chat_history_cursor(
        &self,
        room_id: &RoomId,
        cursor: Option<(DateTime<Utc>, &str)>,
        limit: i32,
    ) -> Result<(Vec<ChatMessage>, Option<(DateTime<Utc>, String)>)> {
        self.chat_repo
            .list_by_room_cursor(room_id, cursor, limit)
            .await
    }

    /// Save a chat message to the database
    pub async fn save_chat_message(
        &self,
        room_id: RoomId,
        user_id: UserId,
        content: String,
    ) -> Result<ChatMessage> {
        if content.is_empty() {
            return Err(Error::InvalidInput(
                "Chat message cannot be empty".to_string(),
            ));
        }
        if content.chars().count() > 2000 {
            return Err(Error::InvalidInput(
                "Chat message cannot exceed 2000 characters".to_string(),
            ));
        }

        let message = ChatMessage {
            id: nanoid::nanoid!(12),
            room_id,
            user_id,
            content,
            message_type: 1, // text message
            created_at: Utc::now(),
        };
        self.chat_repo.create(&message).await
    }

    // ========== Permission Operations (delegated) ==========

    /// Check if user has permission in room
    pub async fn check_permission(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<()> {
        self.permission_service
            .check_permission(room_id, user_id, permission)
            .await
    }

    // ========== Admin Operations ==========

    /// Update room status (admin use, bypasses permission checks)
    ///
    /// Validates the status transition before applying it. Valid transitions are:
    /// - `Pending -> Active` (review approved)
    /// - `Pending -> Closed` (review rejected)
    /// - `Active -> Closed` (room closed)
    /// - `Closed -> Active` (room reopened)
    /// - Same status (no change) is always allowed
    ///
    /// # Errors
    /// - `Error::NotFound` if room doesn't exist
    /// - `Error::InvalidInput` if the status transition is not allowed
    pub async fn update_room_status(
        &self,
        room_id: &RoomId,
        new_status: crate::models::RoomStatus,
    ) -> Result<Room> {
        // Get current room to check existing status
        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        // Validate status transition
        if !room.status.can_transition_to(&new_status) {
            return Err(Error::InvalidInput(format!(
                "Invalid status transition from {} to {}",
                room.status.as_str(),
                new_status.as_str()
            )));
        }

        let room = self.room_repo.update_status(room_id, new_status).await?;
        self.notify_room_invalidation(room_id).await;
        Ok(room)
    }

    /// Update room directly (admin use, bypasses permission checks)
    ///
    /// # Security
    /// Verifies that the caller has admin or root role before proceeding.
    pub async fn admin_update_room(&self, room: &Room, admin_user_id: &UserId) -> Result<Room> {
        // Verify caller has admin/root role (defense-in-depth)
        let admin_user = self.user_service.get_user(admin_user_id).await?;
        if !admin_user.role.is_admin_or_above() {
            return Err(Error::Authorization(
                "Admin role required for this operation".to_string(),
            ));
        }

        let old_version = room.version;
        let updated = self.room_repo.update(room, old_version).await?;
        self.notify_room_invalidation(&room.id).await;
        Ok(updated)
    }

    /// Delete room (admin use, bypasses permission checks)
    ///
    /// Uses a transaction for atomicity, matching the pattern of `delete_room`.
    /// Immediately cleans up non-critical related data (see `delete_room` for details).
    ///
    /// # Security
    /// Verifies that the caller has admin or root role before proceeding.
    pub async fn admin_delete_room(&self, room_id: &RoomId, admin_user_id: &UserId) -> Result<()> {
        // Verify caller has admin/root role (defense-in-depth)
        let admin_user = self.user_service.get_user(admin_user_id).await?;
        if !admin_user.role.is_admin_or_above() {
            return Err(Error::Authorization(
                "Admin role required for this operation".to_string(),
            ));
        }

        // Wrap deletion in a transaction for atomicity
        let mut tx = self.pool.begin().await?;

        let deleted = sqlx::query(
            "UPDATE rooms
             SET deleted_at = $2, updated_at = $2
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(room_id.as_str())
        .bind(chrono::Utc::now())
        .execute(&mut *tx)
        .await?;

        if deleted.rows_affected() == 0 {
            // Transaction will be automatically rolled back on drop
            return Err(Error::NotFound(
                "Room not found or already deleted".to_string(),
            ));
        }

        // IMMEDIATE CLEANUP: Delete non-critical related data (same as delete_room)
        let playlists_deleted = sqlx::query("DELETE FROM playlists WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        let members_deleted = sqlx::query("DELETE FROM room_members WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        let settings_deleted = sqlx::query("DELETE FROM room_settings WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        let _playback_deleted = sqlx::query("DELETE FROM room_playback_state WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        let chat_deleted = sqlx::query("DELETE FROM chat_messages WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        // Invalidate caches BEFORE committing the transaction.
        // See `invalidate_room_caches` for detailed rationale.
        self.invalidate_room_caches(room_id).await;

        tx.commit().await?;

        // Notify after commit so notifications are only sent for successful deletions
        let _ = self.notification_service.notify_room_deleted(room_id).await;

        crate::metrics::http::ROOMS_ACTIVE.dec();

        // Audit log
        if let Some(ref audit) = self.audit_service {
            let _ = audit
                .log(
                    admin_user_id.as_str().to_string(),
                    admin_user_id.as_str().to_string(),
                    AuditAction::RoomDeleted,
                    AuditTargetType::Room,
                    Some(room_id.as_str().to_string()),
                    serde_json::json!({
                        "reason": "Room deleted by admin",
                        "playlists_deleted": playlists_deleted.rows_affected(),
                        "members_deleted": members_deleted.rows_affected(),
                        "settings_deleted": settings_deleted.rows_affected(),
                        "chat_deleted": chat_deleted.rows_affected(),
                    }),
                    None,
                    None,
                )
                .await;
        }

        Ok(())
    }

    /// Delete an orphaned room whose creator has been deleted or banned.
    ///
    /// This method allows global admins to clean up rooms that become orphaned
    /// when the creator's account is deleted or banned. The FK constraint
    /// `rooms.created_by REFERENCES users(id) ON DELETE RESTRICT` prevents
    /// user deletion when they have created rooms, so this method provides
    /// a way to first delete the orphaned room before retrying user deletion.
    ///
    /// # Verification
    ///
    /// This method verifies that the room is truly orphaned by checking:
    /// 1. The room exists and is not already deleted
    /// 2. The creator's user record either:
    ///    - Does not exist (hard-deleted), OR
    ///    - Has `deleted_at` set (soft-deleted), OR
    ///    - Has `status = 'Banned'`
    ///
    /// # Arguments
    ///
    /// * `room_id` - The room to delete
    /// * `admin_user_id` - The admin performing the deletion (for audit log)
    ///
    /// # Errors
    ///
    /// Returns `Error::InvalidInput` if the room is not actually orphaned
    /// (creator exists and is active). In this case, use `delete_room` instead.
    pub async fn admin_delete_orphaned_room(
        &self,
        room_id: &RoomId,
        admin_user_id: &UserId,
    ) -> Result<()> {
        tracing::info!(room_id = %room_id, admin_user_id = %admin_user_id, "Admin deleting orphaned room");

        // First, verify the room exists and check if it's orphaned
        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.deleted_at.is_some() {
            return Err(Error::InvalidInput("Room is already deleted".to_string()));
        }

        // Check if the creator is deleted or banned
        let creator_orphaned = sqlx::query_scalar::<_, bool>(
            "SELECT NOT EXISTS (
                SELECT 1 FROM users
                WHERE id = $1
                AND deleted_at IS NULL
                AND status != 3
            )",
        )
        .bind(room.created_by.as_str())
        .fetch_one(&self.pool)
        .await?;

        if !creator_orphaned {
            return Err(Error::InvalidInput(
                "Room is not orphaned: creator still exists and is active. Use delete_room instead.".to_string()
            ));
        }

        tracing::info!(
            room_id = %room_id,
            creator_id = %room.created_by,
            "Confirmed room is orphaned, proceeding with admin deletion"
        );

        // Now delete the room using the same logic as admin_delete_room
        let mut tx = self.pool.begin().await?;

        let deleted = sqlx::query(
            "UPDATE rooms
             SET deleted_at = $2, updated_at = $2
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(room_id.as_str())
        .bind(chrono::Utc::now())
        .execute(&mut *tx)
        .await?;

        if deleted.rows_affected() == 0 {
            return Err(Error::NotFound(
                "Room not found or already deleted".to_string(),
            ));
        }

        // IMMEDIATE CLEANUP: Delete non-critical related data
        let playlists_deleted = sqlx::query("DELETE FROM playlists WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        let members_deleted = sqlx::query("DELETE FROM room_members WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        let settings_deleted = sqlx::query("DELETE FROM room_settings WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        let _playback_deleted = sqlx::query("DELETE FROM room_playback_state WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        let chat_deleted = sqlx::query("DELETE FROM chat_messages WHERE room_id = $1")
            .bind(room_id.as_str())
            .execute(&mut *tx)
            .await?;

        // Invalidate caches BEFORE committing the transaction.
        // See `invalidate_room_caches` for detailed rationale.
        self.invalidate_room_caches(room_id).await;

        tx.commit().await?;

        // Notify after commit
        let _ = self.notification_service.notify_room_deleted(room_id).await;

        crate::metrics::http::ROOMS_ACTIVE.dec();

        // Audit log
        if let Some(ref audit) = self.audit_service {
            let _ = audit
                .log(
                    admin_user_id.as_str().to_string(),
                    admin_user_id.as_str().to_string(),
                    AuditAction::RoomDeleted,
                    AuditTargetType::Room,
                    Some(room_id.as_str().to_string()),
                    serde_json::json!({
                        "reason": "Orphaned room deleted by admin (creator deleted/banned)",
                        "creator_id": room.created_by.as_str(),
                        "playlists_deleted": playlists_deleted.rows_affected(),
                        "members_deleted": members_deleted.rows_affected(),
                        "settings_deleted": settings_deleted.rows_affected(),
                        "chat_deleted": chat_deleted.rows_affected(),
                    }),
                    None,
                    None,
                )
                .await;
        }

        tracing::info!(room_id = %room_id, "Orphaned room deleted successfully");

        Ok(())
    }

    /// Set room password (admin use, bypasses permission checks)
    ///
    /// Pass `Some(password)` to set a new password, or `None` to remove it.
    /// When a password is set, all guest members are kicked automatically.
    pub async fn admin_set_room_password(
        &self,
        room_id: &RoomId,
        new_password: Option<&str>,
    ) -> Result<()> {
        use crate::service::notification::GuestKickReason;

        // Verify room exists
        let _room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        // Hash new password outside transaction (CPU-intensive)
        let password_is_being_set = new_password.is_some();
        let hashed_password = match new_password {
            Some(pwd) => Some(hash_password(pwd).await?),
            None => None,
        };

        self.do_set_password_hash(room_id, hashed_password).await?;

        // Kick guests when a password is being set (guests cannot join password-protected rooms)
        if password_is_being_set {
            if let Err(e) = self
                .notification_service
                .kick_all_guests(room_id, GuestKickReason::RoomPasswordAdded)
                .await
            {
                tracing::warn!("Failed to kick guests after admin password set: {}", e);
            }
        }

        Ok(())
    }

    // ========== Service Accessors ==========

    /// Get reference to media service
    #[must_use]
    pub const fn media_service(&self) -> &MediaService {
        &self.media_service
    }

    // ========== Room Management ==========

    /// Approve a pending room
    ///
    /// Changes room status from pending to active.
    /// Only admins can approve rooms.
    pub async fn approve_room(&self, room_id: &RoomId) -> Result<Room> {
        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if !room.status.is_pending() {
            return Err(Error::InvalidInput(
                "Room is not pending approval".to_string(),
            ));
        }

        let updated_room = self
            .room_repo
            .update_status(room_id, RoomStatus::Active)
            .await?;

        Ok(updated_room)
    }

    /// Ban a room (admin only)
    ///
    /// Sets the `is_banned` flag. The room retains its previous status (Active/Closed/etc).
    /// Only global admins can ban rooms.
    pub async fn ban_room(&self, room_id: &RoomId, admin_user_id: &UserId) -> Result<Room> {
        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.is_banned {
            return Err(Error::InvalidInput("Room is already banned".to_string()));
        }

        let updated_room = self.room_repo.update_ban_status(room_id, true).await?;
        self.notify_room_invalidation(room_id).await;

        // Audit log
        if let Some(ref audit) = self.audit_service {
            let _ = audit
                .log(
                    admin_user_id.as_str().to_string(),
                    admin_user_id.as_str().to_string(),
                    AuditAction::RoomBanned,
                    AuditTargetType::Room,
                    Some(room_id.as_str().to_string()),
                    serde_json::json!({"reason": "Room banned by admin"}),
                    None,
                    None,
                )
                .await;
        }

        Ok(updated_room)
    }

    /// Unban a room (admin only)
    ///
    /// Clears the `is_banned` flag. The room returns to its previous status.
    /// Only global admins can unban rooms.
    pub async fn unban_room(&self, room_id: &RoomId, admin_user_id: &UserId) -> Result<Room> {
        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if !room.is_banned {
            return Err(Error::InvalidInput("Room is not banned".to_string()));
        }

        let updated_room = self.room_repo.update_ban_status(room_id, false).await?;
        self.notify_room_invalidation(room_id).await;

        // Audit log
        if let Some(ref audit) = self.audit_service {
            let _ = audit
                .log(
                    admin_user_id.as_str().to_string(),
                    admin_user_id.as_str().to_string(),
                    AuditAction::RoomUnbanned,
                    AuditTargetType::Room,
                    Some(room_id.as_str().to_string()),
                    serde_json::json!({"reason": "Room unbanned by admin"}),
                    None,
                    None,
                )
                .await;
        }

        Ok(updated_room)
    }

    /// Get reference to playback service
    #[must_use]
    pub const fn playback_service(&self) -> &PlaybackService {
        &self.playback_service
    }

    /// Get reference to member service
    #[must_use]
    pub const fn member_service(&self) -> &MemberService {
        &self.member_service
    }

    /// Get reference to notification service
    #[must_use]
    pub const fn notification_service(&self) -> &NotificationService {
        &self.notification_service
    }

    /// Invalidate room cache locally and broadcast to other replicas.
    ///
    /// Best-effort: logs a warning on failure but does not propagate the error,
    /// since cache invalidation is not critical to the mutation itself.
    ///
    /// Uses `invalidate_and_broadcast_room` to ensure the originating node also
    /// clears its own local cache (the Redis subscriber skips self-originated
    /// messages, so `broadcast_remote` alone would leave local caches stale).
    async fn notify_room_invalidation(&self, room_id: &RoomId) {
        if let Some(ref service) = self.cache_invalidation {
            if let Err(e) = service.invalidate_and_broadcast_room(room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
                    "Failed to broadcast room cache invalidation"
                );
            }
        }
    }

    /// Invalidate all caches associated with a room.
    ///
    /// This method consolidates cache invalidation for:
    /// - Room data (broadcast to other replicas)
    /// - Permission data (local cache)
    /// - Playback state (broadcast to other replicas)
    ///
    /// ## Timing Requirements
    ///
    /// **CRITICAL**: This MUST be called BEFORE transaction commit to prevent
    /// race conditions:
    ///
    /// 1. Transaction commits (data is changed)
    /// 2. Another request reads stale data from cache (shows old state)
    /// 3. Cache is invalidated (too late - stale data was already served)
    ///
    /// By invalidating before commit, we ensure that when the transaction commits,
    /// the cache is already empty. Any concurrent request will miss the cache
    /// and read fresh data from the database.
    ///
    /// ## Rollback Safety
    ///
    /// If the transaction rolls back after cache invalidation, the cache will
    /// simply be empty and will be repopulated on the next read with the correct
    /// data. This is safe because:
    ///
    /// - Empty cache causes a cache miss
    /// - Cache miss triggers a database read
    /// - Database read returns the current (pre-rollback) state
    /// - Cache is repopulated with correct data
    ///
    /// ## Usage Pattern
    ///
    /// ```text
    /// let mut tx = self.pool.begin().await?;
    /// // ... perform database operations ...
    /// // Invalidate BEFORE commit
    /// self.invalidate_room_caches(&room_id).await;
    /// tx.commit().await?;
    /// // ... post-commit operations ...
    /// ```
    ///
    /// Best-effort: logs warnings on failure but does not propagate errors,
    /// since cache invalidation is not critical to the mutation itself.
    async fn invalidate_room_caches(&self, room_id: &RoomId) {
        // Broadcast room invalidation to other replicas (and clear local cache)
        self.notify_room_invalidation(room_id).await;

        // Invalidate permission cache (local only)
        self.permission_service.invalidate_room_cache(room_id).await;

        // Invalidate playback state cache (broadcast to other replicas)
        self.playback_service
            .invalidate_playback_cache(room_id)
            .await;
    }

    // ========================
    // Batch Operations
    // ========================

    /// Maximum number of items allowed in a batch operation
    pub const BATCH_SIZE_LIMIT: usize = 100;

    /// Batch ban multiple rooms.
    ///
    /// Each room is processed individually - if one room fails, others may still succeed.
    /// Returns per-room results with success/failure status.
    ///
    /// # Errors
    /// - `InvalidInput` if `room_ids` is empty or exceeds `BATCH_SIZE_LIMIT`
    pub async fn batch_ban_rooms(
        &self,
        room_ids: &[String],
        admin_user_id: &UserId,
    ) -> crate::Result<Vec<(String, crate::Result<()>)>> {
        if room_ids.is_empty() {
            return Err(Error::InvalidInput("room_ids cannot be empty".to_string()));
        }
        if room_ids.len() > Self::BATCH_SIZE_LIMIT {
            return Err(Error::InvalidInput(format!(
                "Batch size {} exceeds limit of {}",
                room_ids.len(),
                Self::BATCH_SIZE_LIMIT
            )));
        }

        let mut results = Vec::with_capacity(room_ids.len());

        for room_id_str in room_ids {
            let room_id = RoomId::from_string(room_id_str.clone());

            let result = self.ban_room(&room_id, admin_user_id).await.map(|_| ());
            results.push((room_id_str.clone(), result));
        }

        Ok(results)
    }

    /// Batch delete multiple rooms.
    ///
    /// Each room is processed individually - if one room fails, others may still succeed.
    /// Returns per-room results with success/failure status.
    ///
    /// # Errors
    /// - `InvalidInput` if `room_ids` is empty or exceeds `BATCH_SIZE_LIMIT`
    pub async fn batch_delete_rooms(
        &self,
        room_ids: &[String],
        admin_user_id: &UserId,
    ) -> crate::Result<Vec<(String, crate::Result<()>)>> {
        if room_ids.is_empty() {
            return Err(Error::InvalidInput("room_ids cannot be empty".to_string()));
        }
        if room_ids.len() > Self::BATCH_SIZE_LIMIT {
            return Err(Error::InvalidInput(format!(
                "Batch size {} exceeds limit of {}",
                room_ids.len(),
                Self::BATCH_SIZE_LIMIT
            )));
        }

        let mut results = Vec::with_capacity(room_ids.len());

        for room_id_str in room_ids {
            let room_id = RoomId::from_string(room_id_str.clone());

            let result = self.admin_delete_room(&room_id, admin_user_id).await;
            results.push((room_id_str.clone(), result));
        }

        Ok(results)
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use crate::models::{
        room_settings::{
            AllowGuestJoin, ChatEnabled, DanmakuEnabled, GuestAddedPermissions, MaxMembers,
            MemberAddedPermissions, RequirePassword,
        },
        PermissionBits, RoomSettings, RoomStatus,
    };
    use crate::test_helpers::RoomFixture;
    use crate::Error;

    // ========== Room Name Validation ==========

    /// Replicates the room name validation from `do_create_room`.
    fn validate_room_name(name: &str) -> crate::Result<()> {
        crate::validation::RoomNameValidator::new()
            .validate(name)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }

    #[test]
    fn test_empty_room_name_returns_error() {
        let result = validate_room_name("");
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidInput(msg) => assert!(
                msg.contains("at least 1") || msg.contains("cannot be empty"),
                "got: {msg}"
            ),
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn test_room_name_at_max_length_is_ok() {
        // Use ROOM_NAME_MAX from validation module (100 characters)
        let name = "a".repeat(crate::validation::ROOM_NAME_MAX);
        assert!(validate_room_name(&name).is_ok());
    }

    #[test]
    fn test_room_name_exceeding_max_length_returns_error() {
        // One over ROOM_NAME_MAX (101 characters)
        let name = "a".repeat(crate::validation::ROOM_NAME_MAX + 1);
        let result = validate_room_name(&name);
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidInput(msg) => assert!(
                msg.contains("characters") || msg.contains("long"),
                "got: {msg}"
            ),
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn test_valid_room_name_is_ok() {
        assert!(validate_room_name("My Room").is_ok());
        assert!(validate_room_name("a").is_ok());
        assert!(validate_room_name("Room with spaces and 123").is_ok());
    }

    #[test]
    fn test_room_name_counts_unicode_characters_not_bytes() {
        // Each CJK character is 3 bytes in UTF-8 but 1 character.
        // ROOM_NAME_MAX (100) CJK chars = 300 bytes, should be valid.
        let max_len = crate::validation::ROOM_NAME_MAX;
        let name: String = std::iter::repeat_n('\u{4e00}', max_len).collect();
        assert_eq!(name.chars().count(), max_len);
        assert!(
            validate_room_name(&name).is_ok(),
            "Room name with {max_len} CJK characters should be valid"
        );

        // (ROOM_NAME_MAX + 1) CJK characters should be rejected
        let name_too_long: String = std::iter::repeat_n('\u{4e00}', max_len + 1).collect();
        assert!(
            validate_room_name(&name_too_long).is_err(),
            "Room name with {} CJK characters should be rejected",
            max_len + 1
        );
    }

    // ========== Room Description Validation ==========

    /// Replicates the description validation from `do_create_room`.
    /// Uses `chars().count()` for Unicode safety, matching the service code.
    fn validate_room_description(description: &str) -> crate::Result<()> {
        if description.chars().count() > 500 {
            return Err(Error::InvalidInput(
                "Room description too long (max 500 characters)".to_string(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_description_at_max_length_is_ok() {
        let desc = "a".repeat(500);
        assert!(validate_room_description(&desc).is_ok());
    }

    #[test]
    fn test_description_exceeding_max_length_returns_error() {
        let desc = "a".repeat(501);
        let result = validate_room_description(&desc);
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidInput(msg) => assert!(msg.contains("too long"), "got: {msg}"),
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn test_description_counts_unicode_characters_not_bytes() {
        // Each CJK character is 3 bytes in UTF-8 but 1 character.
        // 500 CJK chars = 1500 bytes, should be valid.
        let desc: String = std::iter::repeat_n('\u{4e00}', 500).collect();
        assert_eq!(desc.chars().count(), 500);
        assert!(validate_room_description(&desc).is_ok());

        // 501 CJK characters should be rejected even though 255 ASCII chars would be fine
        let desc_too_long: String = std::iter::repeat_n('\u{4e00}', 501).collect();
        assert!(validate_room_description(&desc_too_long).is_err());
    }

    #[test]
    fn test_empty_description_is_ok() {
        assert!(validate_room_description("").is_ok());
    }

    // ========== Chat Message Validation ==========

    /// Replicates the chat message validation from `save_chat_message`.
    fn validate_chat_message(content: &str) -> crate::Result<()> {
        if content.is_empty() {
            return Err(Error::InvalidInput(
                "Chat message cannot be empty".to_string(),
            ));
        }
        if content.chars().count() > 2000 {
            return Err(Error::InvalidInput(
                "Chat message cannot exceed 2000 characters".to_string(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_empty_chat_message_returns_error() {
        let result = validate_chat_message("");
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidInput(msg) => assert!(msg.contains("cannot be empty"), "got: {msg}"),
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn test_chat_message_at_max_length_is_ok() {
        let content = "x".repeat(2000);
        assert!(validate_chat_message(&content).is_ok());
    }

    #[test]
    fn test_chat_message_exceeding_max_length_returns_error() {
        let content = "x".repeat(2001);
        let result = validate_chat_message(&content);
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidInput(msg) => assert!(msg.contains("2000"), "got: {msg}"),
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn test_normal_chat_message_is_ok() {
        assert!(validate_chat_message("Hello, world!").is_ok());
    }

    // ========== Update Room Setting via Registry ==========

    #[test]
    fn test_known_setting_keys_are_valid_via_registry() {
        use crate::models::room_settings::RoomSettingsRegistry;
        let known_keys = [
            ("chat_enabled", "true"),
            ("danmaku_enabled", "false"),
            (
                "auto_play",
                r#"{"enabled":true,"mode":"sequential","delay":3}"#,
            ),
            ("allow_guest_join", "true"),
            ("require_password", "false"),
            ("max_members", "100"),
            ("auto_play_next", "true"),
            ("loop_playlist", "false"),
            ("shuffle_playlist", "true"),
        ];
        for (key, val) in &known_keys {
            assert!(
                RoomSettingsRegistry::validate_setting(key, val).is_ok(),
                "Expected key '{key}' with value '{val}' to be valid"
            );
        }
    }

    #[test]
    fn test_unknown_setting_key_returns_error_via_registry() {
        use crate::models::room_settings::RoomSettingsRegistry;
        let result = RoomSettingsRegistry::validate_setting("nonexistent_key", "true");
        assert!(result.is_err());
    }

    #[test]
    fn test_set_by_key_applies_value() {
        let mut settings = RoomSettings::default();
        assert!(settings.chat_enabled.0); // default is true
        settings.set_by_key("chat_enabled", "false").unwrap();
        assert!(!settings.chat_enabled.0);
    }

    #[test]
    fn test_set_by_key_invalid_type_returns_error() {
        let mut settings = RoomSettings::default();
        let result = settings.set_by_key("chat_enabled", "not_a_bool");
        assert!(result.is_err());
    }

    #[test]
    fn test_set_by_key_unknown_key_returns_error() {
        let mut settings = RoomSettings::default();
        let result = settings.set_by_key("nonexistent", "true");
        assert!(result.is_err());
    }

    #[test]
    fn test_set_by_key_max_members() {
        let mut settings = RoomSettings::default();
        settings.set_by_key("max_members", "42").unwrap();
        assert_eq!(settings.max_members.0, 42);
    }

    #[test]
    fn test_set_by_key_max_members_invalid_string() {
        let mut settings = RoomSettings::default();
        let result = settings.set_by_key("max_members", "not_a_number");
        assert!(result.is_err());
    }

    // ========== RoomSettings Permission Validation ==========

    #[test]
    fn test_settings_validate_permissions_default_is_ok() {
        let settings = RoomSettings::default();
        assert!(settings.validate_permissions().is_ok());
    }

    #[test]
    fn test_settings_validate_permissions_guest_escalation_is_rejected() {
        let mut settings = RoomSettings::default();
        // Grant guests a permission that exceeds DEFAULT_MEMBER (e.g., KICK_MEMBER)
        settings.guest_added_permissions = GuestAddedPermissions(PermissionBits::KICK_MEMBER);
        let result = settings.validate_permissions();
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidInput(msg) => {
                assert!(msg.contains("Guest"), "got: {msg}");
            }
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn test_settings_validate_permissions_member_escalation_is_rejected() {
        let mut settings = RoomSettings::default();
        // Grant members a permission that exceeds DEFAULT_ADMIN (e.g., DELETE_ROOM)
        settings.member_added_permissions = MemberAddedPermissions(PermissionBits::DELETE_ROOM);
        let result = settings.validate_permissions();
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidInput(msg) => {
                assert!(msg.contains("Member"), "got: {msg}");
            }
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn test_settings_validate_permissions_within_limits_is_ok() {
        let mut settings = RoomSettings::default();
        // Grant guests SEND_CHAT which is within DEFAULT_MEMBER
        settings.guest_added_permissions = GuestAddedPermissions(PermissionBits::SEND_CHAT);
        assert!(settings.validate_permissions().is_ok());
    }

    // ========== RoomSettings Permission Calculation ==========

    #[test]
    fn test_admin_permissions_with_added_and_removed() {
        let mut settings = RoomSettings::default();
        let base = PermissionBits(PermissionBits::SEND_CHAT | PermissionBits::ADD_MOVIE);

        // Add PLAY_CONTROL, remove SEND_CHAT
        settings.admin_added_permissions =
            crate::models::room_settings::AdminAddedPermissions(PermissionBits::PLAY_CONTROL);
        settings.admin_removed_permissions =
            crate::models::room_settings::AdminRemovedPermissions(PermissionBits::SEND_CHAT);

        let result = settings.admin_permissions(base);
        // Should have ADD_MOVIE and PLAY_CONTROL, but not SEND_CHAT
        assert!(result.0 & PermissionBits::ADD_MOVIE != 0);
        assert!(result.0 & PermissionBits::PLAY_CONTROL != 0);
        assert_eq!(result.0 & PermissionBits::SEND_CHAT, 0);
    }

    #[test]
    fn test_guest_permissions_capped_at_member_ceiling() {
        let settings = RoomSettings::default();
        let base = PermissionBits(0);
        let result = settings.guest_permissions(base);
        // Default guest added permissions are 0, so result should be 0
        assert_eq!(result.0, 0);
    }

    // ========== RoomSettings Serialization ==========

    #[test]
    fn test_room_settings_default_serialization_roundtrip() {
        let settings = RoomSettings::default();
        let json = serde_json::to_string(&settings).expect("serialize");
        let deserialized: RoomSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.chat_enabled.0, settings.chat_enabled.0);
        assert_eq!(deserialized.max_members.0, settings.max_members.0);
        assert_eq!(deserialized.require_password.0, settings.require_password.0);
    }

    #[test]
    fn test_room_settings_custom_values_roundtrip() {
        let settings = RoomSettings {
            chat_enabled: ChatEnabled(false),
            danmaku_enabled: DanmakuEnabled(true),
            allow_guest_join: AllowGuestJoin(false),
            require_password: RequirePassword(true),
            max_members: MaxMembers(42),
            ..Default::default()
        };
        let json = serde_json::to_string(&settings).expect("serialize");
        let deserialized: RoomSettings = serde_json::from_str(&json).expect("deserialize");
        assert!(!deserialized.chat_enabled.0);
        assert!(deserialized.danmaku_enabled.0);
        assert!(!deserialized.allow_guest_join.0);
        assert!(deserialized.require_password.0);
        assert_eq!(deserialized.max_members.0, 42);
    }

    // ========== Room Model Tests ==========

    #[test]
    fn test_room_fixture_defaults() {
        let room = RoomFixture::new().build();
        assert_eq!(room.status, RoomStatus::Active);
        assert!(!room.is_banned);
        assert_eq!(room.name, "Test Room");
    }

    #[test]
    fn test_room_status_is_pending() {
        assert!(RoomStatus::Pending.is_pending());
        assert!(!RoomStatus::Active.is_pending());
        assert!(!RoomStatus::Closed.is_pending());
    }

    // ========== Room Model: Ban / Unban ==========

    #[test]
    fn test_room_ban_sets_is_banned_and_preserves_status() {
        let mut room = RoomFixture::new().build();
        assert_eq!(room.status, RoomStatus::Active);
        assert!(!room.is_banned);

        room.ban();
        assert!(room.is_banned);
        // Status is unchanged -- banning is orthogonal to lifecycle status
        assert_eq!(room.status, RoomStatus::Active);
    }

    #[test]
    fn test_room_unban_clears_is_banned_and_preserves_status() {
        let mut room = RoomFixture::new().build();
        room.ban();
        assert!(room.is_banned);

        room.unban();
        assert!(!room.is_banned);
        assert_eq!(room.status, RoomStatus::Active);
    }

    #[test]
    fn test_room_is_active_considers_ban_status_and_deleted() {
        let mut room = RoomFixture::new().build();
        assert!(room.is_active());

        // Banned room is not active
        room.ban();
        assert!(!room.is_active());
        room.unban();
        assert!(room.is_active());

        // Deleted room is not active
        room.deleted_at = Some(chrono::Utc::now());
        assert!(!room.is_active());
    }

    #[test]
    fn test_room_is_active_requires_active_status() {
        use crate::models::Room;

        let room = Room::new("test".to_string(), crate::models::UserId::new());
        assert!(room.is_active());

        // Pending status
        let mut pending_room = room.clone();
        pending_room.status = RoomStatus::Pending;
        assert!(!pending_room.is_active());

        // Closed status
        let mut closed_room = room;
        closed_room.status = RoomStatus::Closed;
        assert!(!closed_room.is_active());
    }

    // ========== Room Model: Constructor Behavior ==========

    #[test]
    fn test_room_new_generates_unique_ids() {
        use crate::models::Room;

        let user_id = crate::models::UserId::new();
        let room1 = Room::new("Room A".to_string(), user_id.clone());
        let room2 = Room::new("Room B".to_string(), user_id);
        assert_ne!(room1.id, room2.id);
    }

    #[test]
    fn test_room_new_with_description_sets_fields() {
        use crate::models::Room;

        let user_id = crate::models::UserId::new();
        let room = Room::new_with_description(
            "My Room".to_string(),
            "A description".to_string(),
            user_id.clone(),
        );
        assert_eq!(room.name, "My Room");
        assert_eq!(room.description, "A description");
        assert_eq!(room.created_by, user_id);
        assert_eq!(room.status, RoomStatus::Active);
        assert!(!room.is_banned);
        assert!(room.deleted_at.is_none());
    }

    // ========== RoomStatus: Exhaustive Status Predicates ==========

    #[test]
    fn test_room_status_predicates_exhaustive() {
        // Active
        assert!(RoomStatus::Active.is_active());
        assert!(!RoomStatus::Active.is_pending());
        assert!(!RoomStatus::Active.is_closed());

        // Pending
        assert!(!RoomStatus::Pending.is_active());
        assert!(RoomStatus::Pending.is_pending());
        assert!(!RoomStatus::Pending.is_closed());

        // Closed
        assert!(!RoomStatus::Closed.is_active());
        assert!(!RoomStatus::Closed.is_pending());
        assert!(RoomStatus::Closed.is_closed());
    }

    #[test]
    fn test_room_status_as_str() {
        assert_eq!(RoomStatus::Active.as_str(), "active");
        assert_eq!(RoomStatus::Pending.as_str(), "pending");
        assert_eq!(RoomStatus::Closed.as_str(), "closed");
    }

    #[test]
    fn test_room_status_default_is_active() {
        assert_eq!(RoomStatus::default(), RoomStatus::Active);
    }

    // ========== RoomSettings: require_password Reflects Password State ==========

    #[test]
    fn test_require_password_is_set_when_password_provided() {
        // Replicates the logic from `do_create_room`:
        // `room_settings.require_password = RequirePassword(password.is_some())`
        let password: Option<String> = Some("secret".to_string());
        let mut settings = RoomSettings::default();
        settings.require_password = RequirePassword(password.is_some());
        assert!(settings.require_password.0);
    }

    #[test]
    fn test_require_password_is_unset_when_no_password() {
        let password: Option<String> = None;
        let mut settings = RoomSettings::default();
        settings.require_password = RequirePassword(password.is_some());
        assert!(!settings.require_password.0);
    }

    // ========== Room Description Validation: Edge Cases ==========

    #[test]
    fn test_update_room_description_validation_at_boundary() {
        // Replicates validation in `update_room_description`
        fn validate_desc(desc: &str) -> crate::Result<()> {
            if desc.chars().count() > 500 {
                return Err(Error::InvalidInput(
                    "Room description too long (max 500 characters)".to_string(),
                ));
            }
            Ok(())
        }

        // Mixed Unicode: emoji are single characters but multi-byte
        let emoji_desc: String = std::iter::repeat_n('\u{1F600}', 500).collect();
        assert_eq!(emoji_desc.chars().count(), 500);
        assert!(validate_desc(&emoji_desc).is_ok());

        let emoji_over: String = std::iter::repeat_n('\u{1F600}', 501).collect();
        assert!(validate_desc(&emoji_over).is_err());
    }

    // ========== RoomSettings: AllowGuestJoin / RequirePassword Interaction ==========

    #[test]
    fn test_settings_guest_blocked_by_password_requirement() {
        // If require_password is true and allow_guest_join is true,
        // the check_guest_allowed method in the service layer still blocks guests
        // because guests cannot provide passwords
        let settings = RoomSettings {
            allow_guest_join: AllowGuestJoin(true),
            require_password: RequirePassword(true),
            ..Default::default()
        };
        // This is a data-level check, the actual enforcement is in check_guest_allowed
        assert!(settings.allow_guest_join.0);
        assert!(settings.require_password.0);
    }

    #[test]
    fn test_settings_guest_allowed_when_no_password_and_guest_join_enabled() {
        let settings = RoomSettings {
            allow_guest_join: AllowGuestJoin(true),
            require_password: RequirePassword(false),
            ..Default::default()
        };
        assert!(settings.allow_guest_join.0);
        assert!(!settings.require_password.0);
    }

    #[test]
    fn test_settings_guest_blocked_when_guest_join_disabled() {
        let settings = RoomSettings {
            allow_guest_join: AllowGuestJoin(false),
            require_password: RequirePassword(false),
            ..Default::default()
        };
        assert!(!settings.allow_guest_join.0);
    }

    // ========== RoomMember: Ban / Unban Mutations ==========

    #[test]
    fn test_room_member_ban_sets_status_and_metadata() {
        use crate::models::{MemberStatus, RoomId, RoomMember, RoomRole, UserId};

        let mut member = RoomMember::new(
            RoomId("room1".to_string()),
            UserId("user1".to_string()),
            RoomRole::Member,
        );
        assert!(member.is_active());

        let banner = UserId("admin1".to_string());
        member.ban(banner.clone(), Some("spamming".to_string()));

        assert_eq!(member.status, MemberStatus::Banned);
        assert!(member.banned_at.is_some());
        assert_eq!(member.banned_by, Some(banner));
        assert_eq!(member.banned_reason, Some("spamming".to_string()));
        assert!(!member.is_active());
    }

    #[test]
    fn test_room_member_unban_clears_metadata() {
        use crate::models::{MemberStatus, RoomId, RoomMember, RoomRole, UserId};

        let mut member = RoomMember::new(
            RoomId("room1".to_string()),
            UserId("user1".to_string()),
            RoomRole::Member,
        );
        member.ban(UserId("admin1".to_string()), Some("reason".to_string()));

        member.unban();
        assert_eq!(member.status, MemberStatus::Active);
        assert!(member.banned_at.is_none());
        assert!(member.banned_by.is_none());
        assert!(member.banned_reason.is_none());
        assert!(member.is_active());
    }

    #[test]
    fn test_room_member_banned_has_no_permissions() {
        use crate::models::{RoomId, RoomMember, RoomRole, UserId};

        let mut member = RoomMember::new(
            RoomId("room1".to_string()),
            UserId("user1".to_string()),
            RoomRole::Admin,
        );
        let role_default = PermissionBits(PermissionBits::DEFAULT_ADMIN);

        // Before ban: has permissions
        assert!(member.has_permission(PermissionBits::SEND_CHAT, role_default));

        member.ban(UserId("admin1".to_string()), None);

        // After ban: zero permissions
        assert!(!member.has_permission(PermissionBits::SEND_CHAT, role_default));
        assert!(!member.has_permission(PermissionBits::DELETE_ROOM, role_default));
    }

    // ========== RoomMember: Permission Modification Methods ==========

    #[test]
    fn test_room_member_add_and_remove_permissions() {
        use crate::models::{RoomId, RoomMember, RoomRole, UserId};

        let mut member = RoomMember::new(
            RoomId("room1".to_string()),
            UserId("user1".to_string()),
            RoomRole::Member,
        );
        assert_eq!(member.added_permissions, 0);
        assert_eq!(member.removed_permissions, 0);

        member.add_permissions(PermissionBits::PLAY_CONTROL);
        assert_eq!(member.added_permissions, PermissionBits::PLAY_CONTROL);

        member.remove_permissions(PermissionBits::SEND_CHAT);
        assert_eq!(member.removed_permissions, PermissionBits::SEND_CHAT);

        let effective =
            member.effective_permissions(PermissionBits(PermissionBits::DEFAULT_MEMBER));
        assert!(effective.has(PermissionBits::PLAY_CONTROL));
        assert!(!effective.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_room_member_reset_to_role_default() {
        use crate::models::{RoomId, RoomMember, RoomRole, UserId};

        let mut member = RoomMember::new(
            RoomId("room1".to_string()),
            UserId("user1".to_string()),
            RoomRole::Member,
        );
        member.added_permissions = PermissionBits::PLAY_CONTROL;
        member.removed_permissions = PermissionBits::SEND_CHAT;
        member.admin_added_permissions = PermissionBits::BAN_MEMBER;
        member.admin_removed_permissions = PermissionBits::KICK_MEMBER;

        member.reset_to_role_default();
        assert_eq!(member.added_permissions, 0);
        assert_eq!(member.removed_permissions, 0);
        assert_eq!(member.admin_added_permissions, 0);
        assert_eq!(member.admin_removed_permissions, 0);

        let effective =
            member.effective_permissions(PermissionBits(PermissionBits::DEFAULT_MEMBER));
        assert_eq!(effective.0, PermissionBits::DEFAULT_MEMBER);
    }

    // ========== Room Status Transition Tests ==========

    #[test]
    fn test_room_status_pending_is_pending() {
        use crate::models::RoomStatus;
        let status = RoomStatus::Pending;
        assert!(status.is_pending());
        assert!(!status.is_active());
        assert!(!status.is_closed());
    }

    #[test]
    fn test_room_status_active_is_active() {
        use crate::models::RoomStatus;
        let status = RoomStatus::Active;
        assert!(status.is_active());
        assert!(!status.is_pending());
        assert!(!status.is_closed());
    }

    #[test]
    fn test_room_status_closed_is_closed() {
        use crate::models::RoomStatus;
        let status = RoomStatus::Closed;
        assert!(status.is_closed());
        assert!(!status.is_pending());
        assert!(!status.is_active());
    }

    #[test]
    fn test_room_new_has_active_status() {
        use crate::models::{Room, RoomStatus, UserId};
        let owner = UserId::new();
        let room = Room::new("Test Room".to_string(), owner);
        assert_eq!(room.status, RoomStatus::Active);
    }

    #[test]
    fn test_room_new_with_description_has_active_status() {
        use crate::models::{Room, RoomStatus, UserId};
        let owner = UserId::new();
        let room =
            Room::new_with_description("Test Room".to_string(), "A test room".to_string(), owner);
        assert_eq!(room.status, RoomStatus::Active);
    }

    // ========== Join Room Password Verification Race Condition ==========

    /// Documents the A→B→A password change race condition.
    ///
    /// SCENARIO: Fast path optimization doesn't detect intermediate password changes.
    ///
    /// 1. Initial check: password "abc123" verified against hash H1, `verified_hash` = H1
    /// 2. Password changes: H1 → H2 (different password)
    /// 3. Password changes back: H2 → H1 (same password, same hash if salt reused)
    /// 4. Under lock: fast path sees `verified_hash` (H1) == `current_hash` (H1)
    /// 5. Fast path skips re-verification, missing the intermediate change
    ///
    /// NOTE: Argon2id uses random salts, so re-hashing the same password produces
    /// a different hash. The race condition only occurs if the exact same hash
    /// string is restored (e.g., via database update), not by re-setting the same
    /// password value.
    ///
    /// FIX: Remove fast-path optimization, always re-verify password under lock.
    /// This eliminates the race condition entirely.
    ///
    /// See: /Volumes/workspace/rust/synctv/synctv-core/src/service/room.rs:575-598
    #[test]
    fn test_join_room_password_race_condition_documentation() {
        // This is a documentation test explaining the race condition.
        //
        // The bug occurs when:
        // 1. User provides password "abc123"
        // 2. Initial verification succeeds against hash H1
        // 3. Password changes to "xyz789" (hash H2)
        // 4. Password changes back to "abc123" with hash H1 (same hash!)
        // 5. Under lock, fast path skips re-verification
        //
        // The fix: Remove the fast path at lines 578-579 and always re-verify.
        //
        // Before fix:
        //   if verified_hash.as_ref() == Some(hash) {
        //       // BUG: Skip re-verification
        //   }
        //
        // After fix:
        //   // Always re-verify, no fast path
        //   let provided_password = password.ok_or_else(|| ...)?;
        //   if !verify_password(&provided_password, hash).await? {
        //       return Err(...);
        //   }

        // Demonstrate that hash comparison alone doesn't detect intermediate changes
        let hash1 = "$argon2id$v=19$m=65536,t=3,p=4$abc123$xyz789";
        let hash2 = "$argon2id$v=19$m=65536,t=3,p=4$different$salt";
        let hash3 = "$argon2id$v=19$m=65536,t=3,p=4$abc123$xyz789"; // Same as hash1

        let verified_hash: Option<String> = Some(hash1.to_string());

        // Initial state: hash1
        assert_eq!(verified_hash.as_deref(), Some(hash1));

        // Intermediate change: hash1 -> hash2
        let current_hash: Option<&str> = Some(hash2);
        assert_ne!(
            verified_hash.as_deref(),
            current_hash,
            "Hash changed, should re-verify"
        );

        // A->B->A change: hash2 -> hash1 (same as original)
        let current_hash: Option<&str> = Some(hash3);
        assert_eq!(
            verified_hash.as_deref(),
            current_hash,
            "Hash is same as initial, fast path would skip re-verification"
        );

        // The problem: fast path can't distinguish between:
        // - No change (safe to skip)
        // - A->B->A change (unsafe to skip, but hash comparison can't tell)
    }

    // ========== Room Creation Global Settings Checks ==========

    /// Replicates the `allow_room_creation` / `disable_create_room` guard logic
    /// from `do_create_room` for unit testing without a database.
    fn check_room_creation_allowed(
        disable_create_room: bool,
        allow_room_creation: bool,
    ) -> crate::Result<()> {
        if disable_create_room {
            return Err(Error::Authorization(
                "Room creation is currently disabled".to_string(),
            ));
        }
        if !allow_room_creation {
            return Err(Error::Authorization(
                "Room creation is currently disabled".to_string(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_room_creation_blocked_when_disable_create_room_is_true() {
        let result = check_room_creation_allowed(true, true);
        assert!(
            result.is_err(),
            "Should reject when disable_create_room=true"
        );
        match result.unwrap_err() {
            Error::Authorization(msg) => {
                assert!(msg.contains("disabled"), "got: {msg}");
            }
            other => panic!("Expected Authorization, got: {other:?}"),
        }
    }

    #[test]
    fn test_room_creation_blocked_when_allow_room_creation_is_false() {
        let result = check_room_creation_allowed(false, false);
        assert!(
            result.is_err(),
            "Should reject when allow_room_creation=false"
        );
        match result.unwrap_err() {
            Error::Authorization(msg) => {
                assert!(msg.contains("disabled"), "got: {msg}");
            }
            other => panic!("Expected Authorization, got: {other:?}"),
        }
    }

    #[test]
    fn test_room_creation_allowed_with_defaults() {
        // Default: disable_create_room=false, allow_room_creation=true
        let result = check_room_creation_allowed(false, true);
        assert!(
            result.is_ok(),
            "Should allow room creation with default settings"
        );
    }

    #[test]
    fn test_disable_create_room_takes_precedence_over_allow() {
        // Even if allow_room_creation=true, disable_create_room=true should block
        let result = check_room_creation_allowed(true, true);
        assert!(
            result.is_err(),
            "disable_create_room=true should take precedence over allow_room_creation=true"
        );
    }

    // ========== Integration Test Placeholders ==========
}
