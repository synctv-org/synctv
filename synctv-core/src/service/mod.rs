pub mod auth;
pub mod chat;
pub mod email_token;
pub mod oauth2;
pub mod playlist;
pub mod room_settings;
pub mod settings;
pub mod settings_vars;
pub mod global_settings;
pub mod user;
pub mod room;
pub mod rate_limit;
pub mod content_filter;
pub mod remote_provider_manager;
pub mod providers_manager;
pub mod permission;
pub mod member;
pub mod media;
pub mod playback;
pub mod publish_key;
pub mod notification;
pub mod user_notification;
pub mod audit;
pub mod audit_partition_manager;
pub mod chat_partition_manager;
pub mod cleanup;
pub mod credential_encryption;
pub mod db_maintenance;
pub mod distributed_lock;
pub mod email;
pub mod email_templates;
pub mod optimistic_retry;
pub mod turn_server;
pub mod ws_ticket;

pub use auth::{hash_password, verify_password, JwtService, TokenType, Claims, BruteForceProtection, SecurityPipeline, AuthenticatedToken};
pub use chat::ChatService;
pub use email_token::{EmailTokenService, EmailTokenType};
pub use oauth2::{OAuth2Service, OAuth2State, OAuth2UserInfo, OAuthStateStore, RedisOAuthStateStore};
pub use playlist::{PlaylistService, PlaylistBroadcaster};
pub use room_settings::{RoomSettingsService, CacheStats};
pub use settings::{SettingsService, SettingsChangeListener};
pub use settings_vars::{Setting, SettingsStorage};
pub use global_settings::{SettingsRegistry, PublicSettings, TurnServer, TurnServerList, StunServerList};
pub use user::UserService;
pub use room::RoomService;
pub use rate_limit::{RateLimiter, RateLimitConfig, RateLimitError};
pub use content_filter::{ContentFilter, ContentFilterError};
pub use remote_provider_manager::RemoteProviderManager;
pub use providers_manager::ProvidersManager;
pub use permission::PermissionService;
pub use member::{MemberService, AddMemberOptions, MemberEventBroadcaster};
pub use media::MediaService;
pub use playback::{PlaybackService, PlaybackBroadcaster};
pub use publish_key::{PublishKeyService, PublishKey};
pub use notification::{NotificationService, RoomEvent};
pub use user_notification::UserNotificationService;
pub use audit::{AuditService, AuditAction, AuditTargetType, AuditLog, AuditFlushHandle};
pub use audit_partition_manager::{
    AuditPartitionManager, PartitionHealth, PartitionStats,
    ensure_audit_partitions_on_startup
};
pub use chat_partition_manager::{
    ChatPartitionManager, ChatPartitionHealth,
    ensure_chat_partitions_on_startup
};
pub use cleanup::{CleanupService, CleanupConfig, CleanupResult};
pub use credential_encryption::CredentialEncryption;
pub use db_maintenance::DatabaseMaintenanceService;
pub use distributed_lock::{DistributedLock, LockGuard};
pub use email::{EmailService, EmailConfig};
pub use email_templates::{EmailTemplateManager, EmailTemplateType};
pub use optimistic_retry::retry_with_optimistic_lock;
pub use turn_server::{StunServer, StunServerConfig, resolve_external_ip, validate_external_addr};
pub use ws_ticket::{WsTicketService, WsTicketData};

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
