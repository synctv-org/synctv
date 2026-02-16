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

use sqlx::PgPool;
use chrono::{DateTime, Utc};
use rand::RngExt;

use crate::{
    cache::CacheInvalidationService,
    models::{
        Room, RoomId, RoomMember, RoomSettings, RoomStatus, RoomWithCount, UserId,
        PermissionBits, RoomRole, MemberStatus, RoomPlaybackState, Media, MediaId,
        Playlist, PlaylistId, RoomListQuery, ChatMessage, PageParams,
    },
    repository::{RoomRepository, RoomMemberRepository, MediaRepository, PlaylistRepository, RoomPlaybackStateRepository, ChatRepository, RoomSettingsRepository},
    service::{
        auth::password::{hash_password, verify_password},
        permission::PermissionService,
        member::MemberService,
        media::MediaService,
        playlist::PlaylistService,
        playback::PlaybackService,
        notification::NotificationService,
        user::UserService,
        ProvidersManager,
    },
    Error, Result,
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

    /// Set the cache invalidation service for cross-replica room cache sync
    pub fn set_cache_invalidation(&mut self, service: Arc<CacheInvalidationService>) {
        self.cache_invalidation = Some(service);
    }

    /// Set the cluster broadcaster on the inner playback service for cross-replica sync
    pub fn set_playback_cluster_broadcaster(&mut self, broadcaster: Arc<dyn crate::service::PlaybackBroadcaster>) {
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
        let provider_instance_repo = Arc::new(crate::repository::ProviderInstanceRepository::new(pool.clone()));
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
        let provider_instance_manager = Arc::new(crate::service::RemoteProviderManager::new(provider_instance_repo, None, None));
        let providers_manager = Arc::new(ProvidersManager::new(provider_instance_manager));

        // Initialize domain services
        let mut member_service = MemberService::new(member_repo.clone(), room_repo.clone(), permission_service.clone());
        member_service.set_room_settings_repo(room_settings_repo.clone());
        let playlist_service = PlaylistService::new(playlist_repo.clone(), permission_service.clone());
        let media_service = MediaService::new(
            media_repo.clone(),
            playlist_repo.clone(),
            permission_service.clone(),
            providers_manager,
        );
        let notification_service = NotificationService::default();
        let mut playback_service = PlaybackService::new(playback_repo.clone(), permission_service.clone(), media_service.clone(), media_repo);
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
            return lock.with_lock(&lock_key, 15, || {
                let name = name.clone();
                let description = description.clone();
                let created_by = created_by.clone();
                let password = password.clone();
                let settings = settings.clone();
                async move {
                    self.do_create_room(name, description, created_by, password, settings).await
                }
            }).await;
        }

        self.do_create_room(name, description, created_by, password, settings).await
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

        // Validate room name using centralized validator
        crate::validation::RoomNameValidator::new()
            .validate(&name)
            .map_err(|e| Error::InvalidInput(e.to_string()))?;

        // Validate description length (character count for Unicode safety)
        if description.chars().count() > 500 {
            tracing::warn!(user_id = %created_by, desc_len = description.chars().count(), "Attempted to create room with description too long");
            return Err(Error::InvalidInput("Room description too long (max 500 characters)".to_string()));
        }

        // Build settings
        let mut room_settings = settings.unwrap_or_default();
        room_settings.require_password = crate::models::room_settings::RequirePassword(password.is_some());

        // Hash password outside the transaction (CPU-intensive bcrypt work)
        let pwd_hash = if let Some(ref pwd) = password {
            Some(hash_password(pwd).await?)
        } else {
            None
        };

        // Run all DB operations in a single transaction
        let mut tx = self.pool.begin().await?;

        // 1. Create room
        let room = Room::new_with_description(name, description, created_by.clone());
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
        let member = RoomMember::new(created_room.id.clone(), created_by.clone(), RoomRole::Creator);
        let created_member = self.member_repo.add_with_executor(&member, &mut *tx).await?;

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
        };
        self.playlist_repo.create_with_executor(&root_playlist, &mut *tx).await?;

        // 6. Initialize playback state
        self.playback_repo.create_or_get_with_executor(&created_room.id, &mut *tx).await?;

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
        self.permission_service.invalidate_cache(&created_room.id, &created_by).await;

        Ok((created_room, created_member))
    }

    /// Join a room
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

        // Get room
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| {
                tracing::warn!(room_id = %room_id, user_id = %user_id, "Room not found");
                Error::NotFound("Room not found".to_string())
            })?;

        // Check if room is active
        if room.status != RoomStatus::Active {
            tracing::warn!(room_id = %room_id, user_id = %user_id, status = ?room.status, "Attempted to join inactive room");
            return Err(Error::InvalidInput("Room is closed".to_string()));
        }

        // Check if user is banned from this room
        if self.member_service.is_banned(&room_id, &user_id).await? {
            tracing::warn!(room_id = %room_id, user_id = %user_id, "Banned user attempted to join room");
            return Err(Error::Authorization("You are banned from this room".to_string()));
        }

        // Check password if required
        let room_settings = self.room_settings_repo.get(&room_id).await?;
        if room_settings.require_password.0 {
            let password_hash = self.room_settings_repo.get_password_hash(&room_id).await?;

            match password_hash {
                Some(hash) => {
                    let provided_password = password.ok_or_else(|| {
                        tracing::warn!(room_id = %room_id, user_id = %user_id, "Password required but not provided");
                        Error::Authorization("Password required".to_string())
                    })?;

                    if !verify_password(&provided_password, &hash).await? {
                        tracing::warn!(room_id = %room_id, user_id = %user_id, "Invalid password provided");
                        return Err(Error::Authorization("Invalid password".to_string()));
                    }
                    tracing::debug!(room_id = %room_id, user_id = %user_id, "Password verified successfully");
                }
                None => {
                    // Room requires password but none is configured — reject join
                    tracing::warn!(room_id = %room_id, "Room requires password but none is set");
                    return Err(Error::Authorization("Invalid password".to_string()));
                }
            }
        }

        // Add member (will check if already member and max members)
        let created_member = self.member_service.add_member(room_id.clone(), user_id.clone(), RoomRole::Member).await?;

        // Get all members
        let members = self.member_service.list_members(&room_id).await?;

        // Notify room members with username
        let username = self.user_service.get_username(&user_id).await?.unwrap_or_else(|| "Unknown".to_string());
        let _ = self.notification_service.notify_user_joined(&room_id, &user_id, &username).await;

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
    pub async fn leave_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "User leaving room");

        self.member_service.remove_member(room_id.clone(), user_id.clone()).await?;

        // Notify room members with username
        let username = self.user_service.get_username(&user_id).await?.unwrap_or_else(|| "Unknown".to_string());
        let _ = self.notification_service.notify_user_left(&room_id, &user_id, &username).await;

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
    /// * `settings_registry` - Optional global settings registry (if None, guest mode is allowed)
    ///
    /// # Returns
    /// * `Ok(())` if guests are allowed
    /// * `Err` with appropriate error message if guests are not allowed
    pub async fn check_guest_allowed(
        &self,
        room_id: &RoomId,
        settings_registry: Option<&crate::service::SettingsRegistry>,
    ) -> Result<()> {
        // Check global enable_guest setting
        if let Some(registry) = settings_registry {
            let enable_guest = registry.enable_guest.get().unwrap_or(true);
            if !enable_guest {
                tracing::debug!(room_id = %room_id, "Guest access denied: global guest mode disabled");
                return Err(Error::Authorization(
                    "Guest mode is disabled globally".to_string(),
                ));
            }
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

    /// Delete a room (creator only)
    pub async fn delete_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "Deleting room");

        // Check permission without cache - critical operation requires fresh permissions
        self.permission_service
            .check_permission_no_cache(&room_id, &user_id, PermissionBits::DELETE_ROOM)
            .await?;

        // Notify before deletion
        let _ = self.notification_service.notify_room_deleted(&room_id).await;

        // Delete room
        self.room_repo.delete(&room_id).await?;

        // Track room metrics
        crate::metrics::http::ROOMS_ACTIVE.dec();

        // Invalidate room cache across all replicas
        self.notify_room_invalidation(&room_id).await;

        tracing::info!(room_id = %room_id, user_id = %user_id, "Room deleted successfully");

        Ok(())
    }

    /// Set room settings with optimistic locking (CAS).
    ///
    /// Uses version-based CAS to prevent concurrent overwrites. Retries
    /// automatically on version conflicts.
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

        // CAS write with retry
        for attempt in 0..Self::MAX_RETRIES {
            let (_current, version) = self.room_settings_repo.get_with_version(&room_id).await?;
            match self.room_settings_repo.set_settings_with_version(&room_id, &settings, version).await {
                Ok(_new_version) => {
                    // Invalidate permission cache for all room members
                    self.permission_service.invalidate_room_cache(&room_id).await;
                    self.notify_room_invalidation(&room_id).await;
                    let settings_json = serde_json::to_value(&settings)?;
                    let _ = self.notification_service.notify_settings_updated(&room_id, settings_json).await;
                    return Ok(room);
                }
                Err(Error::OptimisticLockConflict) if attempt + 1 < Self::MAX_RETRIES => {
                    let backoff = Self::BACKOFF_BASE_MS * (1 << attempt);
                    let jitter = rand::rng().random_range(0..Self::BACKOFF_BASE_MS);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff + jitter)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal("Settings update failed after maximum retry attempts".to_string()))
    }

    // ========== Query Operations ==========

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
    pub async fn get_room_settings_batch(&self, room_ids: &[&str]) -> Result<std::collections::HashMap<String, RoomSettings>> {
        self.room_settings_repo.get_batch(room_ids).await
    }

    /// Set room settings (replace entire settings object) with optimistic locking.
    pub async fn set_room_settings(&self, room_id: &RoomId, settings: &RoomSettings) -> Result<RoomSettings> {
        super::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Settings update failed after maximum retry attempts",
            || async {
                let (_current, version) = self.room_settings_repo.get_with_version(room_id).await?;
                self.room_settings_repo.set_settings_with_version(room_id, settings, version).await?;
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
    pub async fn update_room_setting(&self, room_id: &RoomId, user_id: &UserId, key: &str, value: &str) -> Result<String> {
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

            match self.room_settings_repo.set_settings_with_version(room_id, &settings, version).await {
                Ok(_new_version) => {
                    final_settings = Some(settings);
                    break;
                }
                Err(Error::OptimisticLockConflict) if attempt + 1 < Self::MAX_RETRIES => {
                    let backoff = Self::BACKOFF_BASE_MS * (1 << attempt);
                    let jitter = rand::rng().random_range(0..Self::BACKOFF_BASE_MS);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff + jitter)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        let settings = final_settings
            .ok_or_else(|| Error::Internal("Settings update failed after maximum retry attempts".to_string()))?;

        // 4. Post-apply hooks (side effects after commit)
        self.permission_service.invalidate_room_cache(room_id).await;
        self.notify_room_invalidation(room_id).await;
        self.run_post_apply_hooks(room_id, key, value).await;

        serde_json::to_string(&settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize settings: {e}")))
    }

    /// Post-apply hooks: side effects triggered after a setting change commits.
    ///
    /// Centralized registry — add new side effects here when a setting
    /// change needs to trigger external actions (notifications, kicks, etc.).
    async fn run_post_apply_hooks(&self, room_id: &RoomId, key: &str, value: &str) {
        use crate::models::room_settings::{AllowGuestJoin, RequirePassword, RoomSetting};
        use crate::service::notification::GuestKickReason;

        let kick_reason = match (key, value) {
            (k, "false") if k == AllowGuestJoin::KEY => Some(GuestKickReason::RoomGuestModeDisabled),
            (k, "true") if k == RequirePassword::KEY => Some(GuestKickReason::RoomPasswordAdded),
            _ => None,
        };

        if let Some(reason) = kick_reason {
            if let Err(e) = self.notification_service.kick_all_guests(room_id, reason).await {
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
                self.room_settings_repo.set_settings_with_version(room_id, &default_settings, version).await?;
                serde_json::to_string(&default_settings)
                    .map_err(|e| Error::Internal(format!("Failed to serialize settings: {e}")))
            },
        )
        .await
    }

    /// Check room password
    pub async fn check_room_password(&self, room_id: &RoomId, password: &str) -> Result<bool> {
        let password_hash = self.room_settings_repo.get_password_hash(room_id).await?;

        match password_hash {
            Some(stored) => {
                verify_password(password, &stored).await
                    .map_err(|e| Error::Internal(format!("Password verification failed: {e}")))
            }
            None => Ok(false),
        }
    }

    /// Update room password
    pub async fn update_room_password(&self, room_id: &RoomId, password_hash: Option<String>) -> Result<()> {
        use crate::service::notification::GuestKickReason;

        let password_was_set = password_hash.is_some();
        self.do_set_password_hash(room_id, password_hash).await?;

        // Invalidate room cache across all replicas
        self.notify_room_invalidation(room_id).await;

        // Side effects outside transaction
        if password_was_set {
            if let Err(e) = self.notification_service.kick_all_guests(
                room_id,
                GuestKickReason::RoomPasswordAdded
            ).await {
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
    async fn do_set_password_hash(&self, room_id: &RoomId, password_hash: Option<String>) -> Result<()> {
        for attempt in 0..Self::MAX_RETRIES {
            // Read current settings and version
            let (mut settings, version) = self.room_settings_repo.get_with_version(room_id).await?;

            // Update password hash in a transaction (separate key row, not version-checked)
            let mut tx = self.pool.begin().await?;
            if let Some(ref pwd_hash) = password_hash {
                self.room_settings_repo.set_with_executor(room_id, "password", pwd_hash, &mut *tx).await?;
                settings.require_password = crate::models::room_settings::RequirePassword(true);
            } else {
                self.room_settings_repo.delete_with_executor(room_id, "password", &mut *tx).await?;
                settings.require_password = crate::models::room_settings::RequirePassword(false);
            }

            // CAS update for the _settings row within the same transaction
            let json_value = serde_json::to_string(&settings)
                .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

            let cas_result = if version == 0 {
                sqlx::query(
                    r"
                    INSERT INTO room_settings (room_id, key, value, version)
                    VALUES ($1, '_settings', $2, 1)
                    ON CONFLICT (room_id, key) DO NOTHING
                    RETURNING version
                    "
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
                    "
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

            // Version mismatch -- rollback and retry
            tx.rollback().await?;
            if attempt + 1 < Self::MAX_RETRIES {
                let backoff = Self::BACKOFF_BASE_MS * (1 << attempt);
                let jitter = rand::rng().random_range(0..Self::BACKOFF_BASE_MS);
                tokio::time::sleep(std::time::Duration::from_millis(backoff + jitter)).await;
            }
        }

        Err(Error::Internal("Password update failed after maximum retry attempts".to_string()))
    }

    /// Update room description
    pub async fn update_room_description(&self, room_id: &RoomId, description: String) -> Result<Room> {
        if description.chars().count() > 500 {
            return Err(Error::InvalidInput("Room description too long (max 500 characters)".to_string()));
        }
        let room = self.room_repo.update_description(room_id, &description).await?;
        self.notify_room_invalidation(room_id).await;
        Ok(room)
    }

    /// List all rooms (paginated)
    pub async fn list_rooms(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        self.room_repo.list(query).await
    }

    /// List all rooms with member count (optimized, single query)
    pub async fn list_rooms_with_count(&self, query: &RoomListQuery) -> Result<(Vec<RoomWithCount>, i64)> {
        self.room_repo.list_with_count(query).await
    }

    /// List rooms created by a specific user
    pub async fn list_rooms_by_creator(&self, creator_id: &UserId, pagination: PageParams) -> Result<(Vec<Room>, i64)> {
        self.room_repo.list_by_creator(creator_id, pagination).await
    }

    /// List rooms created by a specific user with member count (optimized)
    pub async fn list_rooms_by_creator_with_count(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomWithCount>, i64)> {
        self.room_repo.list_by_creator_with_count(creator_id, pagination).await
    }

    /// List rooms where a user is a member
    pub async fn list_joined_rooms(&self, user_id: &UserId, pagination: PageParams) -> Result<(Vec<RoomId>, i64)> {
        self.member_service.list_user_rooms(user_id, pagination).await
    }

    /// List rooms where a user is a member with full details (optimized)
    pub async fn list_joined_rooms_with_details(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        self.member_service.list_user_rooms_with_details(user_id, pagination).await
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
        self.member_service.grant_permission(room_id, granter_id, target_user_id, permission).await
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
        self.member_service.set_member_permissions(room_id, granter_id, target_user_id, added_permissions, removed_permissions).await
    }

    /// Kick member from room
    pub async fn kick_member(
        &self,
        room_id: RoomId,
        kicker_id: UserId,
        target_user_id: UserId,
    ) -> Result<()> {
        self.member_service.kick_member(room_id, kicker_id, target_user_id).await
    }

    /// Get room members with user info
    pub async fn get_room_members(&self, room_id: &RoomId) -> Result<Vec<crate::models::RoomMemberWithUser>> {
        self.member_service.list_members(room_id).await
    }

    /// Get member count for a room
    pub async fn get_member_count(&self, room_id: &RoomId) -> Result<i32> {
        self.member_service.count_members(room_id).await
    }

    /// Check if user is a member of the room
    pub async fn check_membership(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<()> {
        if self.member_service.is_member(room_id, user_id).await? {
            Ok(())
        } else {
            Err(Error::Authorization("Not a member of this room".to_string()))
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

        self.media_service.add_media(room_id, user_id, request).await
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
            .map(|(provider_instance_name, source_config, title)| AddMediaRequest {
                playlist_id: root_playlist.id.clone(),
                name: title,
                provider_instance_name,
                source_config,
            })
            .collect();

        self.media_service.add_media_batch(room_id, user_id, root_playlist.id, requests).await
    }

    /// Remove media from playlist
    pub async fn remove_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<()> {
        self.media_service.remove_media(room_id, user_id, media_id).await
    }

    /// Get playlist (all media in room's root playlist)
    pub async fn get_playlist(&self, room_id: &RoomId) -> Result<Vec<Media>> {
        let root_playlist = self.playlist_service.get_root_playlist(room_id).await?;
        self.media_service.get_playlist_media(&root_playlist.id).await
    }

    /// Get playlist paginated
    pub async fn get_playlist_paginated(
        &self,
        room_id: &RoomId,
        pagination: PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        let root_playlist = self.playlist_service.get_root_playlist(room_id).await?;
        self.media_service.get_playlist_media_paginated(&root_playlist.id, pagination).await
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
        self.media_service.edit_media(room_id, user_id, request).await
    }

    /// Clear all media from room's root playlist
    pub async fn clear_playlist(&self, room_id: RoomId, user_id: UserId) -> Result<i64> {
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::DELETE_MOVIE_ANY)
            .await?;

        let root_playlist = self.playlist_service.get_root_playlist(&room_id).await?;

        // Delete all media in playlist directly (single query, no N+1)
        let count = self.media_service
            .delete_by_playlist(&root_playlist.id)
            .await? as i64;

        Ok(count)
    }

    /// Set current playing media for a room
    pub async fn set_playing_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<RoomPlaybackState> {
        self.playback_service.switch_media(room_id, user_id, media_id).await
    }

    /// Swap positions of two media items in playlist
    pub async fn swap_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id1: MediaId,
        media_id2: MediaId,
    ) -> Result<()> {
        self.media_service.swap_media_positions(room_id, user_id, media_id1, media_id2).await
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

    /// Get chat history for a room
    pub async fn get_chat_history(
        &self,
        room_id: &RoomId,
        before: Option<DateTime<Utc>>,
        limit: i32,
    ) -> Result<Vec<ChatMessage>> {
        self.chat_repo.list_by_room(room_id, before, limit).await
    }

    /// Save a chat message to the database
    pub async fn save_chat_message(
        &self,
        room_id: RoomId,
        user_id: UserId,
        content: String,
    ) -> Result<ChatMessage> {
        if content.is_empty() {
            return Err(Error::InvalidInput("Chat message cannot be empty".to_string()));
        }
        if content.chars().count() > 2000 {
            return Err(Error::InvalidInput("Chat message cannot exceed 2000 characters".to_string()));
        }

        let message = ChatMessage {
            id: nanoid::nanoid!(21),
            room_id,
            user_id,
            content,
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
        self.permission_service.check_permission(room_id, user_id, permission).await
    }

    // ========== Admin Operations ==========

    /// Update room status (admin use, bypasses permission checks)
    pub async fn update_room_status(&self, room_id: &RoomId, status: crate::models::RoomStatus) -> Result<Room> {
        let room = self.room_repo.update_status(room_id, status).await?;
        self.notify_room_invalidation(room_id).await;
        Ok(room)
    }

    /// Update room directly (admin use, bypasses permission checks)
    pub async fn admin_update_room(&self, room: &Room) -> Result<Room> {
        let updated = self.room_repo.update(room).await?;
        self.notify_room_invalidation(&room.id).await;
        Ok(updated)
    }

    /// Delete room (admin use, bypasses permission checks)
    pub async fn admin_delete_room(&self, room_id: &RoomId) -> Result<()> {
        let _ = self.notification_service.notify_room_deleted(room_id).await;
        self.room_repo.delete(room_id).await?;
        crate::metrics::http::ROOMS_ACTIVE.dec();
        self.notify_room_invalidation(room_id).await;
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
            if let Err(e) = self.notification_service.kick_all_guests(
                room_id,
                GuestKickReason::RoomPasswordAdded,
            ).await {
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
        let room = self.room_repo.get_by_id(room_id).await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if !room.status.is_pending() {
            return Err(Error::InvalidInput("Room is not pending approval".to_string()));
        }

        let updated_room = self.room_repo.update_status(room_id, RoomStatus::Active).await?;

        Ok(updated_room)
    }

    /// Ban a room (admin only)
    ///
    /// Sets the is_banned flag. The room retains its previous status (Active/Closed/etc).
    /// Only global admins can ban rooms.
    pub async fn ban_room(&self, room_id: &RoomId) -> Result<Room> {
        let room = self.room_repo.get_by_id(room_id).await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.is_banned {
            return Err(Error::InvalidInput("Room is already banned".to_string()));
        }

        let updated_room = self.room_repo.update_ban_status(room_id, true).await?;
        self.notify_room_invalidation(room_id).await;

        Ok(updated_room)
    }

    /// Unban a room (admin only)
    ///
    /// Clears the is_banned flag. The room returns to its previous status.
    /// Only global admins can unban rooms.
    pub async fn unban_room(&self, room_id: &RoomId) -> Result<Room> {
        let room = self.room_repo.get_by_id(room_id).await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if !room.is_banned {
            return Err(Error::InvalidInput("Room is not banned".to_string()));
        }

        let updated_room = self.room_repo.update_ban_status(room_id, false).await?;
        self.notify_room_invalidation(room_id).await;

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

    /// Broadcast a room cache invalidation message to other replicas.
    ///
    /// Best-effort: logs a warning on failure but does not propagate the error,
    /// since cache invalidation is not critical to the mutation itself.
    async fn notify_room_invalidation(&self, room_id: &RoomId) {
        if let Some(ref service) = self.cache_invalidation {
            if let Err(e) = service.invalidate_room(room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
                    "Failed to broadcast room cache invalidation"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Error;
    use crate::models::{
        RoomSettings, RoomStatus, PermissionBits,
        room_settings::{
            ChatEnabled, DanmakuEnabled, AllowGuestJoin, RequirePassword,
            MaxMembers, GuestAddedPermissions, MemberAddedPermissions,
        },
    };
    use crate::test_helpers::RoomFixture;

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
            Error::InvalidInput(msg) => assert!(msg.contains("at least 1") || msg.contains("cannot be empty"), "got: {msg}"),
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
            Error::InvalidInput(msg) => assert!(msg.contains("characters") || msg.contains("long"), "got: {msg}"),
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
        let name: String = std::iter::repeat('\u{4e00}').take(max_len).collect();
        assert_eq!(name.chars().count(), max_len);
        assert!(validate_room_name(&name).is_ok(), "Room name with {} CJK characters should be valid", max_len);

        // (ROOM_NAME_MAX + 1) CJK characters should be rejected
        let name_too_long: String = std::iter::repeat('\u{4e00}').take(max_len + 1).collect();
        assert!(validate_room_name(&name_too_long).is_err(), "Room name with {} CJK characters should be rejected", max_len + 1);
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
        let desc: String = std::iter::repeat('\u{4e00}').take(500).collect();
        assert_eq!(desc.chars().count(), 500);
        assert!(validate_room_description(&desc).is_ok());

        // 501 CJK characters should be rejected even though 255 ASCII chars would be fine
        let desc_too_long: String = std::iter::repeat('\u{4e00}').take(501).collect();
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
            ("auto_play", r#"{"enabled":true,"mode":"sequential","delay":3}"#),
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
        assert_eq!(
            deserialized.require_password.0,
            settings.require_password.0
        );
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
        let mut closed_room = room.clone();
        closed_room.status = RoomStatus::Closed;
        assert!(!closed_room.is_active());
    }

    // ========== Room Model: Constructor Behavior ==========

    #[test]
    fn test_room_new_generates_unique_ids() {
        use crate::models::Room;

        let user_id = crate::models::UserId::new();
        let room1 = Room::new("Room A".to_string(), user_id.clone());
        let room2 = Room::new("Room B".to_string(), user_id.clone());
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
                return Err(Error::InvalidInput("Room description too long (max 500 characters)".to_string()));
            }
            Ok(())
        }

        // Mixed Unicode: emoji are single characters but multi-byte
        let emoji_desc: String = std::iter::repeat('\u{1F600}').take(500).collect();
        assert_eq!(emoji_desc.chars().count(), 500);
        assert!(validate_desc(&emoji_desc).is_ok());

        let emoji_over: String = std::iter::repeat('\u{1F600}').take(501).collect();
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
        use crate::models::{RoomMember, RoomId, UserId, RoomRole, MemberStatus};

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
        use crate::models::{RoomMember, RoomId, UserId, RoomRole, MemberStatus};

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
        use crate::models::{RoomMember, RoomId, UserId, RoomRole};

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
        use crate::models::{RoomMember, RoomId, UserId, RoomRole};

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

        let effective = member.effective_permissions(PermissionBits(PermissionBits::DEFAULT_MEMBER));
        assert!(effective.has(PermissionBits::PLAY_CONTROL));
        assert!(!effective.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_room_member_reset_to_role_default() {
        use crate::models::{RoomMember, RoomId, UserId, RoomRole};

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

        let effective = member.effective_permissions(PermissionBits(PermissionBits::DEFAULT_MEMBER));
        assert_eq!(effective.0, PermissionBits::DEFAULT_MEMBER);
    }

    // ========== Integration Test Placeholders ==========

}
