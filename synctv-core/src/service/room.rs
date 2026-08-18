//! Room management service (facade)
//!
//! `RoomService` is the main entry point for room_creation-related business logic.
//! It acts as a facade that coordinates between domain sub-services:
//!
//! - **Core room_creation CRUD** — create, join, leave, delete rooms (handled here)
//! - **Member management** — delegated to [`MemberService`]
//! - **Media management** — delegated to [`MediaService`]
//! - **Playback control** — delegated to [`PlaybackService`]
//! - **Permissions** — delegated to [`PermissionService`]
//! - **Chat** — uses [`ChatRepository`] directly (thin layer)
//!
//! Callers should use `RoomService` for operations that span multiple
//! sub-services or require transaction coordination. For domain-specific
//! operations, callers can access sub-services directly via the accessor
//! methods (`member_service()`, `media_service()`, etc.).
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

use sqlx::PgPool;
use std::sync::Arc;

use crate::{
    cache::{CacheInvalidationRuntime, ConsistencyCoordinator},
    repository::{
        realtime_outbox::{NewRealtimeOutboxEvent, RealtimeOutboxRepository},
        ChatRepository, MediaRepository, PlaylistRepository, RoomMemberRepository,
        RoomPasswordRepository, RoomPlaybackStateRepository, RoomRepository,
        RoomSettingsRepository, RoomTaxonomyRepository,
    },
    service::{
        audit::AuditService, media::MediaService, member::MemberService,
        notification::NotificationService, room_settings::RoomSettingsService, user::UserService,
        OpaquePasswordService, PermissionService, PlaybackService, PlaylistService,
    },
};

mod access;
mod admin;
pub use admin::AuthorizedAdminActor;
mod admin_deletion;
mod admin_password;
mod admin_playback;
mod ban;
mod cache_invalidation;
mod constructor;
pub use constructor::RoomServiceOptions;
mod deletion;
use deletion::{
    apply_delete_entries_impact_in_tx, collect_all_room_playlist_nodes_in_tx,
    collect_deleted_media_ids_in_tx, collect_room_root_media_ids_in_tx,
    delete_entries_result_from_impact, plan_clear_playlist_scope_in_tx,
    plan_delete_entries_in_room_in_tx,
};
mod cover;
pub use cover::CreateRoomCoverUploadSession;
mod creation;
mod creation_policy;
mod creation_request;
mod creation_review;
mod entries;
pub(crate) use entries::EntryDeletionImpact;
pub use entries::{DeleteEntriesPlan, DeleteEntriesRequest, DeleteEntriesResult};
mod entries_clear;
mod entries_effects;
mod entries_outbox;
pub use entries_outbox::RealtimeOutboxDeleteEntriesEventFactory;
mod entries_request;
use entries_request::{normalize_delete_entries_request, pending_delete_entries_plan};
mod guest_access;
mod join;
mod lifecycle;
mod media_playback_chat;
mod member_admission;
mod member_display;
pub use member_display::{
    UpdateMemberDisplayTagWithOutboxRequest, UpdateMemberRemarkNameWithOutboxRequest,
};
mod member_kick;
pub use member_kick::KickMemberOutboxOptions;
mod member_leave;
mod member_mutations;
pub use member_mutations::{AddMemberWithOutboxRequest, AdminAddMemberWithOutboxRequest};
mod member_permissions;
mod member_queries;
pub use member_queries::RealtimeMembershipAccess;
mod member_resource_cleanup;
use member_resource_cleanup::cleanup_member_resources_in_tx;
pub use member_resource_cleanup::MemberResourceCleanupResult;
mod member_review;
mod member_review_approval;
mod member_review_rejection;
pub use member_review_rejection::AdminRejectJoinRequestWithOutbox;
mod member_role_policy;
mod member_roles;
mod member_roles_admin;
pub use member_roles::{MemberPermissionPatch, UpdateMemberWithOutboxRequest};
mod member_update_execution;
mod opaque_sessions;
mod ownership;
pub use opaque_sessions::{
    local_room_opaque_password_login_session_store,
    local_room_opaque_password_registration_session_store,
};
pub use opaque_sessions::{
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
pub use outbox::{
    PermissionChangedOutboxSnapshot, RealtimeOutboxMemberResourceCleanupEventFactory,
    RealtimeOutboxPermissionChangedEventFactory, RealtimeOutboxRoomEventFactory,
    RealtimeOutboxSettingsEventFactory, RealtimeOutboxUserLeftEventFactory, UserLeftOutboxSnapshot,
};
mod password;
mod permission_checks;
mod permission_fence_guard;
mod permission_writes;
mod resource_access;
pub use resource_access::ClientResourceAvailability;
mod room_deletion;
pub(crate) use room_deletion::soft_delete_room_and_cleanup_in_tx;
mod settings;
mod settings_effects;
mod settings_validation;
mod settings_writes;
mod taxonomy;
mod visibility;
pub use creation::CreateRoomWithTaxonomyRequest;
pub(super) use permission_checks::has_active_room_membership_in_tx;
#[cfg(test)]
use permission_checks::has_room_permission_from_base;
pub use taxonomy::RoomCategoryUpdate;

/// Room service for business logic
///
/// This is the main service that coordinates between domain services.
/// Core room_creation operations are handled here, while specific domains are delegated.
#[derive(Clone)]
pub struct RoomService {
    // Database pool for transactions
    pool: PgPool,

    // Optional distributed lock (requires Redis, used in multi-replica mode)
    distributed_lock: Option<Arc<dyn crate::service::CoordinationLock>>,

    clock: Arc<dyn crate::Clock>,

    // Core repositories
    room_repo: RoomRepository,
    taxonomy_repo: RoomTaxonomyRepository,
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

    /// Optional brute-force protection for room_creation password verification
    brute_force_service: Option<Arc<dyn crate::service::BruteForceProtectionService>>,

    /// Optional runtime settings store for reading `approval_required` setting
    runtime_settings_store: Option<Arc<crate::service::RuntimeSettingsStore>>,

    /// Optional user notification service for sending admin notifications
    /// (e.g., pending room_creation review alerts)
    user_notification_service: Option<Arc<crate::service::UserNotificationService>>,

    opaque_password_service: Arc<OpaquePasswordService>,
    opaque_password_registration_session_store: Arc<dyn RoomOpaquePasswordRegistrationSessionStore>,
    opaque_password_login_session_store: Arc<dyn RoomOpaquePasswordLoginSessionStore>,

    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    room_file_storage_service: Option<Arc<dyn crate::service::FileStorageService>>,
}

impl std::fmt::Debug for RoomService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomService").finish()
    }
}

impl RoomService {
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
}

#[cfg(test)]
#[path = "room_tests.rs"]
mod tests;
