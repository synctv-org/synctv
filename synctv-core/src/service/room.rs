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

use sqlx::{PgPool, Postgres, Transaction};
use std::collections::HashMap;
use std::sync::Arc;

use crate::{
    cache::{CacheInvalidationRuntime, ConsistencyCoordinator},
    models::{
        AuditAction, AuditTargetType, MediaId, PlaylistId, Room, RoomAdminPermissionBits,
        RoomGuestPermissionBits, RoomId, RoomMember, RoomMemberPermissionBits, RoomPermission,
        RoomPlaybackState, RoomRole, RoomSettings, UserId,
    },
    repository::{
        realtime_outbox::{NewRealtimeOutboxEvent, RealtimeOutboxRepository},
        room_member::RemovedRoomMember,
        room_password::RoomPasswordCredentialState,
        ChatRepository, MediaRepository, PlaylistRepository, RoomMemberRepository,
        RoomPasswordRepository, RoomPlaybackStateRepository, RoomRepository,
        RoomSettingsRepository,
    },
    service::{
        audit::AuditService,
        auth::OpaquePasswordService,
        media::MediaService,
        member::MemberService,
        notification::NotificationService,
        permission::{PermissionService, PermissionWriteFence},
        playback::PlaybackService,
        playlist::PlaylistService,
        room_settings::RoomSettingsService,
        user::UserService,
        FileStorageCleanupOrigin,
    },
    Error, Result,
};

mod access;
mod ban;
mod constructor;
mod deletion;
pub(crate) use deletion::soft_delete_room_and_cleanup_in_tx;
use deletion::{
    apply_delete_entries_impact_in_tx, cleanup_member_resources_in_tx,
    delete_entries_result_from_impact, plan_clear_playlist_scope_in_tx,
    plan_delete_entries_in_room_in_tx,
};
mod cover;
mod creation;
mod entries;
mod guest_access;
mod join;
mod lifecycle;
mod media_playback_chat;
mod member_admin;
mod member_mutations;
mod opaque_sessions;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use opaque_sessions::{
    local_room_opaque_password_login_session_store,
    local_room_opaque_password_registration_session_store,
};
pub(crate) use opaque_sessions::{
    room_opaque_password_login_session_store_from_shared_state_profile,
    room_opaque_password_registration_session_store_from_shared_state_profile,
};
pub use opaque_sessions::{
    RoomOpaqueLoginStartChallenge, RoomOpaquePasswordLoginSession,
    RoomOpaquePasswordLoginSessionStore, RoomOpaquePasswordRegistrationSession,
    RoomOpaquePasswordRegistrationSessionStore, RoomOpaqueRegistrationStartChallenge,
    ROOM_OPAQUE_LOGIN_SESSION_TTL_SECS, ROOM_OPAQUE_REGISTRATION_SESSION_TTL_SECS,
};
mod outbox;
mod password;
mod permission_fence_guard;
mod resource_access;
mod settings;
mod types;
pub use types::{
    AdminAddMemberWithOutboxRequest, AdminRejectJoinRequestWithOutbox, AuthorizedAdminActor,
    ClearPlaylistResult, ClientResourceAvailability, CreateRoomCoverUploadSession,
    DeleteEntriesPlan, DeleteEntriesRequest, DeleteEntriesResult, KickMemberOutboxOptions,
    MemberPermissionPatch, MemberResourceCleanupResult, PermissionChangedOutboxSnapshot,
    RealtimeOutboxDeleteEntriesEventFactory, RealtimeOutboxMemberResourceCleanupEventFactory,
    RealtimeOutboxPermissionChangedEventFactory, RealtimeOutboxRoomEventFactory,
    RealtimeOutboxSettingsEventFactory, RealtimeOutboxUserLeftEventFactory, RoomServiceOptions,
    UpdateMemberWithOutboxRequest, UserLeftOutboxSnapshot,
};
pub(crate) use types::{EntryDeletionImpact, RoomCleanupImpact};

pub const MAX_KICK_COOLDOWN_SECONDS: i64 = 30 * 24 * 60 * 60;

fn merge_json_object_patch(target: &mut serde_json::Value, patch: serde_json::Value) -> Result<()> {
    let serde_json::Value::Object(patch_object) = patch else {
        return Err(Error::InvalidInput(
            "Room settings patch must be a JSON object".to_string(),
        ));
    };

    let Some(target_object) = target.as_object_mut() else {
        return Err(Error::Internal(
            "Serialized room settings must be a JSON object".to_string(),
        ));
    };

    for (key, value) in patch_object {
        match (target_object.get_mut(&key), value) {
            (Some(existing @ serde_json::Value::Object(_)), serde_json::Value::Object(value)) => {
                merge_json_object_patch(existing, serde_json::Value::Object(value))?;
            }
            (_, value) => {
                target_object.insert(key, value);
            }
        }
    }

    Ok(())
}

pub(super) const MAX_DELETE_TARGETS: usize = 100;

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
    room_password_repo: RoomPasswordRepository,

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

    consistency: ConsistencyCoordinator,

    /// Optional audit service for logging security-sensitive operations
    audit_service: Option<Arc<AuditService>>,

    /// Optional brute-force protection for room password verification
    brute_force_service: Option<Arc<dyn crate::service::auth::BruteForceProtectionService>>,

    /// Optional settings registry for reading `create_room_need_review` setting
    settings_registry: Option<Arc<crate::service::SettingsRegistry>>,

    /// Optional user notification service for sending admin notifications
    /// (e.g., pending room review alerts)
    user_notification_service: Option<Arc<crate::service::UserNotificationService>>,

    opaque_password_service: Arc<OpaquePasswordService>,
    opaque_password_registration_session_store: Arc<dyn RoomOpaquePasswordRegistrationSessionStore>,
    opaque_password_login_session_store: Arc<dyn RoomOpaquePasswordLoginSessionStore>,

    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    media_file_storage_service: Option<Arc<dyn crate::service::FileStorageService>>,
    room_file_storage_service: Option<Arc<dyn crate::service::FileStorageService>>,
}

pub(crate) use permission_fence_guard::PendingRoomMemberPermissionFence;
use permission_fence_guard::PermissionFenceGuard;

impl std::fmt::Debug for RoomService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomService").finish()
    }
}

impl RoomService {
    fn room_opaque_credential_identifier(room_id: &RoomId) -> Vec<u8> {
        format!("synctv:room-password:{}", room_id.as_i64()).into_bytes()
    }

    fn validate_override_bits_for_role(
        role: RoomRole,
        added_permissions: u64,
        removed_permissions: u64,
    ) -> Result<()> {
        let valid = match role {
            RoomRole::Creator | RoomRole::Admin => {
                RoomAdminPermissionBits::includes_only_defined(added_permissions)
                    && RoomAdminPermissionBits::includes_only_defined(removed_permissions)
            }
            RoomRole::Member => {
                RoomMemberPermissionBits::includes_only_defined(added_permissions)
                    && RoomMemberPermissionBits::includes_only_defined(removed_permissions)
            }
            RoomRole::Guest => {
                RoomGuestPermissionBits::includes_only_defined(added_permissions)
                    && RoomGuestPermissionBits::includes_only_defined(removed_permissions)
            }
        };

        if valid {
            Ok(())
        } else {
            Err(Error::InvalidInput(
                "Permission set includes bits outside the target role permission bitspace"
                    .to_string(),
            ))
        }
    }

    async fn load_authorized_admin_actor(
        &self,
        admin_user_id: &UserId,
    ) -> Result<AuthorizedAdminActor> {
        let admin_user = self.user_service.get_user(admin_user_id).await?;
        AuthorizedAdminActor::new(*admin_user_id, admin_user.username, admin_user.role)
    }

    async fn ensure_target_user_can_join_now_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        target_user_id: &UserId,
    ) -> Result<()> {
        let target_user = self
            .user_service
            .repository
            .get_by_id_for_update_with_executor(target_user_id, &mut **tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {target_user_id} not found")))?;

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

    async fn ensure_room_can_admit_member_now_tx(
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        target_user_id: &UserId,
    ) -> Result<()> {
        let room_state = sqlx::query!(
            r"
            SELECT closed_at,
                   EXISTS (
                       SELECT 1
                       FROM room_bans rb
                       WHERE rb.room_id = rooms.id
                         AND rb.revoked_at IS NULL
                         AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                   ) AS is_banned,
                   EXISTS (
		                       SELECT 1
		                       FROM room_member_kick_cooldowns rmkc
		                       WHERE rmkc.room_id = rooms.id
	                         AND rmkc.user_id = $2
	                         AND rmkc.ends_at > CURRENT_TIMESTAMP
	                   ) AS is_target_in_kick_cooldown
	            FROM rooms
            WHERE id = $1
              AND deleted_at IS NULL
            FOR UPDATE
            ",
            room_id as &RoomId,
            target_user_id as &UserId,
        )
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room_state.closed_at.is_some() {
            return Err(Error::InvalidInput("Room is closed".to_string()));
        }
        let is_banned = room_state
            .is_banned
            .ok_or_else(|| Error::Internal("Room ban EXISTS query returned NULL".to_string()))?;
        if is_banned {
            return Err(Error::Authorization("Room is banned".to_string()));
        }
        let is_target_in_kick_cooldown =
            room_state.is_target_in_kick_cooldown.ok_or_else(|| {
                Error::Internal("Room kick cooldown EXISTS query returned NULL".to_string())
            })?;
        if is_target_in_kick_cooldown {
            return Err(Error::Authorization(
                crate::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE.to_string(),
            ));
        }

        Ok(())
    }

    /// Maximum retry attempts for optimistic lock conflicts on settings updates
    const MAX_RETRIES: u32 = 3;
    /// Base backoff in milliseconds (exponential: 5ms, 10ms, 20ms)
    const BACKOFF_BASE_MS: u64 = 5;
    /// Total timeout for settings updates with retries (seconds)
    /// Prevents unbounded wait times when database operations are slow.
    const SETTINGS_UPDATE_TIMEOUT_SECS: u64 = 5;
    /// TTL for `create_room` distributed lock (seconds)
    /// Accounts for password credential processing, database transaction latency,
    /// and network delays under high load.
    const CREATE_ROOM_LOCK_TTL_SECS: u64 = 30;

    async fn finalize_committed_permission_write_best_effort(
        &self,
        fence: &PermissionWriteFence,
        room_id: &RoomId,
        user_id: &UserId,
        version: i64,
        operation: &'static str,
    ) {
        if let Err(error) = self.commit_permission_write(fence, version).await {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                user_id = %user_id,
                version,
                operation,
                "Failed to finalize permission fence after committed room/member write"
            );
        }
    }

    async fn begin_permission_write(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        db_version: i64,
    ) -> Result<PermissionWriteFence> {
        self.permission_service
            .begin_permission_write(room_id, user_id, db_version)
            .await
    }

    async fn commit_permission_write(
        &self,
        fence: &PermissionWriteFence,
        version: i64,
    ) -> Result<()> {
        self.permission_service
            .commit_permission_write(fence, version)
            .await
    }

    async fn abort_permission_write(&self, fence: &PermissionWriteFence) {
        self.permission_service.abort_permission_write(fence).await;
    }

    async fn reserve_room_member_permission_fences(
        &self,
        room_id: &RoomId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<PendingRoomMemberPermissionFence>> {
        let members = sqlx::query!(
            r#"SELECT room_id as "room_id: RoomId",
                      user_id as "user_id: UserId",
                      version
             FROM room_members
             WHERE room_id = $1
             FOR UPDATE"#,
            room_id as &RoomId,
        )
        .fetch_all(&mut **tx)
        .await?;

        let mut fences = Vec::with_capacity(members.len());
        for member in members {
            let fence = match self
                .begin_permission_write(&member.room_id, &member.user_id, member.version)
                .await
            {
                Ok(fence) => fence,
                Err(error) => {
                    self.abort_room_member_permission_fences(&fences).await;
                    return Err(error);
                }
            };
            fences.push(PendingRoomMemberPermissionFence {
                room_id: member.room_id,
                user_id: member.user_id,
                fence,
            });
        }

        Ok(fences)
    }

    async fn abort_room_member_permission_fences(
        &self,
        fences: &[PendingRoomMemberPermissionFence],
    ) {
        for pending in fences {
            self.abort_permission_write(&pending.fence).await;
        }
    }

    async fn commit_removed_room_member_permission_fences(
        &self,
        fences: Vec<PendingRoomMemberPermissionFence>,
        removed_members: &[RemovedRoomMember],
    ) -> Result<()> {
        let removed_versions = removed_members
            .iter()
            .map(|member| ((member.room_id, member.user_id), member.version))
            .collect::<HashMap<_, _>>();

        let mut first_error = None;
        for pending in fences {
            let Some(version) = removed_versions.get(&(pending.room_id, pending.user_id)) else {
                self.abort_permission_write(&pending.fence).await;
                continue;
            };
            if let Err(error) = self
                .permission_service
                .commit_permission_write(&pending.fence, *version)
                .await
            {
                tracing::warn!(
                    error = %error,
                    room_id = %pending.room_id,
                    user_id = %pending.user_id,
                    "Failed to finalize removed room member permission fence"
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    async fn invalidate_removed_room_member_permission_caches(
        &self,
        removed_members: &[RemovedRoomMember],
    ) {
        for member in removed_members {
            self.permission_service
                .invalidate_removed_member_cache(&member.room_id, &member.user_id)
                .await;
        }
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

        crate::validation::RoomNameValidator::new()
            .validate(&room.name)
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        if room.description.chars().count() > crate::validation::ROOM_DESCRIPTION_MAX {
            return Err(Error::InvalidInput(format!(
                "Room description too long (max {} characters)",
                crate::validation::ROOM_DESCRIPTION_MAX
            )));
        }

        let current = self
            .room_repo
            .get_by_id(&room.id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let mut tx = self.pool.begin().await?;
        if current.name != room.name {
            self.ensure_room_name_available_for_creator_tx(
                &mut tx,
                &current.created_by,
                &room.name,
            )
            .await?;
        }
        let updated = self
            .room_repo
            .update_with_executor(room, old_version, &mut *tx)
            .await?;
        tx.commit().await?;

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
        self.admin_delete_room_as_with_outbox(room_id, actor, None)
            .await
    }

    pub async fn admin_delete_room_as_with_outbox(
        &self,
        room_id: &RoomId,
        actor: &AuthorizedAdminActor,
        outbox_event: Option<NewRealtimeOutboxEvent>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let guard = PermissionFenceGuard::reserve(Arc::new(self.clone()), room_id, &mut tx).await?;

        let impact = match soft_delete_room_and_cleanup_in_tx(&mut tx, room_id).await {
            Ok(impact) => impact,
            Err(error) => {
                guard.abort().await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_realtime_outbox_tx(&mut tx, outbox_event.as_ref())
            .await
        {
            guard.abort().await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            guard.abort().await;
            return Err(error.into());
        }

        if let Err(error) = guard.commit(&impact.removed_members).await {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                "Failed to finalize one or more admin room deletion permission fences after DB commit"
            );
        }

        self.invalidate_room_caches(room_id).await;
        self.invalidate_removed_room_member_permission_caches(&impact.removed_members)
            .await;

        let subscriber_count = self.notification_service.notify_room_deleted(room_id);
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                "Room deleted event had no local subscribers"
            );
        }

        crate::metrics::http::ROOMS_ACTIVE.dec();

        self.write_audit_event(
            actor.user_id(),
            &actor.user_id().to_string(),
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
        )
        .await?;

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
        let creator_orphaned = sqlx::query_scalar!(
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
            ) AS \"orphaned!\"",
            room.created_by as UserId,
        )
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
        let guard = PermissionFenceGuard::reserve(Arc::new(self.clone()), room_id, &mut tx).await?;

        let impact = match soft_delete_room_and_cleanup_in_tx(&mut tx, room_id).await {
            Ok(impact) => impact,
            Err(error) => {
                guard.abort().await;
                return Err(error);
            }
        };
        if let Err(error) = tx.commit().await {
            guard.abort().await;
            return Err(error.into());
        }

        if let Err(error) = guard.commit(&impact.removed_members).await {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                "Failed to finalize one or more orphaned room deletion permission fences after DB commit"
            );
        }

        self.invalidate_room_caches(room_id).await;
        self.invalidate_removed_room_member_permission_caches(&impact.removed_members)
            .await;

        let subscriber_count = self.notification_service.notify_room_deleted(room_id);
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                "Room deleted event had no local subscribers"
            );
        }

        crate::metrics::http::ROOMS_ACTIVE.dec();

        self.write_audit_event(
            actor.user_id(),
            &actor.user_id().to_string(),
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
        )
        .await?;

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
        self.admin_start_playback_as_with_outbox(
            room_id,
            actor,
            media_id,
            playlist_id,
            target,
            None,
        )
        .await
    }

    pub async fn admin_start_playback_as_with_outbox(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Vec<u8>,
        outbox_event_factory: Option<
            crate::service::playback::RealtimeOutboxPlaybackStateEventFactory,
        >,
    ) -> Result<RoomPlaybackState> {
        self.playback_service
            .admin_switch_with_outbox(
                room_id,
                *actor.user_id(),
                media_id,
                playlist_id,
                target,
                outbox_event_factory,
            )
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
        self.admin_stop_playback_as_with_outbox(room_id, actor, None)
            .await
    }

    pub async fn admin_stop_playback_as_with_outbox(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        outbox_event_factory: Option<
            crate::service::playback::RealtimeOutboxPlaybackStateEventFactory,
        >,
    ) -> Result<RoomPlaybackState> {
        self.playback_service
            .admin_reset_with_outbox(room_id, *actor.user_id(), outbox_event_factory)
            .await
    }

    pub async fn admin_update_playback_as_request(
        &self,
        actor: &AuthorizedAdminActor,
        request: crate::service::playback::PlaybackStateUpdateRequest,
    ) -> Result<RoomPlaybackState> {
        if request.actor_user_id != *actor.user_id() {
            return Err(Error::Authorization(
                "Playback state update actor does not match authorized admin actor".to_string(),
            ));
        }
        self.playback_service
            .admin_update_playback_state(request)
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
        self.admin_set_room_password_as(room_id, new_password, None)
            .await
            .map(|_| ())
    }

    pub async fn admin_set_room_password_as(
        &self,
        room_id: &RoomId,
        new_password: Option<&str>,
        actor_user_id: Option<&UserId>,
    ) -> Result<RoomPasswordCredentialState> {
        self.admin_set_room_password_as_internal(room_id, new_password, actor_user_id)
            .await
    }

    pub async fn admin_set_room_password_as_internal(
        &self,
        room_id: &RoomId,
        new_password: Option<&str>,
        actor_user_id: Option<&UserId>,
    ) -> Result<RoomPasswordCredentialState> {
        let _room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.set_room_password_from_plaintext(room_id, actor_user_id, new_password)
            .await
    }

    /// Get reference to media service
    #[must_use]
    pub const fn media_service(&self) -> &MediaService {
        &self.media_service
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

    async fn broadcast_playback_reset_after_entry_deletion(&self, state: RoomPlaybackState) {
        self.playback_service
            .broadcast_playback_reset_after_force_delete(state.clone())
            .await;
    }

    async fn cleanup_deleted_media_file_references(
        &self,
        references: &[crate::models::FileReferenceTarget],
    ) {
        if references.is_empty() {
            return;
        }

        let Some(storage) = self.media_file_storage_service.as_ref() else {
            return;
        };

        if let Err(error) = storage
            .delete_files(FileStorageCleanupOrigin::ReferenceReleased, references)
            .await
        {
            tracing::warn!(
                error = %error,
                file_references = references.len(),
                "Failed to cleanup media file references after entry deletion"
            );
        }
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
        let subscriber_count = self.notification_service.notify_room_deleted(room_id);
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                "Room deleted event after commit had no local subscribers"
            );
        }
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
        playback_state: Option<&RoomPlaybackState>,
    ) {
        self.invalidate_room_caches(room_id).await;

        if let Some(state) = playback_state {
            self.broadcast_playback_reset_after_entry_deletion(state.clone())
                .await;
        }

        for media_id in deleted_media_ids {
            let subscriber_count = self
                .notification_service
                .notify_media_removed(room_id, None, "", *media_id);
            if subscriber_count == 0 {
                tracing::debug!(
                    room_id = %room_id,
                    media_id = %media_id,
                    "Media removed event after user cleanup had no local subscribers"
                );
            }
        }
    }

    async fn finalize_member_resource_cleanup_after_commit(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        cleanup: &MemberResourceCleanupResult,
    ) {
        if cleanup.is_empty() {
            return;
        }

        self.finalize_entry_deletions_after_commit(
            room_id,
            &cleanup.deleted_media_ids,
            cleanup.playback_state.as_ref(),
        )
        .await;

        let username = match self.resolve_actor_username(user_id).await {
            Ok(username) => username,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    user_id = %user_id,
                    "Skipped member resource cleanup notifications because actor username lookup failed"
                );
                return;
            }
        };
        for playlist_id in &cleanup.deleted_playlist_ids {
            let subscriber_count = self.notification_service.notify_playlist_deleted(
                room_id,
                Some(user_id),
                &username,
                *playlist_id,
            );
            if subscriber_count == 0 {
                tracing::debug!(
                    room_id = %room_id,
                    playlist_id = %playlist_id,
                    "Playlist deleted event after member resource cleanup had no local subscribers"
                );
            }
        }
    }

    // Batch Operations

    /// Maximum number of items allowed in a batch operation
    pub const BATCH_SIZE_LIMIT: usize = 100;

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

pub(crate) fn validate_kick_cooldown_seconds(cooldown_seconds: i64) -> Result<()> {
    if cooldown_seconds <= 0 {
        return Err(Error::InvalidInput(
            "kick_cooldown_seconds must be greater than 0".to_string(),
        ));
    }
    if cooldown_seconds > MAX_KICK_COOLDOWN_SECONDS {
        return Err(Error::InvalidInput(format!(
            "kick_cooldown_seconds must be at most {MAX_KICK_COOLDOWN_SECONDS}"
        )));
    }
    Ok(())
}

async fn has_room_permission_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    permission_service: &PermissionService,
    room_id: &RoomId,
    user_id: &UserId,
    permission: RoomPermission,
) -> Result<bool> {
    let row = sqlx::query!(
        r#"
        SELECT rm.role,
               rm.added_permissions,
               rm.removed_permissions,
               rm.admin_added_permissions,
               rm.admin_removed_permissions,
	               rs.value AS "settings_value?: String"
        FROM room_members rm
        LEFT JOIN room_settings rs
          ON rs.room_id = rm.room_id
         AND rs.key = '_settings'
        WHERE rm.room_id = $1
          AND rm.user_id = $2
          AND NOT EXISTS (
              SELECT 1
              FROM room_member_kick_cooldowns rmkc
              WHERE rmkc.room_id = rm.room_id
                AND rmkc.user_id = rm.user_id
                AND rmkc.ends_at > CURRENT_TIMESTAMP
          )
        FOR UPDATE OF rm
        "#,
        room_id.as_i64(),
        user_id.as_i64()
    )
    .fetch_optional(&mut **tx)
    .await?;

    let Some(row) = row else {
        return Ok(false);
    };

    let role = RoomRole::try_from(i32::from(row.role))
        .map_err(|error| Error::Internal(format!("Invalid room member role: {error}")))?;
    if role == RoomRole::Creator {
        return Ok(true);
    }

    let settings = match row.settings_value {
        Some(settings_value) => {
            serde_json::from_str::<RoomSettings>(&settings_value).map_err(|error| {
                Error::Internal(format!("Failed to deserialize room settings: {error}"))
            })?
        }
        None => RoomSettings::default(),
    };

    let mut member = RoomMember::new(*room_id, *user_id, role);
    member.added_permissions = permission_bits_from_signed(row.added_permissions)?;
    member.removed_permissions = permission_bits_from_signed(row.removed_permissions)?;
    member.admin_added_permissions = permission_bits_from_signed(row.admin_added_permissions)?;
    member.admin_removed_permissions = permission_bits_from_signed(row.admin_removed_permissions)?;

    let permissions = permission_service
        .effective_permission_calculator()
        .effective_for_member(&member, &settings);

    Ok(permissions.has(permission))
}

async fn has_active_room_membership_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<bool> {
    let exists = sqlx::query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM room_members rm
            WHERE rm.room_id = $1
              AND rm.user_id = $2
              AND NOT EXISTS (
                  SELECT 1
                  FROM room_member_kick_cooldowns rmkc
                  WHERE rmkc.room_id = rm.room_id
                    AND rmkc.user_id = rm.user_id
                    AND rmkc.ends_at > CURRENT_TIMESTAMP
              )
            FOR UPDATE
        ) AS "exists!"
        "#,
        room_id.as_i64(),
        user_id.as_i64()
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(exists)
}

async fn ensure_actor_has_room_permission_now_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    permission_service: &PermissionService,
    room_id: &RoomId,
    actor_id: &UserId,
    permission: RoomPermission,
) -> Result<()> {
    let room_state = sqlx::query!(
        r"
        SELECT closed_at,
               EXISTS (
                   SELECT 1
                   FROM room_bans rb
                   WHERE rb.room_id = rooms.id
                     AND rb.revoked_at IS NULL
                     AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
               ) AS is_banned
        FROM rooms
        WHERE id = $1
          AND deleted_at IS NULL
        FOR UPDATE
        ",
        room_id as &RoomId,
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

    let is_banned = room_state
        .is_banned
        .ok_or_else(|| Error::Internal("Room ban EXISTS query returned NULL".to_string()))?;
    if is_banned {
        return Err(Error::Authorization("Room is banned".to_string()));
    }
    if room_state.closed_at.is_some() {
        return Err(Error::Authorization("Room is not active".to_string()));
    }

    if !has_room_permission_in_tx(tx, permission_service, room_id, actor_id, permission).await? {
        return Err(Error::Authorization(
            synctv_common::messages::PERMISSION_DENIED.to_string(),
        ));
    }

    Ok(())
}

fn permission_bits_from_signed(bits: i64) -> Result<u64> {
    u64::try_from(bits).map_err(|error| {
        Error::Internal(format!(
            "Invalid negative permission bitmask loaded from database: {error}"
        ))
    })
}

#[cfg(test)]
fn effective_room_permissions_from_base(
    settings: &RoomSettings,
    member: &RoomMember,
    global_default: crate::models::RoomPermissionSet,
) -> crate::models::RoomPermissionSet {
    let calculator = crate::service::permission::EffectivePermissionCalculator::new(
        crate::service::permission::RuntimePermissionDefaults {
            admin: global_default,
            member: global_default,
            guest: global_default,
        },
    );
    calculator.effective_for_member(member, settings)
}

#[cfg(test)]
fn has_room_permission_from_base(
    settings: &RoomSettings,
    member: &RoomMember,
    global_default: crate::models::RoomPermissionSet,
    permission: RoomPermission,
) -> bool {
    if !member.has_permission(permission, crate::models::RoomPermissionSet::all()) {
        return false;
    }

    effective_room_permissions_from_base(settings, member, global_default).has(permission)
}

#[cfg(test)]
#[path = "room_tests.rs"]
mod tests;
