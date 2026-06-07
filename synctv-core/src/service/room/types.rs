use std::sync::Arc;

use crate::{
    cache::CacheInvalidationRuntime,
    models::{
        MediaId, PlaylistId, ReviewRequestId, Room, RoomId, RoomPermissionSet, RoomPlaybackState,
        RoomRole, RoomSettings, UserId, UserRole,
    },
    repository::{
        realtime_outbox::{NewRealtimeOutboxEvent, RealtimeOutboxRepository},
        room_member::RemovedRoomMember,
        UserProviderCredentialRepository,
    },
    service::{audit::AuditService, auth::OpaquePasswordService},
    Error, Result,
};

use super::{RoomOpaquePasswordLoginSessionStore, RoomOpaquePasswordRegistrationSessionStore};

#[derive(Debug, Clone)]
pub struct CreateRoomCoverUploadSession {
    pub client_cover_id: Option<String>,
    pub mime_type: String,
    pub size_bytes: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub checksum_sha256: Option<String>,
    pub metadata: serde_json::Value,
}

pub type RealtimeOutboxSettingsEventFactory =
    Arc<dyn Fn(&RoomSettings, i64) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;
pub type RealtimeOutboxRoomEventFactory =
    Arc<dyn Fn(&Room) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;
pub type RealtimeOutboxDeleteEntriesEventFactory =
    Arc<dyn Fn(&DeleteEntriesPlan) -> Result<Vec<NewRealtimeOutboxEvent>> + Send + Sync>;
pub type RealtimeOutboxPermissionChangedEventFactory =
    Arc<dyn Fn(&PermissionChangedOutboxSnapshot) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;
pub type RealtimeOutboxUserLeftEventFactory =
    Arc<dyn Fn(&UserLeftOutboxSnapshot) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;
pub type RealtimeOutboxMemberResourceCleanupEventFactory =
    Arc<dyn Fn(&MemberResourceCleanupResult) -> Result<Vec<NewRealtimeOutboxEvent>> + Send + Sync>;

#[derive(Default)]
pub struct KickMemberOutboxOptions {
    pub permission_changed: Option<RealtimeOutboxPermissionChangedEventFactory>,
    pub cleanup: Option<RealtimeOutboxMemberResourceCleanupEventFactory>,
    pub lifecycle: Option<NewRealtimeOutboxEvent>,
}

#[derive(Debug, Clone)]
pub struct PermissionChangedOutboxSnapshot {
    pub room_id: RoomId,
    pub target_user_id: UserId,
    pub target_username: String,
    pub changed_by: UserId,
    pub changed_by_username: String,
    pub new_permissions: RoomPermissionSet,
    pub role: i32,
    pub added_permissions: RoomPermissionSet,
    pub removed_permissions: RoomPermissionSet,
    pub admin_added_permissions: RoomPermissionSet,
    pub admin_removed_permissions: RoomPermissionSet,
}

#[derive(Debug, Clone)]
pub struct UserLeftOutboxSnapshot {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub username: String,
}

pub struct AdminAddMemberWithOutboxRequest<'a> {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub actor_username: &'a str,
    pub target_user_id: UserId,
    pub role: RoomRole,
    pub notify: bool,
    pub outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MemberPermissionPatch {
    pub apply_permission_update: bool,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
}

pub struct UpdateMemberWithOutboxRequest {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub target_user_id: UserId,
    pub role: Option<RoomRole>,
    pub permissions: MemberPermissionPatch,
    pub outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

pub struct AdminRejectJoinRequestWithOutbox<'a> {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub reviewed_by: Option<&'a UserId>,
    pub actor_username: &'a str,
    pub request_id: ReviewRequestId,
    pub reason: Option<&'a str>,
    pub outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

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

#[derive(Clone)]
pub struct RoomServiceOptions {
    pub distributed_lock: Option<Arc<dyn crate::service::distributed_lock::CoordinationLock>>,
    pub cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    pub version_fence: Option<Arc<dyn crate::cache::VersionFenceStore>>,
    pub playback_l2_cache: Option<crate::cache::PlaybackStateCache>,
    pub room_settings_l2_cache: Option<Arc<dyn crate::cache::CacheL2Backend>>,
    pub room_settings_cache_key_prefix: Option<String>,
    pub credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
    pub credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    pub audit_service: Option<Arc<AuditService>>,
    pub brute_force_service: Option<Arc<dyn crate::service::auth::BruteForceProtectionService>>,
    pub settings_registry: Option<Arc<crate::service::SettingsRegistry>>,
    pub user_notification_service: Option<Arc<crate::service::UserNotificationService>>,
    pub opaque_password_service: Arc<OpaquePasswordService>,
    pub opaque_password_registration_session_store:
        Option<Arc<dyn RoomOpaquePasswordRegistrationSessionStore>>,
    pub opaque_password_login_session_store: Option<Arc<dyn RoomOpaquePasswordLoginSessionStore>>,
    pub realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    pub media_file_storage_service: Option<Arc<dyn crate::service::FileStorageService>>,
    pub room_file_storage_service: Option<Arc<dyn crate::service::FileStorageService>>,
    pub playlist_file_storage_service: Option<Arc<dyn crate::service::FileStorageService>>,
}

impl RoomServiceOptions {
    #[must_use]
    pub fn test_defaults() -> Self {
        Self {
            distributed_lock: None,
            cache_invalidation: None,
            version_fence: None,
            playback_l2_cache: None,
            room_settings_l2_cache: None,
            room_settings_cache_key_prefix: None,
            credential_encryption: None,
            credential_repo: None,
            audit_service: None,
            brute_force_service: None,
            settings_registry: None,
            user_notification_service: None,
            opaque_password_service: Arc::new(OpaquePasswordService::new_ephemeral_for_process()),
            opaque_password_registration_session_store: None,
            opaque_password_login_session_store: None,
            realtime_outbox: None,
            media_file_storage_service: None,
            room_file_storage_service: None,
            playlist_file_storage_service: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeleteEntriesRequest {
    pub playlist_ids: Vec<PlaylistId>,
    pub media_ids: Vec<MediaId>,
    pub force: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeleteEntriesResult {
    pub deleted_playlists: usize,
    pub deleted_media: usize,
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_state: Option<RoomPlaybackState>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeleteEntriesPlan {
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_reset: bool,
    pub playback_state: Option<RoomPlaybackState>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClearPlaylistResult {
    pub deleted_count: i64,
    pub deleted_playlists: usize,
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_state: Option<RoomPlaybackState>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct EntryDeletionImpact {
    pub playlist_nodes: Vec<(PlaylistId, i32)>,
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub deleted_media_file_references: Vec<crate::models::FileReferenceTarget>,
    pub playback_reset: bool,
    pub playback_state: Option<RoomPlaybackState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RoomCleanupImpact {
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub members_deleted: u64,
    pub removed_members: Vec<RemovedRoomMember>,
    pub settings_deleted: u64,
    pub playback_rows_deleted: u64,
    pub chat_deleted: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemberResourceCleanupResult {
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_reset: bool,
    pub playback_state: Option<RoomPlaybackState>,
}

impl MemberResourceCleanupResult {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deleted_playlist_ids.is_empty() && self.deleted_media_ids.is_empty()
    }
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
