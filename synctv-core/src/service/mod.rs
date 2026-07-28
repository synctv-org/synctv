pub(crate) mod audit;
pub(crate) mod audit_partition_manager;
pub(crate) mod auth;
pub(crate) mod ban_record;
pub(crate) mod chat;
pub(crate) mod cleanup;
pub(crate) mod cleanup_ops;
pub(crate) mod content_filter;
pub(crate) mod content_report;
pub(crate) mod db_maintenance;
pub(crate) mod distributed_lock;
pub(crate) mod email;
pub(crate) mod email_outbox;
pub(crate) mod email_templates;
pub(crate) mod email_token;
pub(crate) mod file_storage;
pub(crate) mod file_upload_policies;
pub(crate) mod global_settings;
pub(crate) mod media;
pub(crate) mod member;
pub(crate) mod notification;
pub(crate) mod notification_partition_manager;
pub(crate) mod oauth2;
pub(crate) mod optimistic_retry;
pub(crate) mod partitioning;
pub(crate) mod passkey;
pub(crate) mod permission;
pub(crate) mod playback;
pub(crate) mod playback_auto_advance;
pub(crate) mod playback_duration_probe;
pub(crate) mod playback_provider;
pub(crate) mod playlist;
pub(crate) mod presence;
mod provider_binding;
pub(crate) mod providers_manager;
pub(crate) mod publish_key;
pub(crate) mod rate_limit;
pub(crate) mod realtime_outbox;
pub(crate) mod remote_provider_manager;
pub(crate) mod review;
pub(crate) mod room;
pub(crate) mod room_settings;
pub(crate) mod server_state;
pub(crate) mod service_state;
mod session_store;
pub(crate) mod settings;
pub(crate) mod settings_vars;
pub(crate) mod slice_cache_management;
mod source_config;
pub(crate) mod stun_server;
pub(crate) mod system_stats;
pub(crate) mod time_partition_manager;
pub(crate) mod user;
pub(crate) mod user_notification;
pub(crate) mod ws_ticket;

pub use crate::repository::{
    realtime_outbox::NewRealtimeOutboxEvent, MediaListItem, PlaylistListItem,
    RoomResourceEventPayload, RoomResourceKind, WebAuthnCredential,
};
pub use audit::{
    AuditEventParams, AuditFlushHandle, AuditLog, AuditService, StreamKickAuditRequest,
};
pub use audit_partition_manager::{
    ensure_audit_partitions_on_startup, AuditPartitionManager, PartitionHealth, PartitionStats,
};
pub use auth::{
    AttemptTracker, AuthErrorCategory, AuthenticatedToken, BruteForceConfig, BruteForceProtection,
    BruteForceProtectionService, Claims, GuestClaims, GuestTokenValidator, InMemoryAttemptTracker,
    InMemoryTokenBlacklistStore, JwtService, JwtServiceOptions, JwtValidator,
    OpaquePasswordService, PgTokenBlacklistStore, RedisAttemptTracker, SecurityPipeline,
    SecurityPipelineRuntime, TieredTokenBlacklistStore, TokenAuthContext, TokenBlacklistStore,
    TokenCredentialBinding, TokenType,
};
pub use ban_record::{
    BanRecordListQuery, BanRecordPage, BanRecordRow, BanRecordService, BanRecordTargetType,
};
pub use chat::{ChatDependencies, ChatRuntime, ChatService};
pub use cleanup::{CleanupConfig, CleanupResult, CleanupService, CleanupServiceOptions};
pub use content_filter::{ContentFilter, ContentFilterError};
pub use content_report::{
    ContentReportListQuery, ContentReportListScope, ContentReportPage, ContentReportService,
};
pub use db_maintenance::{DatabaseMaintenanceOptions, DatabaseMaintenanceService};
pub use distributed_lock::{with_coordination_lock, CoordinationLock, DistributedLock, LockGuard};
pub use email::{
    mask_email, EmailConfig, EmailConfigProvider, EmailService, SmtpCredentials, SmtpProxyConfig,
};
pub use email_outbox::{EmailOutboxPayload, EmailOutboxService};
pub use email_token::{EmailTokenRateLimitConfig, EmailTokenService};
pub use file_storage::{
    submitted_file_reference_from_reuse_token, submitted_file_reference_from_session_file,
    upload_token_from_session_file, DatabaseFileStorageCompressionConfig,
    DatabaseFileStorageService, DisabledFileStorageService, FileStorageBackendRegistry,
    FileStorageCleanupOrigin, FileStorageContext, FileStorageService, RoutedFileStorageService,
    S3CompatibleFileStorageService, S3FileStorageConfig, FILE_UPLOAD_TOKEN_HEADER,
};
pub use file_upload_policies::{
    chat_attachment_upload_policy, media_cover_upload_policy, media_thumbnail_upload_policy,
    playlist_cover_upload_policy, room_cover_upload_policy, user_avatar_upload_policy,
    MAX_CHAT_ATTACHMENT_SIZE_BYTES, MAX_MEDIA_COVER_SIZE_BYTES, MAX_MEDIA_THUMBNAIL_SIZE_BYTES,
    MAX_PLAYLIST_COVER_SIZE_BYTES, MAX_ROOM_COVER_SIZE_BYTES, MAX_USER_AVATAR_SIZE_BYTES,
};
pub use global_settings::{
    AdminDefaultPermissionsSetting, ChatRuntimeSettings, ConfiguredIceServer, CorsAllowedOrigins,
    CorsAllowedOriginsSetting, CorsRuntimeSettings, DefaultMaxChatMessagesSetting,
    DefaultMaxMembersSetting, EmailEnabledSetting, EmailFromEmailSetting, EmailFromNameSetting,
    EmailRuntimeSettings, EmailSmtpCredentialsSetting, EmailSmtpHostSetting, EmailSmtpPortSetting,
    EmailSmtpProxySetting, EmailUseTlsSetting, EmailWhitelistEnabledSetting, EmailWhitelistSetting,
    EnableEmailSignupSetting, EnableGuestSetting, EnablePasswordSignupSetting,
    EnableWebauthnSignupSetting, ExternalIceServersSetting, GuestDefaultPermissionsSetting,
    IceServerList, LiveProxySetting, MaxMessagesPerRoomSetting, MaxPinnedMessagesPerRoomSetting,
    MaxRoomsPerUserSetting, MaxVoiceParticipantsPerRoomSetting, MemberDefaultPermissionsSetting,
    MessageRetentionDaysSetting, MovieProxySetting, OAuth2GithubProviderConfig,
    OAuth2GoogleProviderConfig, OAuth2LogtoProviderConfig, OAuth2OidcProviderConfig,
    OAuth2ProviderConfig, OAuth2ProviderConfigs, OAuth2ProviderPrivateConfig,
    OAuth2ProvidersSetting, OAuth2RuntimeSettings, OAuth2SignupPolicy, PermissionRuntimeSettings,
    PermissionSet, PlaybackHistoryRuntimeSettings, ProxyRuntimeSettings, PublicSettings,
    RoomCreationApprovalRequiredSetting, RoomCreationEnabledSetting,
    RoomCreationPasswordPolicySetting, RoomCreationRuntimeSettings, RoomDefaultsRuntimeSettings,
    RoomPasswordPolicy, RtmpRuntimeSettings, RuntimeEmailConfigProvider, RuntimeSettings,
    RuntimeSettingsStore, RuntimeSettingsUpdateMask, ServerIdentityIdSetting, ServerNameSetting,
    ServerRuntimeSettings, TsDisguisedAsPngSetting, UserRuntimeSettings, WebRtcRuntimeSettings,
    DEFAULT_MAX_VOICE_PARTICIPANTS_PER_ROOM,
};
pub use media::{
    AddMediaRequest, BackendPlaybackRequest, CreateMediaCoverUploadSession,
    CreateMediaThumbnailUploadSession, DynamicPlaylistPreviewRequest, EditMediaRequest,
    MediaService, MediaServiceRuntime, MoveMediaRequest, RealtimeOutboxMediaBatchEventFactory,
    RealtimeOutboxMediaEventFactory,
};
pub use member::{AdminMemberUpdate, MemberService};
pub use notification::{GuestKickReason, NotificationService, RoomEvent};
pub use notification_partition_manager::{
    ensure_notification_partitions_on_startup, NotificationPartitionHealth,
    NotificationPartitionManager,
};
pub use oauth2::{
    local_oauth_state_store, state_store_from_shared_state_profile, OAuth2LinkResult,
    OAuth2Operation, OAuth2PendingRegistration, OAuth2Service, OAuth2ServiceRuntime, OAuth2State,
    OAuth2UserInfo, OAuthStateStore, RedisOAuthStateStore,
};
pub use optimistic_retry::retry_with_optimistic_lock;
pub use partitioning::acquire_unbounded_ddl_connection;
pub use passkey::{
    local_passkey_session_store, passkey_session_store_from_shared_state_profile, PasskeyService,
    PasskeyServiceOptions, PasskeySessionStore,
};
pub(crate) use permission::PermissionWriteFence;
pub use permission::{
    EffectivePermissionCalculator, PermissionService, PermissionServiceRuntime,
    RuntimePermissionDefaults,
};
pub use playback::{
    PlaybackService, PlaybackServiceRuntime, PlaybackSourceExpectation, PlaybackStatePatch,
    PlaybackStateUpdateRequest, RealtimeOutboxPlaybackStateEventFactory, SeekResponse,
    SwitchPlaybackTarget,
};
pub use playback_auto_advance::{ActivePlaybackRoomSource, PlaybackAutoAdvanceService};
pub use playback_duration_probe::PlaybackDurationProbeService;
pub use playback_provider::{
    AcFunPlaybackProviderService, AlistPlaybackProviderService, BilibiliLiveDanmakuRequest,
    BilibiliPlaybackProviderService, CctvPlaybackProviderService, DirectUrlPlaybackProviderService,
    DouyinPlaybackProviderService, DouyuPlaybackProviderService, EmbyPlaybackProviderService,
    FnosPlaybackProviderService, HuyaPlaybackProviderService, LiveProxyPlaybackProviderService,
    NextcloudPlaybackProviderService, PlaybackProviderServiceDeps, QnapPlaybackProviderService,
    RtmpPlaybackProviderService, SeafilePlaybackProviderService, SynologyPlaybackProviderService,
    TikTokPlaybackProviderService, TrueNasPlaybackProviderService, TwitchPlaybackProviderService,
    YoutubePlaybackProviderService,
};
pub use playlist::{
    CreatePlaylistCoverUploadSession, CreatePlaylistRequest, MovePlaylistRequest, PlaylistService,
    RealtimeOutboxPlaylistEventFactory, SetPlaylistRequest,
};
pub use presence::{
    OnlineNodeStats, OnlinePresenceService, OnlineRoomStats, OnlineUserRoomStats, OnlineUserStats,
    PresenceConnection, PresenceEvent, PresenceOverview,
};
pub use providers_manager::{LocalProviderHttpOptions, MediaProvidersOptions, ProvidersManager};
pub use publish_key::{
    InMemoryJtiStore, JtiStore, PublishClaims, PublishKey, PublishKeyService, RedisJtiStore,
    StreamingPublishKeyService,
};
pub use rate_limit::{RateLimitConfig, RateLimitError, RateLimiter, RequestRateLimiterService};
pub use realtime_outbox::RealtimeOutboxService;
pub use remote_provider_manager::{
    empty_provider_instance_manager, ProviderInstanceStore, RemoteProviderManager,
    RemoteProviderManagerOptions,
};
pub use review::{
    ReviewPage, ReviewService, RoomCreationReviewListQuery, RoomCreationReviewRecord,
    RoomJoinReviewListQuery, RoomJoinReviewRecord, UserRegistrationReviewListQuery,
    UserRegistrationReviewRecord,
};
pub(crate) use room::soft_delete_room_and_cleanup_in_tx;
pub use room::{
    local_room_opaque_password_login_session_store,
    local_room_opaque_password_registration_session_store,
};
pub use room::{
    room_opaque_password_login_session_store_from_shared_state_profile,
    room_opaque_password_registration_session_store_from_shared_state_profile,
};
pub use room::{
    AddMemberWithOutboxRequest, AdminAddMemberWithOutboxRequest, AdminRejectJoinRequestWithOutbox,
    AuthorizedAdminActor, ClientResourceAvailability, CreateRoomCoverUploadSession,
    CreateRoomWithTaxonomyRequest, DeleteEntriesPlan, DeleteEntriesRequest,
    KickMemberOutboxOptions, MemberPermissionPatch, MemberResourceCleanupResult,
    PermissionChangedOutboxSnapshot, RealtimeMembershipAccess,
    RealtimeOutboxDeleteEntriesEventFactory, RealtimeOutboxMemberResourceCleanupEventFactory,
    RealtimeOutboxPermissionChangedEventFactory, RealtimeOutboxRoomEventFactory,
    RealtimeOutboxSettingsEventFactory, RealtimeOutboxUserLeftEventFactory, RoomCategoryUpdate,
    RoomOpaqueLoginStartChallenge, RoomOpaquePasswordLoginSession,
    RoomOpaquePasswordLoginSessionStore, RoomOpaquePasswordRegistrationSession,
    RoomOpaquePasswordRegistrationSessionStore, RoomOpaqueRegistrationStartChallenge, RoomService,
    RoomServiceOptions, UpdateMemberDisplayTagWithOutboxRequest,
    UpdateMemberRemarkNameWithOutboxRequest, UpdateMemberWithOutboxRequest, UserLeftOutboxSnapshot,
    ROOM_OPAQUE_LOGIN_SESSION_TTL_SECS, ROOM_OPAQUE_REGISTRATION_SESSION_TTL_SECS,
};
pub use room_settings::{CacheStats, RoomSettingsRuntime, RoomSettingsService};
pub use server_state::{
    check_memory_health, email_health, livestream_snapshot_from_publishers,
    response_for_server_state_nodes, summarize_server_state, validate_server_state_selection,
    ws_ticket_backend_is_safe_for_mode, ws_ticket_health, ServerStateCluster,
    ServerStateClusterNode, ServerStateClusterOptions, ServerStateClusterRuntime,
    ServerStateClusterStatus, ServerStateClusterTarget, ServerStateCpu, ServerStateCpuStatus,
    ServerStateDatabase, ServerStateDatabaseOptions, ServerStateDatabasePool,
    ServerStateDatabaseStatus, ServerStateEmail, ServerStateEmailStatus, ServerStateError,
    ServerStateFailure, ServerStateHlsStorageBackend, ServerStateHlsStorageOptions,
    ServerStateLivestream, ServerStateLivestreamOptions, ServerStateLivestreamRuntime,
    ServerStateLivestreamSnapshot, ServerStateLivestreamStatus, ServerStateMemory,
    ServerStateMemoryHealth, ServerStateMemoryStatus, ServerStateNode, ServerStateNodeStatus,
    ServerStateRealtime, ServerStateRealtimeMetrics, ServerStateRealtimeRuntime, ServerStateRedis,
    ServerStateRedisOptions, ServerStateRedisStatus, ServerStateRemoteClient, ServerStateResponse,
    ServerStateResult, ServerStateRuntimeParams, ServerStateScope, ServerStateSelection,
    ServerStateService, ServerStateServiceDependencies, ServerStateSliceCache,
    ServerStateSliceCacheRuntime, ServerStateSliceCacheStatus, ServerStateSummary,
    ServerStateWebRtc, ServerStateWebRtcStatus, ServerStateWsTicket, ServerStateWsTicketStatus,
};
pub use service_state::{
    ServiceAdditionalState, ServiceState, ServiceStateService, ServiceStateServiceDependencies,
};
pub use settings::{SettingsService, SettingsServiceRuntime};
pub use settings_vars::{Setting, SettingsStorage};
pub use slice_cache_management::{
    evict_expired_response_from_nodes, purge_response_from_nodes, validate_slice_cache_selection,
    SliceCacheConfigInfo, SliceCacheEvictExpiredNodeResult, SliceCacheEvictExpiredResponse,
    SliceCacheManagementClusterRuntime, SliceCacheManagementError,
    SliceCacheManagementLocalRuntime, SliceCacheManagementRemoteClient, SliceCacheManagementResult,
    SliceCacheManagementService, SliceCacheManagementServiceDependencies, SliceCacheNodeFailure,
    SliceCachePurgeNodeResult, SliceCachePurgeResponse, SliceCachePurgeResult, SliceCacheSelection,
    SliceCacheStats, SliceCacheStatsNode, SliceCacheStatsResponse,
};
pub use stun_server::{
    validate_external_addr, BuiltinStunRuntimeReason, BuiltinStunRuntimeState, StunServer,
    StunServerConfig, WebRtcRuntimeMode, WebRtcRuntimeStatus,
};
pub use system_stats::SystemStatsService;
pub use time_partition_manager::{
    ensure_time_partitions_on_startup, TimePartitionHealth, TimePartitionManager,
};
pub use user::{
    local_login_session_store, local_mfa_session_store, local_opaque_registration_session_store,
    local_sensitive_verification_session_store,
};
pub use user::{
    login_session_store_from_shared_state_profile, mfa_session_store_from_shared_state_profile,
    opaque_registration_session_store_from_shared_state_profile,
    sensitive_verification_session_store_from_shared_state_profile,
};
pub use user::{
    AccountRegistrationOutcome, AuthFactorMethod, AuthenticatedLogin,
    CreateUserAvatarUploadSession, LoginSession, LoginSessionState, LoginSessionStore,
    LoginStartChallenge, MfaChallenge, MfaSession, MfaSessionStore, OpaqueLoginStartChallenge,
    OpaquePasswordUpdateVerification, OpaqueRegistrationPurpose, OpaqueRegistrationSession,
    OpaqueRegistrationSessionStore, OpaqueRegistrationStartChallenge, PendingAccountRegistration,
    RefreshRateLimitConfig, RegistrationMode, RegistrationPolicy, SensitiveVerificationChallenge,
    SensitiveVerificationOutcome, SensitiveVerificationSession, SensitiveVerificationSessionStore,
    TotpRecoveryCodes, TotpSetup, UserDeletedRoomImpact, UserDeletionSummary, UserService,
    UserServiceDependencies, UserServiceRuntimeOptions,
};
pub use user::{
    InMemoryLoginSessionStore, InMemoryMfaSessionStore, InMemoryOpaqueRegistrationSessionStore,
    InMemorySensitiveVerificationSessionStore,
};
pub use user_notification::{NotificationCreatedEvent, UserNotificationService};
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
