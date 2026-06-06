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
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::time::Duration as StdDuration;

use crate::{
    cache::{
        CacheDomain, CacheInvalidationRuntime, ConsistencyCoordinator, VersionFenceReservation,
    },
    models::{
        AddMemberOptions, AuditAction, AuditTargetType, ChatMessage, ChatMessageType, FileBlob,
        FileUploadSession, Media, MediaId, MemberStatus, NewStoredFile, OpaquePasswordRecord,
        PageParams, Playlist, PlaylistId, ReviewRequestId, ReviewStatus, Room,
        RoomAdminPermissionBits, RoomGuestPermissionBits, RoomId, RoomListQuery, RoomMember,
        RoomMemberPermissionBits, RoomPermission, RoomPermissionSet, RoomPlaybackState, RoomRole,
        RoomSettings, RoomStatus, RoomWithCount, UserId, UserListQuery, UserRole, UserStatus,
    },
    repository::{
        media::MediaListItem,
        playlist::PlaylistListItem,
        realtime_outbox::{NewRealtimeOutboxEvent, RealtimeOutboxRepository},
        room_member::{
            KickCooldownInsert, MemberPermissionExactVersionUpdate,
            MemberRolePermissionExactVersionUpdate, RemovedRoomMember,
        },
        ChatRepository, MediaRepository, PlaylistRepository, ReviewRepository,
        RoomMemberRepository, RoomPasswordCredentialState, RoomPasswordRepository,
        RoomPlaybackStateRepository, RoomRepository, RoomSettingsRepository,
        UserProviderCredentialRepository,
    },
    service::{
        audit::{AuditEventParams, AuditService},
        auth::OpaquePasswordService,
        file_storage::FileStorageContext,
        media::MediaService,
        member::{AdminMemberUpdate, MemberService},
        notification::NotificationService,
        permission::{PermissionService, PermissionServiceRuntime, PermissionWriteFence},
        playback::PlaybackService,
        playlist::PlaylistService,
        room_cover_upload_policy,
        room_settings::{RoomSettingsRuntime, RoomSettingsService},
        session_store::RedisJsonSessionStore,
        user::UserService,
        FileStorageCleanupOrigin, ProvidersManager, RoomPasswordPolicy,
    },
    Error, InternalExt, RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile,
};

use serde::{Deserialize, Serialize};

pub const MAX_KICK_COOLDOWN_SECONDS: i64 = 30 * 24 * 60 * 60;
const ROOM_COVER_REFERENCE_KIND: &str = "room_cover";
const ROOM_OPAQUE_REGISTRATION_SESSION_TTL_SECS: u64 = 300;
const ROOM_OPAQUE_LOGIN_SESSION_TTL_SECS: u64 = 300;
const ROOM_OPAQUE_SESSION_CAPACITY: u64 = 10_000;
const ROOM_OPAQUE_REGISTRATION_SESSION_REDIS_NAMESPACE: &str = "room:opaque:password_registration";
const ROOM_OPAQUE_LOGIN_SESSION_REDIS_NAMESPACE: &str = "room:opaque:password_login";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomOpaquePasswordRegistrationSession {
    room_id: RoomId,
    user_id: UserId,
    credential_identifier: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomOpaquePasswordLoginSession {
    room_id: RoomId,
    user_id: UserId,
    expected_password_version: i32,
    server_login_state: Vec<u8>,
    brute_force_subject_key: String,
}

#[derive(Debug, Clone)]
pub struct RoomOpaqueRegistrationStartChallenge {
    pub session_id: String,
    pub registration_response: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct RoomOpaqueLoginStartChallenge {
    pub session_id: String,
    pub credential_response: Vec<u8>,
}

#[derive(Clone, Debug)]
enum RoomPasswordJoinProof {
    None,
    Plaintext(String),
    OpaqueVerified { expected_version: i32 },
}

#[async_trait::async_trait]
pub trait RoomOpaquePasswordRegistrationSessionStore: Send + Sync {
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordRegistrationSession,
        ttl: StdDuration,
    ) -> Result<()>;

    async fn consume(
        &self,
        session_id: &str,
    ) -> Result<Option<RoomOpaquePasswordRegistrationSession>>;
}

#[async_trait::async_trait]
pub trait RoomOpaquePasswordLoginSessionStore: Send + Sync {
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordLoginSession,
        ttl: StdDuration,
    ) -> Result<()>;

    async fn consume(&self, session_id: &str) -> Result<Option<RoomOpaquePasswordLoginSession>>;
}

#[derive(Clone)]
struct RoomOpaqueRegistrationSessionEntry {
    session: RoomOpaquePasswordRegistrationSession,
    ttl: StdDuration,
}

#[derive(Clone)]
struct RoomOpaqueLoginSessionEntry {
    session: RoomOpaquePasswordLoginSession,
    ttl: StdDuration,
}

struct RoomOpaqueRegistrationSessionExpiry;

impl moka::Expiry<String, RoomOpaqueRegistrationSessionEntry>
    for RoomOpaqueRegistrationSessionExpiry
{
    fn expire_after_create(
        &self,
        _key: &String,
        value: &RoomOpaqueRegistrationSessionEntry,
        _now: std::time::Instant,
    ) -> Option<StdDuration> {
        Some(value.ttl)
    }
}

struct RoomOpaqueLoginSessionExpiry;

impl moka::Expiry<String, RoomOpaqueLoginSessionEntry> for RoomOpaqueLoginSessionExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &RoomOpaqueLoginSessionEntry,
        _now: std::time::Instant,
    ) -> Option<StdDuration> {
        Some(value.ttl)
    }
}

pub struct InMemoryRoomOpaquePasswordRegistrationSessionStore {
    entries: moka::sync::Cache<String, RoomOpaqueRegistrationSessionEntry>,
}

pub struct InMemoryRoomOpaquePasswordLoginSessionStore {
    entries: moka::sync::Cache<String, RoomOpaqueLoginSessionEntry>,
}

impl InMemoryRoomOpaquePasswordRegistrationSessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: moka::sync::Cache::builder()
                .max_capacity(ROOM_OPAQUE_SESSION_CAPACITY)
                .expire_after(RoomOpaqueRegistrationSessionExpiry)
                .build(),
        }
    }
}

impl Default for InMemoryRoomOpaquePasswordRegistrationSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryRoomOpaquePasswordLoginSessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: moka::sync::Cache::builder()
                .max_capacity(ROOM_OPAQUE_SESSION_CAPACITY)
                .expire_after(RoomOpaqueLoginSessionExpiry)
                .build(),
        }
    }
}

impl Default for InMemoryRoomOpaquePasswordLoginSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl RoomOpaquePasswordRegistrationSessionStore
    for InMemoryRoomOpaquePasswordRegistrationSessionStore
{
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordRegistrationSession,
        ttl: StdDuration,
    ) -> Result<()> {
        self.entries.insert(
            session_id.to_string(),
            RoomOpaqueRegistrationSessionEntry {
                session: session.clone(),
                ttl,
            },
        );
        Ok(())
    }

    async fn consume(
        &self,
        session_id: &str,
    ) -> Result<Option<RoomOpaquePasswordRegistrationSession>> {
        if self.entries.get(session_id).is_none() {
            return Ok(None);
        }
        Ok(self.entries.remove(session_id).map(|entry| entry.session))
    }
}

#[async_trait::async_trait]
impl RoomOpaquePasswordLoginSessionStore for InMemoryRoomOpaquePasswordLoginSessionStore {
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordLoginSession,
        ttl: StdDuration,
    ) -> Result<()> {
        self.entries.insert(
            session_id.to_string(),
            RoomOpaqueLoginSessionEntry {
                session: session.clone(),
                ttl,
            },
        );
        Ok(())
    }

    async fn consume(&self, session_id: &str) -> Result<Option<RoomOpaquePasswordLoginSession>> {
        if self.entries.get(session_id).is_none() {
            return Ok(None);
        }
        Ok(self.entries.remove(session_id).map(|entry| entry.session))
    }
}

pub struct RedisRoomOpaquePasswordRegistrationSessionStore {
    store: RedisJsonSessionStore,
}

pub struct RedisRoomOpaquePasswordLoginSessionStore {
    store: RedisJsonSessionStore,
}

impl RedisRoomOpaquePasswordRegistrationSessionStore {
    #[must_use]
    pub fn from_runtime(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            store: RedisJsonSessionStore::new(runtime, key_prefix),
        }
    }
}

impl RedisRoomOpaquePasswordLoginSessionStore {
    #[must_use]
    pub fn from_runtime(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            store: RedisJsonSessionStore::new(runtime, key_prefix),
        }
    }
}

#[async_trait::async_trait]
impl RoomOpaquePasswordRegistrationSessionStore
    for RedisRoomOpaquePasswordRegistrationSessionStore
{
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordRegistrationSession,
        ttl: StdDuration,
    ) -> Result<()> {
        self.store
            .store(
                ROOM_OPAQUE_REGISTRATION_SESSION_REDIS_NAMESPACE,
                session_id,
                session,
                ttl,
                "Failed to serialize room OPAQUE registration session",
                "store room OPAQUE registration session in Redis",
            )
            .await
    }

    async fn consume(
        &self,
        session_id: &str,
    ) -> Result<Option<RoomOpaquePasswordRegistrationSession>> {
        self.store
            .consume(
                ROOM_OPAQUE_REGISTRATION_SESSION_REDIS_NAMESPACE,
                session_id,
                "Failed to deserialize room OPAQUE registration session",
                "consume room OPAQUE registration session from Redis",
            )
            .await
    }
}

#[async_trait::async_trait]
impl RoomOpaquePasswordLoginSessionStore for RedisRoomOpaquePasswordLoginSessionStore {
    async fn store(
        &self,
        session_id: &str,
        session: &RoomOpaquePasswordLoginSession,
        ttl: StdDuration,
    ) -> Result<()> {
        self.store
            .store(
                ROOM_OPAQUE_LOGIN_SESSION_REDIS_NAMESPACE,
                session_id,
                session,
                ttl,
                "Failed to serialize room OPAQUE login session",
                "store room OPAQUE login session in Redis",
            )
            .await
    }

    async fn consume(&self, session_id: &str) -> Result<Option<RoomOpaquePasswordLoginSession>> {
        self.store
            .consume(
                ROOM_OPAQUE_LOGIN_SESSION_REDIS_NAMESPACE,
                session_id,
                "Failed to deserialize room OPAQUE login session",
                "consume room OPAQUE login session from Redis",
            )
            .await
    }
}

#[must_use]
pub fn local_room_opaque_password_registration_session_store(
) -> Arc<dyn RoomOpaquePasswordRegistrationSessionStore> {
    Arc::new(InMemoryRoomOpaquePasswordRegistrationSessionStore::new())
}

#[must_use]
pub fn local_room_opaque_password_login_session_store(
) -> Arc<dyn RoomOpaquePasswordLoginSessionStore> {
    Arc::new(InMemoryRoomOpaquePasswordLoginSessionStore::new())
}

#[must_use]
pub fn shared_room_opaque_password_registration_session_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn RoomOpaquePasswordRegistrationSessionStore> {
    Arc::new(RedisRoomOpaquePasswordRegistrationSessionStore::from_runtime(runtime, key_prefix))
}

#[must_use]
pub fn shared_room_opaque_password_login_session_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn RoomOpaquePasswordLoginSessionStore> {
    Arc::new(RedisRoomOpaquePasswordLoginSessionStore::from_runtime(
        runtime, key_prefix,
    ))
}

pub fn room_opaque_password_registration_session_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn RoomOpaquePasswordRegistrationSessionStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let runtime = profile.require_shared_runtime(
                "single-use room OPAQUE password registration session storage",
            )?;
            Ok(shared_room_opaque_password_registration_session_store(
                runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => {
            Ok(shared_room_opaque_password_registration_session_store(
                profile.best_effort_shared_runtime(
                    "single-use room OPAQUE password registration session storage",
                )?,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::LocalOnly => Ok(local_room_opaque_password_registration_session_store()),
    }
}

pub fn room_opaque_password_login_session_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn RoomOpaquePasswordLoginSessionStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let runtime = profile
                .require_shared_runtime("single-use room OPAQUE password login session storage")?;
            Ok(shared_room_opaque_password_login_session_store(
                runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => Ok(shared_room_opaque_password_login_session_store(
            profile.best_effort_shared_runtime(
                "single-use room OPAQUE password login session storage",
            )?,
            profile.key_prefix().to_string(),
        )),
        SharedStateMode::LocalOnly => Ok(local_room_opaque_password_login_session_store()),
    }
}

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

fn room_cover_storage_scope(room_id: RoomId) -> String {
    format!("rooms/{}/cover", room_id.as_i64())
}

#[derive(Debug)]
struct PendingRoomCreationRequest {
    id: RoomId,
    requested_by: UserId,
    name: String,
    description: String,
    settings: RoomSettings,
    opaque_password_record: Option<OpaquePasswordRecord>,
}

struct PendingRoomCreationRequestRow {
    id: RoomId,
    requested_by: UserId,
    name: String,
    description: String,
    settings_payload: Option<serde_json::Value>,
    opaque_password_record: Option<Vec<u8>>,
    opaque_password_credential_identifier: Option<Vec<u8>>,
    opaque_password_ciphersuite: Option<String>,
    opaque_password_server_setup_version: Option<i32>,
}

impl PendingRoomCreationRequestRow {
    fn into_request(self) -> std::result::Result<PendingRoomCreationRequest, sqlx::Error> {
        let settings_payload = self
            .settings_payload
            .unwrap_or_else(|| serde_json::json!({}));
        let settings = serde_json::from_value::<RoomSettings>(settings_payload)
            .map_err(|error| sqlx::Error::Decode(error.into()))?;
        let opaque_password_record = match (
            self.opaque_password_record,
            self.opaque_password_credential_identifier,
            self.opaque_password_ciphersuite,
            self.opaque_password_server_setup_version,
        ) {
            (Some(record), Some(credential_identifier), Some(ciphersuite), Some(version)) => {
                Some(OpaquePasswordRecord {
                    record,
                    credential_identifier,
                    ciphersuite,
                    server_setup_version: version,
                })
            }
            (None, None, None, None) => None,
            _ => {
                return Err(sqlx::Error::Decode(
                    "Incomplete pending room OPAQUE password material".into(),
                ));
            }
        };

        Ok(PendingRoomCreationRequest {
            id: self.id,
            requested_by: self.requested_by,
            name: self.name,
            description: self.description,
            settings,
            opaque_password_record,
        })
    }
}

struct MediaCoverFileReferenceRow {
    id: MediaId,
    storage_backend: String,
    object_key: String,
}

struct CreateRoomCommand {
    name: String,
    description: String,
    created_by: UserId,
    password: Option<String>,
    settings: Option<RoomSettings>,
}

#[derive(Debug, Clone, Copy)]
struct RoomCreationPolicy {
    enforce_creation_toggle: bool,
}

use std::{future::Future, sync::Arc};
use synctv_common::ExecutionControl;

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

fn initial_room_settings(settings: Option<RoomSettings>) -> RoomSettings {
    settings.unwrap_or_default()
}

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

const MAX_DELETE_TARGETS: usize = 100;
const ROOM_JOIN_PENDING_LOCK_NS: i32 = 20_260_419;
const ROOM_NAME_POLICY_LOCK_NS: i32 = 20_260_420;
const ROOM_OWNER_POLICY_LOCK_NS: i32 = 20_260_421;
const ROOM_JOIN_REQUEST_PENDING: ReviewStatus = ReviewStatus::Pending;

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
    /// Stable OPAQUE server setup used for room password credentials.
    ///
    /// Real deployments should pass the same service as `UserService`, derived
    /// from `security.opaque_server_setup_secret`.
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

fn deleted_count_to_i64(value: usize, field: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Internal(format!("{field} exceeds i64::MAX")))
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

#[derive(Debug)]
struct PendingRoomMemberPermissionFence {
    room_id: RoomId,
    user_id: UserId,
    fence: PermissionWriteFence,
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

impl std::fmt::Debug for RoomService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomService").finish()
    }
}

impl RoomService {
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

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

    async fn create_room_creation_request_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        requested_by: &UserId,
        name: &str,
        description: &str,
        settings: &RoomSettings,
        password: Option<&str>,
    ) -> Result<Room> {
        let settings_payload = serde_json::to_value(settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

        let request_id = sqlx::query_scalar!(
            r"
            INSERT INTO room_creation_requests (
                requested_by, name, description, settings_payload, status, requested_at
            )
            VALUES ($1, $2, $3, $4, $5, CURRENT_TIMESTAMP)
            RETURNING id
            ",
            requested_by.as_i64(),
            name,
            description,
            settings_payload,
            i16::from(ReviewStatus::Pending)
        )
        .fetch_one(&mut **tx)
        .await?;

        let mut room =
            Room::new_with_description(name.to_string(), description.to_string(), *requested_by);
        room.id = RoomId::try_from(request_id).map_err(Error::Internal)?;
        if let Some(password) = password {
            let opaque_record = self
                .opaque_password_service
                .register_password(&Self::room_opaque_credential_identifier(&room.id), password)?;
            sqlx::query!(
                r"
                UPDATE room_creation_requests
                SET opaque_password_record = $2,
                    opaque_password_credential_identifier = $3,
                    opaque_password_ciphersuite = $4,
                    opaque_password_server_setup_version = $5
                WHERE id = $1
                ",
                room.id.as_i64(),
                opaque_record.record.as_slice(),
                opaque_record.credential_identifier.as_slice(),
                opaque_record.ciphersuite.as_str(),
                opaque_record.server_setup_version
            )
            .execute(&mut **tx)
            .await?;
        }
        Ok(room)
    }

    async fn load_pending_room_creation_request_for_update(
        request_id: &RoomId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Option<PendingRoomCreationRequest>> {
        let row = sqlx::query_as!(
            PendingRoomCreationRequestRow,
            r#"
            SELECT id AS "id: RoomId",
                   requested_by AS "requested_by: UserId",
                   name,
                   description,
                   settings_payload,
                   opaque_password_record,
                   opaque_password_credential_identifier,
                   opaque_password_ciphersuite,
                   opaque_password_server_setup_version
            FROM room_creation_requests
            WHERE id = $1 AND reviewed_at IS NULL AND status = $2
            FOR UPDATE
            "#,
            request_id.as_i64(),
            i16::from(ReviewStatus::Pending)
        )
        .fetch_optional(&mut **tx)
        .await?;

        row.map(PendingRoomCreationRequestRow::into_request)
            .transpose()
            .map_err(Error::Database)
    }

    async fn ensure_user_can_create_room_now_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: &UserId,
    ) -> Result<()> {
        let user = self
            .user_service
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut **tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        if !user.can_create_room(true) {
            return Err(Error::Authorization(format!(
                "User cannot create rooms while account status is {}",
                user.status
            )));
        }

        Ok(())
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

    fn enforce_current_room_creation_policy(
        &self,
        user_id: &UserId,
        password_enabled: bool,
        policy: RoomCreationPolicy,
    ) -> Result<()> {
        if let Some(ref registry) = self.settings_registry {
            if policy.enforce_creation_toggle {
                if registry.disable_create_room.get()? {
                    tracing::warn!(user_id = %user_id, "Room creation rejected: disable_create_room is true");
                    return Err(Error::Authorization(
                        "Room creation is currently disabled".to_string(),
                    ));
                }
                if !registry.allow_room_creation.get()? {
                    tracing::warn!(user_id = %user_id, "Room creation rejected: allow_room_creation is false");
                    return Err(Error::Authorization(
                        "Room creation is currently disabled".to_string(),
                    ));
                }
            }
            match registry.room_password_policy.get()? {
                RoomPasswordPolicy::Required if !password_enabled => {
                    tracing::warn!(user_id = %user_id, "Room creation rejected: password required by server policy");
                    return Err(Error::InvalidInput(
                        "Room password is required by server policy".to_string(),
                    ));
                }
                RoomPasswordPolicy::Forbidden if password_enabled => {
                    tracing::warn!(user_id = %user_id, "Room creation rejected: passwords not allowed by server policy");
                    return Err(Error::InvalidInput(
                        "Room passwords are not allowed by server policy".to_string(),
                    ));
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn lock_room_name_policy(
        tx: &mut Transaction<'_, Postgres>,
        creator_id: &UserId,
        name: &str,
    ) -> Result<()> {
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1, hashtext($2))",
            ROOM_NAME_POLICY_LOCK_NS,
            format!("{creator_id}:{name}"),
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn lock_room_owner_policy(
        tx: &mut Transaction<'_, Postgres>,
        owner_id: &UserId,
    ) -> Result<()> {
        let lock_key = format!("room-owner-policy:{ROOM_OWNER_POLICY_LOCK_NS}:{owner_id}");
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            lock_key,
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn ensure_room_name_available_for_creator_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        creator_id: &UserId,
        name: &str,
    ) -> Result<()> {
        self.ensure_room_name_available_for_creator_excluding_pending_tx(tx, creator_id, name, None)
            .await
    }

    async fn ensure_room_name_available_for_creator_excluding_pending_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        creator_id: &UserId,
        name: &str,
        excluding_pending_request_id: Option<RoomId>,
    ) -> Result<()> {
        Self::lock_room_name_policy(tx, creator_id, name).await?;
        let exists = RoomRepository::active_name_exists_for_creator_with_executor(
            creator_id, name, &mut **tx,
        )
        .await?;
        let pending_exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM room_creation_requests
                WHERE requested_by = $1
                  AND name = $2
                  AND reviewed_at IS NULL
                  AND status = $3
                  AND ($4::BIGINT IS NULL OR id != $4)
            ) AS "exists!"
            "#,
            creator_id as &UserId,
            name,
            i16::from(ReviewStatus::Pending),
            excluding_pending_request_id.map(|id| id.as_i64()),
        )
        .fetch_one(&mut **tx)
        .await?;
        if exists || pending_exists {
            return Err(Error::AlreadyExists(
                "You already have a room with this name".to_string(),
            ));
        }
        Ok(())
    }

    async fn enforce_room_ownership_limit_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        owner_id: &UserId,
        excluding_room_id: Option<&RoomId>,
    ) -> Result<()> {
        let max_rooms = self
            .settings_registry
            .as_ref()
            .map(|registry| registry.max_rooms_per_user.get())
            .transpose()?
            .unwrap_or(10);

        Self::lock_room_owner_policy(tx, owner_id).await?;

        let owned_room_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) as "count!"
            FROM rooms
            WHERE created_by = $1
              AND deleted_at IS NULL
              AND ($2::BIGINT IS NULL OR id != $2)
            "#,
            owner_id as &UserId,
            excluding_room_id.map(RoomId::as_i64),
        )
        .fetch_one(&mut **tx)
        .await?;

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
    /// Accounts for password credential processing, database transaction latency,
    /// and network delays under high load.
    const CREATE_ROOM_LOCK_TTL_SECS: u64 = 30;

    /// Get the playlist service
    #[must_use]
    pub const fn playlist_service(&self) -> &PlaylistService {
        &self.playlist_service
    }

    #[must_use]
    pub fn file_storage_service(&self) -> Option<&Arc<dyn crate::service::FileStorageService>> {
        self.room_file_storage_service.as_ref()
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
    /// Used by the application realtime event handler to invalidate permission cache
    /// on cross-replica events.
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

    /// Build a room service with test fixture runtime dependencies.
    ///
    /// Composition roots should use the constructors that accept
    /// `RoomServiceOptions` and inject stable OPAQUE/session/cache runtimes.
    pub fn new_for_tests(pool: PgPool, user_service: UserService) -> Result<Self> {
        Self::new_with_options(pool, user_service, RoomServiceOptions::test_defaults())
    }

    pub fn new_with_options(
        pool: PgPool,
        user_service: UserService,
        options: RoomServiceOptions,
    ) -> Result<Self> {
        let provider_instance_repo = Arc::new(crate::repository::ProviderInstanceRepository::new(
            pool.clone(),
        ));
        let provider_instance_manager = Arc::new(crate::service::RemoteProviderManager::new(
            provider_instance_repo,
        ));
        let providers_manager = Arc::new(ProvidersManager::new(provider_instance_manager)?);
        Self::new_with_providers_and_options(pool, user_service, providers_manager, options)
    }

    pub fn new_with_providers_for_tests(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
    ) -> Result<Self> {
        Self::new_with_providers_and_options(
            pool,
            user_service,
            providers_manager,
            RoomServiceOptions::test_defaults(),
        )
    }

    pub fn new_with_providers_and_options(
        pool: PgPool,
        user_service: UserService,
        providers_manager: Arc<ProvidersManager>,
        options: RoomServiceOptions,
    ) -> Result<Self> {
        let permission_service = PermissionService::new_with_runtime(
            RoomMemberRepository::new(pool.clone()),
            RoomRepository::new(pool.clone()),
            PermissionServiceRuntime {
                settings_registry: options.settings_registry.clone(),
                room_settings_repo: Some(RoomSettingsRepository::new(pool.clone())),
                invalidation_service: options.cache_invalidation.clone(),
                version_fence: options.version_fence.clone(),
                member_permission_l2_cache: options.room_settings_l2_cache.clone(),
                member_permission_cache_key_prefix: "member_permission:".to_string(),
                room_settings_l2_cache: options.room_settings_l2_cache.clone(),
                room_settings_cache_key_prefix: options
                    .room_settings_cache_key_prefix
                    .clone()
                    .unwrap_or_else(|| "room_settings:".to_string()),
                ..PermissionServiceRuntime::default()
            },
        )?;
        Ok(Self::new_with_providers_permission_service_and_options(
            pool,
            user_service,
            providers_manager,
            permission_service,
            options,
        ))
    }

    pub fn new_with_providers_and_permission_service_for_tests(
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
            RoomServiceOptions::test_defaults(),
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
        let room_password_repo = RoomPasswordRepository::new(pool.clone());

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

        let playlist_service = PlaylistService::new_with_runtime(
            playlist_repo.clone(),
            permission_service.clone(),
            providers_manager.clone(),
            options.credential_encryption.clone(),
            options.credential_repo.clone(),
            options.realtime_outbox.clone(),
            options.playlist_file_storage_service.clone(),
        );
        let media_service = MediaService::new_with_runtime(
            media_repo.clone(),
            playlist_repo.clone(),
            permission_service.clone(),
            providers_manager,
            notification_service.clone(),
            options.credential_encryption.clone(),
            options.credential_repo.clone(),
            options.realtime_outbox.clone(),
            options.media_file_storage_service.clone(),
        );
        let playback_service = PlaybackService::new_with_runtime(
            playback_repo.clone(),
            permission_service.clone(),
            media_service.clone(),
            user_service.clone(),
            options.cache_invalidation.clone(),
            options.playback_l2_cache.clone(),
            options.version_fence.clone(),
            options.realtime_outbox.clone(),
        );
        let room_settings_service = RoomSettingsService::new_with_version_fence(
            room_settings_repo.clone(),
            options.cache_invalidation.clone(),
            Arc::new(notification_service.clone()),
            RoomSettingsRuntime {
                version_fence: options.version_fence.clone(),
                l2_cache: options.room_settings_l2_cache.clone(),
                cache_key_prefix: options
                    .room_settings_cache_key_prefix
                    .unwrap_or_else(|| "room_settings:".to_string()),
                ..RoomSettingsRuntime::default()
            },
        );

        let version_fence = options
            .version_fence
            .unwrap_or_else(|| Arc::new(crate::cache::NoopVersionFenceStore));

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
            room_password_repo,
            cache_invalidation: options.cache_invalidation,
            audit_service: options.audit_service,
            brute_force_service: options.brute_force_service,
            settings_registry: options.settings_registry,
            user_notification_service: options.user_notification_service,
            opaque_password_service: options.opaque_password_service,
            opaque_password_registration_session_store: options
                .opaque_password_registration_session_store
                .unwrap_or_else(local_room_opaque_password_registration_session_store),
            opaque_password_login_session_store: options
                .opaque_password_login_session_store
                .unwrap_or_else(local_room_opaque_password_login_session_store),
            realtime_outbox: options.realtime_outbox,
            media_file_storage_service: options.media_file_storage_service,
            room_file_storage_service: options.room_file_storage_service,
            consistency: ConsistencyCoordinator::new(version_fence),
        }
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
                .log(AuditEventParams {
                    actor_id: actor_id.to_string(),
                    actor_username: actor_username.to_string(),
                    action,
                    target_type,
                    target_id,
                    details,
                    ip_address: None,
                    user_agent: None,
                })
                .await
            {
                tracing::warn!(error = %e, "Failed to write audit log from RoomService");
            }
        }
    }

    async fn write_audit_event(
        &self,
        actor_id: &UserId,
        actor_username: &str,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: serde_json::Value,
    ) -> Result<()> {
        let Some(ref audit) = self.audit_service else {
            return Ok(());
        };
        audit
            .log(AuditEventParams {
                actor_id: actor_id.to_string(),
                actor_username: actor_username.to_string(),
                action,
                target_type,
                target_id,
                details,
                ip_address: None,
                user_agent: None,
            })
            .await
    }

    async fn membership_snapshot_username(&self, user_id: &UserId) -> Result<String> {
        if *user_id == UserId::MAX {
            return Ok("local-management".to_string());
        }

        self.user_service
            .get_username(user_id)
            .await?
            .ok_or_else(|| Error::NotFound("Membership snapshot user not found".to_string()))
    }

    async fn actor_username_required(&self, user_id: &UserId) -> Result<String> {
        self.user_service
            .get_username(user_id)
            .await?
            .ok_or_else(|| Error::NotFound("Actor user not found".to_string()))
    }

    async fn membership_snapshot_username_tx(
        tx: &mut Transaction<'_, Postgres>,
        user_id: &UserId,
    ) -> Result<String> {
        if *user_id == UserId::MAX {
            return Ok("local-management".to_string());
        }

        sqlx::query_scalar!(
            "SELECT username FROM users WHERE id = $1 AND deleted_at IS NULL",
            user_id as &UserId,
        )
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| Error::NotFound("Membership snapshot user not found".to_string()))
    }

    async fn permission_changed_snapshot_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: RoomId,
        target_user_id: UserId,
        changed_by: UserId,
        member: Option<&RoomMember>,
    ) -> Result<PermissionChangedOutboxSnapshot> {
        let target_username = Self::membership_snapshot_username_tx(tx, &target_user_id).await?;
        let changed_by_username = Self::membership_snapshot_username_tx(tx, &changed_by).await?;
        let room_settings = self
            .room_settings_repo
            .get_for_update(&room_id, &mut **tx)
            .await?;

        let (
            new_permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        ) = if let Some(member) = member.filter(|member| member.is_active()) {
            (
                self.permission_service
                    .effective_member_permissions(member, &room_settings),
                i32::from(member.role),
                RoomPermissionSet(member.added_permissions),
                RoomPermissionSet(member.removed_permissions),
                RoomPermissionSet(member.admin_added_permissions),
                RoomPermissionSet(member.admin_removed_permissions),
            )
        } else {
            (
                RoomPermissionSet::empty(),
                i32::from(RoomRole::Member),
                RoomPermissionSet::empty(),
                RoomPermissionSet::empty(),
                RoomPermissionSet::empty(),
                RoomPermissionSet::empty(),
            )
        };

        Ok(PermissionChangedOutboxSnapshot {
            room_id,
            target_user_id,
            target_username,
            changed_by,
            changed_by_username,
            new_permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        })
    }

    async fn user_left_snapshot(
        &self,
        room_id: RoomId,
        user_id: UserId,
    ) -> Result<UserLeftOutboxSnapshot> {
        Ok(UserLeftOutboxSnapshot {
            room_id,
            user_id,
            username: self.membership_snapshot_username(&user_id).await?,
        })
    }

    async fn insert_permission_changed_outbox_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        snapshot: &PermissionChangedOutboxSnapshot,
        outbox_event_factory: Option<&RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<()> {
        if let Some(event) = outbox_event_factory
            .map(|factory| factory(snapshot))
            .transpose()?
        {
            if let Some(outbox) = &self.realtime_outbox {
                outbox.insert_with_executor(&event, &mut **tx).await?;
            }
        }
        Ok(())
    }

    async fn insert_realtime_outbox_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        outbox_event: Option<&NewRealtimeOutboxEvent>,
    ) -> Result<()> {
        if let (Some(outbox), Some(event)) = (&self.realtime_outbox, outbox_event) {
            outbox.insert_with_executor(event, &mut **tx).await?;
        }
        Ok(())
    }

    async fn insert_realtime_outbox_events_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        outbox_events: &[NewRealtimeOutboxEvent],
    ) -> Result<()> {
        if let Some(outbox) = &self.realtime_outbox {
            for event in outbox_events {
                outbox.insert_with_executor(event, &mut **tx).await?;
            }
        }
        Ok(())
    }

    async fn insert_user_left_outbox_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        snapshot: &UserLeftOutboxSnapshot,
        outbox_event_factory: Option<&RealtimeOutboxUserLeftEventFactory>,
    ) -> Result<()> {
        if let Some(event) = outbox_event_factory
            .map(|factory| factory(snapshot))
            .transpose()?
        {
            if let Some(outbox) = &self.realtime_outbox {
                outbox.insert_with_executor(&event, &mut **tx).await?;
            }
        }
        Ok(())
    }

    /// Create a new room
    ///
    /// All database operations run inside a single transaction so the room is
    /// either fully created or not visible at all — no partially-created rooms.
    /// Duplicate room names for the same creator are a service-level product
    /// policy, checked under a transaction-scoped advisory lock before insert.
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
        self.create_room_with_outbox(name, description, created_by, password, settings, None)
            .await
    }

    pub async fn create_room_with_outbox(
        &self,
        name: String,
        description: String,
        created_by: UserId,
        password: Option<String>,
        settings: Option<RoomSettings>,
        outbox_event_factory: Option<RealtimeOutboxRoomEventFactory>,
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
                    let outbox_event_factory = outbox_event_factory.clone();
                    async move {
                        self.do_create_room(
                            name,
                            description,
                            created_by,
                            password,
                            settings,
                            outbox_event_factory,
                        )
                        .await
                    }
                },
            )
            .await;
        }

        self.do_create_room(
            name,
            description,
            created_by,
            password,
            settings,
            outbox_event_factory,
        )
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
        outbox_event_factory: Option<RealtimeOutboxRoomEventFactory>,
    ) -> Result<(Room, RoomMember)> {
        self.do_create_room_with_policy(
            CreateRoomCommand {
                name,
                description,
                created_by,
                password,
                settings,
            },
            true,
            outbox_event_factory,
        )
        .await
    }

    async fn do_create_room_with_policy(
        &self,
        command: CreateRoomCommand,
        enforce_creation_policy: bool,
        outbox_event_factory: Option<RealtimeOutboxRoomEventFactory>,
    ) -> Result<(Room, RoomMember)> {
        let CreateRoomCommand {
            name,
            description,
            created_by,
            password,
            settings,
        } = command;
        let password_enabled = password.is_some();
        let room_settings = initial_room_settings(settings);
        room_settings.validate()?;

        tracing::info!(
            user_id = %created_by,
            room_name = %name,
            password_provided = password_enabled,
            password_enabled,
            "Creating new room"
        );

        self.enforce_current_room_creation_policy(
            &created_by,
            password_enabled,
            RoomCreationPolicy {
                enforce_creation_toggle: enforce_creation_policy,
            },
        )?;

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

        let need_review = self
            .settings_registry
            .as_ref()
            .map(|registry| registry.create_room_need_review.get())
            .transpose()?
            .unwrap_or(false);

        if need_review {
            tracing::info!(
                user_id = %created_by,
                room_name = %name,
                "Room requires review, creating room creation request"
            );

            let mut tx = self.pool.begin().await?;
            self.ensure_user_can_create_room_now_tx(&mut tx, &created_by)
                .await?;
            self.enforce_room_ownership_limit_tx(&mut tx, &created_by, None)
                .await?;
            self.ensure_room_name_available_for_creator_tx(&mut tx, &created_by, &name)
                .await?;
            let pending_room = self
                .create_room_creation_request_tx(
                    &mut tx,
                    &created_by,
                    &name,
                    &description,
                    &room_settings,
                    password.as_deref(),
                )
                .await?;
            tx.commit().await?;
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

        self.ensure_user_can_create_room_now_tx(&mut tx, &created_by)
            .await?;
        self.enforce_room_ownership_limit_tx(&mut tx, &created_by, None)
            .await?;
        self.ensure_room_name_available_for_creator_tx(&mut tx, &created_by, &name)
            .await?;

        // 1. Create room
        let room = Room::new_with_description(name, description, created_by);
        let created_room = self.room_repo.create_with_executor(&room, &mut *tx).await?;
        if let Some(password) = password.as_deref() {
            let opaque_record = self.opaque_password_service.register_password(
                &Self::room_opaque_credential_identifier(&created_room.id),
                password,
            )?;
            self.room_password_repo
                .set_opaque_credential_with_executor(&created_room.id, &opaque_record, &mut *tx)
                .await?;
        }

        // 2. Set room settings
        self.room_settings_repo
            .set_settings_with_executor(&created_room.id, &room_settings, &mut *tx)
            .await?;

        // 3. Add creator as member with full permissions
        let member = RoomMember::new(created_room.id, created_by, RoomRole::Creator);
        let created_member = self.member_repo.add_with_executor(&member, &mut tx).await?;

        // 4. Initialize playback state
        self.playback_repo
            .create_or_get_with_executor(&created_room.id, &mut tx)
            .await?;

        if let Some(event) = outbox_event_factory
            .as_ref()
            .map(|factory| factory(&created_room))
            .transpose()?
        {
            if let Some(outbox) = &self.realtime_outbox {
                outbox.insert_with_executor(&event, &mut *tx).await?;
            }
        }

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
            .seed_added_member_cache(&created_room.id, &created_by, created_member.version)
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
        self.transfer_room_ownership_with_outbox(room_id, current_owner_id, new_owner_id, None)
            .await
    }

    pub async fn transfer_room_ownership_with_outbox(
        &self,
        room_id: RoomId,
        current_owner_id: UserId,
        new_owner_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
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

        let mut tx = self.pool.begin().await?;
        let current_owner_username =
            Self::membership_snapshot_username_tx(&mut tx, &current_owner_id).await?;
        self.enforce_room_ownership_limit_tx(&mut tx, &new_owner_id, Some(&room_id))
            .await?;
        self.ensure_room_name_available_for_creator_tx(&mut tx, &new_owner_id, &room.name)
            .await?;

        let current_owner_fence = self
            .begin_permission_write(&room_id, &current_owner_id, current_owner_member.version)
            .await?;
        let new_owner_fence = match self
            .begin_permission_write(&room_id, &new_owner_id, new_owner_member.version)
            .await
        {
            Ok(fence) => fence,
            Err(error) => {
                self.abort_permission_write(&current_owner_fence).await;
                return Err(error);
            }
        };

        let updated_room = self
            .room_repo
            .transfer_ownership_with_executor(&room_id, &new_owner_id, &mut *tx)
            .await;
        let updated_room = match updated_room {
            Ok(room) => room,
            Err(error) => {
                self.abort_permission_write(&current_owner_fence).await;
                self.abort_permission_write(&new_owner_fence).await;
                return Err(error);
            }
        };

        let updated_current_owner = if current_owner_fence.version() > 0 {
            match self
                .member_repo
                .update_role_with_exact_version_executor(
                    &room_id,
                    &current_owner_id,
                    RoomRole::Admin,
                    current_owner_member.version,
                    current_owner_fence.version(),
                    &mut *tx,
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.abort_permission_write(&current_owner_fence).await;
                    self.abort_permission_write(&new_owner_fence).await;
                    return Err(error);
                }
            }
        } else {
            match self
                .member_repo
                .update_role_with_version_executor(
                    &room_id,
                    &current_owner_id,
                    RoomRole::Admin,
                    current_owner_member.version,
                    &mut *tx,
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.abort_permission_write(&current_owner_fence).await;
                    self.abort_permission_write(&new_owner_fence).await;
                    return Err(error);
                }
            }
        };
        let updated_new_owner = if new_owner_fence.version() > 0 {
            match self
                .member_repo
                .update_role_with_exact_version_executor(
                    &room_id,
                    &new_owner_id,
                    RoomRole::Creator,
                    new_owner_member.version,
                    new_owner_fence.version(),
                    &mut *tx,
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.abort_permission_write(&current_owner_fence).await;
                    self.abort_permission_write(&new_owner_fence).await;
                    return Err(error);
                }
            }
        } else {
            match self
                .member_repo
                .update_role_with_version_executor(
                    &room_id,
                    &new_owner_id,
                    RoomRole::Creator,
                    new_owner_member.version,
                    &mut *tx,
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.abort_permission_write(&current_owner_fence).await;
                    self.abort_permission_write(&new_owner_fence).await;
                    return Err(error);
                }
            }
        };
        let current_owner_snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                current_owner_id,
                current_owner_id,
                Some(&updated_current_owner),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(&current_owner_fence).await;
                self.abort_permission_write(&new_owner_fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(
                &mut tx,
                &current_owner_snapshot,
                outbox_event_factory.as_ref(),
            )
            .await
        {
            self.abort_permission_write(&current_owner_fence).await;
            self.abort_permission_write(&new_owner_fence).await;
            return Err(error);
        }
        let new_owner_snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                new_owner_id,
                current_owner_id,
                Some(&updated_new_owner),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(&current_owner_fence).await;
                self.abort_permission_write(&new_owner_fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(
                &mut tx,
                &new_owner_snapshot,
                outbox_event_factory.as_ref(),
            )
            .await
        {
            self.abort_permission_write(&current_owner_fence).await;
            self.abort_permission_write(&new_owner_fence).await;
            return Err(error);
        }

        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&current_owner_fence).await;
            self.abort_permission_write(&new_owner_fence).await;
            return Err(error.into());
        }

        self.finalize_committed_permission_write_best_effort(
            &current_owner_fence,
            &room_id,
            &current_owner_id,
            updated_current_owner.version,
            "transfer_room_ownership_with_outbox:current_owner",
        )
        .await;
        self.finalize_committed_permission_write_best_effort(
            &new_owner_fence,
            &room_id,
            &new_owner_id,
            updated_new_owner.version,
            "transfer_room_ownership_with_outbox:new_owner",
        )
        .await;
        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &current_owner_id)
            .await;
        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &new_owner_id)
            .await;

        self.invalidate_room_caches(&room_id).await;
        self.notify_room_settings_invalidation(&room_id).await;

        self.audit_log(
            &current_owner_id,
            &current_owner_username,
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

    /// Join a room
    ///
    /// Optimized: fetches room, ban-status, settings, and password credential in a
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
        self.join_room_with_outbox(room_id, user_id, password, None)
            .await
    }

    pub async fn join_room_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        password: Option<String>,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<(Room, RoomMember, Vec<crate::models::RoomMemberWithUser>)> {
        let proof = password.map_or(
            RoomPasswordJoinProof::None,
            RoomPasswordJoinProof::Plaintext,
        );
        self.join_room_with_password_proof(room_id, user_id, proof, outbox_event_factory)
            .await
    }

    async fn join_room_with_password_proof(
        &self,
        room_id: RoomId,
        user_id: UserId,
        password_proof: RoomPasswordJoinProof,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<(Room, RoomMember, Vec<crate::models::RoomMemberWithUser>)> {
        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            has_password = matches!(password_proof, RoomPasswordJoinProof::Plaintext(_) | RoomPasswordJoinProof::OpaqueVerified { .. }),
            "User attempting to join room"
        );

        // Verify password before acquiring the lock.
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

        if ctx.is_in_kick_cooldown {
            tracing::warn!(room_id = %room_id, user_id = %user_id, "Kicked user attempted to join room during cooldown");
            return Err(Error::Authorization(
                crate::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE.to_string(),
            ));
        }
        if self
            .member_repo
            .is_in_kick_cooldown(&room_id, &user_id)
            .await?
        {
            tracing::warn!(room_id = %room_id, user_id = %user_id, "Kicked user attempted to join room during cooldown");
            return Err(Error::Authorization(
                crate::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE.to_string(),
            ));
        }

        self.verify_room_password_join_proof(&ctx, &room_id, &user_id, &password_proof)?;

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
                    let password_proof = password_proof.clone();
                    let outbox_event_factory = outbox_event_factory.clone();
                    async move {
                        // Re-validate state under lock to catch changes that occurred
                        // between the initial check and lock acquisition.
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
                        if fresh_ctx.is_in_kick_cooldown {
                            return Err(Error::Authorization(
                                crate::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE
                                    .to_string(),
                            ));
                        }
                        if self
                            .member_repo
                            .is_in_kick_cooldown(&room_id, &user_id)
                            .await?
                        {
                            return Err(Error::Authorization(
                                crate::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE
                                    .to_string(),
                            ));
                        }

                        self.verify_room_password_join_proof(
                            &fresh_ctx,
                            &room_id,
                            &user_id,
                            &password_proof,
                        )?;

                        self.do_join_room(
                            fresh_ctx.room,
                            fresh_ctx.settings,
                            room_id,
                            user_id,
                            outbox_event_factory,
                        )
                        .await
                    }
                },
            )
            .await;
        }

        // Single-replica path: no distributed lock, rely on DB-level constraints
        let room = ctx.room;
        self.do_join_room(room, ctx.settings, room_id, user_id, outbox_event_factory)
            .await
    }

    fn verify_room_password_join_proof(
        &self,
        ctx: &crate::repository::room::JoinRoomContext,
        room_id: &RoomId,
        user_id: &UserId,
        proof: &RoomPasswordJoinProof,
    ) -> Result<()> {
        if !ctx.password_enabled {
            return Ok(());
        }
        match proof {
            RoomPasswordJoinProof::Plaintext(password) => {
                let credential = ctx.password_credential.as_ref().ok_or_else(|| {
                    tracing::warn!(room_id = %room_id, "Room requires password but none is set");
                    Error::Authorization("Invalid password".to_string())
                })?;
                if !self
                    .opaque_password_service
                    .verify_password(credential, password)?
                {
                    tracing::warn!(room_id = %room_id, user_id = %user_id, "Invalid password provided");
                    return Err(Error::Authorization("Invalid password".to_string()));
                }
                Ok(())
            }
            RoomPasswordJoinProof::OpaqueVerified { expected_version } => {
                let current_version = ctx.password_version.ok_or_else(|| {
                    tracing::warn!(room_id = %room_id, "Room requires password but credential version is missing");
                    Error::Authorization("Invalid password".to_string())
                })?;
                if current_version != *expected_version {
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
                Ok(())
            }
            RoomPasswordJoinProof::None => {
                tracing::warn!(room_id = %room_id, user_id = %user_id, "Password required but not provided");
                Err(Error::Authorization("Password required".to_string()))
            }
        }
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
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
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

        // AddMemberOptions::new() defaults to check_max_members=false; explicitly
        // enforce the room's max_members inside the same transaction as the join
        // and realtime outbox insert.
        let options = AddMemberOptions::new().with_max_members(settings.max_members.0);
        let member = RoomMember::new(room_id, user_id, RoomRole::Member);
        let username = self.actor_username_required(&user_id).await?;
        let mut tx = self.pool.begin().await?;
        let created_member = match self
            .member_repo
            .add_with_options_tx(&member, &options, &mut tx)
            .await
        {
            Ok(member) => member,
            Err(Error::AlreadyExists(_)) => {
                tracing::debug!(
                    room_id = %room_id,
                    user_id = %user_id,
                    "User is already a member of the room (idempotent join)"
                );
                tx.rollback().await?;
                let existing_member =
                    self.member_repo
                        .get(&room_id, &user_id)
                        .await?
                        .ok_or_else(|| {
                            Error::Internal("Member disappeared after AlreadyExists".to_string())
                        })?;
                let members = self.member_service.list_members(&room_id).await?;
                self.touch_room_activity(room_id).await;
                return Ok((room, existing_member, members));
            }
            Err(e) => return Err(e),
        };
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                user_id,
                user_id,
                Some(&created_member),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;

        self.permission_service
            .seed_added_member_cache(&room_id, &user_id, created_member.version)
            .await;

        // Get all members
        let members = self.member_service.list_members(&room_id).await?;

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
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1, hashtext($2))",
            ROOM_JOIN_PENDING_LOCK_NS,
            format!("{room_id}:{user_id}"),
        )
        .execute(&mut *tx)
        .await?;

        let existing_request_id = sqlx::query_scalar!(
            r#"
            SELECT id
            FROM room_join_requests
            WHERE room_id = $1
              AND user_id = $2
              AND reviewed_at IS NULL
            LIMIT 1
            "#,
            room_id.as_i64(),
            user_id.as_i64(),
        )
        .fetch_optional(&mut *tx)
        .await?;

        if existing_request_id.is_none() {
            let insert_result = sqlx::query!(
                r#"
                INSERT INTO room_join_requests (
                    room_id, user_id, requested_role, status, requested_at
                )
                VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP)
                "#,
                room_id.as_i64(),
                user_id.as_i64(),
                i16::from(role),
                i16::from(ROOM_JOIN_REQUEST_PENDING),
            )
            .execute(&mut *tx)
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

        tx.commit().await?;
        let pending = RoomMember::new(*room_id, *user_id, role);
        Ok(pending)
    }

    async fn load_pending_join_request_by_id_for_update(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        request_id: ReviewRequestId,
    ) -> Result<(UserId, RoomRole)> {
        let row = sqlx::query!(
            r#"
            SELECT user_id AS "user_id: UserId",
                   COALESCE(requested_role, $3) AS "requested_role!: RoomRole"
            FROM room_join_requests
            WHERE id = $1
              AND room_id = $2
              AND reviewed_at IS NULL
              AND status = $4
            FOR UPDATE
            "#,
            request_id.as_i64(),
            room_id.as_i64(),
            i16::from(RoomRole::Member),
            i16::from(ROOM_JOIN_REQUEST_PENDING),
        )
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| Error::NotFound("Pending join request not found".to_string()))?;

        Ok((row.user_id, row.requested_role))
    }

    async fn resolve_pending_join_request_as_approved_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        reviewed_by: Option<&UserId>,
    ) -> Result<u64> {
        ReviewRepository::approve_room_join_by_member_with_executor(
            &mut **tx,
            *room_id,
            *user_id,
            reviewed_by.copied(),
        )
        .await
    }

    async fn resolve_pending_join_request_by_id_as_approved_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        request_id: ReviewRequestId,
        room_id: &RoomId,
        reviewed_by: Option<&UserId>,
    ) -> Result<u64> {
        ReviewRepository::approve_room_join_with_executor(
            &mut **tx,
            request_id,
            *room_id,
            reviewed_by.copied(),
        )
        .await
    }

    async fn active_member_add_options(&self, room_id: &RoomId) -> Result<AddMemberOptions> {
        let room_settings = self.room_settings_repo.get(room_id).await?;
        Ok(AddMemberOptions::new().with_max_members(room_settings.max_members.0))
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
        self.ensure_target_user_can_join_now_tx(tx, target_user_id)
            .await?;
        Self::ensure_room_can_admit_member_now_tx(tx, room_id, target_user_id).await?;

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

    async fn approve_pending_join_request_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        request_id: ReviewRequestId,
        reviewed_by: Option<&UserId>,
    ) -> Result<(UserId, RoomMember)> {
        let (target_user_id, requested_role) =
            Self::load_pending_join_request_by_id_for_update(tx, room_id, request_id).await?;
        self.ensure_target_user_can_join_now_tx(tx, &target_user_id)
            .await?;
        Self::ensure_room_can_admit_member_now_tx(tx, room_id, &target_user_id).await?;
        let role = Self::validate_join_request_role(requested_role)?;

        let mut member = RoomMember::new(*room_id, target_user_id, role);
        member.status = MemberStatus::Active;
        let options = self.active_member_add_options(room_id).await?;
        let created = self
            .member_repo
            .add_with_options_tx(&member, &options, tx)
            .await?;
        let resolved = Self::resolve_pending_join_request_by_id_as_approved_tx(
            tx,
            request_id,
            room_id,
            reviewed_by,
        )
        .await?;
        if resolved == 0 {
            return Err(Error::NotFound(
                "Pending join request not found".to_string(),
            ));
        }

        Ok((target_user_id, created))
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
        self.add_member_with_outbox(room_id, actor_id, target_user_id, role, notify, None)
            .await
    }

    pub async fn add_member_with_outbox(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        target_user_id: UserId,
        role: RoomRole,
        notify: bool,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<RoomMember> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.ensure_room_creator_is_active_for_access(&room, &actor_id)
            .await?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &actor_id,
                crate::models::RoomPermission::ADD_MEMBER,
            )
            .await?;

        self.ensure_target_user_can_join(&target_user_id).await?;

        let mut tx = self.pool.begin().await?;
        ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &self.permission_service,
            &room_id,
            &actor_id,
            crate::models::RoomPermission::ADD_MEMBER,
        )
        .await?;
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
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&created),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;
        self.permission_service
            .seed_added_member_cache(&room_id, &target_user_id, created.version)
            .await;

        let actor_username = self.actor_username_required(&actor_id).await?;

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
        self.approve_join_request_with_outbox(room_id, actor_id, request_id, None)
            .await
    }

    pub async fn approve_join_request_with_outbox(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        request_id: ReviewRequestId,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<RoomMember> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.ensure_room_creator_is_active_for_access(&room, &actor_id)
            .await?;

        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &actor_id,
                crate::models::RoomPermission::APPROVE_MEMBER,
            )
            .await?;

        let mut tx = self.pool.begin().await?;
        ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &self.permission_service,
            &room_id,
            &actor_id,
            crate::models::RoomPermission::APPROVE_MEMBER,
        )
        .await?;
        let (target_user_id, updated) = self
            .approve_pending_join_request_tx(&mut tx, &room_id, request_id, Some(&actor_id))
            .await?;
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&updated),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;

        self.permission_service
            .seed_added_member_cache(&room_id, &target_user_id, updated.version)
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
        self.reject_join_request_with_outbox(room_id, actor_id, request_id, reason, None)
            .await
    }

    pub async fn reject_join_request_with_outbox(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        request_id: ReviewRequestId,
        reason: Option<&str>,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<UserId> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.ensure_room_creator_is_active_for_access(&room, &actor_id)
            .await?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &actor_id,
                crate::models::RoomPermission::APPROVE_MEMBER,
            )
            .await?;

        let mut tx = self.pool.begin().await?;
        ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &self.permission_service,
            &room_id,
            &actor_id,
            crate::models::RoomPermission::APPROVE_MEMBER,
        )
        .await?;
        let (target_user_id, _) =
            Self::load_pending_join_request_by_id_for_update(&mut tx, &room_id, request_id).await?;
        let rejected = ReviewRepository::reject_room_join_with_executor(
            &mut *tx,
            request_id,
            room_id,
            Some(actor_id),
            reason,
        )
        .await?;
        if rejected == 0 {
            return Err(Error::NotFound(
                "Pending join request not found".to_string(),
            ));
        }
        let snapshot = self
            .permission_changed_snapshot_tx(&mut tx, room_id, target_user_id, actor_id, None)
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        let actor_username = self.actor_username_required(&actor_id).await?;

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
        self.admin_add_member_with_outbox(AdminAddMemberWithOutboxRequest {
            room_id,
            actor_id,
            actor_username,
            target_user_id,
            role,
            notify,
            outbox_event_factory: None,
        })
        .await
    }

    pub async fn admin_add_member_with_outbox(
        &self,
        request: AdminAddMemberWithOutboxRequest<'_>,
    ) -> Result<RoomMember> {
        let AdminAddMemberWithOutboxRequest {
            room_id,
            actor_id,
            actor_username,
            target_user_id,
            role,
            notify,
            outbox_event_factory,
        } = request;
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
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&created),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;
        self.permission_service
            .seed_added_member_cache(&room_id, &target_user_id, created.version)
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
        self.admin_approve_join_request_with_outbox(
            room_id,
            actor_id,
            reviewed_by,
            actor_username,
            request_id,
            None,
        )
        .await
    }

    pub async fn admin_approve_join_request_with_outbox(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        reviewed_by: Option<&UserId>,
        actor_username: &str,
        request_id: ReviewRequestId,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<RoomMember> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let mut tx = self.pool.begin().await?;
        let (target_user_id, updated) = self
            .approve_pending_join_request_tx(&mut tx, &room_id, request_id, reviewed_by)
            .await?;
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&updated),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;

        self.permission_service
            .seed_added_member_cache(&room_id, &target_user_id, updated.version)
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
        self.admin_reject_join_request_with_outbox(AdminRejectJoinRequestWithOutbox {
            room_id,
            actor_id,
            reviewed_by,
            actor_username,
            request_id,
            reason,
            outbox_event_factory: None,
        })
        .await
    }

    pub async fn admin_reject_join_request_with_outbox(
        &self,
        request: AdminRejectJoinRequestWithOutbox<'_>,
    ) -> Result<UserId> {
        let AdminRejectJoinRequestWithOutbox {
            room_id,
            actor_id,
            reviewed_by,
            actor_username,
            request_id,
            reason,
            outbox_event_factory,
        } = request;
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let mut tx = self.pool.begin().await?;
        let (target_user_id, _) =
            Self::load_pending_join_request_by_id_for_update(&mut tx, &room_id, request_id).await?;
        let rejected = ReviewRepository::reject_room_join_with_executor(
            &mut *tx,
            request_id,
            room_id,
            reviewed_by.copied(),
            reason,
        )
        .await?;
        if rejected == 0 {
            return Err(Error::NotFound(
                "Pending join request not found".to_string(),
            ));
        }
        let snapshot = self
            .permission_changed_snapshot_tx(&mut tx, room_id, target_user_id, actor_id, None)
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
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
        self.leave_room_with_outbox(room_id, user_id, None, None)
            .await
    }

    pub async fn leave_room_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxUserLeftEventFactory>,
        cleanup_outbox_event_factory: Option<RealtimeOutboxMemberResourceCleanupEventFactory>,
    ) -> Result<()> {
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

        let snapshot = self.user_left_snapshot(room_id, user_id).await?;
        let mut tx = self.pool.begin().await?;
        let Some(observed_version) = (match self
            .member_repo
            .active_member_version_for_update_with_executor(&room_id, &user_id, &mut tx)
            .await
        {
            Ok(version) => version,
            Err(error) => return Err(error),
        }) else {
            return Err(Error::NotFound(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        };
        let fence = self
            .begin_permission_write(&room_id, &user_id, observed_version)
            .await?;
        let removed_version = match self
            .member_repo
            .remove_with_version_executor(&room_id, &user_id, &mut tx)
            .await
        {
            Ok(version) => version,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let Some(removed_version) = removed_version else {
            self.abort_permission_write(&fence).await;
            return Err(Error::NotFound(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        };
        let cleanup = match cleanup_member_resources_in_tx(&mut tx, &room_id, &user_id).await {
            Ok(cleanup) => cleanup,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let cleanup_outbox_events = cleanup_outbox_event_factory
            .as_ref()
            .map(|factory| factory(&cleanup))
            .transpose()?
            .unwrap_or_default();
        if let Err(error) = self
            .insert_user_left_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_realtime_outbox_events_tx(&mut tx, &cleanup_outbox_events)
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            &fence,
            &room_id,
            &user_id,
            removed_version,
            "leave_room_with_outbox",
        )
        .await;

        self.permission_service
            .invalidate_removed_member_cache(&room_id, &user_id)
            .await;
        self.finalize_member_resource_cleanup_after_commit(&room_id, &user_id, &cleanup)
            .await;

        // Notify room members with username
        let username = snapshot.username;
        let _ = self
            .notification_service
            .notify_user_left(&room_id, &user_id, &username);

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            username = %username,
            deleted_playlists = cleanup.deleted_playlist_ids.len(),
            deleted_media = cleanup.deleted_media_ids.len(),
            "User left room"
        );

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
            let enable_guest = registry.enable_guest.get()?;
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
        let password_enabled = self
            .room_password_repo
            .get_state(room_id)
            .await?
            .is_some_and(|state| state.enabled);
        if password_enabled {
            tracing::debug!(room_id = %room_id, "Guest access denied: room has password");
            return Err(Error::Authorization(
                "Guests cannot join password-protected rooms. Please create an account and join as a member.".to_string(),
            ));
        }

        tracing::debug!(room_id = %room_id, "Guest access allowed");
        Ok(())
    }

    /// Return the effective room permissions for guests.
    ///
    /// This is the single entry point for combining the global guest default
    /// permission set with room-level guest added/removed permissions.
    pub async fn get_guest_permissions(&self, room_id: &RoomId) -> Result<RoomPermissionSet> {
        let settings = self.get_room_settings(room_id).await?;
        Ok(self
            .permission_service
            .effective_permission_calculator()
            .role_default(&RoomRole::Guest, &settings))
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
    /// - room members with DELETE_ROOM can delete the room
    /// - global admin/root can delete any room
    pub async fn delete_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        self.delete_room_with_outbox(room_id, user_id, None).await
    }

    pub async fn delete_room_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        outbox_event: Option<NewRealtimeOutboxEvent>,
    ) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "Soft-deleting room");

        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found or already deleted".to_string()))?;

        let actor = self.user_service.get_user(&user_id).await?;
        let is_global_admin = actor.role.is_admin_or_above();
        let is_creator = room.created_by == user_id;
        let has_room_delete_permission = if is_creator || is_global_admin {
            true
        } else {
            self.permission_service
                .check_permission_no_cache(&room_id, &user_id, RoomPermission::DELETE_ROOM)
                .await
                .is_ok()
        };

        if !is_creator && !is_global_admin && !has_room_delete_permission {
            if self.member_repo.get(&room_id, &user_id).await?.is_some() {
                return Err(Error::Authorization(
                    "Only the room creator, a member with delete_room permission, or a global admin can delete this room".to_string(),
                ));
            }

            return Err(Error::Authorization(
                "You are not a member of this room".to_string(),
            ));
        }

        let mut tx = self.pool.begin().await?;
        let permission_fences = self
            .reserve_room_member_permission_fences(&room_id, &mut tx)
            .await?;
        let impact = match soft_delete_room_and_cleanup_in_tx(&mut tx, &room_id).await {
            Ok(impact) => impact,
            Err(error) => {
                self.abort_room_member_permission_fences(&permission_fences)
                    .await;
                return Err(error);
            }
        };
        if let (Some(outbox), Some(event)) = (&self.realtime_outbox, &outbox_event) {
            if let Err(error) = outbox.insert_with_executor(event, &mut *tx).await {
                self.abort_room_member_permission_fences(&permission_fences)
                    .await;
                return Err(error);
            }
        }

        // Commit transaction - all or nothing
        if let Err(error) = tx.commit().await {
            self.abort_room_member_permission_fences(&permission_fences)
                .await;
            return Err(error.into());
        }

        if let Err(error) = self
            .commit_removed_room_member_permission_fences(
                permission_fences,
                &impact.removed_members,
            )
            .await
        {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                "Failed to finalize one or more room deletion permission fences after DB commit"
            );
        }

        self.invalidate_room_caches(&room_id).await;
        self.invalidate_removed_room_member_permission_caches(&impact.removed_members)
            .await;

        let subscriber_count = self.notification_service.notify_room_deleted(&room_id);
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                "Room deleted event had no local subscribers"
            );
        }

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
            &actor.username,
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

        let admin_username = if let Some(admin_id) = admin_id {
            let admin = self.user_service.get_user(admin_id).await?;
            if !admin.role.is_admin_or_above() {
                return Err(Error::Authorization(
                    "Only admins can approve rooms".to_string(),
                ));
            }
            Some(admin.username)
        } else {
            None
        };

        let mut tx = self.pool.begin().await?;
        let request = Self::load_pending_room_creation_request_for_update(&request_id, &mut tx)
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "Pending room creation request {request_id} not found"
                ))
            })?;
        let audit_actor_username = match admin_username {
            Some(username) => username,
            None => Self::membership_snapshot_username_tx(&mut tx, &request.requested_by).await?,
        };

        self.ensure_user_can_create_room_now_tx(&mut tx, &request.requested_by)
            .await?;
        self.enforce_current_room_creation_policy(
            &request.requested_by,
            request.opaque_password_record.is_some(),
            RoomCreationPolicy {
                enforce_creation_toggle: true,
            },
        )?;
        self.enforce_room_ownership_limit_tx(&mut tx, &request.requested_by, None)
            .await?;
        self.ensure_room_name_available_for_creator_excluding_pending_tx(
            &mut tx,
            &request.requested_by,
            &request.name,
            Some(request_id),
        )
        .await?;

        let room = Room::new_with_description(
            request.name.clone(),
            request.description.clone(),
            request.requested_by,
        );
        let updated = self.room_repo.create_with_executor(&room, &mut *tx).await?;

        self.room_settings_repo
            .set_settings_with_executor(&updated.id, &request.settings, &mut *tx)
            .await?;
        if let Some(ref opaque_password_record) = request.opaque_password_record {
            self.room_password_repo
                .set_opaque_credential_with_executor(&updated.id, opaque_password_record, &mut *tx)
                .await?;
        }

        let member = RoomMember::new(updated.id, request.requested_by, RoomRole::Creator);
        self.member_repo.add_with_executor(&member, &mut tx).await?;
        self.playback_repo
            .create_or_get_with_executor(&updated.id, &mut tx)
            .await?;

        let approved = ReviewRepository::approve_room_creation_with_executor(
            &mut *tx,
            request_id,
            admin_id.copied(),
        )
        .await?;
        if approved == 0 {
            return Err(Error::NotFound(format!(
                "Pending room creation request {request_id} not found"
            )));
        }

        tx.commit().await?;

        crate::metrics::http::ROOMS_ACTIVE.inc();

        self.notify_room_invalidation(&updated.id).await;
        self.permission_service
            .invalidate_room_cache(&updated.id)
            .await;

        // Audit log
        self.audit_log(
            admin_id.unwrap_or(&request.requested_by),
            &audit_actor_username,
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

        let admin_username = if let Some(admin_id) = admin_id {
            let admin = self.user_service.get_user(admin_id).await?;
            if !admin.role.is_admin_or_above() {
                return Err(Error::Authorization(
                    "Only admins can reject rooms".to_string(),
                ));
            }
            Some(admin.username)
        } else {
            None
        };

        let mut tx = self.pool.begin().await?;
        let request = Self::load_pending_room_creation_request_for_update(&room_id, &mut tx)
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!("Pending room creation request {room_id} not found"))
            })?;
        let audit_actor_username = match admin_username {
            Some(username) => username,
            None => Self::membership_snapshot_username_tx(&mut tx, &request.requested_by).await?,
        };

        let rejected = ReviewRepository::reject_room_creation_with_executor(
            &mut *tx,
            room_id,
            admin_id.copied(),
            reason.as_deref(),
        )
        .await?;
        if rejected == 0 {
            return Err(Error::NotFound(format!(
                "Pending room creation request {room_id} not found"
            )));
        }
        tx.commit().await?;

        let mut updated =
            Room::new_with_description(request.name, request.description, request.requested_by);
        updated.id = request.id;

        // Audit log
        self.audit_log(
            admin_id.unwrap_or(&updated.created_by),
            &audit_actor_username,
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

        let total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM room_creation_requests
            WHERE reviewed_at IS NULL AND status = $1
            "#,
            i16::from(ReviewStatus::Pending),
        )
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query!(
            r#"
            SELECT id AS "id: RoomId",
                   requested_by AS "requested_by: UserId",
                   name,
                   description,
                   requested_at
            FROM room_creation_requests
            WHERE reviewed_at IS NULL AND status = $1
            ORDER BY requested_at DESC, id DESC
            LIMIT $2 OFFSET $3
            "#,
            i16::from(ReviewStatus::Pending),
            pagination.limit_i64()?,
            pagination.offset_i64()?,
        )
        .fetch_all(&self.pool)
        .await?;

        let rooms = rows
            .into_iter()
            .map(|row| {
                let requested_at = row.requested_at;
                Room {
                    id: row.id,
                    name: row.name,
                    description: row.description,
                    cover_file_reference_id: None,
                    created_by: row.requested_by,
                    status: RoomStatus::Active,
                    is_banned: false,
                    closed_at: None,
                    created_at: requested_at,
                    updated_at: requested_at,
                    deleted_at: None,
                    version: 0,
                    last_activity_at: requested_at,
                }
            })
            .collect();

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
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        // Check permission
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        // Validate permission escalation
        settings.validate()?;

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
                    let consistency = self.consistency.clone();
                    async move {
                        let (current, version) =
                            room_settings_repo.get_with_version(&room_id).await?;
                        let domain = CacheDomain::RoomSettings { room_id };
                        let reservation =
                            Self::begin_room_settings_write_with(&consistency, &room_id, version)
                                .await?;
                        let new_version = if let Some(reservation) = &reservation {
                            match room_settings_repo
                                .set_settings_with_exact_version(
                                    &room_id,
                                    &settings,
                                    version,
                                    reservation.version,
                                )
                                .await
                            {
                                Ok(new_version) => {
                                    if let Err(error) = Self::commit_room_settings_write_with(
                                        &consistency,
                                        &domain,
                                        Some(reservation),
                                        new_version,
                                    )
                                    .await
                                    {
                                        tracing::warn!(
                                            error = %error,
                                            domain = %domain,
                                            version = new_version,
                                            operation = "set_settings",
                                            "Failed to finalize room settings fence after committed DB write"
                                        );
                                    }
                                    new_version
                                }
                                Err(error) => {
                                    Self::abort_room_settings_write_with(
                                        &consistency,
                                        &domain,
                                        Some(reservation),
                                    )
                                    .await;
                                    return Err(error);
                                }
                            }
                        } else {
                            room_settings_repo
                                .set_settings_with_version(&room_id, &settings, version)
                                .await?
                        };
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

        if audit_service.is_some() {
            let settings_json = serde_json::to_value(&snapshot.settings)
                .internal_with_err("Failed to serialize settings")?;
            self.write_audit_event(
                &user_id,
                &user_id.to_string(),
                AuditAction::RoomSettingsUpdated,
                AuditTargetType::Room,
                Some(room_id.to_string()),
                settings_json,
            )
            .await?;
        }

        Ok(snapshot)
    }

    async fn begin_room_settings_write_with(
        consistency: &ConsistencyCoordinator,
        room_id: &RoomId,
        db_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        let domain = CacheDomain::RoomSettings { room_id: *room_id };
        consistency.begin_observed_write(&domain, db_version).await
    }

    async fn commit_room_settings_write_with(
        consistency: &ConsistencyCoordinator,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) -> Result<()> {
        consistency
            .commit_reserved_write(domain, reservation, version)
            .await?;
        Ok(())
    }

    async fn abort_room_settings_write_with(
        consistency: &ConsistencyCoordinator,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
    ) {
        consistency.abort_reserved_write(domain, reservation).await;
    }

    async fn begin_room_settings_write(
        &self,
        room_id: &RoomId,
        db_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        Self::begin_room_settings_write_with(&self.consistency, room_id, db_version).await
    }

    async fn commit_room_settings_write(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) -> Result<()> {
        Self::commit_room_settings_write_with(&self.consistency, domain, reservation, version).await
    }

    async fn finalize_committed_room_settings_write_best_effort(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
        operation: &'static str,
    ) {
        if let Err(error) = self
            .commit_room_settings_write(domain, reservation, version)
            .await
        {
            tracing::warn!(
                error = %error,
                domain = %domain,
                version,
                operation,
                "Failed to finalize room settings fence after committed DB write"
            );
        }
    }

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

    async fn abort_room_settings_write(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
    ) {
        Self::abort_room_settings_write_with(&self.consistency, domain, reservation).await;
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
            .get_version_checked(&key)
            .await?
            .unwrap_or(0))
    }

    async fn resolve_actor_username(&self, user_id: &UserId) -> Result<String> {
        self.user_service
            .get_user(user_id)
            .await
            .map(|user| user.username)
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
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.set_room_settings_with_outbox(room_id, settings, None)
            .await
    }

    pub async fn set_room_settings_with_outbox(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        outbox_event_factory: Option<RealtimeOutboxSettingsEventFactory>,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        settings.validate()?;

        let (previous_settings, updated_settings, updated_version) =
            super::optimistic_retry::retry_with_optimistic_lock(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                "Settings update failed after maximum retry attempts",
                || async {
                    let outbox_event_factory = outbox_event_factory.clone();
                    let (current, version) =
                        self.room_settings_repo.get_with_version(room_id).await?;
                    let domain = CacheDomain::RoomSettings { room_id: *room_id };
                    let reservation = self.begin_room_settings_write(room_id, version).await?;
                    let mut tx = match self.pool.begin().await {
                        Ok(tx) => tx,
                        Err(error) => {
                            self.abort_room_settings_write(&domain, reservation.as_ref())
                                .await;
                            return Err(error.into());
                        }
                    };
                    let new_version = if let Some(reservation) = &reservation {
                        match self
                            .room_settings_repo
                            .set_settings_with_exact_version_with_executor(
                                room_id,
                                settings,
                                version,
                                reservation.version,
                                &mut *tx,
                            )
                            .await
                        {
                            Ok(new_version) => new_version,
                            Err(error) => {
                                self.abort_room_settings_write(&domain, Some(reservation))
                                    .await;
                                return Err(error);
                            }
                        }
                    } else {
                        self.room_settings_repo
                            .set_settings_with_version_with_executor(
                                room_id, settings, version, &mut *tx,
                            )
                            .await?
                    };
                    let outbox_event = outbox_event_factory
                        .as_ref()
                        .map(|factory| factory(settings, new_version))
                        .transpose()?;
                    if let (Some(outbox), Some(event)) = (&self.realtime_outbox, &outbox_event) {
                        if let Err(error) = outbox.insert_with_executor(event, &mut *tx).await {
                            self.abort_room_settings_write(&domain, reservation.as_ref())
                                .await;
                            return Err(error);
                        }
                    }
                    if let Err(error) = tx.commit().await {
                        self.abort_room_settings_write(&domain, reservation.as_ref())
                            .await;
                        return Err(error.into());
                    }
                    self.finalize_committed_room_settings_write_best_effort(
                        &domain,
                        reservation.as_ref(),
                        new_version,
                        "set_room_settings_with_outbox",
                    )
                    .await;
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

    /// Patch room settings with optimistic locking.
    ///
    /// The patch is merged into the current stored settings inside each CAS retry,
    /// so concurrent updates to different fields are preserved instead of being
    /// overwritten by a stale pre-merge snapshot.
    pub async fn patch_settings(
        &self,
        room_id: RoomId,
        user_id: UserId,
        patch: serde_json::Value,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.patch_settings_with_outbox(room_id, user_id, patch, None)
            .await
    }

    pub async fn patch_settings_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        patch: serde_json::Value,
        outbox_event_factory: Option<RealtimeOutboxSettingsEventFactory>,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        self.room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        let patch = std::sync::Arc::new(patch);

        let (previous_settings, updated_settings, updated_version) =
            super::optimistic_retry::retry_with_optimistic_lock_timeout(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                std::time::Duration::from_secs(Self::SETTINGS_UPDATE_TIMEOUT_SECS),
                "Settings patch failed after maximum retry attempts",
                || {
                    let patch = patch.clone();
                    let outbox_event_factory = outbox_event_factory.clone();
                    async move {
                        let (current, version) =
                            self.room_settings_repo.get_with_version(&room_id).await?;
                        let mut merged_json = serde_json::to_value(&current)
                            .internal_with_err("Failed to serialize current room settings")?;
                        merge_json_object_patch(&mut merged_json, (*patch).clone())?;
                        let merged_settings: RoomSettings = serde_json::from_value(merged_json)
                            .map_err(|e| {
                                Error::InvalidInput(format!("Invalid settings JSON: {e}"))
                            })?;
                        merged_settings.validate()?;
                        let mut tx = self.pool.begin().await?;
                        ensure_actor_has_room_permission_now_tx(
                            &mut tx,
                            &self.permission_service,
                            &room_id,
                            &user_id,
                            crate::models::RoomPermission::SET_ROOM_SETTINGS,
                        )
                        .await?;
                        let domain = CacheDomain::RoomSettings { room_id };
                        let reservation = self.begin_room_settings_write(&room_id, version).await?;
                        let new_version = if let Some(reservation) = &reservation {
                            match self
                                .room_settings_repo
                                .set_settings_with_exact_version_with_executor(
                                    &room_id,
                                    &merged_settings,
                                    version,
                                    reservation.version,
                                    &mut *tx,
                                )
                                .await
                            {
                                Ok(new_version) => new_version,
                                Err(error) => {
                                    self.abort_room_settings_write(&domain, Some(reservation))
                                        .await;
                                    return Err(error);
                                }
                            }
                        } else {
                            self.room_settings_repo
                                .set_settings_with_version_with_executor(
                                    &room_id,
                                    &merged_settings,
                                    version,
                                    &mut *tx,
                                )
                                .await?
                        };
                        let outbox_event = outbox_event_factory
                            .as_ref()
                            .map(|factory| factory(&merged_settings, new_version))
                            .transpose()?;
                        if let (Some(outbox), Some(event)) = (&self.realtime_outbox, &outbox_event)
                        {
                            if let Err(error) = outbox.insert_with_executor(event, &mut *tx).await {
                                self.abort_room_settings_write(&domain, reservation.as_ref())
                                    .await;
                                return Err(error);
                            }
                        }
                        if let Err(error) = tx.commit().await {
                            self.abort_room_settings_write(&domain, reservation.as_ref())
                                .await;
                            return Err(error.into());
                        }
                        self.finalize_committed_room_settings_write_best_effort(
                            &domain,
                            reservation.as_ref(),
                            new_version,
                            "patch_settings_with_outbox",
                        )
                        .await;
                        Ok((current, merged_settings, new_version))
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

        let settings_json = serde_json::to_value(&snapshot.settings)
            .internal_with_err("Failed to serialize settings")?;
        self.write_audit_event(
            &user_id,
            &user_id.to_string(),
            AuditAction::RoomSettingsUpdated,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            settings_json,
        )
        .await?;

        Ok(snapshot)
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
            .check_permission(
                room_id,
                user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        // 2. Validate via registry (type parsing + value constraints from macro validators)
        RoomSettingsRegistry::validate_setting(key, value)?;

        // 3. CAS update with retry
        let (previous_settings, settings, version) =
            super::optimistic_retry::retry_with_optimistic_lock(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                "Settings update failed after maximum retry attempts",
                || async {
                    let (mut settings, version) =
                        self.room_settings_repo.get_with_version(room_id).await?;
                    let current = settings.clone();
                    settings.set_by_key(key, value)?;
                    settings.validate()?;

                    let domain = CacheDomain::RoomSettings { room_id: *room_id };
                    let reservation = self.begin_room_settings_write(room_id, version).await?;
                    let new_version = if let Some(reservation) = &reservation {
                        match self
                            .room_settings_repo
                            .set_settings_with_exact_version(
                                room_id,
                                &settings,
                                version,
                                reservation.version,
                            )
                            .await
                        {
                            Ok(new_version) => {
                                self.finalize_committed_room_settings_write_best_effort(
                                    &domain,
                                    Some(reservation),
                                    new_version,
                                    "update_room_setting",
                                )
                                .await;
                                new_version
                            }
                            Err(error) => {
                                self.abort_room_settings_write(&domain, Some(reservation))
                                    .await;
                                return Err(error);
                            }
                        }
                    } else {
                        self.room_settings_repo
                            .set_settings_with_version(room_id, &settings, version)
                            .await?
                    };
                    Ok((current, settings, new_version))
                },
            )
            .await?;

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
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.run_post_apply_hooks_for_settings_update(room_id, previous_settings, updated_settings)
            .await;
        self.room_settings_service.invalidate_local(room_id).await;
        self.permission_service.invalidate_room_cache(room_id).await;
        self.notify_room_invalidation(room_id).await;
        self.notify_room_settings_invalidation(room_id).await;

        let settings_json = serde_json::to_value(updated_settings)
            .internal_with_err("Failed to serialize settings")?;
        let subscriber_count = self.notification_service.notify_settings_updated(
            room_id,
            actor_user_id,
            actor_username,
            settings_json.clone(),
            version,
        );
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                version,
                "Room settings updated event had no local subscribers"
            );
        }

        Ok(crate::cache::RoomSettingsSnapshot {
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
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.reset_room_settings_with_outbox(room_id, user_id, None)
            .await
    }

    pub async fn reset_room_settings_with_outbox(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        outbox_event_factory: Option<RealtimeOutboxSettingsEventFactory>,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.permission_service
            .check_permission(
                room_id,
                user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        let default_settings = RoomSettings::default();

        let (previous_settings, updated_settings, updated_version) =
            super::optimistic_retry::retry_with_optimistic_lock(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                "Settings reset failed after maximum retry attempts",
                || async {
                    let outbox_event_factory = outbox_event_factory.clone();
                    let (current, version) =
                        self.room_settings_repo.get_with_version(room_id).await?;
                    let mut tx = self.pool.begin().await?;
                    ensure_actor_has_room_permission_now_tx(
                        &mut tx,
                        &self.permission_service,
                        room_id,
                        user_id,
                        crate::models::RoomPermission::SET_ROOM_SETTINGS,
                    )
                    .await?;
                    let domain = CacheDomain::RoomSettings { room_id: *room_id };
                    let reservation = self.begin_room_settings_write(room_id, version).await?;
                    let new_version = if let Some(reservation) = &reservation {
                        match self
                            .room_settings_repo
                            .set_settings_with_exact_version_with_executor(
                                room_id,
                                &default_settings,
                                version,
                                reservation.version,
                                &mut *tx,
                            )
                            .await
                        {
                            Ok(new_version) => new_version,
                            Err(error) => {
                                self.abort_room_settings_write(&domain, Some(reservation))
                                    .await;
                                return Err(error);
                            }
                        }
                    } else {
                        self.room_settings_repo
                            .set_settings_with_version_with_executor(
                                room_id,
                                &default_settings,
                                version,
                                &mut *tx,
                            )
                            .await?
                    };
                    let outbox_event = outbox_event_factory
                        .as_ref()
                        .map(|factory| factory(&default_settings, new_version))
                        .transpose()?;
                    if let (Some(outbox), Some(event)) = (&self.realtime_outbox, &outbox_event) {
                        if let Err(error) = outbox.insert_with_executor(event, &mut *tx).await {
                            self.abort_room_settings_write(&domain, reservation.as_ref())
                                .await;
                            return Err(error);
                        }
                    }
                    if let Err(error) = tx.commit().await {
                        self.abort_room_settings_write(&domain, reservation.as_ref())
                            .await;
                        return Err(error.into());
                    }
                    self.finalize_committed_room_settings_write_best_effort(
                        &domain,
                        reservation.as_ref(),
                        new_version,
                        "reset_room_settings_with_outbox",
                    )
                    .await;
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

    pub async fn check_room_password(&self, room_id: &RoomId, password: &str) -> Result<bool> {
        let credential = self
            .room_password_repo
            .get_opaque_credential(room_id)
            .await?;
        match credential {
            Some(stored) if stored.state.enabled => self
                .opaque_password_service
                .verify_password(&stored.record, password),
            Some(_) => Err(Error::InvalidInput("Room password is disabled".to_string())),
            None => Err(Error::InvalidInput("Room has no password set".to_string())),
        }
    }

    pub async fn is_room_password_enabled(&self, room_id: &RoomId) -> Result<bool> {
        Ok(self
            .room_password_repo
            .get_state(room_id)
            .await?
            .is_some_and(|state| state.enabled))
    }

    pub async fn start_room_opaque_password_login_with_control(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        credential_request: Vec<u8>,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<RoomOpaqueLoginStartChallenge> {
        let subject_key = self.room_password_attempts_key(room_id, client_ip);
        if let Some(ref brute_force) = self.brute_force_service {
            brute_force
                .check_subject_key_allowed_with_control(&subject_key, client_ip, control)
                .await?;
        }
        let ctx = self
            .room_repo
            .get_join_context(room_id, user_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        if !ctx.password_enabled {
            return Err(Error::InvalidInput(
                "Room does not require a password".to_string(),
            ));
        }
        let credential = ctx
            .password_credential
            .ok_or_else(|| Error::Authorization("Invalid password".to_string()))?;
        let password_version = ctx.password_version.unwrap_or(0);
        let login_start = self.opaque_password_service.start_login(
            Some(&credential),
            &credential.credential_identifier,
            &credential_request,
        )?;
        let session_id = synctv_common::snanoid!(48);
        self.opaque_password_login_session_store
            .store(
                &session_id,
                &RoomOpaquePasswordLoginSession {
                    room_id: *room_id,
                    user_id: *user_id,
                    expected_password_version: password_version,
                    server_login_state: login_start.server_login_state,
                    brute_force_subject_key: subject_key,
                },
                StdDuration::from_secs(ROOM_OPAQUE_LOGIN_SESSION_TTL_SECS),
            )
            .await?;
        Ok(RoomOpaqueLoginStartChallenge {
            session_id,
            credential_response: login_start.credential_response,
        })
    }

    pub async fn finish_room_opaque_password_login_with_outbox(
        &self,
        expected_room_id: Option<&RoomId>,
        session_id: &str,
        user_id: &UserId,
        credential_finalization: Vec<u8>,
        client_ip: Option<IpAddr>,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<(Room, RoomMember, Vec<crate::models::RoomMemberWithUser>)> {
        let Some(session) = self
            .opaque_password_login_session_store
            .consume(session_id)
            .await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        if session.user_id != *user_id {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        if expected_room_id.is_some_and(|room_id| session.room_id != *room_id) {
            return Err(Error::InvalidInput(
                "Room password login session does not match room".to_string(),
            ));
        }
        if let Some(ref brute_force) = self.brute_force_service {
            brute_force
                .check_subject_key_allowed_with_control(
                    &session.brute_force_subject_key,
                    client_ip,
                    None,
                )
                .await?;
        }
        let finish_result = self
            .opaque_password_service
            .finish_login(&session.server_login_state, &credential_finalization);
        if finish_result.is_err() {
            if let Some(ref brute_force) = self.brute_force_service {
                brute_force
                    .record_subject_key_failure_with_control(
                        &session.brute_force_subject_key,
                        client_ip,
                        None,
                    )
                    .await?;
            }
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        let current_state = self
            .room_password_repo
            .get_state(&session.room_id)
            .await?
            .ok_or_else(|| Error::Authorization("Invalid password".to_string()))?;
        if !current_state.enabled {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        if current_state.version != session.expected_password_version {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        if let Some(ref brute_force) = self.brute_force_service {
            if let Err(error) = brute_force
                .reset_subject_key_with_control(&session.brute_force_subject_key, None)
                .await
            {
                tracing::warn!(
                    room_id = %session.room_id,
                    error = %error,
                    "Failed to reset room password rate limit counter after successful OPAQUE login"
                );
            }
        }
        self.join_room_with_password_proof(
            session.room_id,
            session.user_id,
            RoomPasswordJoinProof::OpaqueVerified {
                expected_version: session.expected_password_version,
            },
            outbox_event_factory,
        )
        .await
    }

    pub async fn start_room_opaque_password_registration(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        registration_request: Vec<u8>,
    ) -> Result<RoomOpaqueRegistrationStartChallenge> {
        self.permission_service
            .check_permission_no_cache(
                room_id,
                user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;
        let credential_identifier = Self::room_opaque_credential_identifier(room_id);
        let registration_start = self
            .opaque_password_service
            .start_registration(&credential_identifier, &registration_request)?;
        let session_id = synctv_common::snanoid!(48);
        self.opaque_password_registration_session_store
            .store(
                &session_id,
                &RoomOpaquePasswordRegistrationSession {
                    room_id: *room_id,
                    user_id: *user_id,
                    credential_identifier,
                },
                StdDuration::from_secs(ROOM_OPAQUE_REGISTRATION_SESSION_TTL_SECS),
            )
            .await?;
        Ok(RoomOpaqueRegistrationStartChallenge {
            session_id,
            registration_response: registration_start.registration_response,
        })
    }

    pub async fn finish_room_opaque_password_registration(
        &self,
        room_id: &RoomId,
        session_id: &str,
        user_id: &UserId,
        registration_upload: Vec<u8>,
    ) -> Result<RoomPasswordCredentialState> {
        let Some(session) = self
            .opaque_password_registration_session_store
            .consume(session_id)
            .await?
        else {
            return Err(Error::Authentication("Authentication failed".to_string()));
        };
        if session.user_id != *user_id {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }
        if session.room_id != *room_id {
            return Err(Error::InvalidInput(
                "Room password registration session does not match room".to_string(),
            ));
        }
        self.permission_service
            .check_permission_no_cache(
                &session.room_id,
                user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;
        let opaque_record = self
            .opaque_password_service
            .finish_registration(session.credential_identifier, &registration_upload)?;
        self.update_room_password_as(&session.room_id, Some(user_id), Some(opaque_record))
            .await
    }

    pub async fn set_room_password_from_plaintext(
        &self,
        room_id: &RoomId,
        actor_user_id: Option<&UserId>,
        new_password: Option<&str>,
    ) -> Result<RoomPasswordCredentialState> {
        let opaque_record = new_password
            .map(|password| {
                self.opaque_password_service
                    .register_password(&Self::room_opaque_credential_identifier(room_id), password)
            })
            .transpose()?;
        self.update_room_password_as(room_id, actor_user_id, opaque_record)
            .await
    }

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
        let subject_key = self.room_password_attempts_key(room_id, client_ip);

        if let Some(ref brute_force) = self.brute_force_service {
            brute_force
                .check_subject_key_allowed_with_control(&subject_key, client_ip, control)
                .await?;
        }

        let is_valid = match self
            .room_password_repo
            .get_opaque_credential(room_id)
            .await?
        {
            Some(stored) if stored.state.enabled => self
                .opaque_password_service
                .verify_password(&stored.record, password),
            Some(_) | None => Ok(false),
        }?;

        // Handle success/failure tracking
        if let Some(ref brute_force) = self.brute_force_service {
            if is_valid {
                // Reset failure counter on successful verification
                if let Err(e) = brute_force
                    .reset_subject_key_with_control(&subject_key, control)
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
                                crate::models::AuditTargetType::Room,
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
                    .record_subject_key_failure_with_control(&subject_key, client_ip, control)
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
            let subject_key = self.room_password_attempts_key(room_id, Some(client_ip));
            brute_force
                .reset_subject_key_with_control(&subject_key, None)
                .await?;
        }
        Ok(())
    }

    fn room_password_attempts_key(&self, room_id: &RoomId, client_ip: Option<IpAddr>) -> String {
        let ip = client_ip.map_or_else(|| "unknown".to_string(), |ip| ip.to_string());
        self.user_service
            .key_builder()
            .room_password_attempts(&room_id.to_string(), &ip)
    }

    /// Update room password
    pub async fn update_room_password(
        &self,
        room_id: &RoomId,
        password: Option<String>,
    ) -> Result<()> {
        let opaque_record = password
            .as_deref()
            .map(|password| {
                self.opaque_password_service
                    .register_password(&Self::room_opaque_credential_identifier(room_id), password)
            })
            .transpose()?;
        self.update_room_password_as(room_id, None, opaque_record)
            .await
            .map(|_| ())
    }

    pub async fn update_room_password_as(
        &self,
        room_id: &RoomId,
        actor_user_id: Option<&UserId>,
        opaque_record: Option<OpaquePasswordRecord>,
    ) -> Result<RoomPasswordCredentialState> {
        let password_was_set = opaque_record.is_some();
        let state = self
            .do_set_room_password_credential(room_id, opaque_record)
            .await?;

        if password_was_set {
            self.revoke_all_guest_access(
                room_id,
                crate::service::notification::GuestKickReason::RoomPasswordAdded,
            )
            .await?;
        }

        self.notify_room_invalidation(room_id).await;
        tracing::debug!(
            room_id = %room_id,
            actor_user_id = ?actor_user_id,
            password_enabled = state.enabled,
            password_version = state.version,
            "Room password state updated"
        );
        Ok(state)
    }

    async fn do_set_room_password_credential(
        &self,
        room_id: &RoomId,
        opaque_record: Option<OpaquePasswordRecord>,
    ) -> Result<RoomPasswordCredentialState> {
        super::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Password update failed after maximum retry attempts",
            || async {
                let mut tx = self.pool.begin().await?;
                let state = if let Some(ref opaque_record) = opaque_record {
                    self.room_password_repo
                        .set_opaque_credential_with_executor(room_id, opaque_record, &mut *tx)
                        .await?
                } else {
                    self.room_password_repo
                        .disable_with_executor(room_id, &mut *tx)
                        .await?
                };

                tx.commit().await?;
                Ok(state)
            },
        )
        .await
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
            .check_permission(
                room_id,
                user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        let room = self
            .room_repo
            .update_description(room_id, &description)
            .await?;
        self.notify_room_invalidation(room_id).await;
        Ok(room)
    }

    pub async fn create_room_cover_upload_session(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: CreateRoomCoverUploadSession,
    ) -> Result<FileUploadSession> {
        let storage = self.room_file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for room covers".to_string())
        })?;
        self.room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        storage
            .create_upload_session(crate::models::CreateFileUploadSession {
                user_id,
                storage_scope: room_cover_storage_scope(room_id),
                client_file_id: request.client_cover_id,
                mime_type: request.mime_type,
                size_bytes: request.size_bytes,
                width: request.width,
                height: request.height,
                checksum_sha256: request.checksum_sha256,
                metadata: request.metadata,
                policy: room_cover_upload_policy(),
            })
            .await
    }

    pub async fn store_room_cover_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        data: Vec<u8>,
    ) -> Result<FileBlob> {
        self.room_file_storage_service
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidInput("file storage is not configured for room covers".to_string())
            })?
            .store_upload_object(encoded_object_key, upload_token, content_type, data)
            .await
    }

    pub async fn get_room_cover_object(
        &self,
        encoded_object_key: &str,
        read_token: &str,
    ) -> Result<FileBlob> {
        self.room_file_storage_service
            .as_ref()
            .ok_or_else(|| Error::NotFound("File object not found".to_string()))?
            .get_object(encoded_object_key, read_token)
            .await
    }

    pub async fn update_room_cover(
        &self,
        room_id: RoomId,
        user_id: UserId,
        file: NewStoredFile,
    ) -> Result<Room> {
        let storage = self.room_file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for room covers".to_string())
        })?;
        let mut tx = self.room_repo.pool().begin().await?;
        let mut room = self
            .room_repo
            .get_by_id_for_update_with_executor(&room_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;

        let storage_scope = room_cover_storage_scope(room_id);
        let prepared = storage
            .prepare_files(
                FileStorageContext {
                    user_id,
                    storage_scope: &storage_scope,
                    client_request_id: None,
                },
                vec![file],
            )
            .await?;
        let file = prepared
            .into_iter()
            .next()
            .ok_or_else(|| Error::InvalidInput("room cover file is required".to_string()))?;
        let new_reference_id = crate::repository::FileStorageRepository::insert_reference_in_tx(
            &mut tx,
            &file.storage_backend,
            &file.object_key,
            ROOM_COVER_REFERENCE_KIND,
            &room_id.as_i64().to_string(),
            None,
            &file.metadata,
        )
        .await?
        .ok_or_else(|| {
            Error::InvalidInput("room cover file object is not registered".to_string())
        })?;
        let old_reference = if let Some(reference_id) = room.cover_file_reference_id {
            crate::repository::FileStorageRepository::new(self.pool.clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| {
                    reference
                        .reference_target(ROOM_COVER_REFERENCE_KIND, room_id.as_i64().to_string())
                })
        } else {
            None
        };

        room.cover_file_reference_id = Some(new_reference_id);
        let updated_room = self
            .room_repo
            .update_with_executor(&room, room.version, &mut *tx)
            .await?;
        tx.commit().await?;

        if let Some(old_reference) = old_reference {
            if old_reference.storage_backend != file.storage_backend
                || old_reference.object_key != file.object_key
            {
                storage
                    .delete_files(
                        FileStorageCleanupOrigin::ReferenceReleased,
                        &[old_reference],
                    )
                    .await?;
            }
        }
        self.notify_room_invalidation(&room_id).await;
        Ok(updated_room)
    }

    pub async fn clear_room_cover(&self, room_id: RoomId, user_id: UserId) -> Result<Room> {
        let mut tx = self.room_repo.pool().begin().await?;
        let mut room = self
            .room_repo
            .get_by_id_for_update_with_executor(&room_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await?;
        let old_reference = if let Some(reference_id) = room.cover_file_reference_id {
            crate::repository::FileStorageRepository::new(self.pool.clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| {
                    reference
                        .reference_target(ROOM_COVER_REFERENCE_KIND, room_id.as_i64().to_string())
                })
        } else {
            None
        };
        room.cover_file_reference_id = None;
        let updated_room = self
            .room_repo
            .update_with_executor(&room, room.version, &mut *tx)
            .await?;
        tx.commit().await?;

        if let (Some(storage), Some(reference)) =
            (self.room_file_storage_service.as_ref(), old_reference)
        {
            storage
                .delete_files(FileStorageCleanupOrigin::ReferenceReleased, &[reference])
                .await?;
        }
        self.notify_room_invalidation(&room_id).await;
        Ok(updated_room)
    }

    /// List all rooms (paginated)
    pub async fn list_rooms(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        query.pagination.validate()?;
        self.room_repo.list(query).await
    }

    pub async fn list_active_unbanned_rooms_by_ids(
        &self,
        room_ids: &[RoomId],
    ) -> Result<Vec<Room>> {
        self.room_repo.list_active_unbanned_by_ids(room_ids).await
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
        self.set_member_permission_with_outbox(
            room_id,
            granter_id,
            target_user_id,
            added_permissions,
            removed_permissions,
            None,
        )
        .await
    }

    pub async fn set_member_permission_with_outbox(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        added_permissions: u64,
        removed_permissions: u64,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<crate::models::RoomMember> {
        let updated_member = super::optimistic_retry::retry_with_optimistic_lock(
            3,
            5,
            "Permission update failed after maximum retry attempts",
            || async {
                let mut tx = self.pool.begin().await?;
                ensure_actor_has_room_permission_now_tx(
                    &mut tx,
                    &self.permission_service,
                    &room_id,
                    &granter_id,
                    crate::models::RoomPermission::SET_MEMBER_PERMISSIONS,
                )
                .await?;
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;
                Self::validate_override_bits_for_role(
                    member.role,
                    added_permissions,
                    removed_permissions,
                )?;
                let fence = self
                    .begin_permission_write(&room_id, &target_user_id, member.version)
                    .await?;
                let reserved_version = fence.version();
                let updated = if matches!(member.role, RoomRole::Admin) {
                    if reserved_version > 0 {
                        match self
                            .member_repo
                            .update_admin_permissions_with_exact_version_executor(
                                MemberPermissionExactVersionUpdate {
                                    room_id: &room_id,
                                    user_id: &target_user_id,
                                    added_permissions,
                                    removed_permissions,
                                    current_version: member.version,
                                    new_version: reserved_version,
                                },
                                &mut *tx,
                            )
                            .await
                        {
                            Ok(updated) => updated,
                            Err(error) => {
                                self.abort_permission_write(&fence).await;
                                return Err(error);
                            }
                        }
                    } else {
                        self.member_repo
                            .update_admin_permissions_with_executor(
                                &room_id,
                                &target_user_id,
                                added_permissions,
                                removed_permissions,
                                member.version,
                                &mut *tx,
                            )
                            .await?
                    }
                } else if reserved_version > 0 {
                    match self
                        .member_repo
                        .update_permissions_with_exact_version_executor(
                            MemberPermissionExactVersionUpdate {
                                room_id: &room_id,
                                user_id: &target_user_id,
                                added_permissions,
                                removed_permissions,
                                current_version: member.version,
                                new_version: reserved_version,
                            },
                            &mut *tx,
                        )
                        .await
                    {
                        Ok(updated) => updated,
                        Err(error) => {
                            self.abort_permission_write(&fence).await;
                            return Err(error);
                        }
                    }
                } else {
                    self.member_repo
                        .update_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            added_permissions,
                            removed_permissions,
                            member.version,
                            &mut *tx,
                        )
                        .await?
                };
                let snapshot = match self
                    .permission_changed_snapshot_tx(
                        &mut tx,
                        room_id,
                        target_user_id,
                        granter_id,
                        Some(&updated),
                    )
                    .await
                {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.abort_permission_write(&fence).await;
                        return Err(error);
                    }
                };
                if let Err(error) = self
                    .insert_permission_changed_outbox_tx(
                        &mut tx,
                        &snapshot,
                        outbox_event_factory.as_ref(),
                    )
                    .await
                {
                    self.abort_permission_write(&fence).await;
                    return Err(error);
                }
                if let Err(error) = tx.commit().await {
                    self.abort_permission_write(&fence).await;
                    return Err(error.into());
                }
                self.finalize_committed_permission_write_best_effort(
                    &fence,
                    &room_id,
                    &target_user_id,
                    updated.version,
                    "grant_member_permissions_with_outbox",
                )
                .await;
                Ok(updated)
            },
        )
        .await?;

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;

        Ok(updated_member)
    }

    pub async fn set_member_role_with_outbox(
        &self,
        room_id: RoomId,
        creator_id: UserId,
        target_user_id: UserId,
        role: RoomRole,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<crate::models::RoomMember> {
        if role == RoomRole::Creator {
            return Err(Error::InvalidInput(
                "Creator role is bound to room ownership and cannot be assigned via set_member_role"
                    .to_string(),
            ));
        }

        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.created_by != creator_id {
            return Err(Error::Authorization(
                "Only room creator can change member roles".to_string(),
            ));
        }

        if target_user_id == room.created_by {
            return Err(Error::InvalidInput(
                "Cannot change the role of the room creator via set_member_role".to_string(),
            ));
        }

        let mut tx = self.pool.begin().await?;
        let member = self
            .member_repo
            .get(&room_id, &target_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;
        let fence = self
            .begin_permission_write(&room_id, &target_user_id, member.version)
            .await?;
        let updated_member = if fence.version() > 0 {
            match self
                .member_repo
                .update_role_with_exact_version_executor(
                    &room_id,
                    &target_user_id,
                    role,
                    member.version,
                    fence.version(),
                    &mut *tx,
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.abort_permission_write(&fence).await;
                    return Err(error);
                }
            }
        } else {
            self.member_repo
                .update_role_with_version_executor(
                    &room_id,
                    &target_user_id,
                    role,
                    member.version,
                    &mut *tx,
                )
                .await?
        };
        let snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                creator_id,
                Some(&updated_member),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            &fence,
            &room_id,
            &target_user_id,
            updated_member.version,
            "set_member_role_with_outbox",
        )
        .await;

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;
        self.notify_room_settings_invalidation(&room_id).await;

        Ok(updated_member)
    }

    /// Kick member from room
    pub async fn kick_member(
        &self,
        room_id: RoomId,
        kicker_id: UserId,
        target_user_id: UserId,
        cooldown_seconds: i64,
    ) -> Result<()> {
        self.kick_member_with_outbox(
            room_id,
            kicker_id,
            target_user_id,
            cooldown_seconds,
            KickMemberOutboxOptions::default(),
        )
        .await
    }

    pub async fn kick_member_with_outbox(
        &self,
        room_id: RoomId,
        kicker_id: UserId,
        target_user_id: UserId,
        cooldown_seconds: i64,
        outbox: KickMemberOutboxOptions,
    ) -> Result<()> {
        validate_kick_cooldown_seconds(cooldown_seconds)?;
        if kicker_id == target_user_id {
            return Err(Error::InvalidInput("Cannot kick yourself".to_string()));
        }

        let mut tx = self.pool.begin().await?;
        ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &self.permission_service,
            &room_id,
            &kicker_id,
            crate::models::RoomPermission::KICK_MEMBER,
        )
        .await?;
        let Some(observed_version) = self
            .member_repo
            .active_member_version_for_update_with_executor(&room_id, &target_user_id, &mut tx)
            .await?
        else {
            return Err(Error::Authorization(
                "User is not a member or cannot kick a member with equal or higher role"
                    .to_string(),
            ));
        };
        let fence = self
            .begin_permission_write(&room_id, &target_user_id, observed_version)
            .await?;
        let removed_version = match self
            .member_repo
            .kick_with_role_check_with_executor(&room_id, &kicker_id, &target_user_id, &mut tx)
            .await
        {
            Ok(version) => version,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let Some(removed_version) = removed_version else {
            self.abort_permission_write(&fence).await;
            return Err(Error::Authorization(
                "User is not a member or cannot kick a member with equal or higher role"
                    .to_string(),
            ));
        };
        let now = Utc::now();
        if let Err(error) = self
            .member_repo
            .add_kick_cooldown_with_executor(
                KickCooldownInsert {
                    room_id: &room_id,
                    user_id: &target_user_id,
                    kicked_by: Some(&kicker_id),
                    starts_at: now,
                    ends_at: now + Duration::seconds(cooldown_seconds),
                    reason: Some("kicked"),
                },
                &mut *tx,
            )
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        let cleanup = match cleanup_member_resources_in_tx(&mut tx, &room_id, &target_user_id).await
        {
            Ok(cleanup) => cleanup,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let cleanup_outbox_events = outbox
            .cleanup
            .as_ref()
            .map(|factory| factory(&cleanup))
            .transpose()?
            .unwrap_or_default();
        let snapshot = match self
            .permission_changed_snapshot_tx(&mut tx, room_id, target_user_id, kicker_id, None)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(
                &mut tx,
                &snapshot,
                outbox.permission_changed.as_ref(),
            )
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_realtime_outbox_tx(&mut tx, outbox.lifecycle.as_ref())
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_realtime_outbox_events_tx(&mut tx, &cleanup_outbox_events)
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            &fence,
            &room_id,
            &target_user_id,
            removed_version,
            "kick_member_with_outbox",
        )
        .await;

        self.permission_service
            .invalidate_removed_member_cache(&room_id, &target_user_id)
            .await;
        self.finalize_member_resource_cleanup_after_commit(&room_id, &target_user_id, &cleanup)
            .await;
        let subscriber_count = self
            .notification_service
            .notify_member_kicked(&room_id, &target_user_id);
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                user_id = %target_user_id,
                "Member kick event had no local subscribers"
            );
        }
        Ok(())
    }

    pub async fn admin_update_member_with_outbox(
        &self,
        update: AdminMemberUpdate,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<crate::models::RoomMember> {
        let AdminMemberUpdate {
            room_id,
            actor_id,
            actor_username: _,
            target_user_id,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = update;
        if !RoomAdminPermissionBits::includes_only_defined(admin_added_permissions)
            || !RoomAdminPermissionBits::includes_only_defined(admin_removed_permissions)
        {
            return Err(Error::InvalidInput(
                "Permission set includes bits outside the target role permission bitspace"
                    .to_string(),
            ));
        }

        let current = self
            .member_repo
            .get(&room_id, &target_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;
        let effective_role = role.unwrap_or(current.role);
        let effective_is_admin = matches!(effective_role, RoomRole::Admin);
        Self::validate_override_bits_for_role(
            effective_role,
            added_permissions,
            removed_permissions,
        )?;

        if let Some(new_role) = role {
            if new_role == RoomRole::Creator {
                return Err(Error::InvalidInput(
                    "Creator role is bound to room ownership and cannot be assigned via set_member_role"
                        .to_string(),
                ));
            }
            let room = self
                .room_repo
                .get_by_id(&room_id)
                .await?
                .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
            if target_user_id == room.created_by {
                return Err(Error::InvalidInput(
                    "Cannot change the role of the room creator via set_member_role".to_string(),
                ));
            }
        }

        if effective_is_admin && (added_permissions > 0 || removed_permissions > 0) {
            return Err(Error::Authorization(
                "Admin members must use admin_added_permissions/admin_removed_permissions"
                    .to_string(),
            ));
        }
        if !effective_is_admin && (admin_added_permissions > 0 || admin_removed_permissions > 0) {
            return Err(Error::Authorization(
                "Only admin members use admin_added_permissions/admin_removed_permissions"
                    .to_string(),
            ));
        }

        let mut tx = self.pool.begin().await?;
        let mut updated = current;
        let mut fence: Option<PermissionWriteFence> = None;
        let has_permission_changes = added_permissions > 0
            || removed_permissions > 0
            || admin_added_permissions > 0
            || admin_removed_permissions > 0;
        let combine_role_and_permissions = role.is_some() && has_permission_changes;

        if let Some(new_role) = role.filter(|_| combine_role_and_permissions) {
            let write_fence = self
                .begin_permission_write(&room_id, &target_user_id, updated.version)
                .await?;
            updated = if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_role_and_permissions_with_exact_version_executor(
                        MemberRolePermissionExactVersionUpdate {
                            room_id: &room_id,
                            user_id: &target_user_id,
                            role: new_role,
                            added_permissions: if effective_is_admin {
                                admin_added_permissions
                            } else {
                                added_permissions
                            },
                            removed_permissions: if effective_is_admin {
                                admin_removed_permissions
                            } else {
                                removed_permissions
                            },
                            use_admin_permissions: effective_is_admin,
                            current_version: updated.version,
                            new_version: write_fence.version(),
                        },
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        self.abort_permission_write(&write_fence).await;
                        return Err(error);
                    }
                }
            } else {
                let updated_role = self
                    .member_repo
                    .update_role_with_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        &mut *tx,
                    )
                    .await?;
                if effective_is_admin {
                    self.member_repo
                        .update_admin_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            admin_added_permissions,
                            admin_removed_permissions,
                            updated_role.version,
                            &mut *tx,
                        )
                        .await?
                } else {
                    self.member_repo
                        .update_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            added_permissions,
                            removed_permissions,
                            updated_role.version,
                            &mut *tx,
                        )
                        .await?
                }
            };
            fence = Some(write_fence);
        } else if let Some(new_role) = role {
            let write_fence = self
                .begin_permission_write(&room_id, &target_user_id, updated.version)
                .await?;
            updated = if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_role_with_exact_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        write_fence.version(),
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        self.abort_permission_write(&write_fence).await;
                        return Err(error);
                    }
                }
            } else {
                self.member_repo
                    .update_role_with_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        &mut *tx,
                    )
                    .await?
            };
            fence = Some(write_fence);
        }

        if !combine_role_and_permissions && (has_permission_changes || role.is_none()) {
            if fence.is_none() {
                fence = Some(
                    self.begin_permission_write(&room_id, &target_user_id, updated.version)
                        .await?,
                );
            }
            let Some(write_fence) = fence.as_ref() else {
                return Err(Error::Internal(
                    "Permission update missing write fence".to_string(),
                ));
            };
            updated = if effective_is_admin {
                if write_fence.version() > 0 {
                    match self
                        .member_repo
                        .update_admin_permissions_with_exact_version_executor(
                            MemberPermissionExactVersionUpdate {
                                room_id: &room_id,
                                user_id: &target_user_id,
                                added_permissions: admin_added_permissions,
                                removed_permissions: admin_removed_permissions,
                                current_version: updated.version,
                                new_version: write_fence.version(),
                            },
                            &mut *tx,
                        )
                        .await
                    {
                        Ok(updated) => updated,
                        Err(error) => {
                            if let Some(fence) = &fence {
                                self.abort_permission_write(fence).await;
                            }
                            return Err(error);
                        }
                    }
                } else {
                    self.member_repo
                        .update_admin_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            admin_added_permissions,
                            admin_removed_permissions,
                            updated.version,
                            &mut *tx,
                        )
                        .await?
                }
            } else if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_permissions_with_exact_version_executor(
                        MemberPermissionExactVersionUpdate {
                            room_id: &room_id,
                            user_id: &target_user_id,
                            added_permissions,
                            removed_permissions,
                            current_version: updated.version,
                            new_version: write_fence.version(),
                        },
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        if let Some(fence) = &fence {
                            self.abort_permission_write(fence).await;
                        }
                        return Err(error);
                    }
                }
            } else {
                self.member_repo
                    .update_permissions_with_executor(
                        &room_id,
                        &target_user_id,
                        added_permissions,
                        removed_permissions,
                        updated.version,
                        &mut *tx,
                    )
                    .await?
            };
        }

        let snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&updated),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Some(fence) = &fence {
                    self.abort_permission_write(fence).await;
                }
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await
        {
            if let Some(fence) = &fence {
                self.abort_permission_write(fence).await;
            }
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            if let Some(fence) = &fence {
                self.abort_permission_write(fence).await;
            }
            return Err(error.into());
        }
        if let Some(fence) = &fence {
            self.finalize_committed_permission_write_best_effort(
                fence,
                &room_id,
                &target_user_id,
                updated.version,
                "admin_update_member_with_outbox",
            )
            .await;
        }

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;
        if role.is_some() {
            self.notify_room_settings_invalidation(&room_id).await;
        }
        Ok(updated)
    }

    pub async fn update_member_with_outbox(
        &self,
        request: UpdateMemberWithOutboxRequest,
    ) -> Result<crate::models::RoomMember> {
        let UpdateMemberWithOutboxRequest {
            room_id,
            actor_id,
            target_user_id,
            role,
            permissions,
            outbox_event_factory,
        } = request;
        let MemberPermissionPatch {
            apply_permission_update,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = permissions;

        if !RoomAdminPermissionBits::includes_only_defined(admin_added_permissions)
            || !RoomAdminPermissionBits::includes_only_defined(admin_removed_permissions)
        {
            return Err(Error::InvalidInput(
                "Permission set includes bits outside the target role permission bitspace"
                    .to_string(),
            ));
        }

        let current = self
            .member_repo
            .get(&room_id, &target_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;
        let effective_role = role.unwrap_or(current.role);
        let effective_is_admin = matches!(effective_role, RoomRole::Admin);
        if apply_permission_update {
            Self::validate_override_bits_for_role(
                effective_role,
                added_permissions,
                removed_permissions,
            )?;
        }

        if let Some(new_role) = role {
            if new_role == RoomRole::Creator {
                return Err(Error::InvalidInput(
                    "Creator role is bound to room ownership and cannot be assigned via set_member_role"
                        .to_string(),
                ));
            }
            let room = self
                .room_repo
                .get_by_id(&room_id)
                .await?
                .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

            if room.created_by != actor_id {
                return Err(Error::Authorization(
                    "Only room creator can change member roles".to_string(),
                ));
            }

            if target_user_id == room.created_by {
                return Err(Error::InvalidInput(
                    "Cannot change the role of the room creator via set_member_role".to_string(),
                ));
            }
        }

        if apply_permission_update {
            if effective_is_admin && (added_permissions > 0 || removed_permissions > 0) {
                return Err(Error::Authorization(
                    "Admin members must use admin_added_permissions/admin_removed_permissions"
                        .to_string(),
                ));
            }
            if !effective_is_admin && (admin_added_permissions > 0 || admin_removed_permissions > 0)
            {
                return Err(Error::Authorization(
                    "Only admin members use admin_added_permissions/admin_removed_permissions"
                        .to_string(),
                ));
            }
        }

        let mut tx = self.pool.begin().await?;
        if apply_permission_update {
            ensure_actor_has_room_permission_now_tx(
                &mut tx,
                &self.permission_service,
                &room_id,
                &actor_id,
                crate::models::RoomPermission::SET_MEMBER_PERMISSIONS,
            )
            .await?;
        }
        let mut updated = current;
        let mut fence: Option<PermissionWriteFence> = None;
        let combine_role_and_permissions = role.is_some() && apply_permission_update;

        if let Some(new_role) = role.filter(|_| combine_role_and_permissions) {
            let write_fence = self
                .begin_permission_write(&room_id, &target_user_id, updated.version)
                .await?;
            updated = if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_role_and_permissions_with_exact_version_executor(
                        MemberRolePermissionExactVersionUpdate {
                            room_id: &room_id,
                            user_id: &target_user_id,
                            role: new_role,
                            added_permissions: if effective_is_admin {
                                admin_added_permissions
                            } else {
                                added_permissions
                            },
                            removed_permissions: if effective_is_admin {
                                admin_removed_permissions
                            } else {
                                removed_permissions
                            },
                            use_admin_permissions: effective_is_admin,
                            current_version: updated.version,
                            new_version: write_fence.version(),
                        },
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        self.abort_permission_write(&write_fence).await;
                        return Err(error);
                    }
                }
            } else {
                let updated_role = self
                    .member_repo
                    .update_role_with_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        &mut *tx,
                    )
                    .await?;
                if effective_is_admin {
                    self.member_repo
                        .update_admin_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            admin_added_permissions,
                            admin_removed_permissions,
                            updated_role.version,
                            &mut *tx,
                        )
                        .await?
                } else {
                    self.member_repo
                        .update_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            added_permissions,
                            removed_permissions,
                            updated_role.version,
                            &mut *tx,
                        )
                        .await?
                }
            };
            fence = Some(write_fence);
        } else if let Some(new_role) = role {
            let write_fence = self
                .begin_permission_write(&room_id, &target_user_id, updated.version)
                .await?;
            updated = if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_role_with_exact_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        write_fence.version(),
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        self.abort_permission_write(&write_fence).await;
                        return Err(error);
                    }
                }
            } else {
                self.member_repo
                    .update_role_with_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        &mut *tx,
                    )
                    .await?
            };
            fence = Some(write_fence);
        }

        if apply_permission_update && !combine_role_and_permissions {
            if fence.is_none() {
                fence = Some(
                    self.begin_permission_write(&room_id, &target_user_id, updated.version)
                        .await?,
                );
            }
            let Some(write_fence) = fence.as_ref() else {
                return Err(Error::Internal(
                    "Permission update missing write fence".to_string(),
                ));
            };
            updated = if effective_is_admin {
                if write_fence.version() > 0 {
                    match self
                        .member_repo
                        .update_admin_permissions_with_exact_version_executor(
                            MemberPermissionExactVersionUpdate {
                                room_id: &room_id,
                                user_id: &target_user_id,
                                added_permissions: admin_added_permissions,
                                removed_permissions: admin_removed_permissions,
                                current_version: updated.version,
                                new_version: write_fence.version(),
                            },
                            &mut *tx,
                        )
                        .await
                    {
                        Ok(updated) => updated,
                        Err(error) => {
                            if let Some(fence) = &fence {
                                self.abort_permission_write(fence).await;
                            }
                            return Err(error);
                        }
                    }
                } else {
                    self.member_repo
                        .update_admin_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            admin_added_permissions,
                            admin_removed_permissions,
                            updated.version,
                            &mut *tx,
                        )
                        .await?
                }
            } else if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_permissions_with_exact_version_executor(
                        MemberPermissionExactVersionUpdate {
                            room_id: &room_id,
                            user_id: &target_user_id,
                            added_permissions,
                            removed_permissions,
                            current_version: updated.version,
                            new_version: write_fence.version(),
                        },
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        if let Some(fence) = &fence {
                            self.abort_permission_write(fence).await;
                        }
                        return Err(error);
                    }
                }
            } else {
                self.member_repo
                    .update_permissions_with_executor(
                        &room_id,
                        &target_user_id,
                        added_permissions,
                        removed_permissions,
                        updated.version,
                        &mut *tx,
                    )
                    .await?
            };
        }

        let snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&updated),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Some(fence) = &fence {
                    self.abort_permission_write(fence).await;
                }
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await
        {
            if let Some(fence) = &fence {
                self.abort_permission_write(fence).await;
            }
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            if let Some(fence) = &fence {
                self.abort_permission_write(fence).await;
            }
            return Err(error.into());
        }
        if let Some(fence) = &fence {
            self.finalize_committed_permission_write_best_effort(
                fence,
                &room_id,
                &target_user_id,
                updated.version,
                "admin_set_member_role_with_outbox",
            )
            .await;
        }

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;
        if role.is_some() {
            self.notify_room_settings_invalidation(&room_id).await;
        }
        Ok(updated)
    }

    pub async fn admin_kick_member_with_outbox(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        target_user_id: UserId,
        cooldown_seconds: i64,
        persisted_kicked_by: Option<UserId>,
        outbox: KickMemberOutboxOptions,
    ) -> Result<()> {
        validate_kick_cooldown_seconds(cooldown_seconds)?;
        if actor_id == target_user_id {
            return Err(Error::InvalidInput("Cannot kick yourself".to_string()));
        }

        let mut tx = self.pool.begin().await?;
        let Some(observed_version) = self
            .member_repo
            .active_member_version_for_update_with_executor(&room_id, &target_user_id, &mut tx)
            .await?
        else {
            return Err(Error::NotFound(
                "User is not an active member of this room".to_string(),
            ));
        };
        let fence = self
            .begin_permission_write(&room_id, &target_user_id, observed_version)
            .await?;
        let removed_version = match self
            .member_repo
            .remove_with_version_executor(&room_id, &target_user_id, &mut tx)
            .await
        {
            Ok(version) => version,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let Some(removed_version) = removed_version else {
            self.abort_permission_write(&fence).await;
            return Err(Error::NotFound(
                "User is not an active member of this room".to_string(),
            ));
        };
        let now = Utc::now();
        if let Err(error) = self
            .member_repo
            .add_kick_cooldown_with_executor(
                KickCooldownInsert {
                    room_id: &room_id,
                    user_id: &target_user_id,
                    kicked_by: persisted_kicked_by.as_ref(),
                    starts_at: now,
                    ends_at: now + Duration::seconds(cooldown_seconds),
                    reason: Some("kicked"),
                },
                &mut *tx,
            )
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        let cleanup = match cleanup_member_resources_in_tx(&mut tx, &room_id, &target_user_id).await
        {
            Ok(cleanup) => cleanup,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let cleanup_outbox_events = outbox
            .cleanup
            .as_ref()
            .map(|factory| factory(&cleanup))
            .transpose()?
            .unwrap_or_default();
        let snapshot = match self
            .permission_changed_snapshot_tx(&mut tx, room_id, target_user_id, actor_id, None)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(
                &mut tx,
                &snapshot,
                outbox.permission_changed.as_ref(),
            )
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_realtime_outbox_tx(&mut tx, outbox.lifecycle.as_ref())
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_realtime_outbox_events_tx(&mut tx, &cleanup_outbox_events)
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            &fence,
            &room_id,
            &target_user_id,
            removed_version,
            "admin_kick_member_with_outbox",
        )
        .await;

        self.permission_service
            .invalidate_removed_member_cache(&room_id, &target_user_id)
            .await;
        self.finalize_member_resource_cleanup_after_commit(&room_id, &target_user_id, &cleanup)
            .await;
        let subscriber_count = self
            .notification_service
            .notify_member_kicked(&room_id, &target_user_id);
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                user_id = %target_user_id,
                "Admin member kick event had no local subscribers"
            );
        }
        Ok(())
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
        self.delete_entries_with_outbox(room_id, user_id, request, None)
            .await
    }

    pub async fn delete_entries_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: DeleteEntriesRequest,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
    ) -> Result<DeleteEntriesResult> {
        let (result, ()) = self
            .delete_entries_with_precommit_and_outbox(
                room_id,
                user_id,
                request,
                |_| async { Ok(()) },
                outbox_event_factory,
            )
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
        self.delete_entries_with_precommit_and_outbox(room_id, user_id, request, precommit, None)
            .await
    }

    async fn delete_entries_with_precommit_and_outbox<T, F, Fut>(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: DeleteEntriesRequest,
        precommit: F,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
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
            .get_by_room_and_ids_with_executor(&room_id, &playlist_ids, &mut *tx)
            .await?;
        if playlists.len() != playlist_ids.len() {
            return Err(Error::NotFound(
                "One or more playlists not found".to_string(),
            ));
        }

        let media_items = self
            .media_repo
            .get_by_room_and_ids_with_executor(&room_id, &media_ids, &mut *tx)
            .await?;
        if media_items.len() != media_ids.len() {
            return Err(Error::NotFound(
                "One or more media items not found".to_string(),
            ));
        }

        let mut impact =
            plan_delete_entries_in_room_in_tx(&mut tx, &room_id, &playlist_ids, &media_ids, force)
                .await?;

        let affected_playlists = self
            .playlist_repo
            .get_by_room_and_ids_with_executor(&room_id, &impact.deleted_playlist_ids, &mut *tx)
            .await?;
        if affected_playlists.len() != impact.deleted_playlist_ids.len() {
            return Err(Error::Internal(
                "Delete plan referenced a playlist that no longer exists".to_string(),
            ));
        }

        let affected_media = self
            .media_repo
            .get_by_room_and_ids_with_executor(&room_id, &impact.deleted_media_ids, &mut *tx)
            .await?;
        if affected_media.len() != impact.deleted_media_ids.len() {
            return Err(Error::Internal(
                "Delete plan referenced a media item that no longer exists".to_string(),
            ));
        }

        let has_foreign_resources = affected_playlists
            .iter()
            .any(|playlist| playlist.creator_id.as_ref() != Some(&user_id))
            || affected_media
                .iter()
                .any(|media| media.creator_id.as_ref() != Some(&user_id));

        if !has_active_room_membership_in_tx(&mut tx, &room_id, &user_id).await? {
            return Err(Error::Authorization(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        }

        if has_foreign_resources
            && !has_room_permission_in_tx(
                &mut tx,
                &self.permission_service,
                &room_id,
                &user_id,
                crate::models::RoomPermission::DELETE_MEDIA_RESOURCE_ANY,
            )
            .await?
        {
            return Err(Error::Authorization(
                synctv_common::messages::PERMISSION_DENIED.to_string(),
            ));
        }
        let plan = DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: None,
        };
        let precommit_result = precommit(plan.clone()).await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &mut impact).await?;
        let committed_plan = DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: impact.playback_state.clone(),
        };
        let outbox_events = outbox_event_factory
            .as_ref()
            .map(|factory| factory(&committed_plan))
            .transpose()?
            .unwrap_or_default();
        if let Some(outbox) = &self.realtime_outbox {
            for event in &outbox_events {
                outbox.insert_with_executor(event, &mut *tx).await?;
            }
        }

        tx.commit().await?;

        if let Some(state) = impact.playback_state.clone() {
            self.broadcast_playback_reset_after_entry_deletion(state)
                .await;
        }
        self.cleanup_deleted_media_file_references(&impact.deleted_media_file_references)
            .await;

        let should_notify_playlist_delete = !impact.deleted_playlist_ids.is_empty();
        if !impact.deleted_media_ids.is_empty() || should_notify_playlist_delete {
            let actor_username = match self.resolve_actor_username(&user_id).await {
                Ok(username) => username,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        user_id = %user_id,
                        "Skipped delete entries notifications because actor username lookup failed"
                    );
                    return Ok((delete_entries_result_from_impact(impact), precommit_result));
                }
            };
            for media_id in &impact.deleted_media_ids {
                let subscriber_count = self.notification_service.notify_media_removed(
                    &room_id,
                    Some(&user_id),
                    &actor_username,
                    *media_id,
                );
                if subscriber_count == 0 {
                    tracing::debug!(
                        room_id = %room_id,
                        media_id = %media_id,
                        "Media removed event had no local subscribers"
                    );
                }
            }
            for playlist_id in &impact.deleted_playlist_ids {
                let subscriber_count = self.notification_service.notify_playlist_deleted(
                    &room_id,
                    Some(&user_id),
                    &actor_username,
                    *playlist_id,
                );
                if subscriber_count == 0 {
                    tracing::debug!(
                        room_id = %room_id,
                        playlist_id = %playlist_id,
                        "Playlist deleted event had no local subscribers"
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
        self.admin_delete_entries_as_with_precommit_and_outbox(
            room_id, actor, request, precommit, None,
        )
        .await
    }

    async fn admin_delete_entries_as_with_precommit_and_outbox<T, F, Fut>(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        request: DeleteEntriesRequest,
        precommit: F,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
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
            .get_by_room_and_ids_with_executor(&room_id, &playlist_ids, &mut *tx)
            .await?;
        if playlists.len() != playlist_ids.len() {
            return Err(Error::NotFound(
                "One or more playlists not found".to_string(),
            ));
        }

        let media_items = self
            .media_repo
            .get_by_room_and_ids_with_executor(&room_id, &media_ids, &mut *tx)
            .await?;
        if media_items.len() != media_ids.len() {
            return Err(Error::NotFound(
                "One or more media items not found".to_string(),
            ));
        }

        let mut impact =
            plan_delete_entries_in_room_in_tx(&mut tx, &room_id, &playlist_ids, &media_ids, force)
                .await?;
        let plan = DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: None,
        };
        let precommit_result = precommit(plan.clone()).await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &mut impact).await?;
        let committed_plan = DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: impact.playback_state.clone(),
        };
        let outbox_events = outbox_event_factory
            .as_ref()
            .map(|factory| factory(&committed_plan))
            .transpose()?
            .unwrap_or_default();
        if let Some(outbox) = &self.realtime_outbox {
            for event in &outbox_events {
                outbox.insert_with_executor(event, &mut *tx).await?;
            }
        }

        tx.commit().await?;

        if let Some(state) = impact.playback_state.clone() {
            self.broadcast_playback_reset_after_entry_deletion(state)
                .await;
        }
        self.cleanup_deleted_media_file_references(&impact.deleted_media_file_references)
            .await;

        if !impact.deleted_media_ids.is_empty() || !impact.deleted_playlist_ids.is_empty() {
            for media_id in &impact.deleted_media_ids {
                let subscriber_count = self.notification_service.notify_media_removed(
                    &room_id,
                    Some(&admin_user_id),
                    actor.username(),
                    *media_id,
                );
                if subscriber_count == 0 {
                    tracing::debug!(
                        room_id = %room_id,
                        media_id = %media_id,
                        "Media removed event had no local subscribers"
                    );
                }
            }
            for playlist_id in &impact.deleted_playlist_ids {
                let subscriber_count = self.notification_service.notify_playlist_deleted(
                    &room_id,
                    Some(&admin_user_id),
                    actor.username(),
                    *playlist_id,
                );
                if subscriber_count == 0 {
                    tracing::debug!(
                        room_id = %room_id,
                        playlist_id = %playlist_id,
                        "Playlist deleted event had no local subscribers"
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

    pub async fn admin_delete_entries_as_with_outbox(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        request: DeleteEntriesRequest,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
    ) -> Result<DeleteEntriesResult> {
        let (result, ()) = self
            .admin_delete_entries_as_with_precommit_and_outbox(
                room_id,
                actor,
                request,
                |_| async { Ok(()) },
                outbox_event_factory,
            )
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
            Ok(self
                .media_service
                .get_room_media(room_id, &media_id)
                .await?)
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
            description: None,
        };
        self.media_service
            .edit_media(room_id, user_id, request)
            .await
    }

    /// Clear media and child playlists in a playlist scope.
    ///
    /// The `CLEAR_MEDIA_RESOURCES` permission check is performed inside the
    /// transaction so revocations cannot race with the clear operation.
    ///
    /// `playlist_id = None` clears the room-root scope. `Some(id)` clears the
    /// given playlist's contents while keeping the playlist itself.
    pub async fn clear_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: Option<PlaylistId>,
    ) -> Result<ClearPlaylistResult> {
        self.clear_playlist_with_outbox(room_id, user_id, playlist_id, None)
            .await
    }

    pub async fn clear_playlist_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: Option<PlaylistId>,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
    ) -> Result<ClearPlaylistResult> {
        let mut tx = self.pool.begin().await?;
        ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &self.permission_service,
            &room_id,
            &user_id,
            crate::models::RoomPermission::CLEAR_MEDIA_RESOURCES,
        )
        .await?;

        if let Some(playlist_id) = playlist_id {
            let exists = sqlx::query_scalar!(
                r#"SELECT EXISTS(
                    SELECT 1
                    FROM playlists
                    WHERE room_id = $1 AND id = $2
                ) AS "exists!""#,
                room_id.as_i64(),
                playlist_id.as_i64()
            )
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                return Err(Error::NotFound("Playlist not found".to_string()));
            }
        }

        let mut impact = plan_clear_playlist_scope_in_tx(&mut tx, &room_id, playlist_id).await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &mut impact).await?;
        let committed_plan = DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: impact.playback_state.clone(),
        };
        let outbox_events = outbox_event_factory
            .as_ref()
            .map(|factory| factory(&committed_plan))
            .transpose()?
            .unwrap_or_default();
        if let Some(outbox) = &self.realtime_outbox {
            for event in &outbox_events {
                outbox.insert_with_executor(event, &mut *tx).await?;
            }
        }

        tx.commit().await?;

        if let Some(state) = impact.playback_state.clone() {
            self.broadcast_playback_reset_after_entry_deletion(state)
                .await;
        }
        self.cleanup_deleted_media_file_references(&impact.deleted_media_file_references)
            .await;

        let actor_username = match self.resolve_actor_username(&user_id).await {
            Ok(username) => username,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    user_id = %user_id,
                    "Skipped clear playlist notifications because actor username lookup failed"
                );
                let deleted_count =
                    deleted_count_to_i64(impact.deleted_media_ids.len(), "deleted media count")?;
                return Ok(ClearPlaylistResult {
                    deleted_count,
                    deleted_playlists: impact.deleted_playlist_ids.len(),
                    deleted_playlist_ids: impact.deleted_playlist_ids,
                    deleted_media_ids: impact.deleted_media_ids,
                    playback_state: impact.playback_state,
                });
            }
        };
        for media_id in &impact.deleted_media_ids {
            let subscriber_count = self.notification_service.notify_media_removed(
                &room_id,
                Some(&user_id),
                &actor_username,
                *media_id,
            );
            if subscriber_count == 0 {
                tracing::debug!(
                    room_id = %room_id,
                    media_id = %media_id,
                    "Media removed event after clear_playlist had no local subscribers"
                );
            }
        }
        for playlist_id in &impact.deleted_playlist_ids {
            let subscriber_count = self.notification_service.notify_playlist_deleted(
                &room_id,
                Some(&user_id),
                &actor_username,
                *playlist_id,
            );
            if subscriber_count == 0 {
                tracing::debug!(
                    room_id = %room_id,
                    playlist_id = %playlist_id,
                    "Playlist deleted event after clear_playlist had no local subscribers"
                );
            }
        }

        let deleted_count =
            deleted_count_to_i64(impact.deleted_media_ids.len(), "deleted media count")?;
        Ok(ClearPlaylistResult {
            deleted_count,
            deleted_playlists: impact.deleted_playlist_ids.len(),
            deleted_playlist_ids: impact.deleted_playlist_ids,
            deleted_media_ids: impact.deleted_media_ids,
            playback_state: impact.playback_state,
        })
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
        required_permission: RoomPermission,
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
        let cursor =
            cursor.map(|(created_at, id)| crate::models::ChatHistoryCursor { created_at, id });
        let (messages, next) = self
            .chat_repo
            .list_by_room_cursor(room_id, cursor, limit, true)
            .await?;
        Ok((
            messages
                .into_iter()
                .map(|message| message.message)
                .collect(),
            next.map(|cursor| (cursor.created_at, cursor.id)),
        ))
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
            client_message_id: None,
            content,
            message_type: ChatMessageType::Text,
            status: crate::models::ChatMessageStatus::Active,
            version: 1,
            reply_to_message_id: None,
            reply_to_message_created_at: None,
            metadata: serde_json::Value::Object(Default::default()),
            edited_at: None,
            deleted_at: None,
            deleted_by: None,
            delete_reason: None,
            created_at: Utc::now(),
        };
        self.chat_repo.create(&message).await
    }

    /// Check if user has permission in room
    pub async fn check_permission(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: RoomPermission,
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
        let permission_fences = self
            .reserve_room_member_permission_fences(room_id, &mut tx)
            .await?;
        let impact = match soft_delete_room_and_cleanup_in_tx(&mut tx, room_id).await {
            Ok(impact) => impact,
            Err(error) => {
                self.abort_room_member_permission_fences(&permission_fences)
                    .await;
                return Err(error);
            }
        };
        if let (Some(outbox), Some(event)) = (&self.realtime_outbox, &outbox_event) {
            if let Err(error) = outbox.insert_with_executor(event, &mut *tx).await {
                self.abort_room_member_permission_fences(&permission_fences)
                    .await;
                return Err(error);
            }
        }

        if let Err(error) = tx.commit().await {
            self.abort_room_member_permission_fences(&permission_fences)
                .await;
            return Err(error.into());
        }

        if let Err(error) = self
            .commit_removed_room_member_permission_fences(
                permission_fences,
                &impact.removed_members,
            )
            .await
        {
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
        let permission_fences = self
            .reserve_room_member_permission_fences(room_id, &mut tx)
            .await?;
        let impact = match soft_delete_room_and_cleanup_in_tx(&mut tx, room_id).await {
            Ok(impact) => impact,
            Err(error) => {
                self.abort_room_member_permission_fences(&permission_fences)
                    .await;
                return Err(error);
            }
        };

        if let Err(error) = tx.commit().await {
            self.abort_room_member_permission_fences(&permission_fences)
                .await;
            return Err(error.into());
        }

        if let Err(error) = self
            .commit_removed_room_member_permission_fences(
                permission_fences,
                &impact.removed_members,
            )
            .await
        {
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
        request: crate::service::playback::PlaybackUpdateRequest,
    ) -> Result<RoomPlaybackState> {
        if request.actor_user_id != *actor.user_id() {
            return Err(Error::Authorization(
                "Playback update actor does not match authorized admin actor".to_string(),
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

    /// Remove all active `RoomRole::Guest` members from a room.
    ///
    /// This is one part of room-wide guest revocation when room policy changes
    /// make guest access invalid.
    async fn remove_guest_role_members(&self, room_id: &RoomId) -> Result<()> {
        let members = self.member_service.list_members(room_id).await?;

        for member in members {
            if member.role == RoomRole::Guest {
                self.member_service
                    .delete_active_membership(*room_id, member.user_id)
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
        let subscriber_count = self.notification_service.kick_all_guests(room_id, reason);
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                "Guest kick event had no local subscribers"
            );
        }
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
            .set_version(&key, next, Self::room_guest_version_ttl_secs())
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
        self.ban_room_with_outbox(room_id, admin_user_id, None)
            .await
    }

    pub async fn ban_room_with_outbox(
        &self,
        room_id: &RoomId,
        admin_user_id: &UserId,
        outbox_event: Option<NewRealtimeOutboxEvent>,
    ) -> Result<Room> {
        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.is_banned {
            return Err(Error::InvalidInput("Room is already banned".to_string()));
        }

        let mut tx = self.pool.begin().await?;
        let updated_room = crate::repository::RoomRepository::update_ban_status_with_executor(
            room_id, true, &mut tx,
        )
        .await?;
        if let (Some(outbox), Some(event)) = (&self.realtime_outbox, &outbox_event) {
            outbox.insert_with_executor(event, &mut *tx).await?;
        }
        tx.commit().await?;
        self.notify_room_invalidation(room_id).await;

        self.write_audit_event(
            admin_user_id,
            &admin_user_id.to_string(),
            AuditAction::RoomBanned,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({"reason": "Room banned by admin"}),
        )
        .await?;

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

        self.write_audit_event(
            admin_user_id,
            &admin_user_id.to_string(),
            AuditAction::RoomUnbanned,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({"reason": "Room unbanned by admin"}),
        )
        .await?;

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
    global_default: RoomPermissionSet,
) -> RoomPermissionSet {
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
    global_default: RoomPermissionSet,
    permission: RoomPermission,
) -> bool {
    if !member.has_permission(permission, RoomPermissionSet::all()) {
        return false;
    }

    effective_room_permissions_from_base(settings, member, global_default).has(permission)
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

    let rows = sqlx::query!(
        r#"WITH RECURSIVE target_playlists AS (
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
        SELECT id AS "id!: PlaylistId", MAX(depth) AS depth
        FROM target_playlists
        GROUP BY id
        ORDER BY MAX(depth) DESC, id"#,
        room_id.as_i64(),
        &playlist_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push((row.id, row.depth.unwrap_or(0)));
    }
    Ok(result)
}

async fn collect_all_room_playlist_nodes_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<Vec<(PlaylistId, i32)>> {
    let rows = sqlx::query!(
        r#"WITH RECURSIVE playlist_tree AS (
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
        SELECT id AS "id!: PlaylistId", MAX(depth) AS depth
        FROM playlist_tree
        GROUP BY id
        ORDER BY MAX(depth) DESC, id"#,
        room_id.as_i64()
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push((row.id, row.depth.unwrap_or(0)));
    }
    Ok(result)
}

async fn collect_room_root_media_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<Vec<MediaId>> {
    let media_ids = sqlx::query_scalar!(
        r#"SELECT id AS "id: MediaId"
         FROM media
         WHERE room_id = $1
           AND playlist_id IS NULL
         ORDER BY id"#,
        room_id.as_i64(),
    )
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

    let media_ids = sqlx::query_scalar!(
        r#"WITH RECURSIVE target_playlists AS (
            SELECT id
            FROM playlists
            WHERE id = ANY($1)
            UNION ALL
            SELECT p.id
            FROM playlists p
            JOIN target_playlists tp ON p.parent_id = tp.id
        )
        SELECT DISTINCT m.id AS "id: MediaId"
        FROM media m
        WHERE m.room_id = $2
          AND (
              m.id = ANY($3)
              OR m.playlist_id IN (SELECT id FROM target_playlists)
          )
        ORDER BY m.id"#,
        &playlist_id_strs,
        room_id.as_i64(),
        &explicit_media_id_strs
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(media_ids)
}

async fn collect_media_cover_file_references_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    media_ids: &[MediaId],
) -> Result<Vec<crate::models::FileReferenceTarget>> {
    if media_ids.is_empty() {
        return Ok(Vec::new());
    }

    let media_id_strs: Vec<i64> = media_ids.iter().map(MediaId::as_i64).collect();
    let rows = sqlx::query_as!(
        MediaCoverFileReferenceRow,
        r#"
        SELECT m.id AS "id: MediaId",
               fr.storage_backend,
               fr.object_key
          FROM media m
          JOIN file_references fr
            ON fr.id = m.cover_file_reference_id
           AND fr.released_at IS NULL
         WHERE m.room_id = $1
           AND m.id = ANY($2)
        "#,
        room_id.as_i64(),
        &media_id_strs
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| crate::models::FileReferenceTarget {
            storage_backend: row.storage_backend,
            object_key: row.object_key,
            reference_kind: "media_cover".to_string(),
            reference_id: row.id.to_string(),
        })
        .collect())
}

async fn plan_playback_reset_for_deleted_entries_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    deleted_playlist_ids: &[PlaylistId],
    deleted_media_ids: &[MediaId],
    force: bool,
) -> Result<bool> {
    let playback_row = sqlx::query!(
        r#"SELECT playing_media_id AS "playing_media_id?: MediaId",
                  playing_playlist_id AS "playing_playlist_id?: PlaylistId"
         FROM room_playback_state
         WHERE room_id = $1
         FOR UPDATE"#,
        room_id.as_i64(),
    )
    .fetch_optional(&mut **tx)
    .await?;

    let Some(row) = playback_row else {
        return Ok(false);
    };

    let deletes_playing_media = row.playing_media_id.as_ref().is_some_and(|current_id| {
        deleted_media_ids
            .iter()
            .any(|media_id| media_id == current_id)
    });

    let deletes_playing_playlist = row.playing_playlist_id.as_ref().is_some_and(|current_id| {
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
        sqlx::query!("DELETE FROM playlists WHERE id = ANY($1)", &ids)
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
        playback_state: impact.playback_state,
    }
}

async fn apply_delete_entries_impact_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    impact: &mut EntryDeletionImpact,
) -> Result<()> {
    if impact.playback_reset {
        let state = sqlx::query_as!(
            RoomPlaybackState,
            r#"WITH current_state AS (
                SELECT room_id, current_progress_id
                FROM room_playback_state
                WHERE room_id = $1
                FOR UPDATE
            ),
            reset_progress AS (
                UPDATE room_playback_progress progress
                SET "position" = 0,
                    version = version + 1
                FROM current_state
                WHERE progress.id = current_state.current_progress_id
                RETURNING progress.id
            ),
            updated AS (
                UPDATE room_playback_state state
                SET playing_media_id = NULL,
                    playing_playlist_id = NULL,
                    target = ''::bytea,
                    current_progress_id = NULL,
                    speed = 1.0,
                    is_playing = false,
                    version = version + 1,
                    updated_at = NOW()
                FROM current_state
                WHERE state.room_id = current_state.room_id
                RETURNING state.room_id,
                          state.playing_media_id,
                          state.playing_playlist_id,
                          state.target,
                          state.current_progress_id,
                          state.speed,
                          state.is_playing,
                          state.updated_at,
                          state.version
            )
            SELECT room_id AS "room_id: RoomId",
                   playing_media_id AS "playing_media_id: MediaId",
                   playing_playlist_id AS "playing_playlist_id: PlaylistId",
                   target,
                   current_progress_id,
                   0.0::DOUBLE PRECISION AS "position!",
                   speed AS "speed!",
                   is_playing,
                   updated_at,
                   version
            FROM updated"#,
            room_id.as_i64(),
        )
        .fetch_one(&mut **tx)
        .await?;
        impact.playback_state = Some(state);
    }

    if !impact.deleted_media_ids.is_empty() {
        let media_id_strs: Vec<i64> = impact
            .deleted_media_ids
            .iter()
            .map(MediaId::as_i64)
            .collect();
        sqlx::query!(
            "DELETE FROM room_playback_progress WHERE room_id = $1 AND media_id = ANY($2)",
            room_id.as_i64(),
            &media_id_strs,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query!("DELETE FROM media WHERE id = ANY($1)", &media_id_strs)
            .execute(&mut **tx)
            .await?;
    }

    if !impact.deleted_playlist_ids.is_empty() {
        let playlist_id_strs: Vec<i64> = impact
            .deleted_playlist_ids
            .iter()
            .map(PlaylistId::as_i64)
            .collect();
        sqlx::query!(
            "DELETE FROM room_playback_progress WHERE room_id = $1 AND playlist_id = ANY($2)",
            room_id.as_i64(),
            &playlist_id_strs,
        )
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
    let deleted_media_file_references =
        collect_media_cover_file_references_in_tx(tx, room_id, &deleted_media_ids).await?;
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
        deleted_media_file_references,
        playback_reset,
        playback_state: None,
    })
}

async fn collect_child_playlist_nodes_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    parent_playlist_id: Option<PlaylistId>,
) -> Result<Vec<(PlaylistId, i32)>> {
    let rows = sqlx::query!(
        r#"WITH RECURSIVE child_playlists AS (
            SELECT id, 0 AS depth
            FROM playlists
            WHERE room_id = $1
              AND (
                  ($2::BIGINT IS NULL AND parent_id IS NULL)
                  OR parent_id = $2
              )
            UNION ALL
            SELECT p.id, cp.depth + 1
            FROM playlists p
            JOIN child_playlists cp ON p.parent_id = cp.id
            WHERE p.room_id = $1
        )
        SELECT id AS "playlist_id!: PlaylistId", COALESCE(MAX(depth), 0) AS "depth!: i32"
        FROM child_playlists
        GROUP BY id
        ORDER BY MAX(depth) DESC, id"#,
        room_id.as_i64(),
        parent_playlist_id.map(|playlist_id| playlist_id.as_i64())
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push((row.playlist_id, row.depth));
    }
    Ok(result)
}

async fn collect_direct_scope_media_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    playlist_id: Option<PlaylistId>,
) -> Result<Vec<MediaId>> {
    let media_ids = sqlx::query_scalar!(
        r#"SELECT id AS "id: MediaId"
         FROM media
         WHERE room_id = $1
           AND (
               ($2::BIGINT IS NULL AND playlist_id IS NULL)
               OR playlist_id = $2
         )
         ORDER BY id"#,
        room_id.as_i64(),
        playlist_id.map(|playlist_id| playlist_id.as_i64())
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(media_ids)
}

async fn plan_clear_playlist_scope_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    playlist_id: Option<PlaylistId>,
) -> Result<EntryDeletionImpact> {
    let playlist_nodes = collect_child_playlist_nodes_in_tx(tx, room_id, playlist_id).await?;
    let deleted_playlist_ids: Vec<PlaylistId> = playlist_nodes
        .iter()
        .map(|(playlist_id, _)| *playlist_id)
        .collect();
    let direct_media_ids = collect_direct_scope_media_ids_in_tx(tx, room_id, playlist_id).await?;
    let deleted_media_ids =
        collect_deleted_media_ids_in_tx(tx, room_id, &deleted_playlist_ids, &direct_media_ids)
            .await?;
    let deleted_media_file_references =
        collect_media_cover_file_references_in_tx(tx, room_id, &deleted_media_ids).await?;
    let playback_reset = plan_playback_reset_for_deleted_entries_in_tx(
        tx,
        room_id,
        &deleted_playlist_ids,
        &deleted_media_ids,
        true,
    )
    .await?;

    Ok(EntryDeletionImpact {
        playlist_nodes,
        deleted_playlist_ids,
        deleted_media_ids,
        deleted_media_file_references,
        playback_reset,
        playback_state: None,
    })
}

async fn collect_member_owned_root_playlist_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<Vec<PlaylistId>> {
    let playlist_ids = sqlx::query_scalar!(
        r#"SELECT id AS "id: PlaylistId"
           FROM playlists
           WHERE room_id = $1
             AND creator_id = $2
             AND (
                 parent_id IS NULL
                 OR parent_id NOT IN (
                     SELECT id
                     FROM playlists
                     WHERE room_id = $1
                       AND creator_id = $2
                 )
             )
           ORDER BY id"#,
        room_id.as_i64(),
        user_id.as_i64()
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(playlist_ids)
}

async fn collect_member_owned_root_media_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<Vec<MediaId>> {
    let media_ids = sqlx::query_scalar!(
        r#"SELECT id AS "id: MediaId"
           FROM media
           WHERE room_id = $1
             AND creator_id = $2
             AND (
                 playlist_id IS NULL
                 OR playlist_id NOT IN (
                     SELECT id
                     FROM playlists
                     WHERE room_id = $1
                       AND creator_id = $2
                 )
             )
           ORDER BY id"#,
        room_id.as_i64(),
        user_id.as_i64()
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(media_ids)
}

async fn cleanup_member_resources_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<MemberResourceCleanupResult> {
    let playlist_ids = collect_member_owned_root_playlist_ids_in_tx(tx, room_id, user_id).await?;
    let media_ids = collect_member_owned_root_media_ids_in_tx(tx, room_id, user_id).await?;

    if playlist_ids.is_empty() && media_ids.is_empty() {
        return Ok(MemberResourceCleanupResult::default());
    }

    let mut impact =
        plan_delete_entries_in_room_in_tx(tx, room_id, &playlist_ids, &media_ids, true).await?;
    apply_delete_entries_impact_in_tx(tx, room_id, &mut impact).await?;

    Ok(MemberResourceCleanupResult {
        deleted_playlist_ids: impact.deleted_playlist_ids,
        deleted_media_ids: impact.deleted_media_ids,
        playback_reset: impact.playback_reset,
        playback_state: impact.playback_state,
    })
}

pub(crate) async fn soft_delete_room_and_cleanup_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<RoomCleanupImpact> {
    let now = chrono::Utc::now();
    let deleted = sqlx::query!(
        r#"UPDATE rooms
         SET deleted_at = $2, updated_at = $2, version = version + 1
         WHERE id = $1 AND deleted_at IS NULL"#,
        room_id.as_i64(),
        now,
    )
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

    let playback_rows_deleted = sqlx::query!(
        "DELETE FROM room_playback_state WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();

    if !deleted_media_ids.is_empty() {
        let media_id_strs: Vec<i64> = deleted_media_ids.iter().map(MediaId::as_i64).collect();
        sqlx::query!("DELETE FROM media WHERE id = ANY($1)", &media_id_strs)
            .execute(&mut **tx)
            .await?;
    }

    delete_playlist_ids_in_depth_order_in_tx(tx, &playlist_nodes).await?;

    let mut removed_members: Vec<RemovedRoomMember> = sqlx::query!(
        r#"DELETE FROM room_members
         WHERE room_id = $1
         RETURNING room_id as "room_id: RoomId",
                   user_id as "user_id: UserId",
                   version"#,
        room_id as &RoomId,
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|row| RemovedRoomMember {
        room_id: row.room_id,
        user_id: row.user_id,
        version: row.version,
    })
    .collect();
    for member in &mut removed_members {
        member.version = sqlx::query_scalar!(
            "INSERT INTO room_member_versions (room_id, user_id, version, is_member, updated_at)
             VALUES ($1, $2, $3::BIGINT + 1, FALSE, CURRENT_TIMESTAMP)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET version = GREATEST(room_member_versions.version + 1, EXCLUDED.version),
                 is_member = FALSE,
                 updated_at = CURRENT_TIMESTAMP
             RETURNING version",
            &member.room_id as &RoomId,
            &member.user_id as &UserId,
            member.version,
        )
        .fetch_one(&mut **tx)
        .await?;
    }
    let members_deleted = removed_members.len() as u64;

    let settings_deleted = sqlx::query!(
        "DELETE FROM room_settings WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();

    let chat_deleted = sqlx::query!(
        "DELETE FROM chat_messages WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();

    Ok(RoomCleanupImpact {
        deleted_playlist_ids,
        deleted_media_ids,
        members_deleted,
        removed_members,
        settings_deleted,
        playback_rows_deleted,
        chat_deleted,
    })
}

#[cfg(test)]
#[path = "room_tests.rs"]
mod tests;
