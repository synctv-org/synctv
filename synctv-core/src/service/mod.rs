pub(crate) mod audit;
pub(crate) mod audit_partition_manager;
pub mod auth;
pub(crate) mod ban_record;
pub mod chat;
pub(crate) mod chat_partition_manager;
pub mod cleanup;
pub(crate) mod content_filter;
pub mod content_report;
pub mod db_maintenance;
pub mod distributed_lock;
pub(crate) mod email;
pub(crate) mod email_templates;
pub(crate) mod email_token;
pub mod file_storage;
pub(crate) mod file_upload_policies;
pub mod global_settings;
pub mod media;
pub mod member;
pub mod notification;
pub(crate) mod notification_partition_manager;
pub(crate) mod oauth2;
pub(crate) mod optimistic_retry;
mod partitioning;
pub(crate) mod passkey;
pub mod permission;
pub mod playback;
pub mod playback_auto_advance;
pub mod playback_duration_probe;
pub mod playlist;
pub mod presence;
mod provider_binding;
pub(crate) mod providers_manager;
pub mod publish_key;
pub mod rate_limit;
pub(crate) mod remote_provider_manager;
pub(crate) mod review;
pub mod room;
pub(crate) mod room_settings;
mod session_store;
pub(crate) mod settings;
pub(crate) mod settings_vars;
mod source_config;
pub(crate) mod stun_server;
pub mod user;
pub mod user_notification;
pub(crate) mod ws_ticket;

pub use audit::{
    AuditEventParams, AuditFlushHandle, AuditLog, AuditService, StreamKickAuditRequest,
};
pub use audit_partition_manager::{
    ensure_audit_partitions_on_startup, AuditPartitionManager, PartitionHealth, PartitionStats,
};
pub use auth::brute_force::{AttemptTracker, BruteForceConfig};
pub use auth::token_blacklist::{
    InMemoryTokenBlacklistStore, PgTokenBlacklistStore, TieredTokenBlacklistStore,
};
pub use auth::{
    AuthenticatedToken, BruteForceProtection, BruteForceProtectionService, Claims, GuestClaims,
    JwtService, SecurityPipeline, SecurityPipelineRuntime, TokenAuthContext, TokenBlacklistStore,
    TokenType,
};
pub use ban_record::{
    BanRecordListQuery, BanRecordPage, BanRecordRow, BanRecordService, BanRecordTargetType,
};
pub use chat::ChatService;
pub use chat_partition_manager::{
    ensure_chat_partitions_on_startup, ChatPartitionHealth, ChatPartitionManager,
};
pub use cleanup::{CleanupConfig, CleanupResult, CleanupService};
pub use content_filter::{ContentFilter, ContentFilterError};
pub use content_report::{
    ContentReportListQuery, ContentReportListScope, ContentReportPage, ContentReportService,
};
pub use db_maintenance::DatabaseMaintenanceService;
pub use distributed_lock::{with_coordination_lock, CoordinationLock, DistributedLock, LockGuard};
pub use email::{mask_email, EmailConfig, EmailConfigProvider, EmailService};
pub use email_token::EmailTokenService;
pub use file_storage::{
    submitted_file_reference_from_reuse_token, submitted_file_reference_from_session_file,
    upload_token_from_session_file, DatabaseFileStorageCompressionConfig,
    DatabaseFileStorageService, DisabledFileStorageService, FileStorageBackendRegistry,
    FileStorageCleanupOrigin, FileStorageContext, FileStorageService, RoutedFileStorageService,
    S3CompatibleFileStorageService, S3FileStorageConfig,
};
pub use file_upload_policies::{
    chat_attachment_upload_policy, media_cover_upload_policy, playlist_cover_upload_policy,
    room_cover_upload_policy, user_avatar_upload_policy, MAX_CHAT_ATTACHMENT_SIZE_BYTES,
    MAX_MEDIA_COVER_SIZE_BYTES, MAX_PLAYLIST_COVER_SIZE_BYTES, MAX_ROOM_COVER_SIZE_BYTES,
    MAX_USER_AVATAR_SIZE_BYTES,
};
pub use global_settings::{
    ConfiguredIceServer, IceServerList, OAuth2ProviderConfig, OAuth2ProviderConfigs,
    OAuth2SignupPolicy, PublicSettings, RoomPasswordPolicy, RuntimeEmailConfigProvider,
    SettingsRegistry,
};
pub use media::{
    MediaService, RealtimeOutboxMediaBatchEventFactory, RealtimeOutboxMediaEventFactory,
};
pub use member::MemberService;
pub use notification::{NotificationService, RoomEvent};
pub use notification_partition_manager::{
    ensure_notification_partitions_on_startup, NotificationPartitionHealth,
    NotificationPartitionManager,
};
pub use oauth2::{
    local_oauth_state_store, OAuth2LinkResult, OAuth2PendingRegistration, OAuth2Service,
    OAuth2ServiceRuntime, OAuth2State, OAuth2UserInfo, OAuthStateStore, RedisOAuthStateStore,
};
pub use optimistic_retry::retry_with_optimistic_lock;
pub use passkey::{local_passkey_session_store, PasskeyService, PasskeySessionStore};
pub use permission::{EffectivePermissionCalculator, PermissionService, RuntimePermissionDefaults};
pub use playback::{PlaybackService, PlaybackStatePatch, PlaybackStateUpdateRequest, SeekResponse};
pub use playback_auto_advance::PlaybackAutoAdvanceService;
pub use playback_duration_probe::PlaybackDurationProbeService;
pub use playlist::{PlaylistService, RealtimeOutboxPlaylistEventFactory};
pub use presence::{
    OnlineNodeStats, OnlinePresenceService, OnlineRoomStats, OnlineUserRoomStats, OnlineUserStats,
    PresenceConnection, PresenceEvent, PresenceOverview,
};
pub use providers_manager::ProvidersManager;
pub use publish_key::{JtiStore, PublishKey, PublishKeyService, StreamingPublishKeyService};
pub use rate_limit::{RateLimitConfig, RateLimitError, RateLimiter, RequestRateLimiterService};
pub use remote_provider_manager::{ProviderInstanceStore, RemoteProviderManager};
pub use review::{
    ReviewPage, ReviewService, RoomCreationReviewListQuery, RoomCreationReviewRecord,
    RoomJoinReviewListQuery, RoomJoinReviewRecord, UserRegistrationReviewListQuery,
    UserRegistrationReviewRecord,
};
pub use room::{
    AdminAddMemberWithOutboxRequest, AdminRejectJoinRequestWithOutbox, AuthorizedAdminActor,
    MemberPermissionPatch, PermissionChangedOutboxSnapshot,
    RealtimeOutboxDeleteEntriesEventFactory, RealtimeOutboxPermissionChangedEventFactory,
    RealtimeOutboxRoomEventFactory, RealtimeOutboxSettingsEventFactory,
    RealtimeOutboxUserLeftEventFactory, RoomService, UpdateMemberWithOutboxRequest,
    UserLeftOutboxSnapshot,
};
pub use room_settings::{CacheStats, RoomSettingsService};
pub use settings::SettingsService;
pub use settings_vars::{Setting, SettingsStorage};
pub use stun_server::{
    resolve_external_ip, validate_external_addr, BuiltinStunRuntimeReason, BuiltinStunRuntimeState,
    StunServer, StunServerConfig, WebRtcRuntimeMode, WebRtcRuntimeStatus,
};
pub use user::UserService;
pub use user::{
    AccountRegistrationOutcome, AuthFactorMethod, AuthenticatedLogin, MfaChallenge,
    MfaSessionStore, OpaqueLoginSessionStore, OpaqueRegistrationSessionStore,
    PendingAccountRegistration, RegistrationMode, RegistrationPolicy,
    SensitiveVerificationChallenge, SensitiveVerificationOutcome,
    SensitiveVerificationSessionStore,
};
pub use user_notification::UserNotificationService;
pub use ws_ticket::{
    CreateGuestTicketRequest, PendingValidatedTicket, RedisTicketStore, TicketStore,
    UserValidationResult, UserValidator, ValidatedGuestTicket, ValidatedTicket,
    WebSocketTicketService, WsTicketData, WsTicketService,
};

/// Trait for checking if the current node is the cluster leader.
///
/// Singleton tasks (cleanup, partition management, etc.) should only run
/// on the leader node to avoid duplicate work across replicas.
///
/// In single-node mode (no cluster), implementations should always return `true`.
pub trait LeaderCheck: Send + Sync {
    /// Returns `true` if this node is currently the cluster leader.
    fn is_leader(&self) -> bool;
}

/// A `LeaderCheck` implementation that always returns `true`.
///
/// Used in single-node deployments where there is no cluster and
/// every node should run singleton tasks.
pub struct AlwaysLeader;

impl LeaderCheck for AlwaysLeader {
    fn is_leader(&self) -> bool {
        true
    }
}
