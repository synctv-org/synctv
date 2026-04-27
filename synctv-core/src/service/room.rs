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
//! ## Transactional Operations (After Commit)
//!
//! For operations wrapped in transactions (e.g., `delete_room`, `admin_delete_room`),
//! cache invalidation MUST happen AFTER `tx.commit()` succeeds. Broadcasting
//! invalidation before commit creates a worse race:
//!
//! 1. Cache is invalidated while the transaction is still open
//! 2. Concurrent request misses cache and reads pre-commit database state
//! 3. Old data is written back into cache
//! 4. Transaction commits, leaving replicas with stale cache state
//!
//! By invalidating only after commit, every cache miss observes committed state.
//!
//! ### Implementation
//!
//! Use the `invalidate_room_caches()` helper method for transactional operations:
//!
//! ```text
//! let mut tx = self.pool.begin().await?;
//! //... perform database operations...
//! tx.commit().await?;
//! self.invalidate_room_caches(&room_id).await; // AFTER commit
//! //... post-commit operations...
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
//! self.notify_room_invalidation(&room_id).await; // Room cache only
//! ```
//!
//! ## Cache Types
//!
//! - **Room cache**: Broadcast to all replicas via `CacheInvalidationService`
//! - **Permission cache**: Local only (cleared on each replica independently)
//! - **Playback cache**: Broadcast to all replicas via `CacheInvalidationService`
//!
//! The `invalidate_room_caches()` method handles all three types appropriately.

use chrono::{DateTime, Duration, Utc};
use rand::RngExt;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;

use crate::{
    cache::CacheInvalidationRuntime,
    models::{
        ChatMessage, Media, MediaId, MemberStatus, PageParams, PermissionBits, Playlist,
        PlaylistId, ReviewRequestId, ReviewStatus, Room, RoomId, RoomListQuery, RoomMember,
        RoomPlaybackState, RoomRole, RoomSettings, RoomStatus, RoomWithCount, UserId,
        UserListQuery, UserRole, UserStatus,
    },
    repository::{
        media::MediaListItem, playlist::PlaylistListItem, ChatRepository, MediaRepository,
        PlaylistRepository, RoomMemberRepository, RoomPlaybackStateRepository, RoomRepository,
        RoomSettingsRepository, UserProviderCredentialRepository,
    },
    service::{
        audit::{AuditAction, AuditService, AuditTargetType},
        media::MediaService,
        member::AddMemberOptions,
        member::MemberService,
        notification::NotificationService,
        permission::PermissionService,
        playback::PlaybackService,
        playlist::PlaylistService,
        room_settings::RoomSettingsService,
        user::UserService,
        ProvidersManager,
    },
    Error, InternalExt, Result,
};

#[derive(Debug)]
struct PendingRoomCreationRequest {
    id: RoomId,
    requested_by: UserId,
    name: String,
    description: String,
    password_hash: Option<String>,
    settings: RoomSettings,
}
use std::{future::Future, sync::Arc};
use synctv_common::ExecutionControl;

fn usize_to_i64_saturating(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

const MAX_DELETE_TARGETS: usize = 100;
const ROOM_JOIN_REQUEST_PENDING: ReviewStatus = ReviewStatus::Pending;
const ROOM_JOIN_REQUEST_APPROVED: ReviewStatus = ReviewStatus::Approved;
const ROOM_JOIN_REQUEST_REJECTED: ReviewStatus = ReviewStatus::Rejected;

#[derive(Debug, Clone)]
pub struct AuthorizedAdminActor {
    user_id: UserId,
    username: String,
}

impl AuthorizedAdminActor {
    pub fn new(user_id: UserId, username: String, role: UserRole) -> Result<Self> {
        if !role.is_admin_or_above() {
            return Err(Error::Authorization(
                "Admin role required for this operation".to_string(),
            ));
        }

        Ok(Self { user_id, username })
    }

    #[must_use]
    pub fn user_id(&self) -> &UserId {
        &self.user_id
    }

    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }
}

/// Room service for business logic
///
/// This is the main service that coordinates between domain services.
#[derive(Clone, Default)]
pub struct RoomServiceOptions {
    pub distributed_lock: Option<Arc<dyn crate::service::distributed_lock::CoordinationLock>>,
    pub cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    pub playback_l2_cache: Option<crate::cache::PlaybackStateCache>,
    pub credential_encryption: Option<crate::service::CredentialEncryption>,
    pub credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    pub audit_service: Option<Arc<AuditService>>,
    pub brute_force_service: Option<Arc<dyn crate::service::auth::BruteForceProtectionService>>,
    pub settings_registry: Option<Arc<crate::service::SettingsRegistry>>,
    pub user_notification_service: Option<Arc<crate::service::UserNotificationService>>,
    pub password_hasher: Option<Arc<dyn crate::service::auth::PasswordHasherService>>,
}

/// Room service for business logic
///
/// This is the main service that coordinates between domain services.
/// Core room operations are handled here, while specific domains are delegated.
#[derive(Clone)]
pub struct RoomService {
    // Database pool for transactions
    pool: PgPool,

    // Optional distributed lock (requires Redis, used in multi-replica mode)
    distributed_lock: Option<Arc<dyn crate::service::distributed_lock::CoordinationLock>>,

    // Core repositories
    room_repo: RoomRepository,
    room_settings_repo: RoomSettingsRepository,
    member_repo: RoomMemberRepository,
    media_repo: MediaRepository,
    playlist_repo: PlaylistRepository,
    playback_repo: RoomPlaybackStateRepository,
    chat_repo: ChatRepository,

    // Domain services
    member_service: MemberService,
    permission_service: PermissionService,
    playlist_service: PlaylistService,
    media_service: MediaService,
    playback_service: PlaybackService,
    room_settings_service: RoomSettingsService,
    notification_service: NotificationService,
    user_service: UserService,

    /// Optional cache invalidation service for cross-replica room cache sync
    cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,

    /// Optional audit service for logging security-sensitive operations
    audit_service: Option<Arc<AuditService>>,

    /// Optional brute-force protection for room password verification
    brute_force_service: Option<Arc<dyn crate::service::auth::BruteForceProtectionService>>,

    /// Optional settings registry for reading `create_room_need_review` setting
    settings_registry: Option<Arc<crate::service::SettingsRegistry>>,

    /// Optional user notification service for sending admin notifications
    /// (e.g., pending room review alerts)
    user_notification_service: Option<Arc<crate::service::UserNotificationService>>,

    /// Password hasher (Argon2id). Defaults to production params;
    /// inject `TestPasswordHasher` in integration tests for speed.
    password_hasher: Arc<dyn crate::service::auth::PasswordHasherService>,
}

#[derive(Debug, Clone, Default)]
pub struct DeleteEntriesRequest {
    pub playlist_ids: Vec<PlaylistId>,
    pub media_ids: Vec<MediaId>,
    pub force: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteEntriesResult {
    pub deleted_playlists: usize,
    pub deleted_media: usize,
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteEntriesPlan {
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_reset: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClearPlaylistResult {
    pub deleted_count: i64,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_state: Option<RoomPlaybackState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct EntryDeletionImpact {
    pub playlist_nodes: Vec<(PlaylistId, i32)>,
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_reset: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RoomCleanupImpact {
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub members_deleted: u64,
    pub settings_deleted: u64,
    pub playback_rows_deleted: u64,
    pub chat_deleted: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientResourceAvailability {
    Available,
    CreatorInactive,
}

impl ClientResourceAvailability {
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

impl std::fmt::Debug for RoomService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomService").finish()
    }
}

impl RoomService {
    async fn load_authorized_admin_actor(
        &self,
        admin_user_id: &UserId,
    ) -> Result<AuthorizedAdminActor> {
        let admin_user = self.user_service.get_user(admin_user_id).await?;
        AuthorizedAdminActor::new(*admin_user_id, admin_user.username, admin_user.role)
    }

    async fn create_room_creation_request(
        &self,
        requested_by: &UserId,
        name: &str,
        description: &str,
        password_hash: Option<&str>,
        settings: &RoomSettings,
    ) -> Result<Room> {
        let settings_payload = serde_json::to_value(settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

        let request_id = sqlx::query_scalar::<_, i64>(
            r"
            INSERT INTO room_creation_requests (
                requested_by, name, description, password_hash, settings_payload, status, requested_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, CURRENT_TIMESTAMP)
            RETURNING id
            ",
        )
        .bind(requested_by)
        .bind(name)
        .bind(description)
        .bind(password_hash)
        .bind(settings_payload)
        .bind(ReviewStatus::Pending)
        .fetch_one(&self.pool)
        .await?;

        let mut room =
            Room::new_with_description(name.to_string(), description.to_string(), *requested_by);
        room.id = RoomId::from(request_id);
        Ok(room)
    }

    async fn load_pending_room_creation_request_for_update(
        request_id: &RoomId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Option<PendingRoomCreationRequest>> {
        let row = sqlx::query(
            r"
            SELECT id, requested_by, name, description, password_hash, settings_payload
            FROM room_creation_requests
            WHERE id = $1 AND reviewed_at IS NULL AND status = $2
            FOR UPDATE
            ",
        )
        .bind(request_id)
        .bind(ReviewStatus::Pending)
        .fetch_optional(&mut **tx)
        .await?;

        row.map(|row| {
            let settings_payload = row
                .try_get::<Option<serde_json::Value>, _>("settings_payload")?
                .unwrap_or_else(|| serde_json::json!({}));
            let settings = serde_json::from_value::<RoomSettings>(settings_payload)
                .map_err(|e| sqlx::Error::Decode(e.into()))?;
            Ok(PendingRoomCreationRequest {
                id: row.try_get("id")?,
                requested_by: row.try_get("requested_by")?,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                password_hash: row.try_get("password_hash")?,
                settings,
            })
        })
        .transpose()
        .map_err(Error::Database)
    }

    async fn enforce_room_ownership_limit(
        &self,
        owner_id: &UserId,
        excluding_room_id: Option<&RoomId>,
    ) -> Result<()> {
        let max_rooms = self
            .settings_registry
            .as_ref()
            .map(|registry| registry.max_rooms_per_user.get())
            .transpose()?
            .unwrap_or(10);

        let (rooms, total) = self
            .room_repo
            .list_by_creator(
                owner_id,
                PageParams::new(
                    Some(1),
                    Some(
                        u32::try_from(max_rooms)
                            .unwrap_or(u32::MAX)
                            .saturating_add(1),
                    ),
                ),
            )
            .await?;

        let owned_room_count = match excluding_room_id {
            Some(room_id) => {
                usize_to_i64_saturating(rooms.iter().filter(|room| room.id != *room_id).count())
            }
            None => total,
        };

        if owned_room_count >= max_rooms {
            return Err(Error::InvalidInput(format!(
                "User has reached the maximum number of rooms ({max_rooms})"
            )));
        }

        Ok(())
    }

    fn playlist_client_availability(
        playlist: &Playlist,
        active_creators: &HashSet<UserId>,
    ) -> ClientResourceAvailability {
        match playlist.creator_id.as_ref() {
            Some(creator_id) if !active_creators.contains(creator_id) => {
                ClientResourceAvailability::CreatorInactive
            }
            _ => ClientResourceAvailability::Available,
        }
    }

    fn media_client_availability(
        media: &Media,
        active_creators: &HashSet<UserId>,
    ) -> ClientResourceAvailability {
        match media.creator_id.as_ref() {
            Some(creator_id) if !active_creators.contains(creator_id) => {
                ClientResourceAvailability::CreatorInactive
            }
            _ => ClientResourceAvailability::Available,
        }
    }

    fn room_client_availability(
        room: &Room,
        active_creators: &HashSet<UserId>,
    ) -> ClientResourceAvailability {
        if active_creators.contains(&room.created_by) {
            ClientResourceAvailability::Available
        } else {
            ClientResourceAvailability::CreatorInactive
        }
    }

    async fn load_active_creators<'a, I>(&self, creator_ids: I) -> Result<HashSet<UserId>>
    where
        I: IntoIterator<Item = &'a UserId>,
    {
        let unique_ids: Vec<UserId> = creator_ids
            .into_iter()
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        if unique_ids.is_empty() {
            return Ok(HashSet::new());
        }

        Ok(self
            .user_service
            .get_users_by_ids(&unique_ids)
            .await?
            .into_iter()
            .filter(|user| user.status.is_active() && !user.is_banned)
            .map(|user| user.id)
            .collect())
    }

    async fn ensure_resource_creator_is_active_for_client_access(
        &self,
        creator_id: Option<&UserId>,
        resource_kind: &'static str,
    ) -> Result<()> {
        let Some(creator_id) = creator_id else {
            return Ok(());
        };

        match self.user_service.get_user(creator_id).await {
            Ok(user) if user.status.is_active() && !user.is_banned => Ok(()),
            Ok(_) | Err(Error::NotFound(_)) => Err(Error::Authorization(format!(
                "{resource_kind} is unavailable because its creator is not active"
            ))),
            Err(error) => Err(error),
        }
    }

    async fn ensure_room_creator_is_active_for_access(
        &self,
        room: &Room,
        actor_user_id: &UserId,
    ) -> Result<()> {
        let actor = self.user_service.get_user(actor_user_id).await?;
        if actor.role.is_admin_or_above() {
            return Ok(());
        }

        let creator = self.user_service.get_user(&room.created_by).await;
        match creator {
            Ok(user) if user.status.is_active() && !user.is_banned => Ok(()),
            Ok(_) | Err(Error::NotFound(_)) => Err(Error::Authorization(
                "Room is unavailable because its creator is not active".to_string(),
            )),
            Err(error) => Err(error),
        }
    }

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

    pub async fn ensure_client_usable_playlist(&self, playlist: &Playlist) -> Result<()> {
        if !playlist.is_dynamic() {
            return Ok(());
        }

        self.ensure_resource_creator_is_active_for_client_access(
            playlist.creator_id.as_ref(),
            "Dynamic playlist",
        )
        .await
    }

    pub async fn playlist_availability(
        &self,
        playlist: &Playlist,
    ) -> Result<ClientResourceAvailability> {
        let active_creators = self
            .load_active_creators(playlist.creator_id.iter())
            .await?;
        Ok(Self::playlist_client_availability(
            playlist,
            &active_creators,
        ))
    }

    pub async fn room_availability(&self, room: &Room) -> Result<ClientResourceAvailability> {
        let active_creators = self
            .load_active_creators(std::iter::once(&room.created_by))
            .await?;
        Ok(Self::room_client_availability(room, &active_creators))
    }

    pub async fn room_availability_batch(
        &self,
        rooms: &[Room],
    ) -> Result<HashMap<RoomId, ClientResourceAvailability>> {
        let active_creators = self
            .load_active_creators(rooms.iter().map(|room| &room.created_by))
            .await?;
        Ok(rooms
            .iter()
            .map(|room| {
                (
                    room.id,
                    Self::room_client_availability(room, &active_creators),
                )
            })
            .collect())
    }

    pub async fn playlist_availability_map(
        &self,
        playlists: &[Playlist],
    ) -> Result<HashMap<PlaylistId, ClientResourceAvailability>> {
        let active_creators = self
            .load_active_creators(
                playlists
                    .iter()
                    .filter_map(|playlist| playlist.creator_id.as_ref()),
            )
            .await?;

        Ok(playlists
            .iter()
            .map(|playlist| {
                (
                    playlist.id,
                    Self::playlist_client_availability(playlist, &active_creators),
                )
            })
            .collect())
    }

    pub async fn count_client_playlists(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        query: &crate::models::PlaylistListQuery,
    ) -> Result<i64> {
        self.playlist_repo
            .count_filtered_by_parent(room_id, parent_id, query)
            .await
    }

    pub async fn list_client_playlists(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        query: &crate::models::PlaylistListQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PlaylistListItem>> {
        self.playlist_repo
            .list_filtered_by_parent(room_id, parent_id, query, limit, offset)
            .await
    }

    pub async fn media_availability(&self, media: &Media) -> Result<ClientResourceAvailability> {
        let active_creators = self.load_active_creators(media.creator_id.iter()).await?;
        Ok(Self::media_client_availability(media, &active_creators))
    }

    pub async fn media_availability_map(
        &self,
        media: &[Media],
    ) -> Result<HashMap<MediaId, ClientResourceAvailability>> {
        let active_creators = self
            .load_active_creators(media.iter().filter_map(|item| item.creator_id.as_ref()))
            .await?;

        Ok(media
            .iter()
            .map(|item| {
                (
                    item.id,
                    Self::media_client_availability(item, &active_creators),
                )
            })
            .collect())
    }

    pub async fn count_client_media(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        query: &crate::models::MediaListQuery,
    ) -> Result<i64> {
        self.media_repo
            .count_filtered_by_scope(room_id, playlist_id, query)
            .await
    }

    pub async fn list_client_media(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        query: &crate::models::MediaListQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<MediaListItem>> {
        self.media_repo
            .list_filtered_by_scope(room_id, playlist_id, query, limit, offset)
            .await
    }

    /// Get the permission service
    ///
    /// Used by `ClusterManager` to invalidate permission cache on cross-replica events.
    #[must_use]
    pub const fn permission_service(&self) -> &PermissionService {
        &self.permission_service
    }

    #[must_use]
    pub const fn room_settings_service(&self) -> &RoomSettingsService {
        &self.room_settings_service
    }

    /// Get the user service used by room coordination flows.
    #[must_use]
    pub const fn user_service(&self) -> &UserService {
        &self.user_service
    }

    /// Set the distributed lock (enables multi-replica safety for room creation)
    pub fn set_distributed_lock(
        &mut self,
        lock: Arc<dyn crate::service::distributed_lock::CoordinationLock>,
    ) {
        self.distributed_lock = Some(lock);
    }

    /// Set the cache invalidation service for cross-replica room cache sync.
    ///
    /// Also propagates to the inner `MemberService` so that permission/role
    /// changes are broadcast to other replicas.
    pub fn set_cache_invalidation(&mut self, service: Arc<dyn CacheInvalidationRuntime>) {
        self.permission_service
            .set_invalidation_service(Arc::clone(&service));
        self.member_service
            .set_cache_invalidation(Arc::clone(&service));
        self.room_settings_service
            .set_invalidation_service(Arc::clone(&service));
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

    /// Set the cluster broadcaster on the inner member service for cross-replica kick/ban sync.
    /// Uses interior mutability so this can be called through `Arc<RoomService>`.
    pub fn set_member_event_broadcaster(
        &self,
        broadcaster: Arc<dyn crate::service::MemberEventBroadcaster>,
    ) {
        self.member_service.set_event_broadcaster(broadcaster);
    }

    /// Set the cluster broadcaster on the inner playlist service for cross-replica sync.
    /// Uses interior mutability so this can be called through `Arc<RoomService>`.
    pub fn set_playlist_cluster_broadcaster(
        &self,
        broadcaster: Arc<dyn crate::service::PlaylistBroadcaster>,
    ) {
        self.playlist_service.set_cluster_broadcaster(broadcaster);
    }

    /// Wire the cache invalidation service into the inner playback service
    /// so it can broadcast invalidation messages to other replicas on updates.
    pub fn set_playback_cache_invalidation(&mut self, service: Arc<dyn CacheInvalidationRuntime>) {
        self.playback_service.set_invalidation_service(service);
    }

    /// Wire the playback L2 cache into the inner playback service.
    ///
    /// This keeps bootstrap ownership at the `RoomService` boundary instead of
    /// reaching through to private service internals.
    pub fn set_playback_l2_cache(&mut self, cache: crate::cache::PlaybackStateCache) {
        self.playback_service.set_l2_cache(cache);
    }

    #[must_use]
    pub fn new(pool: PgPool, user_service: UserService) -> Self {
        Self::new_with_options(pool, user_service, RoomServiceOptions::default())
    }

    #[must_use]
    pub fn new_with_options(
        pool: PgPool,
        user_service: UserService,
        options: RoomServiceOptions,
    ) -> Self {
        let provider_instance_repo = Arc::new(crate::repository::ProviderInstanceRepository::new(
            pool.clone(),
        ));
        let provider_instance_manager = Arc::new(crate::service::RemoteProviderManager::new(
            provider_instance_repo,
        ));
        let providers_manager = Arc::new(ProvidersManager::new(provider_instance_manager));
        Self::new_with_providers_and_options(pool, user_service, providers_manager, options)
    }

    #[must_use]
    pub fn new_with_providers(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
    ) -> Self {
        Self::new_with_providers_and_options(
            pool,
            user_service,
            providers_manager,
            RoomServiceOptions::default(),
        )
    }

    #[must_use]
    pub fn new_with_providers_and_options(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
        options: RoomServiceOptions,
    ) -> Self {
        let permission_service = PermissionService::new_with_runtime(
            RoomMemberRepository::new(pool.clone()),
            None,
            PermissionService::DEFAULT_CACHE_SIZE,
            PermissionService::DEFAULT_CACHE_TTL_SECS,
            Some(RoomSettingsRepository::new(pool.clone())),
            options.cache_invalidation.clone(),
        );
        Self::new_with_providers_permission_service_and_options(
            pool,
            user_service,
            providers_manager,
            permission_service,
            options,
        )
    }

    #[must_use]
    pub fn new_with_providers_and_permission_service(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
        permission_service: PermissionService,
    ) -> Self {
        Self::new_with_providers_permission_service_and_options(
            pool,
            user_service,
            providers_manager,
            permission_service,
            RoomServiceOptions::default(),
        )
    }

    #[must_use]
    pub fn new_with_providers_permission_service_and_options(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
        permission_service: PermissionService,
        options: RoomServiceOptions,
    ) -> Self {
        // Initialize repositories
        let room_repo = RoomRepository::new(pool.clone());
        let room_settings_repo = RoomSettingsRepository::new(pool.clone());
        let member_repo = RoomMemberRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
        let chat_repo = ChatRepository::new(pool.clone());

        let notification_service = NotificationService::default();

        // Initialize domain services
        let member_service = MemberService::new_with_runtime(
            member_repo.clone(),
            room_repo.clone(),
            Some(room_settings_repo.clone()),
            permission_service.clone(),
            options.audit_service.clone(),
            options.cache_invalidation.clone(),
            notification_service.clone(),
        );

        let playlist_service = PlaylistService::new_with_provider_credentials(
            playlist_repo.clone(),
            permission_service.clone(),
            providers_manager.clone(),
            options.credential_encryption.clone(),
            options.credential_repo.clone(),
        );
        let media_service = MediaService::new_with_provider_credentials(
            media_repo.clone(),
            playlist_repo.clone(),
            permission_service.clone(),
            providers_manager,
            notification_service.clone(),
            options.credential_encryption.clone(),
            options.credential_repo.clone(),
        );
        let playback_service = PlaybackService::new_with_runtime(
            playback_repo.clone(),
            permission_service.clone(),
            media_service.clone(),
            user_service.clone(),
            options.cache_invalidation.clone(),
            options.playback_l2_cache.clone(),
        );
        let room_settings_service = RoomSettingsService::new(
            room_settings_repo.clone(),
            options.cache_invalidation.clone(),
            Arc::new(notification_service.clone()),
            None,
            None,
        );

        Self {
            pool,
            distributed_lock: options.distributed_lock,
            room_repo,
            room_settings_repo,
            member_repo,
            media_repo,
            playlist_repo,
            playback_repo,
            chat_repo,
            member_service,
            permission_service,
            playlist_service,
            media_service,
            playback_service,
            room_settings_service,
            notification_service,
            user_service,
            cache_invalidation: options.cache_invalidation,
            audit_service: options.audit_service,
            brute_force_service: options.brute_force_service,
            settings_registry: options.settings_registry,
            user_notification_service: options.user_notification_service,
            password_hasher: options
                .password_hasher
                .unwrap_or_else(|| Arc::new(crate::service::auth::ProdPasswordHasher::default())),
        }
    }

    /// Inject the brute-force protection service for room password rate limiting.
    pub fn set_brute_force_service<T>(&mut self, service: T)
    where
        T: crate::service::auth::BruteForceProtectionService + 'static,
    {
        self.set_brute_force_service_arc(Arc::new(service));
    }

    /// Inject a pre-built brute-force protection service trait object.
    pub fn set_brute_force_service_arc(
        &mut self,
        service: Arc<dyn crate::service::auth::BruteForceProtectionService>,
    ) {
        self.brute_force_service = Some(service);
    }

    #[cfg(test)]
    pub(crate) const fn has_brute_force_service(&self) -> bool {
        self.brute_force_service.is_some()
    }

    #[cfg(test)]
    pub(crate) const fn has_distributed_lock(&self) -> bool {
        self.distributed_lock.is_some()
    }

    #[cfg(test)]
    pub(crate) const fn has_settings_registry(&self) -> bool {
        self.settings_registry.is_some()
    }

    #[doc(hidden)]
    pub const fn settings_registry(&self) -> Option<&Arc<crate::service::SettingsRegistry>> {
        self.settings_registry.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn has_playback_l2_cache(&self) -> bool {
        self.playback_service.has_l2_cache()
    }

    #[doc(hidden)]
    pub fn has_member_event_broadcaster(&self) -> bool {
        self.member_service.has_event_broadcaster()
    }

    #[doc(hidden)]
    pub fn has_playlist_cluster_broadcaster(&self) -> bool {
        self.playlist_service.has_cluster_broadcaster()
    }

    /// Inject the settings registry for reading `create_room_need_review` and other global settings.
    pub fn set_settings_registry(&mut self, registry: Arc<crate::service::SettingsRegistry>) {
        self.settings_registry = Some(registry);
    }

    /// Override the password hasher (e.g. inject `TestPasswordHasher` in tests).
    pub fn set_password_hasher(
        &mut self,
        hasher: Arc<dyn crate::service::auth::PasswordHasherService>,
    ) {
        self.password_hasher = hasher;
    }

    /// Log an audit event if the audit service is configured.
    /// Failures are logged as warnings but never propagated.
    ///
    /// The `actor_username` is passed from the caller (API layer) to avoid
    /// a separate DB lookup. Pass an empty string if the username is not
    /// available (e.g., in background tasks).
    async fn audit_log(
        &self,
        actor_id: &UserId,
        actor_username: &str,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: serde_json::Value,
    ) {
        if let Some(ref audit) = self.audit_service {
            if let Err(e) = audit
                .log(
                    actor_id.to_string(),
                    actor_username.to_string(),
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

    /// Create a new room
    ///
    /// All database operations run inside a single transaction so the room is
    /// either fully created or not visible at all — no partially-created rooms.
    /// Duplicate room names for the same creator are rejected atomically by
    /// the database UNIQUE constraint on `(rooms.created_by, rooms.name)`.
    ///
    /// When a distributed lock is configured (multi-replica mode), a per-user
    /// lock still coalesces repeated requests from the same user (network
    /// retries, double-clicks) before they reach the database.
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
            let lock_key = format!("create_room:{created_by}");
            return crate::service::distributed_lock::with_coordination_lock(
                lock.as_ref(),
                &lock_key,
                Self::CREATE_ROOM_LOCK_TTL_SECS,
                || {
                    let name = name.clone();
                    let description = description.clone();
                    let password = password.clone();
                    let settings = settings.clone();
                    async move {
                        self.do_create_room(name, description, created_by, password, settings)
                            .await
                    }
                },
            )
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
        self.do_create_room_with_policy(name, description, created_by, password, settings, true)
            .await
    }

    async fn do_create_room_with_policy(
        &self,
        name: String,
        description: String,
        created_by: UserId,
        password: Option<String>,
        settings: Option<RoomSettings>,
        enforce_creation_policy: bool,
    ) -> Result<(Room, RoomMember)> {
        tracing::info!(
            user_id = %created_by,
            room_name = %name,
            has_password = password.is_some(),
            "Creating new room"
        );

        // Check global settings: room creation must be allowed.
        // Setting precedence:
        // 1. `disable_create_room` (highest priority) — when true, room creation is
        // unconditionally blocked. This is the "kill switch" for emergencies or
        // maintenance windows.
        // 2. `allow_room_creation` — when explicitly set to false, room creation is
        // blocked. Defaults to true when unset. This is the normal admin toggle
        // for controlling whether users can create rooms.
        // Both settings exist to serve different admin workflows:
        // - `disable_create_room` = emergency override (takes priority over everything)
        // - `allow_room_creation` = standard policy control (opt-out, default-allow)
        if let Some(ref registry) = self.settings_registry {
            if enforce_creation_policy {
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
            // `room_must_need_pwd`: if true, rooms must have a password
            if registry.room_must_need_pwd.get().unwrap_or(false) && password.is_none() {
                tracing::warn!(user_id = %created_by, "Room creation rejected: password required by server policy");
                return Err(Error::InvalidInput(
                    "Room password is required by server policy".to_string(),
                ));
            }
            // `room_must_no_need_pwd`: if true, rooms must NOT have a password
            if registry.room_must_no_need_pwd.get().unwrap_or(false) && password.is_some() {
                tracing::warn!(user_id = %created_by, "Room creation rejected: passwords not allowed by server policy");
                return Err(Error::InvalidInput(
                    "Room passwords are not allowed by server policy".to_string(),
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
            Some(self.password_hasher.hash_password(pwd).await?)
        } else {
            None
        };

        self.enforce_room_ownership_limit(&created_by, None).await?;

        let need_review = self
            .settings_registry
            .as_ref()
            .is_some_and(|r| r.create_room_need_review.get().unwrap_or(false));

        if need_review {
            tracing::info!(
                user_id = %created_by,
                room_name = %name,
                "Room requires review, creating room creation request"
            );

            let pending_room = self
                .create_room_creation_request(
                    &created_by,
                    &name,
                    &description,
                    pwd_hash.as_deref(),
                    &room_settings,
                )
                .await?;
            let pending_member = RoomMember::new(pending_room.id, created_by, RoomRole::Creator);

            if let Some(ref notif_service) = self.user_notification_service {
                let mut all_admins = Vec::new();
                for role in [UserRole::Root, UserRole::Admin] {
                    let query = UserListQuery {
                        pagination: PageParams::new(Some(1), Some(100)),
                        search: None,
                        status: Some(UserStatus::Active),
                        role: Some(role),
                        is_banned: Some(false),
                        sort_by: crate::models::UserListSortBy::CreatedAt,
                        sort_direction: crate::models::SortDirection::Desc,
                    };
                    if let Ok((users, _)) = self.user_service.list_users(&query).await {
                        all_admins.extend(users);
                    }
                }

                for admin in all_admins {
                    if let Err(e) = notif_service
                        .create_system_announcement(
                            admin.id,
                            format!("Room Pending Review: {name}"),
                            format!(
                                "User {created_by} requested room \"{name}\" which requires admin review."
                            ),
                            Some(serde_json::json!({
                                "room_request_id": pending_room.id,
                                "room_name": &name,
                                "creator_id": created_by,
                            })),
                        )
                        .await
                    {
                        tracing::warn!(
                            admin_id = %admin.id,
                            error = %e,
                            "Failed to notify admin about pending room"
                        );
                    }
                }
            }

            return Ok((pending_room, pending_member));
        }

        // Transaction: Create room with all related data atomically.
        // On error, the transaction will be automatically rolled back.
        let mut tx = self.pool.begin().await?;

        // 1. Create room
        let room = Room::new_with_description(name, description, created_by);
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
        let member = RoomMember::new(created_room.id, created_by, RoomRole::Creator);
        let created_member = self.member_repo.add_with_executor(&member, &mut tx).await?;

        // 5. Initialize playback state
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

    /// Transfer room ownership to another active member.
    ///
    /// This updates `rooms.created_by` and the corresponding creator/admin
    /// member roles in one transaction so ownership semantics stay consistent.
    pub async fn transfer_room_ownership(
        &self,
        room_id: RoomId,
        current_owner_id: UserId,
        new_owner_id: UserId,
    ) -> Result<Room> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.created_by != current_owner_id {
            return Err(Error::Authorization(
                "Only the current room owner can transfer ownership".to_string(),
            ));
        }

        if current_owner_id == new_owner_id {
            return Err(Error::InvalidInput(
                "Room ownership is already assigned to this user".to_string(),
            ));
        }

        let new_owner = self.user_service.get_user(&new_owner_id).await?;
        if !new_owner.status.is_active() {
            return Err(Error::Authorization(
                "New room owner must be an active user".to_string(),
            ));
        }

        let new_owner_member = self
            .member_repo
            .get(&room_id, &new_owner_id)
            .await?
            .ok_or_else(|| {
                Error::InvalidInput(
                    "New room owner must already be an active member of this room".to_string(),
                )
            })?;

        if !new_owner_member.status.is_active() {
            return Err(Error::InvalidInput(
                "New room owner must already be an active member of this room".to_string(),
            ));
        }

        let current_owner_member = self
            .member_repo
            .get(&room_id, &current_owner_id)
            .await?
            .ok_or_else(|| {
                Error::Internal(
                    "Current room owner is missing the required creator membership".to_string(),
                )
            })?;

        self.enforce_room_ownership_limit(&new_owner_id, Some(&room_id))
            .await?;

        let mut tx = self.pool.begin().await?;

        let updated_room = self
            .room_repo
            .transfer_ownership_with_executor(&room_id, &new_owner_id, &mut *tx)
            .await?;

        self.member_repo
            .update_role_with_executor(&room_id, &current_owner_id, RoomRole::Admin, &mut *tx)
            .await?;
        self.member_repo
            .update_role_with_executor(&room_id, &new_owner_id, RoomRole::Creator, &mut *tx)
            .await?;

        tx.commit().await?;

        self.invalidate_room_caches(&room_id).await;
        self.notify_room_settings_invalidation(&room_id).await;

        self.audit_log(
            &current_owner_id,
            "",
            AuditAction::RoomOwnershipTransferred,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({
                "operation": "transfer_ownership",
                "previous_owner_id": current_owner_id,
                "new_owner_id": new_owner_id,
                "previous_owner_role": format!("{:?}", current_owner_member.role),
                "new_owner_previous_role": format!("{:?}", new_owner_member.role),
            }),
        )
        .await;

        Ok(updated_room)
    }

    /// Create a room as a global admin.
    ///
    /// This bypasses user-facing room creation switches while preserving the
    /// rest of the room bootstrap lifecycle.
    pub async fn admin_create_room(
        &self,
        name: String,
        description: String,
        created_by: UserId,
        password: Option<String>,
        settings: Option<RoomSettings>,
    ) -> Result<(Room, RoomMember)> {
        let admin_user = self.user_service.get_user(&created_by).await?;
        if !admin_user.role.is_admin_or_above() {
            return Err(Error::Authorization(
                "Admin role required for this operation".to_string(),
            ));
        }

        self.do_create_room_with_policy(name, description, created_by, password, settings, false)
            .await
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

        self.ensure_room_creator_is_active_for_access(&ctx.room, &user_id)
            .await?;

        if ctx.room.is_banned {
            tracing::warn!(room_id = %room_id, user_id = %user_id, "Attempted to join banned room");
            return Err(Error::Authorization("Room is banned".to_string()));
        }

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

                if !self
                    .password_hasher
                    .verify_password(provided_password, hash)
                    .await?
                {
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
            let lock_key = format!("join_room:{room_id}:{user_id}");
            return crate::service::distributed_lock::with_coordination_lock(
                lock.as_ref(),
                &lock_key,
                10,
                || {
                    let password = password.clone();
                    async move {
 // Re-validate state under lock to catch changes that occurred
 // between the initial check and lock acquisition
                    let fresh_ctx = self
                        .room_repo
                        .get_join_context(&room_id, &user_id)
                        .await?
                        .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

                    self.ensure_room_creator_is_active_for_access(&fresh_ctx.room, &user_id)
                        .await?;

                    if fresh_ctx.room.is_banned {
                        return Err(Error::Authorization("Room is banned".to_string()));
                    }

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

                            if !self.password_hasher.verify_password(&provided_password, hash).await? {
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

                    self.do_join_room(fresh_ctx.room, fresh_ctx.settings, room_id, user_id)
                        .await
                    }
                },
            )
            .await;
        }

        // Single-replica path: no distributed lock, rely on DB-level constraints
        let room = ctx.room;
        self.do_join_room(room, ctx.settings, room_id, user_id)
            .await
    }

    /// Internal join implementation: adds member, lists members, notifies.
    ///
    /// Called after all validation (room active, not banned, password checked).
    /// When used with a distributed lock, the lock ensures atomicity of the
    /// re-validation + `add_member` sequence.
    ///
    /// **Idempotent**: if the user is already a member the call succeeds and
    /// returns the existing membership record. This handles the concurrent-join
    /// race where two simultaneous requests both pass validation and
    /// then one gets `AlreadyExists` from the repository.
    async fn do_join_room(
        &self,
        room: Room,
        settings: RoomSettings,
        room_id: RoomId,
        user_id: UserId,
    ) -> Result<(Room, RoomMember, Vec<crate::models::RoomMemberWithUser>)> {
        if let Some(existing_member) = self.member_repo.get(&room_id, &user_id).await? {
            tracing::debug!(
                room_id = %room_id,
                user_id = %user_id,
                "User is already an active member of the room"
            );
            let members = self.member_service.list_members(&room_id).await?;
            self.touch_room_activity(room_id).await;
            return Ok((room, existing_member, members));
        }

        if !settings.allow_auto_join.0 {
            return Err(Error::Authorization(
                "This room does not allow self-service joins. Ask a room manager to add you."
                    .to_string(),
            ));
        }

        if settings.require_approval.0 {
            let pending_member = self
                .create_or_get_pending_join_request(&room_id, &user_id, RoomRole::Member)
                .await?;
            tracing::info!(
                room_id = %room_id,
                user_id = %user_id,
                "Join request created and is awaiting approval"
            );
            return Ok((room, pending_member, Vec::new()));
        }

        // R-P2-1: Enforce room capacity limits by enabling max_members check.
        // AddMemberOptions::new() defaults to check_max_members=false; explicitly
        // enable it so the member_service reads max_members from RoomSettings and
        // rejects the join if the room is at capacity.
        let options = AddMemberOptions::new()
            .with_max_members(0)
            .with_initial_status(MemberStatus::Active); // 0 = read from RoomSettings
        let created_member = match self
            .member_service
            .add_member_with_options(room_id, user_id, RoomRole::Member, options)
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
            .notify_user_joined(&room_id, &user_id, &username);

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            username = %username,
            member_count = members.len(),
            "User joined room successfully"
        );

        // Touch room activity to prevent TTL expiry on active rooms
        self.touch_room_activity(room_id).await;

        Ok((room, created_member, members))
    }

    async fn create_or_get_pending_join_request(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        role: RoomRole,
    ) -> Result<RoomMember> {
        let existing_request_id = sqlx::query_scalar::<_, i64>(
            r"
            SELECT id
            FROM room_join_requests
            WHERE room_id = $1
              AND user_id = $2
              AND reviewed_at IS NULL
            LIMIT 1
            ",
        )
        .bind(room_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        if existing_request_id.is_none() {
            let insert_result = sqlx::query(
                r"
                INSERT INTO room_join_requests (
                    room_id, user_id, requested_role, status, requested_at
                )
                VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP)
                ",
            )
            .bind(room_id)
            .bind(user_id)
            .bind(role)
            .bind(ROOM_JOIN_REQUEST_PENDING)
            .execute(&self.pool)
            .await;

            if let Err(error) = insert_result {
                if !matches!(
                    &error,
                    sqlx::Error::Database(db_error)
                        if db_error.constraint()
                            == Some("idx_room_join_requests_pending_unique")
                ) {
                    return Err(Error::Database(error));
                }
            }
        }

        let pending = RoomMember::new(*room_id, *user_id, role);
        Ok(pending)
    }

    async fn load_pending_join_request_by_id_for_update(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        request_id: ReviewRequestId,
    ) -> Result<(UserId, RoomRole)> {
        let row = sqlx::query(
            r"
            SELECT user_id, COALESCE(requested_role, $3) AS requested_role
            FROM room_join_requests
            WHERE id = $1
              AND room_id = $2
              AND reviewed_at IS NULL
              AND status = $4
            FOR UPDATE
            ",
        )
        .bind(request_id)
        .bind(room_id)
        .bind(RoomRole::Member)
        .bind(ROOM_JOIN_REQUEST_PENDING)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| Error::NotFound("Pending join request not found".to_string()))?;

        let user_id: UserId = row.try_get("user_id")?;
        let requested_role: RoomRole = row.try_get("requested_role")?;
        Ok((user_id, requested_role))
    }

    async fn load_pending_join_request_by_id(
        &self,
        room_id: &RoomId,
        request_id: ReviewRequestId,
    ) -> Result<(UserId, RoomRole)> {
        let row = sqlx::query(
            r"
            SELECT user_id, COALESCE(requested_role, $3) AS requested_role
            FROM room_join_requests
            WHERE id = $1
              AND room_id = $2
              AND reviewed_at IS NULL
              AND status = $4
            ",
        )
        .bind(request_id)
        .bind(room_id)
        .bind(RoomRole::Member)
        .bind(ROOM_JOIN_REQUEST_PENDING)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::NotFound("Pending join request not found".to_string()))?;

        let user_id: UserId = row.try_get("user_id")?;
        let requested_role: RoomRole = row.try_get("requested_role")?;
        Ok((user_id, requested_role))
    }

    async fn resolve_pending_join_request_as_approved_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        reviewed_by: Option<&UserId>,
    ) -> Result<u64> {
        let result = sqlx::query(
            r"
            UPDATE room_join_requests
            SET status = $3,
                reviewed_at = CURRENT_TIMESTAMP,
                reviewed_by = $4
            WHERE room_id = $1
              AND user_id = $2
              AND reviewed_at IS NULL
              AND status = $5
            ",
        )
        .bind(room_id)
        .bind(user_id)
        .bind(ROOM_JOIN_REQUEST_APPROVED)
        .bind(reviewed_by.map(UserId::as_i64))
        .bind(ROOM_JOIN_REQUEST_PENDING)
        .execute(&mut **tx)
        .await?;
        Ok(result.rows_affected())
    }

    async fn active_member_add_options(&self, room_id: &RoomId) -> Result<AddMemberOptions> {
        let room_settings = self.room_settings_repo.get(room_id).await?;
        Ok(AddMemberOptions::new()
            .with_max_members(room_settings.max_members.0)
            .with_initial_status(MemberStatus::Active))
    }

    async fn add_active_member_and_resolve_join_review_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        target_user_id: &UserId,
        role: RoomRole,
        reviewed_by: Option<&UserId>,
        require_pending_review: bool,
    ) -> Result<RoomMember> {
        let mut member = RoomMember::new(*room_id, *target_user_id, role);
        member.status = MemberStatus::Active;
        let options = self.active_member_add_options(room_id).await?;
        let created = self
            .member_repo
            .add_with_options_tx(&member, &options, tx)
            .await?;
        let resolved = Self::resolve_pending_join_request_as_approved_tx(
            tx,
            room_id,
            target_user_id,
            reviewed_by,
        )
        .await?;
        if require_pending_review && resolved == 0 {
            return Err(Error::NotFound(
                "Pending join request not found".to_string(),
            ));
        }
        Ok(created)
    }

    async fn ensure_target_user_can_join(&self, target_user_id: &UserId) -> Result<()> {
        let target_user = self.user_service.get_user(target_user_id).await?;
        if target_user.is_banned {
            return Err(Error::Authorization(
                "Target user cannot be added while banned".to_string(),
            ));
        }
        if !target_user.status.can_join_room() {
            return Err(Error::Authorization(format!(
                "Target user cannot be added while account status is {}",
                target_user.status
            )));
        }
        Ok(())
    }

    fn validate_join_request_role(role: RoomRole) -> Result<RoomRole> {
        match role {
            RoomRole::Guest | RoomRole::Member => Ok(role),
            RoomRole::Admin | RoomRole::Creator => Err(Error::InvalidInput(
                "Join requests cannot grant elevated room roles".to_string(),
            )),
        }
    }

    async fn notify_membership_event_best_effort(
        &self,
        target_user_id: &UserId,
        room: &Room,
        event: String,
    ) {
        let Some(ref notif_service) = self.user_notification_service else {
            return;
        };

        if let Err(error) = notif_service
            .create_room_event(
                *target_user_id,
                room.id.to_string(),
                room.name.clone(),
                event,
            )
            .await
        {
            tracing::warn!(
                room_id = %room.id,
                user_id = %target_user_id,
                error = %error,
                "Failed to create room membership notification"
            );
        }
    }

    async fn notify_room_invitation_best_effort(
        &self,
        target_user_id: &UserId,
        room: &Room,
        actor_username: &str,
    ) {
        let Some(ref notif_service) = self.user_notification_service else {
            return;
        };

        if let Err(error) = notif_service
            .create_room_invitation(
                *target_user_id,
                room.id.to_string(),
                room.name.clone(),
                actor_username.to_string(),
            )
            .await
        {
            tracing::warn!(
                room_id = %room.id,
                user_id = %target_user_id,
                error = %error,
                "Failed to create room invitation notification"
            );
        }
    }

    /// Explicitly add a user as an active member.
    ///
    /// This is the manager-side admission path used when `allow_auto_join=false`.
    pub async fn add_member(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        target_user_id: UserId,
        role: RoomRole,
        notify: bool,
    ) -> Result<RoomMember> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.ensure_room_creator_is_active_for_access(&room, &actor_id)
            .await?;
        self.permission_service
            .check_permission_no_cache(&room_id, &actor_id, PermissionBits::ADD_MEMBER)
            .await?;

        self.ensure_target_user_can_join(&target_user_id).await?;

        let mut tx = self.pool.begin().await?;
        let created = self
            .add_active_member_and_resolve_join_review_tx(
                &mut tx,
                &room_id,
                &target_user_id,
                role,
                Some(&actor_id),
                false,
            )
            .await?;
        tx.commit().await?;
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        let actor_username = self
            .user_service
            .get_username(&actor_id)
            .await?
            .unwrap_or_else(|| actor_id.to_string());

        self.audit_log(
            &actor_id,
            &actor_username,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "new_status": "active",
                "role": role.to_string(),
                "source": "explicit_add_member",
            }),
        )
        .await;

        if notify {
            self.notify_room_invitation_best_effort(&target_user_id, &room, &actor_username)
                .await;
        }

        Ok(created)
    }

    /// Approve a specific pending join request and promote it to an active membership.
    pub async fn approve_join_request(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        request_id: ReviewRequestId,
    ) -> Result<RoomMember> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.ensure_room_creator_is_active_for_access(&room, &actor_id)
            .await?;

        self.permission_service
            .check_permission_no_cache(&room_id, &actor_id, PermissionBits::APPROVE_MEMBER)
            .await?;

        let (target_user_id, requested_role) = self
            .load_pending_join_request_by_id(&room_id, request_id)
            .await?;
        self.ensure_target_user_can_join(&target_user_id).await?;
        let role = Self::validate_join_request_role(requested_role)?;
        let mut tx = self.pool.begin().await?;
        let updated = self
            .add_active_member_and_resolve_join_review_tx(
                &mut tx,
                &room_id,
                &target_user_id,
                role,
                Some(&actor_id),
                true,
            )
            .await?;
        tx.commit().await?;

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        self.notify_membership_event_best_effort(
            &target_user_id,
            &room,
            "Your join request was approved".to_string(),
        )
        .await;

        Ok(updated)
    }

    /// Reject a specific pending join request without banning the user from the room.
    pub async fn reject_join_request(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        request_id: ReviewRequestId,
        reason: Option<&str>,
    ) -> Result<UserId> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.ensure_room_creator_is_active_for_access(&room, &actor_id)
            .await?;
        self.permission_service
            .check_permission_no_cache(&room_id, &actor_id, PermissionBits::APPROVE_MEMBER)
            .await?;

        let mut tx = self.pool.begin().await?;
        let (target_user_id, _) =
            Self::load_pending_join_request_by_id_for_update(&mut tx, &room_id, request_id).await?;
        sqlx::query(
            r"
            UPDATE room_join_requests
            SET status = $2,
                reviewed_at = CURRENT_TIMESTAMP,
                reviewed_by = $3,
                rejection_reason = $4
            WHERE id = $1
            ",
        )
        .bind(request_id)
        .bind(ROOM_JOIN_REQUEST_REJECTED)
        .bind(actor_id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        let actor_username = self
            .user_service
            .get_username(&actor_id)
            .await?
            .unwrap_or_else(|| actor_id.to_string());

        self.audit_log(
            &actor_id,
            &actor_username,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "request_id": request_id,
                "previous_review_status": "pending",
                "new_review_status": "rejected",
                "source": "reject_join_request",
                "reason": reason.unwrap_or_default(),
            }),
        )
        .await;

        let event = if let Some(reason) = reason.filter(|value| !value.is_empty()) {
            format!("Your join request was rejected: {reason}")
        } else {
            "Your join request was rejected".to_string()
        };
        self.notify_membership_event_best_effort(&target_user_id, &room, event)
            .await;

        Ok(target_user_id)
    }

    /// Administrative override: add a room member without requiring room-local membership.
    pub async fn admin_add_member(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        actor_username: &str,
        target_user_id: UserId,
        role: RoomRole,
        notify: bool,
    ) -> Result<RoomMember> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.ensure_target_user_can_join(&target_user_id).await?;

        let mut tx = self.pool.begin().await?;
        let created = self
            .add_active_member_and_resolve_join_review_tx(
                &mut tx,
                &room_id,
                &target_user_id,
                role,
                Some(&actor_id),
                false,
            )
            .await?;
        tx.commit().await?;
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        self.audit_log(
            &actor_id,
            actor_username,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "new_status": "active",
                "role": role.to_string(),
                "source": "admin_add_member",
            }),
        )
        .await;

        if notify {
            self.notify_room_invitation_best_effort(&target_user_id, &room, actor_username)
                .await;
        }

        Ok(created)
    }

    /// Administrative override: approve a specific pending join request.
    pub async fn admin_approve_join_request(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        reviewed_by: Option<&UserId>,
        actor_username: &str,
        request_id: ReviewRequestId,
    ) -> Result<RoomMember> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let (target_user_id, requested_role) = self
            .load_pending_join_request_by_id(&room_id, request_id)
            .await?;
        self.ensure_target_user_can_join(&target_user_id).await?;
        let role = Self::validate_join_request_role(requested_role)?;
        let mut tx = self.pool.begin().await?;
        let updated = self
            .add_active_member_and_resolve_join_review_tx(
                &mut tx,
                &room_id,
                &target_user_id,
                role,
                reviewed_by,
                true,
            )
            .await?;
        tx.commit().await?;

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        self.audit_log(
            &actor_id,
            actor_username,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "request_id": request_id,
                "previous_review_status": "pending",
                "new_review_status": "approved",
                "source": "admin_approve_join_request",
            }),
        )
        .await;

        self.notify_membership_event_best_effort(
            &target_user_id,
            &room,
            "Your join request was approved".to_string(),
        )
        .await;

        Ok(updated)
    }

    /// Administrative override: reject a specific pending join request without banning the user.
    pub async fn admin_reject_join_request(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        reviewed_by: Option<&UserId>,
        actor_username: &str,
        request_id: ReviewRequestId,
        reason: Option<&str>,
    ) -> Result<UserId> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let mut tx = self.pool.begin().await?;
        let (target_user_id, _) =
            Self::load_pending_join_request_by_id_for_update(&mut tx, &room_id, request_id).await?;
        sqlx::query(
            r"
            UPDATE room_join_requests
            SET status = $2,
                reviewed_at = CURRENT_TIMESTAMP,
                reviewed_by = $3,
                rejection_reason = $4
            WHERE id = $1
            ",
        )
        .bind(request_id)
        .bind(ROOM_JOIN_REQUEST_REJECTED)
        .bind(reviewed_by.map(UserId::as_i64))
        .bind(reason)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        self.audit_log(
            &actor_id,
            actor_username,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "request_id": request_id,
                "previous_review_status": "pending",
                "new_review_status": "rejected",
                "source": "admin_reject_join_request",
                "reason": reason.unwrap_or_default(),
            }),
        )
        .await;

        let event = if let Some(reason) = reason.filter(|value| !value.is_empty()) {
            format!("Your join request was rejected: {reason}")
        } else {
            "Your join request was rejected".to_string()
        };
        self.notify_membership_event_best_effort(&target_user_id, &room, event)
            .await;

        Ok(target_user_id)
    }

    /// Leave a room.
    ///
    /// Lifecycle rules:
    /// - the actor must currently be an active member of the room
    /// - the creator cannot leave and must transfer ownership or delete the room
    ///
    /// **Important for callers**: This method only removes the membership record
    /// and sends an in-app notification. It does NOT disconnect active room
    /// connections or fan out cluster disconnect events.
    pub async fn leave_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "User leaving room");

        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let membership = self
            .member_repo
            .get(&room_id, &user_id)
            .await?
            .ok_or_else(|| Error::Authorization("You are not a member of this room".to_string()))?;

        if room.created_by == user_id {
            return Err(Error::Authorization(
                "Room creator cannot leave the room. Transfer ownership or delete the room instead."
                    .to_string(),
            ));
        }

        if membership.role == RoomRole::Creator {
            return Err(Error::Authorization(
                "Room creator cannot leave the room. Transfer ownership or delete the room instead."
                    .to_string(),
            ));
        }

        self.member_service.remove_member(room_id, user_id).await?;

        // Notify room members with username
        let username = self
            .user_service
            .get_username(&user_id)
            .await?
            .unwrap_or_else(|| "Unknown".to_string());
        let _ = self
            .notification_service
            .notify_user_left(&room_id, &user_id, &username);

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

    /// Soft-delete a room.
    ///
    /// Sets the `deleted_at` timestamp on the room row. The room and its related
    /// data (members, playlists, media, chat messages, settings, playback state)
    /// remain in the database until the periodic `CleanupService` permanently
    /// purges rows whose `deleted_at` exceeds the configured retention period
    /// (default: 90 days). Permanent purge uses the same explicit cleanup path
    /// as normal room deletion before removing the room row itself.
    ///
    /// **Soft-delete lifecycle (optimized):**
    /// 1. This method sets `rooms.deleted_at = NOW()` (room becomes invisible to queries)
    /// 2. IMMEDIATELY deletes non-critical related data to free storage:
    /// - playlists and nested media via explicit subtree cleanup
    /// - `room_members`
    /// - `room_settings`
    /// - `room_playback_state`
    /// - `chat_messages`
    /// 3. Preserves only the room row (for audit) and `audit_logs` entries
    /// 4. `CleanupService::purge_soft_deleted_rooms()` eventually purges the room row
    ///    after `room_soft_delete_retention_days` (default: 90 days)
    ///
    /// Authorization model:
    /// - room creator can delete their own room
    /// - global admin/root can delete any room
    /// - room-scoped admins cannot delete a room unless they are also a global admin
    pub async fn delete_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "Soft-deleting room");

        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found or already deleted".to_string()))?;

        let actor = self.user_service.get_user(&user_id).await?;
        let is_global_admin = actor.role.is_admin_or_above();
        let is_creator = room.created_by == user_id;

        if !is_creator && !is_global_admin {
            if self.member_repo.get(&room_id, &user_id).await?.is_some() {
                return Err(Error::Authorization(
                    "Only the room creator or a global admin can delete this room".to_string(),
                ));
            }

            return Err(Error::Authorization(
                "You are not a member of this room".to_string(),
            ));
        }

        let mut tx = self.pool.begin().await?;
        let impact = soft_delete_room_and_cleanup_in_tx(&mut tx, &room_id).await?;

        // Commit transaction - all or nothing
        tx.commit().await?;

        self.invalidate_room_caches(&room_id).await;

        // Notify after commit so notifications are only sent for successful deletions
        let _ = self.notification_service.notify_room_deleted(&room_id);

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            is_creator,
            is_global_admin,
            playlists_deleted = impact.deleted_playlist_ids.len(),
            media_deleted = impact.deleted_media_ids.len(),
            members_deleted = impact.members_deleted,
            settings_deleted = impact.settings_deleted,
            chat_deleted = impact.chat_deleted,
            "Room soft-deleted with immediate cleanup of related data (room row preserved for audit, will be purged by CleanupService after retention period)"
        );

        // Track room metrics
        crate::metrics::http::ROOMS_ACTIVE.dec();

        // Audit log (preserved - not deleted with room data)
        self.audit_log(
            &user_id,
            "",
            AuditAction::RoomDeleted,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({
                "reason": "Room deleted by user",
                "playlists_deleted": impact.deleted_playlist_ids.len(),
                "media_deleted": impact.deleted_media_ids.len(),
                "members_deleted": impact.members_deleted,
                "settings_deleted": impact.settings_deleted,
                "chat_deleted": impact.chat_deleted,
            }),
        )
        .await;

        Ok(())
    }

    /// Approve a pending room creation request and create the room.
    ///
    /// This is an admin-only operation for rooms created when `create_room_need_review=true`.
    /// After approval, the room becomes visible and usable by its creator.
    ///
    /// # Errors
    /// - `Error::NotFound` if the pending request does not exist
    /// - `Error::Authorization` if caller is not a global admin
    pub async fn approve_pending_room(
        &self,
        request_id: RoomId,
        admin_id: Option<&UserId>,
    ) -> Result<Room> {
        tracing::info!(request_id = %request_id, ?admin_id, "Approving room creation request");

        if let Some(admin_id) = admin_id {
            let admin = self.user_service.get_user(admin_id).await?;
            if !admin.role.is_admin_or_above() {
                return Err(Error::Authorization(
                    "Only admins can approve rooms".to_string(),
                ));
            }
        }

        let mut tx = self.pool.begin().await?;
        let request = Self::load_pending_room_creation_request_for_update(&request_id, &mut tx)
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "Pending room creation request {request_id} not found"
                ))
            })?;

        self.enforce_room_ownership_limit(&request.requested_by, None)
            .await?;

        let room = Room::new_with_description(
            request.name.clone(),
            request.description.clone(),
            request.requested_by,
        );
        let updated = self.room_repo.create_with_executor(&room, &mut *tx).await?;

        if let Some(password_hash) = request.password_hash.as_deref() {
            self.room_settings_repo
                .set_with_executor(&updated.id, "password", password_hash, &mut *tx)
                .await?;
        }
        self.room_settings_repo
            .set_settings_with_executor(&updated.id, &request.settings, &mut *tx)
            .await?;

        let member = RoomMember::new(updated.id, request.requested_by, RoomRole::Creator);
        self.member_repo.add_with_executor(&member, &mut tx).await?;
        self.playback_repo
            .create_or_get_with_executor(&updated.id, &mut tx)
            .await?;

        sqlx::query(
            r"
            UPDATE room_creation_requests
            SET status = $2, reviewed_at = CURRENT_TIMESTAMP, reviewed_by = $3
            WHERE id = $1
            ",
        )
        .bind(request_id)
        .bind(ReviewStatus::Approved)
        .bind(admin_id.map(UserId::as_i64))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        crate::metrics::http::ROOMS_ACTIVE.inc();

        self.notify_room_invalidation(&updated.id).await;
        self.permission_service
            .invalidate_room_cache(&updated.id)
            .await;

        // Audit log
        self.audit_log(
            admin_id.unwrap_or(&request.requested_by),
            "",
            AuditAction::RoomApproved,
            AuditTargetType::Room,
            Some(updated.id.to_string()),
            serde_json::json!({
                "request_id": request_id.to_string(),
                "previous_review_status": "pending",
                "new_review_status": "approved",
            }),
        )
        .await;

        tracing::info!(request_id = %request_id, room_id = %updated.id, ?admin_id, "Room approved and activated");

        Ok(updated)
    }

    /// Reject a pending room creation request.
    ///
    /// This is an admin-only operation for rooms created when `create_room_need_review=true`.
    /// Rejected requests are preserved for review/audit; no room row is created.
    ///
    /// # Errors
    /// - `Error::NotFound` if room doesn't exist
    /// - `Error::NotFound` if the pending request does not exist
    /// - Permission error if caller is not a global admin
    pub async fn reject_room(
        &self,
        room_id: RoomId,
        admin_id: Option<&UserId>,
        reason: Option<String>,
    ) -> Result<Room> {
        tracing::info!(room_id = %room_id, ?admin_id, "Rejecting pending room");

        if let Some(admin_id) = admin_id {
            let admin = self.user_service.get_user(admin_id).await?;
            if !admin.role.is_admin_or_above() {
                return Err(Error::Authorization(
                    "Only admins can reject rooms".to_string(),
                ));
            }
        }

        let mut tx = self.pool.begin().await?;
        let request = Self::load_pending_room_creation_request_for_update(&room_id, &mut tx)
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!("Pending room creation request {room_id} not found"))
            })?;

        sqlx::query(
            r"
            UPDATE room_creation_requests
            SET status = $2, reviewed_at = CURRENT_TIMESTAMP, reviewed_by = $3, rejection_reason = $4
            WHERE id = $1
            ",
        )
        .bind(room_id)
        .bind(ReviewStatus::Rejected)
        .bind(admin_id.map(UserId::as_i64))
        .bind(reason.as_deref())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        let mut updated =
            Room::new_with_description(request.name, request.description, request.requested_by);
        updated.id = request.id;

        // Audit log
        self.audit_log(
            admin_id.unwrap_or(&updated.created_by),
            "",
            AuditAction::RoomRejected,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({
                "previous_review_status": "pending",
                "new_review_status": "rejected",
                "reason": reason,
            }),
        )
        .await;

        tracing::info!(room_id = %room_id, ?admin_id, "Room rejected");

        Ok(updated)
    }

    /// List pending room creation requests (admin only).
    ///
    /// Returns room-shaped DTOs synthesized from pending request records.
    pub async fn list_pending_rooms(
        &self,
        admin_id: UserId,
        pagination: PageParams,
    ) -> Result<(Vec<Room>, i64)> {
        pagination.validate()?;

        // Verify admin permission
        let admin = self.user_service.get_user(&admin_id).await?;

        if !admin.role.is_admin_or_above() {
            return Err(Error::Authorization(
                "Only admins can list pending rooms".to_string(),
            ));
        }

        let total = sqlx::query_scalar::<_, i64>(
            r"
            SELECT COUNT(*)
            FROM room_creation_requests
            WHERE reviewed_at IS NULL AND status = $1
            ",
        )
        .bind(ReviewStatus::Pending)
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query(
            r"
            SELECT id, requested_by, name, description, requested_at
            FROM room_creation_requests
            WHERE reviewed_at IS NULL AND status = $1
            ORDER BY requested_at DESC, id DESC
            LIMIT $2 OFFSET $3
            ",
        )
        .bind(ReviewStatus::Pending)
        .bind(i64::try_from(pagination.limit()).unwrap_or(i64::MAX))
        .bind(i64::try_from(pagination.offset()).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;

        let rooms = rows
            .into_iter()
            .map(|row| {
                let requested_at = row.try_get("requested_at")?;
                Ok(Room {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    description: row.try_get("description")?,
                    created_by: row.try_get("requested_by")?,
                    status: RoomStatus::Active,
                    is_banned: false,
                    closed_at: None,
                    created_at: requested_at,
                    updated_at: requested_at,
                    deleted_at: None,
                    version: 0,
                    last_activity_at: requested_at,
                })
            })
            .collect::<std::result::Result<Vec<_>, sqlx::Error>>()?;

        Ok((rooms, total))
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
    ) -> Result<crate::service::room_settings::RoomSettingsSnapshot> {
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::SET_ROOM_SETTINGS)
            .await?;

        // Validate permission escalation
        settings.validate_permissions()?;

        // Verify room exists
        self.room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        // CAS write with retry and total timeout
        let room_id_clone = room_id;
        let settings_clone = settings.clone();
        let room_settings_repo = self.room_settings_repo.clone();
        let audit_service = self.audit_service.clone();

        let (previous_settings, updated_settings, updated_version) =
            super::optimistic_retry::retry_with_optimistic_lock_timeout(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                std::time::Duration::from_secs(Self::SETTINGS_UPDATE_TIMEOUT_SECS),
                "Settings update failed after maximum retry attempts",
                || {
                    let room_id = room_id_clone;
                    let settings = settings_clone.clone();
                    let room_settings_repo = room_settings_repo.clone();
                    async move {
                        let (current, version) =
                            room_settings_repo.get_with_version(&room_id).await?;
                        let new_version = room_settings_repo
                            .set_settings_with_version(&room_id, &settings, version)
                            .await?;
                        Ok((current, settings, new_version))
                    }
                },
            )
            .await?;

        let snapshot = self
            .finalize_room_settings_update(
                &room_id,
                &previous_settings,
                &updated_settings,
                updated_version,
                Some(&user_id),
                "",
            )
            .await?;

        if let Some(ref audit) = audit_service {
            let settings_json = serde_json::to_value(&snapshot.settings)
                .internal_with_err("Failed to serialize settings")?;
            let _ = audit
                .log(
                    user_id.to_string(),
                    user_id.to_string(),
                    AuditAction::RoomSettingsUpdated,
                    AuditTargetType::Room,
                    Some(room_id.to_string()),
                    settings_json,
                    None,
                    None,
                )
                .await;
        }

        Ok(snapshot)
    }

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
        let settings = self.room_settings_service.get(room_id).await?;
        Ok((room, settings))
    }

    /// Get room settings
    pub async fn get_room_settings(&self, room_id: &RoomId) -> Result<RoomSettings> {
        self.room_settings_service.get(room_id).await
    }

    /// Get room settings together with the optimistic-lock version.
    pub async fn get_room_settings_with_version(
        &self,
        room_id: &RoomId,
    ) -> Result<(RoomSettings, i64)> {
        let snapshot = self.room_settings_service.get_with_version(room_id).await?;
        Ok((snapshot.settings, snapshot.version))
    }

    /// Get the current room-wide guest token version.
    ///
    /// Anonymous guest JWTs embed this version at issuance time. Bumping it
    /// revokes every previously issued guest token for the room.
    pub async fn get_room_guest_version(&self, room_id: &RoomId) -> Result<i64> {
        let key = self
            .user_service
            .key_builder()
            .room_guest_version(&room_id.to_string());
        Ok(self
            .user_service
            .token_blacklist_store()
            .get_family_revoked_at_checked(&key)
            .await?
            .unwrap_or(0))
    }

    async fn resolve_actor_username(&self, user_id: &UserId) -> String {
        self.user_service
            .get_user(user_id)
            .await
            .map(|user| user.username)
            .unwrap_or_default()
    }

    /// Get settings for multiple rooms in a single query (avoids N+1)
    pub async fn get_room_settings_batch(
        &self,
        room_ids: &[RoomId],
    ) -> Result<std::collections::HashMap<RoomId, RoomSettings>> {
        self.room_settings_repo.get_batch(room_ids).await
    }

    /// Set room settings (replace entire settings object) with optimistic locking.
    pub async fn set_room_settings(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
    ) -> Result<crate::service::room_settings::RoomSettingsSnapshot> {
        settings.validate_permissions()?;

        let (previous_settings, updated_settings, updated_version) =
            super::optimistic_retry::retry_with_optimistic_lock(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                "Settings update failed after maximum retry attempts",
                || async {
                    let (current, version) =
                        self.room_settings_repo.get_with_version(room_id).await?;
                    let new_version = self
                        .room_settings_repo
                        .set_settings_with_version(room_id, settings, version)
                        .await?;
                    Ok((current, settings.clone(), new_version))
                },
            )
            .await?;
        self.finalize_room_settings_update(
            room_id,
            &previous_settings,
            &updated_settings,
            updated_version,
            None,
            "",
        )
        .await
    }

    /// Update single room setting by key (requires `SET_ROOM_SETTINGS` permission)
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
            .check_permission(room_id, user_id, PermissionBits::SET_ROOM_SETTINGS)
            .await?;

        // 2. Validate via registry (type parsing + value constraints from macro validators)
        RoomSettingsRegistry::validate_setting(key, value)?;

        // 3. CAS update with retry
        let mut previous_settings = None;
        let mut final_settings = None;
        let mut final_version = None;
        for attempt in 0..Self::MAX_RETRIES {
            let (mut settings, version) = self.room_settings_repo.get_with_version(room_id).await?;
            let current = settings.clone();
            settings.set_by_key(key, value)?;
            settings.validate_permissions()?;

            match self
                .room_settings_repo
                .set_settings_with_version(room_id, &settings, version)
                .await
            {
                Ok(new_version) => {
                    previous_settings = Some(current);
                    final_settings = Some(settings);
                    final_version = Some(new_version);
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

        let previous_settings = previous_settings.ok_or_else(|| {
            Error::Internal("Settings update failed after maximum retry attempts".to_string())
        })?;
        let settings = final_settings.ok_or_else(|| {
            Error::Internal("Settings update failed after maximum retry attempts".to_string())
        })?;
        let version = final_version.ok_or_else(|| {
            Error::Internal("Settings update failed after maximum retry attempts".to_string())
        })?;

        let snapshot = self
            .finalize_room_settings_update(
                room_id,
                &previous_settings,
                &settings,
                version,
                Some(user_id),
                "",
            )
            .await?;

        serde_json::to_string(&snapshot.settings).internal_with_err("Failed to serialize settings")
    }

    async fn finalize_room_settings_update(
        &self,
        room_id: &RoomId,
        previous_settings: &RoomSettings,
        updated_settings: &RoomSettings,
        version: i64,
        actor_user_id: Option<&UserId>,
        actor_username: &str,
    ) -> Result<crate::service::room_settings::RoomSettingsSnapshot> {
        self.run_post_apply_hooks_for_settings_update(room_id, previous_settings, updated_settings)
            .await;
        self.room_settings_service.invalidate_local(room_id).await;
        self.permission_service.invalidate_room_cache(room_id).await;
        self.notify_room_invalidation(room_id).await;
        self.notify_room_settings_invalidation(room_id).await;

        let settings_json = serde_json::to_value(updated_settings)
            .internal_with_err("Failed to serialize settings")?;
        let _ = self.notification_service.notify_settings_updated(
            room_id,
            actor_user_id,
            actor_username,
            settings_json.clone(),
            version,
        );

        Ok(crate::service::room_settings::RoomSettingsSnapshot {
            settings: updated_settings.clone(),
            version,
        })
    }

    async fn run_post_apply_hooks_for_settings_update(
        &self,
        room_id: &RoomId,
        previous_settings: &RoomSettings,
        updated_settings: &RoomSettings,
    ) {
        use crate::service::notification::GuestKickReason;

        let guest_kick_reason =
            if previous_settings.allow_guest_join.0 && !updated_settings.allow_guest_join.0 {
                Some(GuestKickReason::RoomGuestModeDisabled)
            } else if !previous_settings.require_password.0 && updated_settings.require_password.0 {
                Some(GuestKickReason::RoomPasswordAdded)
            } else {
                None
            };

        if let Some(reason) = guest_kick_reason {
            if let Err(e) = self.revoke_all_guest_access(room_id, reason).await {
                tracing::warn!(
                    room_id = %room_id,
                    error = %e,
                    "Failed to revoke guest access after settings change"
                );
            }
        }
    }

    /// Reset room settings to default values with optimistic locking.
    pub async fn reset_room_settings(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<crate::service::room_settings::RoomSettingsSnapshot> {
        self.permission_service
            .check_permission(room_id, user_id, PermissionBits::SET_ROOM_SETTINGS)
            .await?;

        let default_settings = RoomSettings::default();

        let (previous_settings, updated_settings, updated_version) =
            super::optimistic_retry::retry_with_optimistic_lock(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                "Settings reset failed after maximum retry attempts",
                || async {
                    let (current, version) =
                        self.room_settings_repo.get_with_version(room_id).await?;
                    let new_version = self
                        .room_settings_repo
                        .set_settings_with_version(room_id, &default_settings, version)
                        .await?;
                    Ok((current, default_settings.clone(), new_version))
                },
            )
            .await?;
        self.finalize_room_settings_update(
            room_id,
            &previous_settings,
            &updated_settings,
            updated_version,
            Some(user_id),
            "",
        )
        .await
    }

    /// Check room password
    ///
    /// Returns:
    /// - `Ok(true)` if the password matches the stored hash
    /// - `Ok(false)` if the password does not match
    /// - `Err(InvalidInput("Room has no password set"))` if the room has no password
    pub async fn check_room_password(&self, room_id: &RoomId, password: &str) -> Result<bool> {
        let password_hash = self.room_settings_repo.get_password_hash(room_id).await?;

        match password_hash {
            Some(stored) => self
                .password_hasher
                .verify_password(password, &stored)
                .await
                .internal_with_err("Password verification failed"),
            None => Err(Error::InvalidInput("Room has no password set".to_string())),
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
        self.check_room_password_with_rate_limit_with_control(room_id, password, client_ip, None)
            .await
    }

    pub async fn check_room_password_with_rate_limit_with_control(
        &self,
        room_id: &RoomId,
        password: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<bool> {
        // Build the rate limit key: room_id + client_ip (or just room_id if no IP)
        let rate_limit_key = match client_ip {
            Some(ip) => format!("{room_id}:{ip}"),
            None => room_id.to_string(),
        };

        // Check rate limit if brute-force service is configured
        if let Some(ref brute_force) = self.brute_force_service {
            brute_force
                .check_allowed_with_control(&rate_limit_key, client_ip, control)
                .await?;
        }

        // Verify the password
        let password_hash = self.room_settings_repo.get_password_hash(room_id).await?;
        let is_valid = match password_hash {
            Some(stored) => self
                .password_hasher
                .verify_password(password, &stored)
                .await
                .internal_with_err("Password verification failed"),
            None => Ok(false),
        }?;

        // Handle success/failure tracking
        if let Some(ref brute_force) = self.brute_force_service {
            if is_valid {
                // Reset failure counter on successful verification
                if let Err(e) = brute_force
                    .reset_with_control(&rate_limit_key, control)
                    .await
                {
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
                                room_id.to_string(),
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
                    .record_failure_with_control(&rate_limit_key, client_ip, control)
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
            let rate_limit_key = format!("{room_id}:{client_ip}");
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
        self.update_room_password_as(room_id, None, "", password_hash)
            .await
            .map(|_| ())
    }

    pub async fn update_room_password_as(
        &self,
        room_id: &RoomId,
        actor_user_id: Option<&UserId>,
        actor_username: &str,
        password_hash: Option<String>,
    ) -> Result<crate::service::room_settings::RoomSettingsSnapshot> {
        let password_was_set = password_hash.is_some();
        self.do_set_password_hash(room_id, password_hash).await?;
        self.room_settings_service.invalidate_local(room_id).await;

        if password_was_set {
            if let Err(e) = self
                .revoke_all_guest_access(
                    room_id,
                    crate::service::notification::GuestKickReason::RoomPasswordAdded,
                )
                .await
            {
                tracing::warn!(
                    room_id = %room_id,
                    error = %e,
                    "Failed to revoke guest access after password was added"
                );
            }
        }

        // Invalidate room cache across all replicas after membership side effects complete
        self.notify_room_invalidation(room_id).await;
        self.notify_room_settings_invalidation(room_id).await;
        self.emit_room_settings_snapshot_after_password_update(
            room_id,
            actor_user_id,
            actor_username,
        )
        .await
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
                .bind(room_id)
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
                .bind(room_id)
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
    /// Requires `SET_ROOM_SETTINGS` permission.
    ///
    /// # Errors
    ///
    /// - `Error::InvalidInput` - Description exceeds 500 characters
    /// - `Error::Authentication` - User lacks `SET_ROOM_SETTINGS` permission
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
            .check_permission(room_id, user_id, PermissionBits::SET_ROOM_SETTINGS)
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
        query.pagination.validate()?;
        self.room_repo.list(query).await
    }

    pub async fn list_accessible_rooms(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        query.pagination.validate()?;
        self.room_repo.list_accessible(query).await
    }

    /// List rooms related to a user either by ownership or active membership.
    pub async fn list_related_rooms_for_user(
        &self,
        user_id: &UserId,
        query: &RoomListQuery,
    ) -> Result<(Vec<Room>, i64)> {
        query.pagination.validate()?;
        self.room_repo.list_related_to_user(user_id, query).await
    }

    /// List all rooms with member count (optimized, single query)
    pub async fn list_rooms_with_count(
        &self,
        query: &RoomListQuery,
    ) -> Result<(Vec<RoomWithCount>, i64)> {
        query.pagination.validate()?;
        self.room_repo.list_with_count(query).await
    }

    /// List rooms created by a specific user
    pub async fn list_rooms_by_creator(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<Room>, i64)> {
        pagination.validate()?;
        self.room_repo.list_by_creator(creator_id, pagination).await
    }

    /// List rooms created by a specific user with member count (optimized)
    pub async fn list_rooms_by_creator_with_count(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomWithCount>, i64)> {
        pagination.validate()?;
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
        pagination.validate()?;
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
        pagination.validate()?;
        self.member_service
            .list_user_rooms_with_details(user_id, pagination)
            .await
    }

    /// List rooms where a user participates, with filtering, sorting and pagination.
    pub async fn list_joined_rooms_with_query(
        &self,
        user_id: &UserId,
        query: &crate::models::MyRoomListQuery,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        query.pagination.validate()?;
        self.member_service
            .list_user_rooms_with_details_query(user_id, query)
            .await
    }

    pub async fn list_accessible_joined_rooms_with_query(
        &self,
        user_id: &UserId,
        query: &crate::models::MyRoomListQuery,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        query.pagination.validate()?;
        self.member_repo
            .list_accessible_by_user_with_query(user_id, query)
            .await
    }

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

    /// Update the room's `last_activity_at` timestamp.
    ///
    /// Call this after chat messages, playback state changes, or member
    /// joins/leaves to prevent active rooms from being expired by the TTL
    /// cleanup.
    pub async fn touch_room_activity(&self, room_id: RoomId) {
        if let Err(e) = self.room_repo.touch_activity(&room_id).await {
            tracing::debug!(error = %e, room_id = %room_id, "Failed to touch room activity");
        }
    }

    /// Get room members with user info
    pub async fn get_room_members(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<crate::models::RoomMemberWithUser>> {
        self.member_service.list_members(room_id).await
    }

    /// Get room members with database-level pagination
    ///
    /// Uses `COUNT(*) OVER()` for atomic count + fetch.
    /// Returns (members, total_count) tuple.
    ///
    /// # Performance
    ///
    /// This method should be preferred over `get_room_members` for admin endpoints
    /// where rooms may have large numbers of members.
    pub async fn get_room_members_paginated(
        &self,
        room_id: &RoomId,
        pagination: crate::models::PageParams,
    ) -> Result<(Vec<crate::models::RoomMemberWithUser>, i64)> {
        self.member_service
            .list_members_paginated(room_id, pagination)
            .await
    }

    pub async fn get_room_members_query(
        &self,
        room_id: &RoomId,
        query: crate::models::RoomMemberListQuery,
    ) -> Result<(Vec<crate::models::RoomMemberWithUser>, i64)> {
        self.member_service.list_members_query(room_id, query).await
    }

    /// Get member count for a room
    pub async fn get_member_count(&self, room_id: &RoomId) -> Result<i32> {
        self.member_service.count_members(room_id).await
    }

    /// Get member counts for multiple rooms in a single query.
    pub async fn get_member_count_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<std::collections::HashMap<RoomId, i32>> {
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
        let room = self.get_room(room_id).await?;
        self.check_membership_with_room(&room, user_id).await
    }

    pub async fn check_membership_with_room(&self, room: &Room, user_id: &UserId) -> Result<()> {
        self.ensure_room_creator_is_active_for_access(room, user_id)
            .await?;

        if self.member_service.is_member(&room.id, user_id).await? {
            Ok(())
        } else {
            Err(Error::Authorization(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ))
        }
    }

    /// Remove a media entry from the room in one transactional operation.
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
        self.delete_entries(
            room_id,
            user_id,
            DeleteEntriesRequest {
                playlist_ids: Vec::new(),
                media_ids: vec![media_id],
                force: false,
            },
        )
        .await?;
        Ok(())
    }

    /// Delete a mixed set of playlists and media in one transaction.
    pub async fn delete_entries(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: DeleteEntriesRequest,
    ) -> Result<DeleteEntriesResult> {
        let (result, ()) = self
            .delete_entries_with_precommit(room_id, user_id, request, |_| async { Ok(()) })
            .await?;
        Ok(result)
    }

    pub async fn delete_entries_with_precommit<T, F, Fut>(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: DeleteEntriesRequest,
        precommit: F,
    ) -> Result<(DeleteEntriesResult, T)>
    where
        F: FnOnce(DeleteEntriesPlan) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let playlist_ids = dedup_ids(request.playlist_ids);
        let media_ids = dedup_ids(request.media_ids);
        let force = request.force;
        let total_targets = playlist_ids.len() + media_ids.len();

        if total_targets == 0 {
            return Ok((
                DeleteEntriesResult::default(),
                precommit(DeleteEntriesPlan::default()).await?,
            ));
        }

        if total_targets > MAX_DELETE_TARGETS {
            return Err(Error::InvalidInput(format!(
                "Delete batch size exceeds maximum of {MAX_DELETE_TARGETS}"
            )));
        }

        let mut tx = self.pool.begin().await?;

        let playlists = self
            .playlist_repo
            .get_by_ids_with_executor(&playlist_ids, &mut *tx)
            .await?;
        if playlists.len() != playlist_ids.len() {
            return Err(Error::NotFound(
                "One or more playlists not found".to_string(),
            ));
        }

        for playlist in &playlists {
            if playlist.room_id != room_id {
                return Err(Error::Authorization(
                    "Playlist does not belong to this room".to_string(),
                ));
            }
        }

        if !playlist_ids.is_empty()
            && !has_room_permission_in_tx(
                &mut tx,
                &room_id,
                &user_id,
                PermissionBits::REORDER_PLAYLIST,
            )
            .await?
        {
            return Err(Error::Authorization(
                synctv_common::messages::PERMISSION_DENIED.to_string(),
            ));
        }

        let media_items = self
            .media_repo
            .get_by_ids_with_executor(&media_ids, &mut *tx)
            .await?;
        if media_items.len() != media_ids.len() {
            return Err(Error::NotFound(
                "One or more media items not found".to_string(),
            ));
        }

        let mut has_owned_media = false;
        let mut has_foreign_media = false;
        for media in &media_items {
            if media.room_id != room_id {
                return Err(Error::Authorization(
                    "Media does not belong to this room".to_string(),
                ));
            }
            if media.creator_id.as_ref() == Some(&user_id) {
                has_owned_media = true;
            } else {
                has_foreign_media = true;
            }
        }

        if has_owned_media
            && !has_room_permission_in_tx(
                &mut tx,
                &room_id,
                &user_id,
                PermissionBits::DELETE_MEDIA_SELF,
            )
            .await?
        {
            return Err(Error::Authorization(
                synctv_common::messages::PERMISSION_DENIED.to_string(),
            ));
        }
        if has_foreign_media
            && !has_room_permission_in_tx(
                &mut tx,
                &room_id,
                &user_id,
                PermissionBits::DELETE_MEDIA_ANY,
            )
            .await?
        {
            return Err(Error::Authorization(
                synctv_common::messages::PERMISSION_DENIED.to_string(),
            ));
        }

        let impact =
            plan_delete_entries_in_room_in_tx(&mut tx, &room_id, &playlist_ids, &media_ids, force)
                .await?;
        let precommit_result = precommit(DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
        })
        .await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &impact).await?;

        tx.commit().await?;

        if impact.playback_reset {
            let state = self.playback_service.get_state(&room_id).await?;
            self.playback_service
                .broadcast_playback_reset_after_force_delete(state)
                .await;
        }

        let should_notify_playlist_delete = !impact.deleted_playlist_ids.is_empty();
        if !impact.deleted_media_ids.is_empty() || should_notify_playlist_delete {
            let actor_username = self.resolve_actor_username(&user_id).await;
            for media_id in &impact.deleted_media_ids {
                if let Err(error) = self.notification_service.notify_media_removed(
                    &room_id,
                    Some(&user_id),
                    &actor_username,
                    *media_id,
                ) {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        media_id = %media_id,
                        "Failed to broadcast media removed event"
                    );
                }
            }
            for playlist_id in &impact.deleted_playlist_ids {
                if let Err(error) = self.notification_service.notify_playlist_deleted(
                    &room_id,
                    Some(&user_id),
                    &actor_username,
                    *playlist_id,
                ) {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        playlist_id = %playlist_id,
                        "Failed to broadcast playlist deleted event"
                    );
                }
            }
        }

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            deleted_playlists = impact.deleted_playlist_ids.len(),
            deleted_media = impact.deleted_media_ids.len(),
            "Entries deleted"
        );

        Ok((delete_entries_result_from_impact(impact), precommit_result))
    }

    /// Delete a mixed set of media and playlists as a global admin.
    pub async fn admin_delete_entries(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        request: DeleteEntriesRequest,
    ) -> Result<DeleteEntriesResult> {
        let actor = self.load_authorized_admin_actor(&admin_user_id).await?;
        self.admin_delete_entries_as(room_id, &actor, request).await
    }

    pub async fn admin_delete_entries_as_with_precommit<T, F, Fut>(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        request: DeleteEntriesRequest,
        precommit: F,
    ) -> Result<(DeleteEntriesResult, T)>
    where
        F: FnOnce(DeleteEntriesPlan) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let admin_user_id = *actor.user_id();

        let playlist_ids = dedup_ids(request.playlist_ids);
        let media_ids = dedup_ids(request.media_ids);
        let force = request.force;
        let total_targets = playlist_ids.len() + media_ids.len();

        if total_targets == 0 {
            return Ok((
                DeleteEntriesResult::default(),
                precommit(DeleteEntriesPlan::default()).await?,
            ));
        }

        if total_targets > MAX_DELETE_TARGETS {
            return Err(Error::InvalidInput(format!(
                "Delete batch size exceeds maximum of {MAX_DELETE_TARGETS}"
            )));
        }

        let mut tx = self.pool.begin().await?;

        let playlists = self
            .playlist_repo
            .get_by_ids_with_executor(&playlist_ids, &mut *tx)
            .await?;
        if playlists.len() != playlist_ids.len() {
            return Err(Error::NotFound(
                "One or more playlists not found".to_string(),
            ));
        }

        for playlist in &playlists {
            if playlist.room_id != room_id {
                return Err(Error::Authorization(
                    "Playlist does not belong to this room".to_string(),
                ));
            }
        }

        let media_items = self
            .media_repo
            .get_by_ids_with_executor(&media_ids, &mut *tx)
            .await?;
        if media_items.len() != media_ids.len() {
            return Err(Error::NotFound(
                "One or more media items not found".to_string(),
            ));
        }

        for media in &media_items {
            if media.room_id != room_id {
                return Err(Error::Authorization(
                    "Media does not belong to this room".to_string(),
                ));
            }
        }

        let impact =
            plan_delete_entries_in_room_in_tx(&mut tx, &room_id, &playlist_ids, &media_ids, force)
                .await?;
        let precommit_result = precommit(DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
        })
        .await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &impact).await?;

        tx.commit().await?;

        if impact.playback_reset {
            let state = self.playback_service.get_state(&room_id).await?;
            self.playback_service
                .broadcast_playback_reset_after_force_delete(state)
                .await;
        }

        if !impact.deleted_media_ids.is_empty() || !impact.deleted_playlist_ids.is_empty() {
            for media_id in &impact.deleted_media_ids {
                if let Err(error) = self.notification_service.notify_media_removed(
                    &room_id,
                    Some(&admin_user_id),
                    actor.username(),
                    *media_id,
                ) {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        media_id = %media_id,
                        "Failed to broadcast media removed event"
                    );
                }
            }
            for playlist_id in &impact.deleted_playlist_ids {
                if let Err(error) = self.notification_service.notify_playlist_deleted(
                    &room_id,
                    Some(&admin_user_id),
                    actor.username(),
                    *playlist_id,
                ) {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        playlist_id = %playlist_id,
                        "Failed to broadcast playlist deleted event"
                    );
                }
            }
        }

        tracing::info!(
            room_id = %room_id,
            admin_user_id = %admin_user_id,
            deleted_playlists = impact.deleted_playlist_ids.len(),
            deleted_media = impact.deleted_media_ids.len(),
            "Entries deleted by admin"
        );

        Ok((delete_entries_result_from_impact(impact), precommit_result))
    }

    pub async fn admin_delete_entries_as(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        request: DeleteEntriesRequest,
    ) -> Result<DeleteEntriesResult> {
        let (result, ()) = self
            .admin_delete_entries_as_with_precommit(room_id, actor, request, |_| async { Ok(()) })
            .await?;
        Ok(result)
    }

    /// Get media directly under the room root.
    pub async fn get_room_root_media(&self, room_id: &RoomId) -> Result<Vec<Media>> {
        self.media_service.get_room_root_media(room_id).await
    }

    /// Get room-root media paginated.
    pub async fn get_room_root_media_paginated(
        &self,
        room_id: &RoomId,
        pagination: PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        self.media_service
            .get_room_root_media_paginated(room_id, pagination)
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
        let request = EditMediaRequest { media_id, name };
        self.media_service
            .edit_media(room_id, user_id, request)
            .await
    }

    /// Clear all media directly under the room root.
    ///
    /// Permission check is handled by the API layer (`CLEAR_PLAYLIST`).
    /// This method no longer performs its own permission check to avoid
    /// inconsistency with the API layer's `CLEAR_PLAYLIST` check.
    ///
    /// If the currently playing media is in the room root being cleared,
    /// the playback state is reset to stopped within the same transaction
    /// before deleting media. Playback references are protected by `RESTRICT`
    /// FKs, so the state must be cleared explicitly before the delete can
    /// commit.
    pub async fn clear_playlist(
        &self,
        room_id: RoomId,
        _user_id: UserId,
    ) -> Result<ClearPlaylistResult> {
        // Atomic reset-and-clear within a transaction to prevent TOCTOU race
        // where another user starts playing media between the check and the clear.
        let mut tx = self.pool.begin().await?;

        let deleted_media_ids: Vec<MediaId> = sqlx::query_scalar(
            "SELECT id
             FROM media
             WHERE room_id = $1
               AND playlist_id IS NULL
             ORDER BY position ASC",
        )
        .bind(room_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();

        // Lock the playback state row to prevent concurrent playback switches
        let row = sqlx::query(
            "SELECT playing_media_id, playing_playlist_id FROM room_playback_state
             WHERE room_id = $1
             FOR UPDATE",
        )
        .bind(room_id)
        .fetch_optional(&mut *tx)
        .await?;

        // If the currently playing media is at the room root, reset playback state
        let mut playback_reset = false;
        if let Some(row) = row {
            use sqlx::Row;
            let playing_media_id: Option<MediaId> = row.try_get("playing_media_id")?;
            if let Some(ref mid) = playing_media_id {
                // Check if the playing media belongs to the room root.
                let in_playlist: bool = sqlx::query_scalar(
                    "SELECT EXISTS(
                        SELECT 1
                        FROM media
                        WHERE id = $1
                          AND room_id = $2
                          AND playlist_id IS NULL
                    )",
                )
                .bind(mid)
                .bind(room_id)
                .fetch_one(&mut *tx)
                .await?;

                if in_playlist {
                    // Reset playback state to stopped within the same transaction
                    sqlx::query(
                        "UPDATE room_playback_state
                         SET playing_media_id = NULL, playing_playlist_id = NULL,
                             \"current_time\" = 0, is_playing = false,
                             version = version + 1, updated_at = NOW()
                         WHERE room_id = $1",
                    )
                    .bind(room_id)
                    .execute(&mut *tx)
                    .await?;
                    playback_reset = true;
                }
            }
        }

        // Delete all media at the room root within the transaction
        let result = sqlx::query("DELETE FROM media WHERE room_id = $1 AND playlist_id IS NULL")
            .bind(room_id)
            .execute(&mut *tx)
            .await?;

        let count = result.rows_affected().cast_signed();
        tx.commit().await?;

        for media_id in &deleted_media_ids {
            if let Err(error) = self
                .notification_service
                .notify_media_removed(&room_id, None, "", *media_id)
            {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    media_id = %media_id,
                    "Failed to broadcast media removed event after clear_playlist"
                );
            }
        }

        let playback_state = if playback_reset {
            self.playback_service
                .invalidate_playback_cache(&room_id)
                .await;

            match self.playback_service.get_state(&room_id).await {
                Ok(state) => {
                    if let Err(error) = self.notification_service.notify_playback_state_changed(
                        &room_id,
                        state.is_playing,
                        state.current_time,
                        state.speed,
                        state.playing_media_id,
                    ) {
                        tracing::warn!(
                            error = %error,
                            room_id = %room_id,
                            "Failed to broadcast playback reset after clear_playlist"
                        );
                    }
                    Some(state)
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        "Failed to reload playback state after clear_playlist reset"
                    );
                    None
                }
            }
        } else {
            None
        };

        Ok(ClearPlaylistResult {
            deleted_count: count,
            deleted_media_ids,
            playback_state,
        })
    }

    /// Set current playing media for a room
    pub async fn set_playing_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<RoomPlaybackState> {
        self.playback_service
            .switch(room_id, user_id, Some(media_id), None, Vec::new())
            .await
    }

    /// Move a media item relative to a sibling in the same scope.
    pub async fn move_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: crate::service::media::MoveMediaRequest,
    ) -> Result<Vec<crate::models::Media>> {
        self.media_service
            .move_media(room_id, user_id, request)
            .await
    }

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

    /// Get chat history using keyset (cursor) pagination.
    ///
    /// Returns `(messages, next_cursor)`.
    pub async fn get_chat_history_cursor(
        &self,
        room_id: &RoomId,
        cursor: Option<(DateTime<Utc>, i64)>,
        limit: i32,
    ) -> Result<(Vec<ChatMessage>, Option<(DateTime<Utc>, i64)>)> {
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
            id: 0,
            room_id,
            user_id: Some(user_id),
            content,
            message_type: 1, // text message
            created_at: Utc::now(),
        };
        self.chat_repo.create(&message).await
    }

    /// Check if user has permission in room
    pub async fn check_permission(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<()> {
        let room = self.get_room(room_id).await?;
        self.ensure_room_creator_is_active_for_access(&room, user_id)
            .await?;

        self.permission_service
            .check_permission(room_id, user_id, permission)
            .await
    }

    /// Update room status (admin use, bypasses permission checks)
    ///
    /// Validates the status transition before applying it. Rooms only support
    /// `Active` and `Closed`; review workflows use dedicated request tables.
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
        let actor = self.load_authorized_admin_actor(admin_user_id).await?;
        self.admin_update_room_as(room, &actor).await
    }

    pub async fn admin_update_room_as(
        &self,
        room: &Room,
        _actor: &AuthorizedAdminActor,
    ) -> Result<Room> {
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
        let actor = self.load_authorized_admin_actor(admin_user_id).await?;
        self.admin_delete_room_as(room_id, &actor).await
    }

    pub async fn admin_delete_room_as(
        &self,
        room_id: &RoomId,
        actor: &AuthorizedAdminActor,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let impact = soft_delete_room_and_cleanup_in_tx(&mut tx, room_id).await?;

        tx.commit().await?;

        self.invalidate_room_caches(room_id).await;

        // Notify after commit so notifications are only sent for successful deletions
        let _ = self.notification_service.notify_room_deleted(room_id);

        crate::metrics::http::ROOMS_ACTIVE.dec();

        // Audit log
        if let Some(ref audit) = self.audit_service {
            let _ = audit
                .log(
                    actor.user_id().to_string(),
                    actor.user_id().to_string(),
                    AuditAction::RoomDeleted,
                    AuditTargetType::Room,
                    Some(room_id.to_string()),
                    serde_json::json!({
                        "reason": "Room deleted by admin",
                        "playlists_deleted": impact.deleted_playlist_ids.len(),
                        "media_deleted": impact.deleted_media_ids.len(),
                        "members_deleted": impact.members_deleted,
                        "settings_deleted": impact.settings_deleted,
                        "chat_deleted": impact.chat_deleted,
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
    /// - Does not exist (hard-deleted), OR
    /// - Has `deleted_at` set (soft-deleted), OR
    /// - Has an active global ban
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
        let actor = self.load_authorized_admin_actor(admin_user_id).await?;
        self.admin_delete_orphaned_room_as(room_id, &actor).await
    }

    pub async fn admin_delete_orphaned_room_as(
        &self,
        room_id: &RoomId,
        actor: &AuthorizedAdminActor,
    ) -> Result<()> {
        let admin_user_id = actor.user_id();
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
                SELECT 1
                FROM users u
                WHERE u.id = $1
                  AND u.deleted_at IS NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM user_bans ub
                      WHERE ub.user_id = u.id
                        AND ub.revoked_at IS NULL
                        AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                  )
            )",
        )
        .bind(room.created_by)
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

        let mut tx = self.pool.begin().await?;
        let impact = soft_delete_room_and_cleanup_in_tx(&mut tx, room_id).await?;

        tx.commit().await?;

        self.invalidate_room_caches(room_id).await;

        // Notify after commit
        let _ = self.notification_service.notify_room_deleted(room_id);

        crate::metrics::http::ROOMS_ACTIVE.dec();

        // Audit log
        if let Some(ref audit) = self.audit_service {
            let _ = audit
                .log(
                    actor.user_id().to_string(),
                    actor.user_id().to_string(),
                    AuditAction::RoomDeleted,
                    AuditTargetType::Room,
                    Some(room_id.to_string()),
                    serde_json::json!({
                        "reason": "Orphaned room deleted by admin (creator deleted/banned)",
                        "creator_id": room.created_by.to_string(),
                        "playlists_deleted": impact.deleted_playlist_ids.len(),
                        "media_deleted": impact.deleted_media_ids.len(),
                        "members_deleted": impact.members_deleted,
                        "settings_deleted": impact.settings_deleted,
                        "chat_deleted": impact.chat_deleted,
                    }),
                    None,
                    None,
                )
                .await;
        }

        tracing::info!(room_id = %room_id, "Orphaned room deleted successfully");

        Ok(())
    }

    /// Start playback from the management plane, bypassing room membership permissions.
    ///
    /// Only global admin/root identities may use this path.
    pub async fn admin_start_playback(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Vec<u8>,
    ) -> Result<RoomPlaybackState> {
        let actor = self.load_authorized_admin_actor(&admin_user_id).await?;
        self.admin_start_playback_as(room_id, &actor, media_id, playlist_id, target)
            .await
    }

    pub async fn admin_start_playback_as(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Vec<u8>,
    ) -> Result<RoomPlaybackState> {
        self.playback_service
            .admin_switch(room_id, *actor.user_id(), media_id, playlist_id, target)
            .await
    }

    /// Stop playback from the management plane, bypassing room membership permissions.
    ///
    /// Only global admin/root identities may use this path.
    pub async fn admin_stop_playback(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
    ) -> Result<RoomPlaybackState> {
        let actor = self.load_authorized_admin_actor(&admin_user_id).await?;
        self.admin_stop_playback_as(room_id, &actor).await
    }

    pub async fn admin_stop_playback_as(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
    ) -> Result<RoomPlaybackState> {
        self.playback_service
            .admin_reset(room_id, *actor.user_id())
            .await
    }

    /// Patch playback from the management plane, bypassing room membership permissions.
    ///
    /// Only global admin/root identities may use this path.
    pub async fn admin_update_playback_as(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        playing: Option<bool>,
        current_time: Option<f64>,
        speed: Option<f64>,
        expected_version: Option<i64>,
    ) -> Result<RoomPlaybackState> {
        self.playback_service
            .admin_update_multiple_with_version(
                room_id,
                *actor.user_id(),
                playing,
                current_time,
                speed,
                expected_version,
            )
            .await
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
        self.admin_set_room_password_as(room_id, new_password, None, "")
            .await
            .map(|_| ())
    }

    pub async fn admin_set_room_password_as(
        &self,
        room_id: &RoomId,
        new_password: Option<&str>,
        actor_user_id: Option<&UserId>,
        actor_username: &str,
    ) -> Result<crate::service::room_settings::RoomSettingsSnapshot> {
        // Verify room exists
        let _room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        // Hash new password outside transaction (CPU-intensive)
        let password_is_being_set = new_password.is_some();
        let hashed_password = match new_password {
            Some(pwd) => Some(self.password_hasher.hash_password(pwd).await?),
            None => None,
        };

        self.do_set_password_hash(room_id, hashed_password).await?;
        self.room_settings_service.invalidate_local(room_id).await;

        if password_is_being_set {
            if let Err(e) = self.remove_guest_role_members(room_id).await {
                tracing::warn!(
                    "Failed to remove guest-role members after admin password set: {}",
                    e
                );
            }
        }

        self.notify_room_invalidation(room_id).await;
        self.notify_room_settings_invalidation(room_id).await;

        self.emit_room_settings_snapshot_after_password_update(
            room_id,
            actor_user_id,
            actor_username,
        )
        .await
    }

    async fn emit_room_settings_snapshot_after_password_update(
        &self,
        room_id: &RoomId,
        actor_user_id: Option<&UserId>,
        actor_username: &str,
    ) -> Result<crate::service::room_settings::RoomSettingsSnapshot> {
        let snapshot = self
            .room_settings_service
            .get_refresh_with_version(room_id)
            .await?;
        let resolved_actor_username = match actor_user_id {
            Some(user_id) if actor_username.is_empty() => {
                self.resolve_actor_username(user_id).await
            }
            Some(_) => actor_username.to_string(),
            None => String::new(),
        };
        let settings_json = serde_json::to_value(&snapshot.settings)
            .internal_with_err("Failed to serialize settings")?;
        let _ = self.notification_service.notify_settings_updated(
            room_id,
            actor_user_id,
            &resolved_actor_username,
            settings_json,
            snapshot.version,
        );
        Ok(snapshot)
    }

    /// Remove all active `RoomRole::Guest` members from a room.
    ///
    /// This is one part of room-wide guest revocation when room policy changes
    /// make guest access invalid.
    async fn remove_guest_role_members(&self, room_id: &RoomId) -> Result<()> {
        let members = self.member_service.list_members(room_id).await?;

        for member in members {
            if member.role == RoomRole::Guest {
                self.member_service
                    .remove_member(*room_id, member.user_id)
                    .await?;
            }
        }

        Ok(())
    }

    /// Revoke all guest access for the room.
    ///
    /// This removes persisted guest members, invalidates anonymous guest JWTs
    /// via the room guest version, and notifies online guest connections to
    /// disconnect immediately.
    async fn revoke_all_guest_access(
        &self,
        room_id: &RoomId,
        reason: crate::service::notification::GuestKickReason,
    ) -> Result<()> {
        self.remove_guest_role_members(room_id).await?;
        self.bump_room_guest_version(room_id).await?;
        self.notification_service.kick_all_guests(room_id, reason)?;
        Ok(())
    }

    async fn bump_room_guest_version(&self, room_id: &RoomId) -> Result<i64> {
        let current = self.get_room_guest_version(room_id).await?;
        let next = current
            .checked_add(1)
            .ok_or_else(|| Error::Internal("Room guest version overflowed".to_string()))?;

        let key = self
            .user_service
            .key_builder()
            .room_guest_version(&room_id.to_string());
        self.user_service
            .token_blacklist_store()
            .set_family_revoked(&key, next, Self::room_guest_version_ttl_secs())
            .await?;

        Ok(next)
    }

    const fn room_guest_version_ttl_secs() -> u64 {
        Duration::hours(4).num_seconds().cast_unsigned()
    }

    /// Get reference to media service
    #[must_use]
    pub const fn media_service(&self) -> &MediaService {
        &self.media_service
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
                    admin_user_id.to_string(),
                    admin_user_id.to_string(),
                    AuditAction::RoomBanned,
                    AuditTargetType::Room,
                    Some(room_id.to_string()),
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
                    admin_user_id.to_string(),
                    admin_user_id.to_string(),
                    AuditAction::RoomUnbanned,
                    AuditTargetType::Room,
                    Some(room_id.to_string()),
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
                    room_id = %room_id,
                    "Failed to broadcast room cache invalidation"
                );
            }
        }
    }

    /// Broadcast room settings cache invalidation to other replicas.
    async fn notify_room_settings_invalidation(&self, room_id: &RoomId) {
        if let Some(ref service) = self.cache_invalidation {
            if let Err(e) = service
                .invalidate_and_broadcast_room_settings(room_id)
                .await
            {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    "Failed to broadcast room settings cache invalidation"
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
    /// **CRITICAL**: This must be called only after a successful transaction
    /// commit. Invalidating before commit lets other replicas miss cache and
    /// repopulate stale state from rows that are still visible.
    ///
    /// ## Usage Pattern
    ///
    /// ```text
    /// let mut tx = self.pool.begin().await?;
    /// //... perform database operations...
    /// tx.commit().await?;
    /// self.invalidate_room_caches(&room_id).await;
    /// //... post-commit operations...
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

    /// Run best-effort post-commit side effects after a room has already been
    /// deleted transactionally elsewhere.
    pub async fn finalize_deleted_room_after_commit(&self, room_id: &RoomId) {
        self.invalidate_room_caches(room_id).await;
        let _ = self.notification_service.notify_room_deleted(room_id);
    }

    /// Run best-effort post-commit side effects after a room became unusable
    /// because its creator account is no longer active.
    pub async fn finalize_room_owner_inactive_after_commit(&self, room_id: &RoomId) {
        self.invalidate_room_caches(room_id).await;
    }

    /// Run best-effort post-commit side effects after entry deletions have
    /// already committed.
    pub async fn finalize_entry_deletions_after_commit(
        &self,
        room_id: &RoomId,
        deleted_media_ids: &[MediaId],
        playback_reset: bool,
    ) {
        self.invalidate_room_caches(room_id).await;

        if playback_reset {
            match self.playback_service.get_state(room_id).await {
                Ok(state) => {
                    self.playback_service
                        .broadcast_playback_reset_after_force_delete(state)
                        .await;
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        "Failed to reload playback state after user cleanup reset"
                    );
                }
            }
        }

        for media_id in deleted_media_ids {
            if let Err(error) = self
                .notification_service
                .notify_media_removed(room_id, None, "", *media_id)
            {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    media_id = %media_id,
                    "Failed to broadcast media removed event after user cleanup"
                );
            }
        }
    }

    // Batch Operations

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
        room_ids: &[RoomId],
        admin_user_id: &UserId,
    ) -> crate::Result<Vec<(RoomId, crate::Result<()>)>> {
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

        for room_id in room_ids {
            let result = self.ban_room(room_id, admin_user_id).await.map(|_| ());
            results.push((*room_id, result));
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
        room_ids: &[RoomId],
        admin_user_id: &UserId,
    ) -> crate::Result<Vec<(RoomId, crate::Result<()>)>> {
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

        for room_id in room_ids {
            let result = self.admin_delete_room(room_id, admin_user_id).await;
            results.push((*room_id, result));
        }

        Ok(results)
    }
}

fn dedup_ids<T>(ids: Vec<T>) -> Vec<T>
where
    T: Eq + std::hash::Hash + Clone,
{
    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(ids.len());
    for id in ids {
        if seen.insert(id.clone()) {
            deduped.push(id);
        }
    }
    deduped
}

async fn has_room_permission_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    user_id: &UserId,
    permission: u64,
) -> Result<bool> {
    let required_permission = permission.cast_signed();
    let admin_default = PermissionBits::DEFAULT_ADMIN.cast_signed();
    let member_default = PermissionBits::DEFAULT_MEMBER.cast_signed();
    let guest_default = PermissionBits::DEFAULT_GUEST.cast_signed();
    let has_permission: Option<bool> = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
            FROM room_members rm
            LEFT JOIN room_settings rs
              ON rs.room_id = rm.room_id
             AND rs.key = '_settings'
            WHERE rm.room_id = $1
              AND rm.user_id = $2
              AND rm.left_at IS NULL
              AND (
                  rm.role = 1
                  OR (
                      CASE rm.role
                          WHEN 2 THEN
                              (((($4 | COALESCE((rs.value::jsonb ->> 'admin_added_permissions')::bigint, 0::bigint) | rm.admin_added_permissions) &
                               ~COALESCE((rs.value::jsonb ->> 'admin_removed_permissions')::bigint, 0::bigint) & ~rm.admin_removed_permissions) & $3) = $3)
                          WHEN 3 THEN
                              (((($5 | (COALESCE((rs.value::jsonb ->> 'member_added_permissions')::bigint, 0::bigint) & $4) | rm.added_permissions) &
                               ~COALESCE((rs.value::jsonb ->> 'member_removed_permissions')::bigint, 0::bigint) & ~rm.removed_permissions) & $3) = $3)
                          WHEN 4 THEN
                              (((($6 | (COALESCE((rs.value::jsonb ->> 'guest_added_permissions')::bigint, 0::bigint) & $5) | rm.added_permissions) &
                               ~COALESCE((rs.value::jsonb ->> 'guest_removed_permissions')::bigint, 0::bigint) & ~rm.removed_permissions) & $3) = $3)
                          ELSE FALSE
                      END
                  )
              )
        )",
    )
    .bind(room_id)
    .bind(user_id)
    .bind(required_permission)
    .bind(admin_default)
    .bind(member_default)
    .bind(guest_default)
    .fetch_optional(&mut **tx)
    .await?
    .flatten();

    Ok(has_permission.unwrap_or(false))
}

async fn collect_target_playlist_nodes_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    root_playlist_ids: &[PlaylistId],
) -> Result<Vec<(PlaylistId, i32)>> {
    if root_playlist_ids.is_empty() {
        return Ok(Vec::new());
    }

    let playlist_ids: Vec<i64> = root_playlist_ids.iter().map(PlaylistId::as_i64).collect();

    let rows = sqlx::query(
        "WITH RECURSIVE target_playlists AS (
            SELECT id, 0 AS depth
            FROM playlists
            WHERE room_id = $1
              AND id = ANY($2)
            UNION ALL
            SELECT p.id, tp.depth + 1
            FROM playlists p
            JOIN target_playlists tp ON p.parent_id = tp.id
            WHERE p.room_id = $1
        )
        SELECT id, MAX(depth) AS depth
        FROM target_playlists
        GROUP BY id
        ORDER BY MAX(depth) DESC, id",
    )
    .bind(room_id)
    .bind(&playlist_ids)
    .fetch_all(&mut **tx)
    .await?;

    rows.into_iter()
        .map(|row| {
            let playlist_id = row.try_get::<i64, _>("id")?;
            let depth = row.try_get::<i32, _>("depth")?;
            Ok((PlaylistId::from(playlist_id), depth))
        })
        .collect()
}

async fn collect_all_room_playlist_nodes_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<Vec<(PlaylistId, i32)>> {
    let rows = sqlx::query(
        "WITH RECURSIVE playlist_tree AS (
            SELECT id, 0 AS depth
            FROM playlists
            WHERE room_id = $1
              AND parent_id IS NULL
            UNION ALL
            SELECT p.id, pt.depth + 1
            FROM playlists p
            JOIN playlist_tree pt ON p.parent_id = pt.id
            WHERE p.room_id = $1
        )
        SELECT id, MAX(depth) AS depth
        FROM playlist_tree
        GROUP BY id
        ORDER BY MAX(depth) DESC, id",
    )
    .bind(room_id)
    .fetch_all(&mut **tx)
    .await?;

    rows.into_iter()
        .map(|row| {
            let playlist_id = row.try_get::<i64, _>("id")?;
            let depth = row.try_get::<i32, _>("depth")?;
            Ok((PlaylistId::from(playlist_id), depth))
        })
        .collect()
}

async fn collect_room_root_media_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<Vec<MediaId>> {
    let media_ids: Vec<MediaId> = sqlx::query_scalar(
        "SELECT id
         FROM media
         WHERE room_id = $1
           AND playlist_id IS NULL
         ORDER BY id",
    )
    .bind(room_id)
    .fetch_all(&mut **tx)
    .await?;

    Ok(media_ids)
}

async fn collect_deleted_media_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    playlist_ids: &[PlaylistId],
    explicit_media_ids: &[MediaId],
) -> Result<Vec<MediaId>> {
    if playlist_ids.is_empty() && explicit_media_ids.is_empty() {
        return Ok(Vec::new());
    }

    let playlist_id_strs: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();
    let explicit_media_id_strs: Vec<i64> = explicit_media_ids.iter().map(MediaId::as_i64).collect();

    let rows = sqlx::query(
        "WITH RECURSIVE target_playlists AS (
            SELECT id
            FROM playlists
            WHERE id = ANY($1)
            UNION ALL
            SELECT p.id
            FROM playlists p
            JOIN target_playlists tp ON p.parent_id = tp.id
        )
        SELECT DISTINCT m.id
        FROM media m
        WHERE m.room_id = $2
          AND (
              m.id = ANY($3)
              OR m.playlist_id IN (SELECT id FROM target_playlists)
          )
        ORDER BY m.id",
    )
    .bind(&playlist_id_strs)
    .bind(room_id)
    .bind(&explicit_media_id_strs)
    .fetch_all(&mut **tx)
    .await?;

    rows.into_iter()
        .map(|row| {
            let media_id: MediaId = row.try_get("id")?;
            Ok(media_id)
        })
        .collect()
}

async fn plan_playback_reset_for_deleted_entries_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    deleted_playlist_ids: &[PlaylistId],
    deleted_media_ids: &[MediaId],
    force: bool,
) -> Result<bool> {
    let playback_row = sqlx::query(
        "SELECT playing_media_id, playing_playlist_id
         FROM room_playback_state
         WHERE room_id = $1
         FOR UPDATE",
    )
    .bind(room_id)
    .fetch_optional(&mut **tx)
    .await?;

    let Some(row) = playback_row else {
        return Ok(false);
    };

    let playing_media_id: Option<MediaId> = row.try_get("playing_media_id")?;
    let playing_playlist_id: Option<PlaylistId> = row.try_get("playing_playlist_id")?;

    let deletes_playing_media = playing_media_id.as_ref().is_some_and(|current_id| {
        deleted_media_ids
            .iter()
            .any(|media_id| media_id == current_id)
    });

    let deletes_playing_playlist = playing_playlist_id.as_ref().is_some_and(|current_id| {
        deleted_playlist_ids
            .iter()
            .any(|playlist_id| playlist_id == current_id)
    });

    if !(deletes_playing_media || deletes_playing_playlist) {
        return Ok(false);
    }

    if !force {
        return Err(Error::InvalidInput(
            "Cannot delete entries that include the currently playing media".to_string(),
        ));
    }

    Ok(true)
}

async fn delete_playlist_ids_in_depth_order_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    playlist_nodes: &[(PlaylistId, i32)],
) -> Result<()> {
    if playlist_nodes.is_empty() {
        return Ok(());
    }

    let mut ids_by_depth = BTreeMap::<i32, Vec<i64>>::new();
    for (playlist_id, depth) in playlist_nodes {
        ids_by_depth
            .entry(*depth)
            .or_default()
            .push(playlist_id.as_i64());
    }

    for (_depth, ids) in ids_by_depth.into_iter().rev() {
        sqlx::query("DELETE FROM playlists WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&mut **tx)
            .await?;
    }

    Ok(())
}

fn delete_entries_result_from_impact(impact: EntryDeletionImpact) -> DeleteEntriesResult {
    DeleteEntriesResult {
        deleted_playlists: impact.deleted_playlist_ids.len(),
        deleted_media: impact.deleted_media_ids.len(),
        deleted_playlist_ids: impact.deleted_playlist_ids,
        deleted_media_ids: impact.deleted_media_ids,
    }
}

async fn apply_delete_entries_impact_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    impact: &EntryDeletionImpact,
) -> Result<()> {
    if impact.playback_reset {
        sqlx::query(
            "UPDATE room_playback_state
             SET playing_media_id = NULL,
                 playing_playlist_id = NULL,
                 target = ''::bytea,
                 \"current_time\" = 0,
                 speed = 1.0,
                 is_playing = false,
                 version = version + 1,
                 updated_at = NOW()
             WHERE room_id = $1",
        )
        .bind(room_id)
        .execute(&mut **tx)
        .await?;
    }

    if !impact.deleted_media_ids.is_empty() {
        let media_id_strs: Vec<i64> = impact
            .deleted_media_ids
            .iter()
            .map(MediaId::as_i64)
            .collect();
        sqlx::query("DELETE FROM media WHERE id = ANY($1)")
            .bind(&media_id_strs)
            .execute(&mut **tx)
            .await?;
    }

    delete_playlist_ids_in_depth_order_in_tx(tx, &impact.playlist_nodes).await?;

    Ok(())
}

async fn plan_delete_entries_in_room_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    root_playlist_ids: &[PlaylistId],
    explicit_media_ids: &[MediaId],
    force: bool,
) -> Result<EntryDeletionImpact> {
    let playlist_nodes =
        collect_target_playlist_nodes_in_tx(tx, room_id, root_playlist_ids).await?;
    let deleted_playlist_ids: Vec<PlaylistId> = playlist_nodes
        .iter()
        .map(|(playlist_id, _)| *playlist_id)
        .collect();
    let deleted_media_ids =
        collect_deleted_media_ids_in_tx(tx, room_id, &deleted_playlist_ids, explicit_media_ids)
            .await?;
    let playback_reset = plan_playback_reset_for_deleted_entries_in_tx(
        tx,
        room_id,
        &deleted_playlist_ids,
        &deleted_media_ids,
        force,
    )
    .await?;

    Ok(EntryDeletionImpact {
        playlist_nodes,
        deleted_playlist_ids,
        deleted_media_ids,
        playback_reset,
    })
}

pub(crate) async fn soft_delete_room_and_cleanup_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<RoomCleanupImpact> {
    let deleted = sqlx::query(
        "UPDATE rooms
         SET deleted_at = $2, updated_at = $2, version = version + 1
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(room_id)
    .bind(chrono::Utc::now())
    .execute(&mut **tx)
    .await?;

    if deleted.rows_affected() == 0 {
        return Err(Error::NotFound(
            "Room not found or already deleted".to_string(),
        ));
    }

    let playlist_nodes = collect_all_room_playlist_nodes_in_tx(tx, room_id).await?;
    let deleted_playlist_ids: Vec<PlaylistId> = playlist_nodes
        .iter()
        .map(|(playlist_id, _)| *playlist_id)
        .collect();
    let root_media_ids = collect_room_root_media_ids_in_tx(tx, room_id).await?;
    let deleted_media_ids =
        collect_deleted_media_ids_in_tx(tx, room_id, &deleted_playlist_ids, &root_media_ids)
            .await?;

    let playback_rows_deleted = sqlx::query("DELETE FROM room_playback_state WHERE room_id = $1")
        .bind(room_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();

    if !deleted_media_ids.is_empty() {
        let media_id_strs: Vec<i64> = deleted_media_ids.iter().map(MediaId::as_i64).collect();
        sqlx::query("DELETE FROM media WHERE id = ANY($1)")
            .bind(&media_id_strs)
            .execute(&mut **tx)
            .await?;
    }

    delete_playlist_ids_in_depth_order_in_tx(tx, &playlist_nodes).await?;

    let members_deleted = sqlx::query("DELETE FROM room_members WHERE room_id = $1")
        .bind(room_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();

    let settings_deleted = sqlx::query("DELETE FROM room_settings WHERE room_id = $1")
        .bind(room_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();

    let chat_deleted = sqlx::query("DELETE FROM chat_messages WHERE room_id = $1")
        .bind(room_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();

    Ok(RoomCleanupImpact {
        deleted_playlist_ids,
        deleted_media_ids,
        members_deleted,
        settings_deleted,
        playback_rows_deleted,
        chat_deleted,
    })
}

pub(crate) async fn hard_delete_room_and_cleanup_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<bool> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM rooms WHERE id = $1)")
        .bind(room_id)
        .fetch_one(&mut **tx)
        .await?;
    if !exists {
        return Ok(false);
    }

    let playlist_nodes = collect_all_room_playlist_nodes_in_tx(tx, room_id).await?;
    let deleted_playlist_ids: Vec<PlaylistId> = playlist_nodes
        .iter()
        .map(|(playlist_id, _)| *playlist_id)
        .collect();
    let root_media_ids = collect_room_root_media_ids_in_tx(tx, room_id).await?;
    let deleted_media_ids =
        collect_deleted_media_ids_in_tx(tx, room_id, &deleted_playlist_ids, &root_media_ids)
            .await?;

    sqlx::query("DELETE FROM room_playback_state WHERE room_id = $1")
        .bind(room_id)
        .execute(&mut **tx)
        .await?;

    if !deleted_media_ids.is_empty() {
        let media_id_strs: Vec<i64> = deleted_media_ids.iter().map(MediaId::as_i64).collect();
        sqlx::query("DELETE FROM media WHERE id = ANY($1)")
            .bind(&media_id_strs)
            .execute(&mut **tx)
            .await?;
    }

    delete_playlist_ids_in_depth_order_in_tx(tx, &playlist_nodes).await?;

    sqlx::query("DELETE FROM room_members WHERE room_id = $1")
        .bind(room_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM room_settings WHERE room_id = $1")
        .bind(room_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM chat_messages WHERE room_id = $1")
        .bind(room_id)
        .execute(&mut **tx)
        .await?;

    let deleted = sqlx::query("DELETE FROM rooms WHERE id = $1")
        .bind(room_id)
        .execute(&mut **tx)
        .await?;

    Ok(deleted.rows_affected() > 0)
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::RoomService;
    use crate::models::{
        room_settings::{
            AllowGuestJoin, ChatEnabled, DanmakuEnabled, GuestAddedPermissions, MaxMembers,
            MemberAddedPermissions, RequirePassword,
        },
        PermissionBits, RoomSettings, RoomStatus,
    };
    use crate::test_helpers::RoomFixture;
    use crate::Error;
    use crate::{
        cache::{CacheInvalidationService, KeyBuilder, UsernameCache},
        config::PasswordComplexityConfig,
        service::{
            auth::{BruteForceProtection, JwtService},
            InMemoryTokenBlacklistStore, UserService,
        },
    };
    use async_trait::async_trait;
    use sqlx::PgPool;
    use std::sync::Arc;

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

    fn make_user_service(pool: PgPool) -> UserService {
        let jwt_service = JwtService::new("room-service-test-secret-key-32bytes!!").unwrap();
        let username_cache =
            UsernameCache::local_only("room-service:test:username:".to_string(), 128, 60);
        let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
        let brute_force = BruteForceProtection::in_memory("room-service-test".to_string());

        UserService::new(
            pool,
            jwt_service,
            username_cache,
            PasswordComplexityConfig::default(),
            token_blacklist,
            KeyBuilder::new("room-service-test"),
            brute_force,
        )
    }

    #[tokio::test]
    async fn test_set_cache_invalidation_wires_permission_service_for_room_service_new() {
        let pool = PgPool::connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused").unwrap();
        let user_service = make_user_service(pool.clone());
        let mut room_service = RoomService::new(pool, user_service);

        assert!(
            !room_service.permission_service().has_invalidation_service(),
            "plain RoomService::new should start without permission invalidation wiring"
        );

        room_service.set_cache_invalidation(Arc::new(CacheInvalidationService::new(
            "room-service-node".to_string(),
            "room-service-stream".to_string(),
        )));

        assert!(
            room_service.permission_service().has_invalidation_service(),
            "post-construction cache invalidation wiring must reach the shared permission service"
        );
    }

    struct FailingCoordinationLock;

    #[async_trait]
    impl crate::service::distributed_lock::CoordinationLock for FailingCoordinationLock {
        async fn acquire(&self, key: &str, _ttl_secs: u64) -> crate::Result<Option<String>> {
            Err(Error::ServiceUnavailable(format!(
                "synthetic lock failure for {key}"
            )))
        }

        async fn release(&self, _key: &str, _lock_value: &str) -> crate::Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn test_create_room_uses_injected_coordination_lock_trait_object() {
        let pool = PgPool::connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused").unwrap();
        let user_service = make_user_service(pool.clone());
        let mut room_service = RoomService::new(pool, user_service);
        room_service.set_distributed_lock(Arc::new(FailingCoordinationLock));

        let error = room_service
            .create_room(
                "locked room".to_string(),
                "desc".to_string(),
                crate::models::UserId::new(),
                None,
                None,
            )
            .await
            .expect_err("lock failure should short-circuit before any database work");

        assert!(
            matches!(error, Error::ServiceUnavailable(ref message) if message.contains("synthetic lock failure")),
            "unexpected error: {error:?}"
        );
    }

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
        // Grant members a lifecycle permission that is not assignable in room settings.
        settings.member_added_permissions = MemberAddedPermissions(PermissionBits::DELETE_ROOM);
        let result = settings.validate_permissions();
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidInput(msg) => {
                assert!(
                    msg.contains("lifecycle") || msg.contains("Member"),
                    "got: {msg}"
                );
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

    #[test]
    fn test_admin_permissions_with_added_and_removed() {
        let mut settings = RoomSettings::default();
        let base = PermissionBits(PermissionBits::SEND_CHAT | PermissionBits::ADD_MEDIA);

        // Add PLAY_CONTROL, remove SEND_CHAT
        settings.admin_added_permissions =
            crate::models::room_settings::AdminAddedPermissions(PermissionBits::PLAY_CONTROL);
        settings.admin_removed_permissions =
            crate::models::room_settings::AdminRemovedPermissions(PermissionBits::SEND_CHAT);

        let result = settings.admin_permissions(base);
        // Should have ADD_MEDIA and PLAY_CONTROL, but not SEND_CHAT
        assert!(result.0 & PermissionBits::ADD_MEDIA != 0);
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
    fn test_room_is_active_considers_lifecycle_and_deleted() {
        let mut room = RoomFixture::new().build();
        assert!(room.is_active());

        // Ban is independent moderation state, not lifecycle state.
        room.ban();
        assert!(room.is_active());
        room.unban();
        assert!(room.is_active());

        // Deleted room is not active
        room.deleted_at = Some(chrono::Utc::now());
        assert!(!room.is_active());
    }

    #[test]
    fn test_room_is_active_requires_open_lifecycle() {
        use crate::models::Room;

        let room = Room::new("test".to_string(), crate::models::UserId::new());
        assert!(room.is_active());

        // Closed rooms have closed_at set.
        let mut closed_room = room;
        closed_room.close();
        assert!(!closed_room.is_active());
    }

    #[test]
    fn test_room_member_ban_sets_status_and_metadata() {
        use crate::models::{MemberStatus, RoomId, RoomMember, RoomRole, UserId};

        let mut member = RoomMember::new(RoomId::from(1), UserId::from(1), RoomRole::Member);
        assert!(member.is_active());

        let banner = UserId::from(2);
        member.ban(banner, Some("spamming".to_string()));

        assert_eq!(member.status, MemberStatus::Left);
        assert!(member.banned_at.is_some());
        assert_eq!(member.banned_by, Some(banner));
        assert_eq!(member.banned_reason, Some("spamming".to_string()));
        assert!(!member.is_active());
    }

    #[test]
    fn test_room_member_unban_clears_metadata() {
        use crate::models::{MemberStatus, RoomId, RoomMember, RoomRole, UserId};

        let mut member = RoomMember::new(RoomId::from(1), UserId::from(1), RoomRole::Member);
        member.ban(UserId::from(2), Some("reason".to_string()));

        member.unban();
        assert_eq!(member.status, MemberStatus::Left);
        assert!(member.banned_at.is_none());
        assert!(member.banned_by.is_none());
        assert!(member.banned_reason.is_none());
        assert!(!member.is_active());
    }

    #[test]
    fn test_room_member_banned_has_no_permissions() {
        use crate::models::{RoomId, RoomMember, RoomRole, UserId};

        let mut member = RoomMember::new(RoomId::from(1), UserId::from(1), RoomRole::Admin);
        let role_default = PermissionBits(PermissionBits::DEFAULT_ADMIN);

        // Before ban: has permissions
        assert!(member.has_permission(PermissionBits::SEND_CHAT, role_default));

        member.ban(UserId::from(2), None);

        // After ban: zero permissions
        assert!(!member.has_permission(PermissionBits::SEND_CHAT, role_default));
        assert!(!member.has_permission(PermissionBits::DELETE_ROOM, role_default));
    }

    #[test]
    fn test_room_member_add_and_remove_permissions() {
        use crate::models::{RoomId, RoomMember, RoomRole, UserId};

        let mut member = RoomMember::new(RoomId::from(1), UserId::from(1), RoomRole::Member);
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
        // The bug occurs when:
        // 1. User provides password "abc123"
        // 2. Initial verification succeeds against hash H1
        // 3. Password changes to "xyz789" (hash H2)
        // 4. Password changes back to "abc123" with hash H1 (same hash!)
        // 5. Under lock, fast path skips re-verification
        // The fix: Remove the fast path at lines 578-579 and always re-verify.
        // Before fix:
        // if verified_hash.as_ref() == Some(hash) {
        // // BUG: Skip re-verification
        // }
        // After fix:
        // // Always re-verify, no fast path
        // let provided_password = password.ok_or_else(||...)?;
        // if !verify_password(&provided_password, hash).await? {
        // return Err(...);
        // }

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
    fn test_disable_create_room_takes_precedence_over_allow() {
        // Even if allow_room_creation=true, disable_create_room=true should block
        let result = check_room_creation_allowed(true, true);
        assert!(
            result.is_err(),
            "disable_create_room=true should take precedence over allow_room_creation=true"
        );
    }
}
