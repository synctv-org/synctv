pub mod audit;
pub mod audit_partition_manager;
pub mod auth;
pub mod chat;
pub mod chat_partition_manager;
pub mod cleanup;
pub mod content_filter;
pub mod credential_encryption;
pub mod db_maintenance;
pub mod distributed_lock;
pub mod email;
pub mod email_templates;
pub mod email_token;
pub mod global_settings;
pub mod media;
pub mod member;
pub mod notification;
pub mod notification_partition_manager;
pub mod oauth2;
pub mod optimistic_retry;
pub mod permission;
pub mod playback;
pub mod playlist;
pub mod providers_manager;
pub mod proxy_signature;
pub mod publish_key;
pub mod rate_limit;
pub mod remote_provider_manager;
pub mod room;
pub mod room_settings;
pub mod settings;
pub mod settings_vars;
pub mod turn_health;
pub mod turn_server;
pub mod user;
pub mod user_notification;
pub mod ws_ticket;

pub use audit::{AuditAction, AuditFlushHandle, AuditLog, AuditService, AuditTargetType};
pub use audit_partition_manager::{
    ensure_audit_partitions_on_startup, AuditPartitionManager, PartitionHealth, PartitionStats,
};
pub use auth::brute_force::{
    AttemptTracker, BruteForceConfig, InMemoryAttemptTracker, RedisAttemptTracker,
};
pub use auth::guest_validator::GuestTokenValidator;
pub use auth::security_pipeline::{
    BlacklistEnforcement, SecurityPipelineBuildError, SecurityPipelineBuilder,
};
pub use auth::token_blacklist::{
    FallbackTokenBlacklistStore, InMemoryTokenBlacklistStore, PgTokenBlacklistStore,
    RedisSyncableTokenBlacklistStore, SyncStats, TieredTokenBlacklistStore,
};
pub use auth::{
    hash_password, verify_password, AuthenticatedToken, BruteForceProtection, Claims, GuestClaims,
    JwtService, SecurityPipeline, TokenBlacklistStore, TokenType,
};
pub use chat::ChatService;
pub use chat_partition_manager::{
    ensure_chat_partitions_on_startup, ChatPartitionHealth, ChatPartitionManager,
};
pub use cleanup::{CleanupConfig, CleanupResult, CleanupService};
pub use content_filter::{ContentFilter, ContentFilterError};
pub use credential_encryption::CredentialEncryption;
pub use db_maintenance::DatabaseMaintenanceService;
pub use distributed_lock::{DistributedLock, LockGuard, MigrationLock, PgAdvisoryMigrationLock};
pub use email::{
    mask_email, EmailConfig, EmailService, InMemoryVerificationCodeStore,
    RedisVerificationCodeStore, VerificationCodeStore,
};
pub use email_templates::{EmailTemplateManager, EmailTemplateType};
pub use email_token::{EmailTokenService, EmailTokenType};
pub use global_settings::{
    PublicSettings, SettingsRegistry, StunServerList, TurnServer, TurnServerList,
};
pub use media::MediaService;
pub use member::{AddMemberOptions, MemberEventBroadcaster, MemberService};
pub use notification::{NotificationService, RoomEvent};
pub use notification_partition_manager::{
    ensure_notification_partitions_on_startup, NotificationPartitionHealth,
    NotificationPartitionManager,
};
pub use oauth2::{
    InMemoryOAuthStateStore, OAuth2Service, OAuth2State, OAuth2UserInfo, OAuthStateStore,
    RedisOAuthStateStore,
};
pub use optimistic_retry::retry_with_optimistic_lock;
pub use permission::PermissionService;
pub use playback::{BroadcastResult, PlaybackBroadcaster, PlaybackService, SeekResponse};
pub use playlist::{PlaylistBroadcaster, PlaylistService};
pub use providers_manager::ProvidersManager;
pub use proxy_signature::{
    build_signed_proxy_url, ProxySignatureError, ProxySigningKey, ProxyUrlClaims,
};
pub use publish_key::{InMemoryJtiStore, JtiStore, PublishKey, PublishKeyService, RedisJtiStore};
pub use rate_limit::{
    InMemoryRateLimitBackend, RateLimitBackend, RateLimitConfig, RateLimitError, RateLimiter,
    RedisRateLimitBackend,
};
pub use remote_provider_manager::RemoteProviderManager;
pub use room::RoomService;
pub use room_settings::{CacheStats, RoomSettingsService};
pub use settings::{SettingsChangeListener, SettingsService};
pub use settings_vars::{Setting, SettingsStorage};
pub use turn_health::{
    HealthCheckResult, TurnHealthCheckConfig, TurnHealthChecker, TurnServerHealth,
};
pub use turn_server::{resolve_external_ip, validate_external_addr, StunServer, StunServerConfig};
pub use user::UserService;
pub use user_notification::UserNotificationService;
pub use ws_ticket::{
    InMemoryTicketStore, PendingValidatedTicket, RedisTicketStore, TicketStore,
    UserValidationResult, UserValidator, WsTicketData, WsTicketService,
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
