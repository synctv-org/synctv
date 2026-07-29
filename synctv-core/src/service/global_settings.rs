//! Global setting variables
//!
//! This module defines all setting variables used throughout the application.
//! Each variable is type-safe, thread-safe, and automatically persists to the database.
//!
//! # Usage
//!
//! ```text
//! use synctv_core::service::*;
//!
//! // Initialize during app startup
//! let registry = RuntimeSettingsStore::new(settings_service);
//! let cancel = tokio_util::sync::CancellationToken::new();
//! registry.init(cancel)?;
//!
//! // Read - type-safe, returns cached value
//! if registry.user.enable_password_signup.get()? {
//!     // Password signup is enabled
//! }
//!
//! ```

use crate::models::{room_settings::MaxMembers, SettingsValidationContext};
use crate::service::email::{EmailConfig, EmailConfigProvider, SmtpCredentials, SmtpProxyConfig};
use crate::service::{
    settings_vars::{Setting, SettingChangeReceiver, SettingsStorage},
    SettingsService,
};
use crate::setting;
use std::fmt;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::warn;

mod types;
pub use types::{
    ChatRuntimeSettings, ConfiguredIceServer, CorsAllowedOrigins, CorsRuntimeSettings,
    EmailRuntimeSettings, IceServerList, OAuth2AppleProviderConfig, OAuth2CasdoorProviderConfig,
    OAuth2GithubProviderConfig, OAuth2GoogleProviderConfig, OAuth2LogtoProviderConfig,
    OAuth2OidcProviderConfig, OAuth2ProviderConfig, OAuth2ProviderConfigs,
    OAuth2ProviderPrivateConfig, OAuth2RuntimeSettings, OAuth2SignupPolicy, OptionalRuntimeConfig,
    PermissionRuntimeSettings, PermissionSet, PlaybackHistoryRuntimeSettings, ProxyRuntimeSettings,
    PublicSettings, RoomCreationRuntimeSettings, RoomDefaultsRuntimeSettings, RoomPasswordPolicy,
    RtmpRuntimeSettings, RuntimeSettings, RuntimeSettingsUpdateMask, ServerRuntimeSettings,
    UserRuntimeSettings, WebRtcRuntimeSettings,
};

/// Maximum allowed value for `default_max_chat_messages` setting (0 = unlimited)
const MAX_CHAT_MESSAGES_LIMIT: u64 = 10_000;
/// Maximum allowed value for `max_pinned_messages_per_room` setting (0 = unlimited)
const MAX_PINNED_CHAT_MESSAGES_PER_ROOM_LIMIT: u64 = 1_000;
pub const DEFAULT_MAX_VOICE_PARTICIPANTS_PER_ROOM: u32 = 8;

setting!(
    ServerNameSetting,
    String,
    "server.name",
    "SyncTV".to_string(),
    |value: &String| types::validate_server_name(value)
);

setting!(
    ServerIdentityIdSetting,
    String,
    "server.identity_id",
    String::new(),
    |value: &String| -> crate::Result<()> {
        let value = value.trim();
        if value.starts_with("srv_")
            && value.len() == 36
            && value[4..].chars().all(|ch| ch.is_ascii_hexdigit())
        {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(
                "server.identity_id must be a generated srv_ prefixed UUID value".into(),
            ))
        }
    }
);

setting!(
    DefaultMaxMembersSetting,
    i64,
    "room_defaults.default_max_members",
    100,
    |value: &i64| -> crate::Result<()> {
        if *value > 0 {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(format!(
                "room_defaults.default_max_members must be between 1 and {}",
                MaxMembers::MAX
            )))
        }
    }
);

setting!(
    DefaultMaxChatMessagesSetting,
    u64,
    "room_defaults.default_max_chat_messages",
    500,
    |value: &u64| -> crate::Result<()> {
        if *value <= MAX_CHAT_MESSAGES_LIMIT {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(format!(
                "room_defaults.default_max_chat_messages must be at most {MAX_CHAT_MESSAGES_LIMIT} (0 = unlimited)"
            )))
        }
    }
);

setting!(
    AdminDefaultPermissionsSetting,
    PermissionSet,
    "permissions.admin_default",
    PermissionSet::admin_default()
);
setting!(
    MemberDefaultPermissionsSetting,
    PermissionSet,
    "permissions.member_default",
    PermissionSet::member_default()
);
setting!(
    GuestDefaultPermissionsSetting,
    PermissionSet,
    "permissions.guest_default",
    PermissionSet::guest_default(),
    |permissions: &PermissionSet| permissions.validate_guest_default()
);

setting!(
    RoomCreationEnabledSetting,
    bool,
    "room_creation.enabled",
    true
);
setting!(
    RoomCreationApprovalRequiredSetting,
    bool,
    "room_creation.approval_required",
    false
);
setting!(
    RoomCreationPasswordPolicySetting,
    RoomPasswordPolicy,
    "room_creation.password_policy",
    RoomPasswordPolicy::Optional
);
setting!(
    MaxRoomsPerUserSetting,
    i64,
    "room_creation.max_rooms_per_user",
    10,
    |value: &i64| -> crate::Result<()> {
        if *value > 0 && *value <= 1000 {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(
                "room_creation.max_rooms_per_user must be between 1 and 1000".into(),
            ))
        }
    }
);

setting!(
    EnablePasswordSignupSetting,
    bool,
    "user.enable_password_signup",
    false
);
setting!(
    PasswordSignupNeedReviewSetting,
    bool,
    "user.password_signup_need_review",
    false
);
setting!(
    EnableEmailSignupSetting,
    bool,
    "user.enable_email_signup",
    false
);
setting!(
    EmailSignupNeedReviewSetting,
    bool,
    "user.email_signup_need_review",
    false
);
setting!(
    EnableWebauthnSignupSetting,
    bool,
    "user.enable_webauthn_signup",
    false
);
setting!(
    WebauthnSignupNeedReviewSetting,
    bool,
    "user.webauthn_signup_need_review",
    false
);
setting!(EnableGuestSetting, bool, "user.enable_guest", true);

setting!(MovieProxySetting, bool, "proxy.movie_proxy", true);
setting!(LiveProxySetting, bool, "proxy.live_proxy", true);
setting!(
    CustomPublishHostSetting,
    OptionalRuntimeConfig<String>,
    "rtmp.custom_publish_host",
    OptionalRuntimeConfig::default(),
    |value: &OptionalRuntimeConfig<String>| -> crate::Result<()> {
        if value.0.as_ref().is_some_and(|host| host.trim().is_empty()) {
            return Err(crate::Error::InvalidInput(
                "rtmp.custom_publish_host must be non-empty when configured".to_string(),
            ));
        }
        Ok(())
    }
);
setting!(
    TsDisguisedAsPngSetting,
    bool,
    "rtmp.ts_disguised_as_png",
    false
);

setting!(EmailEnabledSetting, bool, "email.enabled", false);
setting!(
    EmailSmtpHostSetting,
    OptionalRuntimeConfig<String>,
    "email.smtp_host",
    OptionalRuntimeConfig::default(),
    |value: &OptionalRuntimeConfig<String>| -> crate::Result<()> {
        if value.0.as_ref().is_some_and(|host| host.trim().is_empty()) {
            return Err(crate::Error::InvalidInput(
                "email.smtp_host must be non-empty when configured".to_string(),
            ));
        }
        Ok(())
    }
);
setting!(
    EmailSmtpPortSetting,
    u16,
    "email.smtp_port",
    587,
    |port: &u16| -> crate::Result<()> {
        if *port > 0 {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(
                "email.smtp_port must be between 1 and 65535".into(),
            ))
        }
    }
);
setting!(
    EmailSmtpCredentialsSetting,
    OptionalRuntimeConfig<SmtpCredentials>,
    "email.smtp_credentials",
    OptionalRuntimeConfig::default(),
    |value: &OptionalRuntimeConfig<SmtpCredentials>| -> crate::Result<()> {
        match &value.0 {
            Some(credentials) => credentials.validate(),
            None => Ok(()),
        }
    }
);
setting!(
    EmailSmtpProxySetting,
    OptionalRuntimeConfig<SmtpProxyConfig>,
    "email.smtp_proxy",
    OptionalRuntimeConfig::default(),
    |value: &OptionalRuntimeConfig<SmtpProxyConfig>| -> crate::Result<()> {
        match &value.0 {
            Some(proxy) => proxy.validate(),
            None => Ok(()),
        }
    }
);
setting!(EmailUseTlsSetting, bool, "email.use_tls", true);
setting!(
    EmailFromEmailSetting,
    OptionalRuntimeConfig<String>,
    "email.from_email",
    OptionalRuntimeConfig::default(),
    |value: &OptionalRuntimeConfig<String>| -> crate::Result<()> {
        if let Some(email) = &value.0 {
            crate::service::EmailService::validate_email(email)?;
        }
        Ok(())
    }
);
setting!(
    EmailFromNameSetting,
    String,
    "email.from_name",
    "SyncTV".to_string()
);
setting!(
    EmailWhitelistEnabledSetting,
    bool,
    "email.whitelist_enabled",
    false
);
setting!(
    EmailWhitelistSetting,
    String,
    "email.whitelist",
    String::new(),
    |value: &String| validate_email_whitelist_domains(value)
);

setting!(
    ExternalIceServersSetting,
    IceServerList,
    "webrtc.external_ice_servers",
    IceServerList::new()
);
setting!(
    MaxVoiceParticipantsPerRoomSetting,
    u32,
    "webrtc.max_voice_participants_per_room",
    DEFAULT_MAX_VOICE_PARTICIPANTS_PER_ROOM,
    |value: &u32| -> crate::Result<()> {
        if (2..=32).contains(value) {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(
                "webrtc.max_voice_participants_per_room must be between 2 and 32".into(),
            ))
        }
    }
);
setting!(
    MaxMessagesPerRoomSetting,
    u64,
    "chat.max_messages_per_room",
    500,
    |value: &u64| -> crate::Result<()> {
        if *value <= 100_000 {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(
                "chat.max_messages_per_room must be <= 100000 (0 = unlimited)".into(),
            ))
        }
    }
);
setting!(
    MaxPinnedMessagesPerRoomSetting,
    u64,
    "chat.max_pinned_messages_per_room",
    20,
    |value: &u64| -> crate::Result<()> {
        if *value <= MAX_PINNED_CHAT_MESSAGES_PER_ROOM_LIMIT {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(format!(
                "chat.max_pinned_messages_per_room must be <= {MAX_PINNED_CHAT_MESSAGES_PER_ROOM_LIMIT} (0 = unlimited)"
            )))
        }
    }
);
setting!(
    MessageRetentionDaysSetting,
    i64,
    "chat.message_retention_days",
    90,
    |value: &i64| -> crate::Result<()> {
        if *value >= 1 && *value <= 3650 {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(
                "chat.message_retention_days must be between 1 and 3650".into(),
            ))
        }
    }
);
setting!(
    PlaybackHistoryRetentionDaysSetting,
    u32,
    "playback_history.retention_days",
    90,
    |value: &u32| -> crate::Result<()> {
        if *value <= 3_650 {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(
                "playback_history.retention_days must be between 0 and 3650".into(),
            ))
        }
    }
);
setting!(
    PlaybackHistoryMaxEntriesPerRoomSetting,
    i64,
    "playback_history.max_entries_per_room",
    1_000,
    |value: &i64| -> crate::Result<()> {
        if (0..=100_000).contains(value) {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(
                "playback_history.max_entries_per_room must be between 0 and 100000".into(),
            ))
        }
    }
);
setting!(
    CorsAllowedOriginsSetting,
    CorsAllowedOrigins,
    "cors.allowed_origins",
    CorsAllowedOrigins::new()
);

#[derive(Clone)]
pub struct OAuth2ProvidersSetting(Setting<OAuth2ProviderConfigs>);

impl OAuth2ProvidersSetting {
    pub const KEY: &'static str = "oauth2.providers";

    #[must_use]
    pub fn new(storage: Arc<SettingsStorage>, ssrf_guard: synctv_common::ssrf::SsrfGuard) -> Self {
        Self(
            Setting::new(Self::KEY, storage, OAuth2ProviderConfigs::default()).with_validator(
                move |configs: &OAuth2ProviderConfigs| {
                    configs.validate(&SettingsValidationContext::new(&ssrf_guard))
                },
            ),
        )
    }
}

impl std::ops::Deref for OAuth2ProvidersSetting {
    type Target = Setting<OAuth2ProviderConfigs>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn validate_email_whitelist_domains(raw: &str) -> crate::Result<()> {
    for entry in raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let domain = entry.trim_start_matches('@');
        if domain.is_empty()
            || domain.contains('@')
            || domain.len() > 253
            || domain.starts_with('.')
            || domain.ends_with('.')
            || !domain.contains('.')
            || domain.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
            })
        {
            return Err(crate::Error::InvalidInput(
                "email.whitelist must contain comma-separated email domains only".to_string(),
            ));
        }
    }

    Ok(())
}

async fn recv_email_setting_change<T>(
    receiver: &mut crate::service::settings_vars::SettingChangeReceiver<T>,
) -> Result<(), crate::service::settings_vars::SettingChangeError>
where
    T: Clone + fmt::Display + std::str::FromStr + Send + Sync + 'static,
    <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
{
    match receiver.recv().await {
        Ok(_) => Ok(()),
        Err(crate::service::settings_vars::SettingChangeError::Lagged(count)) => {
            warn!(
                count,
                "Email setting change receiver lagged; treating settings as changed"
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Runtime settings store for runtime initialization
///
/// Use this to initialize and manage all settings during app startup
#[derive(Clone)]
pub struct RuntimeSettingsStore {
    /// Storage for managing all settings
    pub storage: Arc<SettingsStorage>,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
    pub server: ServerSettingsStore,
    pub room_defaults: RoomDefaultsSettingsStore,
    pub permissions: PermissionSettingsStore,
    pub room_creation: RoomCreationSettingsStore,
    pub user: UserSettingsStore,
    pub oauth2: OAuth2SettingsStore,
    pub proxy: ProxySettingsStore,
    pub rtmp: RtmpSettingsStore,
    pub email: EmailSettingsStore,
    pub webrtc: WebRtcSettingsStore,
    pub chat: ChatSettingsStore,
    pub playback_history: PlaybackHistorySettingsStore,
    pub cors: CorsSettingsStore,
}

#[derive(Clone)]
pub struct ServerSettingsStore {
    /// Stable logical server identity, automatically initialized by the runtime.
    pub identity_id: ServerIdentityIdSetting,
    pub name: ServerNameSetting,
}

impl std::fmt::Debug for RuntimeSettingsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSettingsStore").finish()
    }
}

#[derive(Clone)]
pub struct RoomDefaultsSettingsStore {
    pub default_max_members: DefaultMaxMembersSetting,
    pub default_max_chat_messages: DefaultMaxChatMessagesSetting,
}

#[derive(Clone)]
pub struct PermissionSettingsStore {
    pub admin_default_permissions: AdminDefaultPermissionsSetting,
    pub member_default_permissions: MemberDefaultPermissionsSetting,
    pub guest_default_permissions: GuestDefaultPermissionsSetting,
}

#[derive(Clone)]
pub struct RoomCreationSettingsStore {
    pub enabled: RoomCreationEnabledSetting,
    pub approval_required: RoomCreationApprovalRequiredSetting,
    pub password_policy: RoomCreationPasswordPolicySetting,
    pub max_rooms_per_user: MaxRoomsPerUserSetting,
}

#[derive(Clone)]
pub struct UserSettingsStore {
    pub enable_password_signup: EnablePasswordSignupSetting,
    pub password_signup_need_review: PasswordSignupNeedReviewSetting,
    pub enable_email_signup: EnableEmailSignupSetting,
    pub email_signup_need_review: EmailSignupNeedReviewSetting,
    pub enable_webauthn_signup: EnableWebauthnSignupSetting,
    pub webauthn_signup_need_review: WebauthnSignupNeedReviewSetting,
    pub enable_guest: EnableGuestSetting,
}

#[derive(Clone)]
pub struct OAuth2SettingsStore {
    pub providers: OAuth2ProvidersSetting,
}

#[derive(Clone)]
pub struct ProxySettingsStore {
    pub movie_proxy: MovieProxySetting,
    pub live_proxy: LiveProxySetting,
}

#[derive(Clone)]
pub struct RtmpSettingsStore {
    pub custom_publish_host: CustomPublishHostSetting,
    pub ts_disguised_as_png: TsDisguisedAsPngSetting,
}

#[derive(Clone)]
pub struct EmailSettingsStore {
    pub enabled: EmailEnabledSetting,
    pub smtp_host: EmailSmtpHostSetting,
    pub smtp_port: EmailSmtpPortSetting,
    pub smtp_credentials: EmailSmtpCredentialsSetting,
    pub smtp_proxy: EmailSmtpProxySetting,
    pub use_tls: EmailUseTlsSetting,
    pub from_email: EmailFromEmailSetting,
    pub from_name: EmailFromNameSetting,
    pub whitelist_enabled: EmailWhitelistEnabledSetting,
    pub whitelist: EmailWhitelistSetting,
}

#[derive(Clone)]
pub struct WebRtcSettingsStore {
    pub external_ice_servers: ExternalIceServersSetting,
    pub max_voice_participants_per_room: MaxVoiceParticipantsPerRoomSetting,
}

#[derive(Clone)]
pub struct ChatSettingsStore {
    pub max_messages_per_room: MaxMessagesPerRoomSetting,
    pub max_pinned_messages_per_room: MaxPinnedMessagesPerRoomSetting,
    pub message_retention_days: MessageRetentionDaysSetting,
}

#[derive(Clone)]
pub struct PlaybackHistorySettingsStore {
    pub retention_days: PlaybackHistoryRetentionDaysSetting,
    pub max_entries_per_room: PlaybackHistoryMaxEntriesPerRoomSetting,
}

#[derive(Clone)]
pub struct CorsSettingsStore {
    pub allowed_origins: CorsAllowedOriginsSetting,
}

pub struct RuntimeEmailConfigProvider {
    settings: Arc<RuntimeSettingsStore>,
    changes: broadcast::Sender<()>,
}

impl RuntimeEmailConfigProvider {
    #[must_use]
    pub fn new(settings: &Arc<RuntimeSettingsStore>) -> Self {
        let (changes, _) = broadcast::channel(64);

        let provider = Self {
            settings: Arc::clone(settings),
            changes: changes.clone(),
        };

        if let Err(error) = provider.current_config() {
            tracing::warn!(
                error = %error,
                "Failed to load initial runtime email configuration"
            );
        }

        let mut subscriptions = match subscribe_email_settings(settings) {
            Ok(subscriptions) => subscriptions,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Runtime email config changes are disabled because setting subscriptions could not be created"
                );
                return provider;
            }
        };

        crate::spawn::spawn_monitored("runtime_email_config_provider_changes", async move {
            loop {
                let event = tokio::select! {
                    event = recv_email_setting_change(&mut subscriptions.enabled) => event,
                    event = recv_email_setting_change(&mut subscriptions.smtp_host) => event,
                    event = recv_email_setting_change(&mut subscriptions.smtp_port) => event,
                    event = recv_email_setting_change(&mut subscriptions.smtp_credentials) => event,
                    event = recv_email_setting_change(&mut subscriptions.smtp_proxy) => event,
                    event = recv_email_setting_change(&mut subscriptions.use_tls) => event,
                    event = recv_email_setting_change(&mut subscriptions.from_email) => event,
                    event = recv_email_setting_change(&mut subscriptions.from_name) => event,
                };

                match event {
                    Ok(()) => match changes.send(()) {
                        Ok(subscriber_count) => tracing::debug!(
                            subscriber_count,
                            "Email runtime setting change notified subscribers"
                        ),
                        Err(error) => {
                            tracing::debug!(
                                error = %error,
                                "Email runtime setting change had no active subscribers"
                            );
                        }
                    },
                    Err(error) => {
                        warn!(
                            error = %error,
                            "Email settings watcher stopped after setting change subscription error"
                        );
                        break;
                    }
                }
            }
        });

        provider
    }
}

struct RuntimeEmailSettingSubscriptions {
    enabled: SettingChangeReceiver<bool>,
    smtp_host: SettingChangeReceiver<OptionalRuntimeConfig<String>>,
    smtp_port: SettingChangeReceiver<u16>,
    smtp_credentials: SettingChangeReceiver<OptionalRuntimeConfig<SmtpCredentials>>,
    smtp_proxy: SettingChangeReceiver<OptionalRuntimeConfig<SmtpProxyConfig>>,
    use_tls: SettingChangeReceiver<bool>,
    from_email: SettingChangeReceiver<OptionalRuntimeConfig<String>>,
    from_name: SettingChangeReceiver<String>,
}

fn subscribe_email_settings(
    settings: &RuntimeSettingsStore,
) -> crate::Result<RuntimeEmailSettingSubscriptions> {
    Ok(RuntimeEmailSettingSubscriptions {
        enabled: settings.email.enabled.subscribe_changes()?,
        smtp_host: settings.email.smtp_host.subscribe_changes()?,
        smtp_port: settings.email.smtp_port.subscribe_changes()?,
        smtp_credentials: settings.email.smtp_credentials.subscribe_changes()?,
        smtp_proxy: settings.email.smtp_proxy.subscribe_changes()?,
        use_tls: settings.email.use_tls.subscribe_changes()?,
        from_email: settings.email.from_email.subscribe_changes()?,
        from_name: settings.email.from_name.subscribe_changes()?,
    })
}

impl EmailConfigProvider for RuntimeEmailConfigProvider {
    fn current_config(&self) -> crate::Result<Option<EmailConfig>> {
        if !self.settings.email.enabled.get()? {
            return Ok(None);
        }

        Ok(Some(EmailConfig {
            smtp_host: self
                .settings
                .email
                .smtp_host
                .get()?
                .0
                .ok_or_else(|| {
                    crate::Error::InvalidInput(
                        "email.smtp_host is required when email.enabled is true".to_string(),
                    )
                })?
                .trim()
                .to_string(),
            smtp_port: self.settings.email.smtp_port.get()?,
            smtp_credentials: self.settings.email.smtp_credentials.get()?.0,
            smtp_proxy: self.settings.email.smtp_proxy.get()?.0,
            from_email: self
                .settings
                .email
                .from_email
                .get()?
                .0
                .ok_or_else(|| {
                    crate::Error::InvalidInput(
                        "email.from_email is required when email.enabled is true".to_string(),
                    )
                })?
                .trim()
                .to_string(),
            from_name: self.settings.email.from_name.get()?.trim().to_string(),
            use_tls: self.settings.email.use_tls.get()?,
        }))
    }

    fn subscribe_changes(&self) -> Option<broadcast::Receiver<()>> {
        Some(self.changes.subscribe())
    }
}

impl RuntimeSettingsStore {
    /// Create a new runtime settings store with all setting instances
    #[must_use]
    pub fn new(settings_service: Arc<SettingsService>) -> Self {
        Self::new_with_ssrf_guard(
            settings_service,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    /// Create a new runtime settings store using the runtime SSRF policy for
    /// settings that validate outbound provider URLs.
    #[must_use]
    pub fn new_with_ssrf_guard(
        settings_service: Arc<SettingsService>,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        let storage = Arc::new(SettingsStorage::new(settings_service));
        Self::from_storage(&storage, ssrf_guard)
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests() -> Self {
        Self::new_for_tests_with_ssrf_guard(&synctv_common::ssrf::SsrfGuard::strict_policy())
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests_with_ssrf_guard(
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        let storage = Arc::new(SettingsStorage::new_for_tests());
        Self::from_storage(&storage, ssrf_guard)
    }

    fn from_storage(
        storage: &Arc<SettingsStorage>,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        let server = ServerSettingsStore {
            identity_id: ServerIdentityIdSetting::new(storage.clone()),
            name: ServerNameSetting::new(storage.clone()),
        };

        let room_defaults = RoomDefaultsSettingsStore {
            default_max_members: DefaultMaxMembersSetting::new(storage.clone()),
            default_max_chat_messages: DefaultMaxChatMessagesSetting::new(storage.clone()),
        };

        let permissions = PermissionSettingsStore {
            admin_default_permissions: AdminDefaultPermissionsSetting::new(storage.clone()),
            member_default_permissions: MemberDefaultPermissionsSetting::new(storage.clone()),
            guest_default_permissions: GuestDefaultPermissionsSetting::new(storage.clone()),
        };

        let room_creation = RoomCreationSettingsStore {
            enabled: RoomCreationEnabledSetting::new(storage.clone()),
            approval_required: RoomCreationApprovalRequiredSetting::new(storage.clone()),
            password_policy: RoomCreationPasswordPolicySetting::new(storage.clone()),
            max_rooms_per_user: MaxRoomsPerUserSetting::new(storage.clone()),
        };

        let user = UserSettingsStore {
            enable_password_signup: EnablePasswordSignupSetting::new(storage.clone()),
            password_signup_need_review: PasswordSignupNeedReviewSetting::new(storage.clone()),
            enable_email_signup: EnableEmailSignupSetting::new(storage.clone()),
            email_signup_need_review: EmailSignupNeedReviewSetting::new(storage.clone()),
            enable_webauthn_signup: EnableWebauthnSignupSetting::new(storage.clone()),
            webauthn_signup_need_review: WebauthnSignupNeedReviewSetting::new(storage.clone()),
            enable_guest: EnableGuestSetting::new(storage.clone()),
        };

        let oauth2 = OAuth2SettingsStore {
            providers: OAuth2ProvidersSetting::new(storage.clone(), ssrf_guard.clone()),
        };

        let proxy = ProxySettingsStore {
            movie_proxy: MovieProxySetting::new(storage.clone()),
            live_proxy: LiveProxySetting::new(storage.clone()),
        };

        let rtmp = RtmpSettingsStore {
            custom_publish_host: CustomPublishHostSetting::new(storage.clone()),
            ts_disguised_as_png: TsDisguisedAsPngSetting::new(storage.clone()),
        };

        let email = EmailSettingsStore {
            enabled: EmailEnabledSetting::new(storage.clone()),
            smtp_host: EmailSmtpHostSetting::new(storage.clone()),
            smtp_port: EmailSmtpPortSetting::new(storage.clone()),
            smtp_credentials: EmailSmtpCredentialsSetting::new(storage.clone()),
            smtp_proxy: EmailSmtpProxySetting::new(storage.clone()),
            use_tls: EmailUseTlsSetting::new(storage.clone()),
            from_email: EmailFromEmailSetting::new(storage.clone()),
            from_name: EmailFromNameSetting::new(storage.clone()),
            whitelist_enabled: EmailWhitelistEnabledSetting::new(storage.clone()),
            whitelist: EmailWhitelistSetting::new(storage.clone()),
        };

        let webrtc = WebRtcSettingsStore {
            external_ice_servers: ExternalIceServersSetting::new(storage.clone()),
            max_voice_participants_per_room: MaxVoiceParticipantsPerRoomSetting::new(
                storage.clone(),
            ),
        };

        let chat = ChatSettingsStore {
            max_messages_per_room: MaxMessagesPerRoomSetting::new(storage.clone()),
            max_pinned_messages_per_room: MaxPinnedMessagesPerRoomSetting::new(storage.clone()),
            message_retention_days: MessageRetentionDaysSetting::new(storage.clone()),
        };
        let playback_history = PlaybackHistorySettingsStore {
            retention_days: PlaybackHistoryRetentionDaysSetting::new(storage.clone()),
            max_entries_per_room: PlaybackHistoryMaxEntriesPerRoomSetting::new(storage.clone()),
        };

        let cors = CorsSettingsStore {
            allowed_origins: CorsAllowedOriginsSetting::new(storage.clone()),
        };

        Self {
            storage: storage.clone(),
            ssrf_guard: ssrf_guard.clone(),
            server,
            room_defaults,
            permissions,
            room_creation,
            user,
            oauth2,
            proxy,
            rtmp,
            email,
            webrtc,
            chat,
            playback_history,
            cors,
        }
    }

    /// Initialize storage from database
    ///
    /// The `cancel` token is forwarded to the background reload listener so it
    /// can be stopped cleanly during graceful shutdown.
    pub fn init(&self, cancel: tokio_util::sync::CancellationToken) -> anyhow::Result<()> {
        // Load raw values from database into shared storage
        // Individual settings will lazy-load on first get()
        self.storage.init()?;

        // Start background listener to keep SettingsStorage in sync with
        // remote replica changes propagated via PostgreSQL LISTEN/NOTIFY
        self.storage.start_reload_listener(cancel);

        Ok(())
    }

    pub async fn set_room_password_policy(&self, policy: RoomPasswordPolicy) -> crate::Result<()> {
        let mut settings = self.runtime_settings()?;
        settings.room_creation.password_policy = policy;
        self.persist_runtime_settings(&settings).await?;
        Ok(())
    }

    pub async fn get_or_initialize_server_id(&self) -> crate::Result<String> {
        self.server
            .identity_id
            .get_or_initialize_with(|| format!("srv_{}", uuid::Uuid::new_v4().simple()))
            .await
    }

    pub fn runtime_settings(&self) -> crate::Result<RuntimeSettings> {
        Ok(RuntimeSettings {
            server: ServerRuntimeSettings {
                name: self.server.name.get()?,
            },
            room_defaults: RoomDefaultsRuntimeSettings {
                default_max_members: self.room_defaults.default_max_members.get()?,
                default_max_chat_messages: self.room_defaults.default_max_chat_messages.get()?,
            },
            permissions: PermissionRuntimeSettings {
                admin_default_permissions: self.permissions.admin_default_permissions.get()?,
                member_default_permissions: self.permissions.member_default_permissions.get()?,
                guest_default_permissions: self.permissions.guest_default_permissions.get()?,
            },
            room_creation: RoomCreationRuntimeSettings {
                enabled: self.room_creation.enabled.get()?,
                approval_required: self.room_creation.approval_required.get()?,
                password_policy: self.room_creation.password_policy.get()?,
                max_rooms_per_user: self.room_creation.max_rooms_per_user.get()?,
            },
            user: UserRuntimeSettings {
                enable_password_signup: self.user.enable_password_signup.get()?,
                password_signup_need_review: self.user.password_signup_need_review.get()?,
                enable_email_signup: self.user.enable_email_signup.get()?,
                email_signup_need_review: self.user.email_signup_need_review.get()?,
                enable_webauthn_signup: self.user.enable_webauthn_signup.get()?,
                webauthn_signup_need_review: self.user.webauthn_signup_need_review.get()?,
                enable_guest: self.user.enable_guest.get()?,
            },
            oauth2: OAuth2RuntimeSettings {
                providers: self.oauth2.providers.get()?,
            },
            proxy: ProxyRuntimeSettings {
                movie_proxy: self.proxy.movie_proxy.get()?,
                live_proxy: self.proxy.live_proxy.get()?,
            },
            rtmp: RtmpRuntimeSettings {
                custom_publish_host: self.rtmp.custom_publish_host.get()?.0,
                ts_disguised_as_png: self.rtmp.ts_disguised_as_png.get()?,
            },
            email: EmailRuntimeSettings {
                enabled: self.email.enabled.get()?,
                smtp_host: self.email.smtp_host.get()?.0,
                smtp_port: self.email.smtp_port.get()?,
                smtp_credentials: self.email.smtp_credentials.get()?.0,
                smtp_proxy: self.email.smtp_proxy.get()?.0,
                use_tls: self.email.use_tls.get()?,
                from_email: self.email.from_email.get()?.0,
                from_name: self.email.from_name.get()?,
                whitelist_enabled: self.email.whitelist_enabled.get()?,
                whitelist_domains: Self::normalize_email_whitelist_domains(
                    &self.email.whitelist.get()?,
                ),
            },
            webrtc: WebRtcRuntimeSettings {
                external_ice_servers: self.webrtc.external_ice_servers.get()?,
                max_voice_participants_per_room: self
                    .webrtc
                    .max_voice_participants_per_room
                    .get()?,
            },
            chat: ChatRuntimeSettings {
                max_messages_per_room: self.chat.max_messages_per_room.get()?,
                max_pinned_messages_per_room: self.chat.max_pinned_messages_per_room.get()?,
                message_retention_days: self.chat.message_retention_days.get()?,
            },
            playback_history: PlaybackHistoryRuntimeSettings {
                retention_days: self.playback_history.retention_days.get()?,
                max_entries_per_room: self.playback_history.max_entries_per_room.get()?,
            },
            cors: CorsRuntimeSettings {
                allowed_origins: self.cors.allowed_origins.get()?,
            },
        })
    }

    fn push_update_entry<T>(
        entries: &mut Vec<(String, String)>,
        enabled: bool,
        setting: &Setting<T>,
        value: &T,
    ) -> crate::Result<()>
    where
        T: fmt::Display + std::str::FromStr + Clone + Send + Sync + 'static,
        <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
    {
        if enabled {
            entries.push(setting.update_entry(value)?);
        }
        Ok(())
    }

    pub fn runtime_settings_update_entries(
        &self,
        settings: &RuntimeSettings,
    ) -> crate::Result<Vec<(String, String)>> {
        self.runtime_settings_update_entries_for_mask(settings, &RuntimeSettingsUpdateMask::all())
    }

    pub fn runtime_settings_update_entries_for_mask(
        &self,
        settings: &RuntimeSettings,
        update_mask: &RuntimeSettingsUpdateMask,
    ) -> crate::Result<Vec<(String, String)>> {
        let mut entries = Vec::new();
        Self::push_update_entry(
            &mut entries,
            update_mask.server.name,
            &self.server.name,
            &settings.server.name,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.room_defaults.default_max_members,
            &self.room_defaults.default_max_members,
            &settings.room_defaults.default_max_members,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.room_defaults.default_max_chat_messages,
            &self.room_defaults.default_max_chat_messages,
            &settings.room_defaults.default_max_chat_messages,
        )?;

        Self::push_update_entry(
            &mut entries,
            update_mask.permissions.admin_default_permissions,
            &self.permissions.admin_default_permissions,
            &settings.permissions.admin_default_permissions,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.permissions.member_default_permissions,
            &self.permissions.member_default_permissions,
            &settings.permissions.member_default_permissions,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.permissions.guest_default_permissions,
            &self.permissions.guest_default_permissions,
            &settings.permissions.guest_default_permissions,
        )?;

        Self::push_update_entry(
            &mut entries,
            update_mask.room_creation.enabled,
            &self.room_creation.enabled,
            &settings.room_creation.enabled,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.room_creation.approval_required,
            &self.room_creation.approval_required,
            &settings.room_creation.approval_required,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.room_creation.password_policy,
            &self.room_creation.password_policy,
            &settings.room_creation.password_policy,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.room_creation.max_rooms_per_user,
            &self.room_creation.max_rooms_per_user,
            &settings.room_creation.max_rooms_per_user,
        )?;

        Self::push_update_entry(
            &mut entries,
            update_mask.user.enable_password_signup,
            &self.user.enable_password_signup,
            &settings.user.enable_password_signup,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.user.password_signup_need_review,
            &self.user.password_signup_need_review,
            &settings.user.password_signup_need_review,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.user.enable_email_signup,
            &self.user.enable_email_signup,
            &settings.user.enable_email_signup,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.user.email_signup_need_review,
            &self.user.email_signup_need_review,
            &settings.user.email_signup_need_review,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.user.enable_webauthn_signup,
            &self.user.enable_webauthn_signup,
            &settings.user.enable_webauthn_signup,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.user.webauthn_signup_need_review,
            &self.user.webauthn_signup_need_review,
            &settings.user.webauthn_signup_need_review,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.user.enable_guest,
            &self.user.enable_guest,
            &settings.user.enable_guest,
        )?;

        Self::push_update_entry(
            &mut entries,
            update_mask.oauth2.providers,
            &self.oauth2.providers,
            &settings.oauth2.providers,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.proxy.movie_proxy,
            &self.proxy.movie_proxy,
            &settings.proxy.movie_proxy,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.proxy.live_proxy,
            &self.proxy.live_proxy,
            &settings.proxy.live_proxy,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.rtmp.custom_publish_host,
            &self.rtmp.custom_publish_host,
            &OptionalRuntimeConfig(settings.rtmp.custom_publish_host.clone()),
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.rtmp.ts_disguised_as_png,
            &self.rtmp.ts_disguised_as_png,
            &settings.rtmp.ts_disguised_as_png,
        )?;

        Self::push_update_entry(
            &mut entries,
            update_mask.email.enabled,
            &self.email.enabled,
            &settings.email.enabled,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.email.smtp_host,
            &self.email.smtp_host,
            &OptionalRuntimeConfig(settings.email.smtp_host.clone()),
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.email.smtp_port,
            &self.email.smtp_port,
            &settings.email.smtp_port,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.email.smtp_credentials,
            &self.email.smtp_credentials,
            &OptionalRuntimeConfig(settings.email.smtp_credentials.clone()),
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.email.smtp_proxy,
            &self.email.smtp_proxy,
            &OptionalRuntimeConfig(settings.email.smtp_proxy.clone()),
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.email.use_tls,
            &self.email.use_tls,
            &settings.email.use_tls,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.email.from_email,
            &self.email.from_email,
            &OptionalRuntimeConfig(settings.email.from_email.clone()),
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.email.from_name,
            &self.email.from_name,
            &settings.email.from_name,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.email.whitelist_enabled,
            &self.email.whitelist_enabled,
            &settings.email.whitelist_enabled,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.email.whitelist_domains,
            &self.email.whitelist,
            &settings.email.whitelist_raw(),
        )?;

        Self::push_update_entry(
            &mut entries,
            update_mask.webrtc.external_ice_servers,
            &self.webrtc.external_ice_servers,
            &settings.webrtc.external_ice_servers,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.webrtc.max_voice_participants_per_room,
            &self.webrtc.max_voice_participants_per_room,
            &settings.webrtc.max_voice_participants_per_room,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.chat.max_messages_per_room,
            &self.chat.max_messages_per_room,
            &settings.chat.max_messages_per_room,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.chat.max_pinned_messages_per_room,
            &self.chat.max_pinned_messages_per_room,
            &settings.chat.max_pinned_messages_per_room,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.chat.message_retention_days,
            &self.chat.message_retention_days,
            &settings.chat.message_retention_days,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.playback_history.retention_days,
            &self.playback_history.retention_days,
            &settings.playback_history.retention_days,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.playback_history.max_entries_per_room,
            &self.playback_history.max_entries_per_room,
            &settings.playback_history.max_entries_per_room,
        )?;
        Self::push_update_entry(
            &mut entries,
            update_mask.cors.allowed_origins,
            &self.cors.allowed_origins,
            &settings.cors.allowed_origins,
        )?;
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries)
    }

    pub fn validate_runtime_settings(&self, settings: &RuntimeSettings) -> crate::Result<()> {
        settings.validate(&self.validation_context())
    }

    #[must_use]
    pub fn validation_context(&self) -> SettingsValidationContext<'_> {
        SettingsValidationContext::new(&self.ssrf_guard)
    }

    pub async fn persist_runtime_settings(
        &self,
        settings: &RuntimeSettings,
    ) -> crate::Result<Vec<crate::models::settings::RuntimeSetting>> {
        self.persist_runtime_settings_with_mask(settings, &RuntimeSettingsUpdateMask::all())
            .await
    }

    pub async fn persist_runtime_settings_patch(
        &self,
        settings: &RuntimeSettings,
        update_mask: &RuntimeSettingsUpdateMask,
    ) -> crate::Result<Vec<crate::models::settings::RuntimeSetting>> {
        self.persist_runtime_settings_with_mask(settings, update_mask)
            .await
    }

    async fn persist_runtime_settings_with_mask(
        &self,
        settings: &RuntimeSettings,
        update_mask: &RuntimeSettingsUpdateMask,
    ) -> crate::Result<Vec<crate::models::settings::RuntimeSetting>> {
        self.validate_runtime_settings(settings)?;
        if update_mask.is_empty() {
            return Ok(Vec::new());
        }
        let entries = self.runtime_settings_update_entries_for_mask(settings, update_mask)?;
        let persisted = self
            .storage
            .settings_service()?
            .persist_raw_settings_batch_internal(entries)
            .await?;
        self.storage.apply_persisted_updates(persisted.clone());
        Ok(persisted)
    }

    /// Build a `PublicSettings` snapshot from the current registry values.
    pub fn to_public_settings(&self) -> crate::Result<PublicSettings> {
        let email_whitelist_enabled = self.email.whitelist_enabled.get()?;
        let email_whitelist_domains = if email_whitelist_enabled {
            Self::normalize_email_whitelist_domains(&self.email.whitelist.get()?)
        } else {
            Vec::new()
        };

        Ok(PublicSettings {
            server_name: self.server.name.get()?,
            room_creation_enabled: self.room_creation.enabled.get()?,
            max_rooms_per_user: self.room_creation.max_rooms_per_user.get()?,
            default_max_members: self.room_defaults.default_max_members.get()?,
            max_pinned_chat_messages_per_room: self.chat.max_pinned_messages_per_room.get()?,
            approval_required: self.room_creation.approval_required.get()?,
            room_password_policy: self.room_creation.password_policy.get()?,
            enable_password_signup: self.user.enable_password_signup.get()?,
            password_signup_need_review: self.user.password_signup_need_review.get()?,
            enable_email_signup: self.user.enable_email_signup.get()?,
            email_signup_need_review: self.user.email_signup_need_review.get()?,
            enable_webauthn_signup: self.user.enable_webauthn_signup.get()?,
            webauthn_signup_need_review: self.user.webauthn_signup_need_review.get()?,
            enable_guest: self.user.enable_guest.get()?,
            enable_email: self.email.enabled.get()?,
            enable_webauthn: false,
            movie_proxy: self.proxy.movie_proxy.get()?,
            live_proxy: self.proxy.live_proxy.get()?,
            ts_disguised_as_png: self.rtmp.ts_disguised_as_png.get()?,
            custom_publish_host: self.rtmp.custom_publish_host.get()?.0,
            email_whitelist_enabled,
            email_whitelist_domains,
        })
    }

    #[must_use]
    pub fn normalize_email_whitelist_domains(raw: &str) -> Vec<String> {
        let mut domains: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.trim_start_matches('@').to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect();
        domains.sort();
        domains.dedup();
        domains
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => std::panic::panic_any(context.to_string()),
            Err(error) => error,
        }
    }

    fn validate_settings(settings: &RuntimeSettings) -> crate::Result<()> {
        SettingsValidationContext::with_strict_policy(|ctx| settings.validate(ctx))
    }

    fn validate_configs(configs: &OAuth2ProviderConfigs) -> crate::Result<()> {
        SettingsValidationContext::with_strict_policy(|ctx| configs.validate(ctx))
    }

    #[test]
    fn test_server_name_defaults_and_validation() {
        let store = RuntimeSettingsStore::new_for_tests();
        let mut settings = store
            .runtime_settings()
            .expect("runtime settings should load");
        assert_eq!(settings.server.name, "SyncTV");
        assert_eq!(
            store
                .to_public_settings()
                .expect("public settings should load")
                .server_name,
            "SyncTV"
        );

        settings.server.name = "Family TV".to_string();
        assert!(validate_settings(&settings).is_ok());

        for invalid in [String::new(), " SyncTV".to_string(), "SyncTV\n".to_string()] {
            settings.server.name = invalid;
            assert!(validate_settings(&settings).is_err());
        }
        settings.server.name = "x".repeat(129);
        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn test_public_settings_registration_defaults_are_closed() {
        let settings = PublicSettings::defaults();
        assert!(!settings.enable_password_signup);
        assert!(!settings.password_signup_need_review);
        assert!(!settings.enable_email_signup);
        assert!(!settings.email_signup_need_review);
        assert!(!settings.enable_webauthn_signup);
        assert!(!settings.webauthn_signup_need_review);
    }

    #[test]
    fn test_guest_default_permissions_accept_only_guest_safe_permissions() {
        let allowed: PermissionSet = ok(
            r#"["view_members","view_chat_history","use_voice_chat","use_p2p_media"]"#.parse(),
            "guest-safe permission set should parse",
        );
        assert!(allowed.validate_guest_default().is_ok());
        assert!(allowed
            .bits()
            .has(crate::models::RoomPermission::USE_P2P_MEDIA));

        let empty: PermissionSet = ok("[]".parse(), "empty permission set should parse");
        assert!(empty.validate_guest_default().is_ok());

        let rejected: PermissionSet = ok(
            r#"["browse_library","send_chat_messages","manage_own_media"]"#.parse(),
            "unsafe permission set should parse",
        );
        let error = err(
            rejected.validate_guest_default(),
            "media-resource and chat permissions must not be guest defaults",
        );
        assert!(error.to_string().contains("permissions.guest_default"));
        assert!(error.to_string().contains("browse_library"));
        assert!(error.to_string().contains("send_chat_messages"));
        assert!(error.to_string().contains("manage_own_media"));
    }

    #[test]
    fn test_permission_set_accepts_send_chat_messages_name() {
        let parsed: PermissionSet = ok(
            r#"["send_chat_messages"]"#.parse(),
            "send_chat_messages permission set should parse",
        );
        assert!(parsed
            .bits()
            .has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
        assert_eq!(parsed.to_string(), r#"["send_chat_messages"]"#);
    }

    #[test]
    fn test_runtime_settings_rejects_unsafe_guest_default_permissions() {
        let mut settings = RuntimeSettingsStore::new_for_tests()
            .runtime_settings()
            .expect("runtime settings should load");
        settings.permissions.guest_default_permissions =
            r#"["browse_library","send_chat_messages"]"#
                .parse()
                .expect("valid names");

        let error = validate_settings(&settings).expect_err("unsafe guest default");
        assert!(
            error.to_string().contains("guest-safe permissions"),
            "unexpected error: {error}"
        );

        settings.permissions.guest_default_permissions =
            r#"["view_members","use_voice_chat","use_p2p_media"]"#
                .parse()
                .expect("valid names");
        assert!(validate_settings(&settings).is_ok());
    }

    #[test]
    fn test_runtime_settings_validates_email_whitelist_domains() {
        let mut settings = RuntimeSettingsStore::new_for_tests()
            .runtime_settings()
            .expect("runtime settings should load");
        settings.email.whitelist_enabled = true;
        settings.email.whitelist_domains =
            vec!["example.com".to_string(), "team.example.org".to_string()];
        assert!(validate_settings(&settings).is_ok());

        settings.email.whitelist_domains = vec!["alice@example.com".to_string()];
        assert!(validate_settings(&settings).is_err());

        settings.email.whitelist_domains = vec!["example".to_string()];
        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn test_runtime_settings_validates_typed_bounds() {
        let mut settings = RuntimeSettingsStore::new_for_tests()
            .runtime_settings()
            .expect("runtime settings should load");
        settings.room_creation.max_rooms_per_user = 0;
        assert!(validate_settings(&settings).is_err());

        settings.room_creation.max_rooms_per_user = 1001;
        assert!(validate_settings(&settings).is_err());

        settings.room_creation.max_rooms_per_user = 10;
        settings.room_defaults.default_max_members = 0;
        assert!(validate_settings(&settings).is_err());

        settings.room_defaults.default_max_members = 100;
        settings.room_defaults.default_max_chat_messages = 10_001;
        assert!(validate_settings(&settings).is_err());

        settings.room_defaults.default_max_chat_messages = 500;
        assert!(validate_settings(&settings).is_ok());

        settings.playback_history.retention_days = 3_651;
        assert!(validate_settings(&settings).is_err());
        settings.playback_history.retention_days = 90;
        settings.playback_history.max_entries_per_room = 100_001;
        assert!(validate_settings(&settings).is_err());
        settings.playback_history.max_entries_per_room = 1_000;
        assert!(validate_settings(&settings).is_ok());
    }

    #[tokio::test]
    async fn test_public_settings_hides_disabled_email_whitelist_domains() {
        let registry = RuntimeSettingsStore::new_for_tests();

        ok(
            registry.email.whitelist_enabled.set_for_test(&false),
            "email whitelist enabled setting should update",
        );
        ok(
            registry
                .email
                .whitelist
                .set_for_test(&"example.com,@team.example.org".to_string()),
            "email whitelist setting should update",
        );

        let settings = ok(
            registry.to_public_settings(),
            "public settings should serialize",
        );
        assert!(!settings.email_whitelist_enabled);
        assert!(settings.email_whitelist_domains.is_empty());
    }

    #[tokio::test]
    async fn test_public_settings_returns_enabled_email_whitelist_domains() {
        let registry = RuntimeSettingsStore::new_for_tests();

        ok(
            registry.email.whitelist_enabled.set_for_test(&true),
            "email whitelist enabled setting should update",
        );
        ok(
            registry
                .email
                .whitelist
                .set_for_test(&"Example.com,@team.example.org,example.com".to_string()),
            "email whitelist setting should update",
        );

        let settings = ok(
            registry.to_public_settings(),
            "public settings should serialize",
        );
        assert!(settings.email_whitelist_enabled);
        assert_eq!(
            settings.email_whitelist_domains,
            vec!["example.com", "team.example.org"]
        );
    }

    #[test]
    fn test_oauth2_provider_configs_default_closed() {
        let configs = OAuth2ProviderConfigs::default();
        let policy = configs.policy_for("github");
        assert!(!policy.enable_signup);
        assert!(!policy.signup_need_review);
        assert_eq!(configs.to_string(), "{}");
    }

    #[test]
    fn test_oauth2_provider_configs_parse_dynamic_instances() {
        let configs: OAuth2ProviderConfigs = ok(
            r#"{"github":{"type":"github","enableSignup":true,"clientId":"id","clientSecret":"secret","redirectUrl":"https://app.example.com/cb"},"corp_oidc":{"type":"oidc","enableSignup":true,"signupNeedReview":true,"clientId":"id","clientSecret":"secret","redirectUrl":"https://app.example.com/cb","issuer":"https://idp.example.com"}}"#
                .parse(),
            "OAuth2 provider configs should parse",
        );
        assert!(configs.policy_for("github").enable_signup);
        assert!(!configs.policy_for("github").signup_need_review);
        assert!(configs.policy_for("corp_oidc").enable_signup);
        assert!(configs.policy_for("corp_oidc").signup_need_review);
        assert!(!configs.policy_for("missing").enable_signup);
        let OAuth2ProviderPrivateConfig::GitHub(config) = &configs.0["github"].config else {
            panic!("expected github OAuth2 config");
        };
        assert_eq!(config.redirect_url, "https://app.example.com/cb");
    }

    #[test]
    fn test_oauth2_provider_configs_reject_snake_case_fields() {
        let error = r#"{"github":{"type":"github","enable_signup":true,"client_id":"id","client_secret":"secret","redirect_url":"https://app.example.com/cb"}}"#
            .parse::<OAuth2ProviderConfigs>()
            .expect_err("OAuth2 provider runtime JSON should reject snake_case fields");

        assert!(!error.to_string().is_empty());

        let error = r#"{"github":{"type":"github","enable_signup":true,"clientId":"id","clientSecret":"secret","redirectUrl":"https://app.example.com/cb"}}"#
            .parse::<OAuth2ProviderConfigs>()
            .expect_err("OAuth2 provider runtime JSON should reject outer snake_case fields");

        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn test_oauth2_provider_configs_validate_instance_names() {
        let configs: OAuth2ProviderConfigs = ok(
            r#"{"github_enterprise-1":{"type":"github","enableSignup":true,"clientId":"id","clientSecret":"secret","redirectUrl":"https://app.example.com/cb"}}"#
                .parse(),
            "OAuth2 provider configs should parse",
        );
        assert!(validate_configs(&configs).is_ok());

        let dotted: OAuth2ProviderConfigs = ok(
            r#"{"github.enterprise-1":{"type":"github","enableSignup":true,"clientId":"id","clientSecret":"secret","redirectUrl":"https://app.example.com/cb"}}"#
                .parse(),
            "OAuth2 provider configs should parse",
        );
        assert!(validate_configs(&dotted).is_err());

        let invalid: OAuth2ProviderConfigs = ok(
            r#"{"bad/name":{"type":"github","enableSignup":true,"clientId":"id","clientSecret":"secret","redirectUrl":"https://app.example.com/cb"}}"#
                .parse(),
            "OAuth2 provider configs should parse",
        );
        assert!(validate_configs(&invalid).is_err());
    }

    #[test]
    fn test_runtime_settings_store_validation_uses_configured_ssrf_guard() {
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "corp_oidc".to_string(),
            OAuth2ProviderConfig {
                enable_signup: true,
                signup_need_review: false,
                config: OAuth2ProviderPrivateConfig::Oidc(OAuth2OidcProviderConfig {
                    client_id: "id".to_string(),
                    client_secret: "secret".to_string(),
                    redirect_url: "https://app.example.com/callback".to_string(),
                    issuer: "http://127.0.0.1:8443".to_string(),
                    auth_url: None,
                    token_url: None,
                    userinfo_url: None,
                    jwks_url: None,
                    scopes: Vec::new(),
                }),
            },
        );

        let mut settings = RuntimeSettingsStore::new_for_tests()
            .runtime_settings()
            .expect("runtime settings should load");
        settings.oauth2.providers = OAuth2ProviderConfigs(providers);

        assert!(validate_settings(&settings).is_err());

        let guard = synctv_common::ssrf::SsrfGuard::builder()
            .allow_private_network_targets(true)
            .build();
        let store = RuntimeSettingsStore::new_for_tests_with_ssrf_guard(&guard);
        assert!(store.validate_runtime_settings(&settings).is_ok());
    }

    #[test]
    fn test_oauth2_provider_configs_validate_rejects_unimplemented_or_invalid_provider() {
        let unimplemented = r#"{"microsoft":{"type":"microsoft","enableSignup":true,"clientId":"id","clientSecret":"secret","redirectUrl":"https://app.example.com/cb"}}"#
            .parse::<OAuth2ProviderConfigs>();
        assert!(unimplemented.is_err());

        let invalid_config: OAuth2ProviderConfigs = ok(
            r#"{"github":{"type":"github","enableSignup":true,"clientId":"id"}}"#.parse(),
            "OAuth2 provider configs should parse",
        );
        assert!(validate_configs(&invalid_config).is_err());
    }

    #[test]
    fn test_public_settings_includes_nonempty_custom_publish_host() {
        let mut settings = PublicSettings::defaults();
        settings.custom_publish_host = Some("rtmp://live.example.com".to_string());
        let json = ok(
            serde_json::to_string(&settings),
            "public settings should serialize",
        );
        assert!(json.contains("custom_publish_host"));
        assert!(json.contains("rtmp://live.example.com"));
    }
}
