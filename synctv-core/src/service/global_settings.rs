//! Global setting variables
//!
//! This module defines all setting variables used throughout the application.
//! Each variable is type-safe, thread-safe, and automatically persists to the database.
//!
//! # Usage
//!
//! ```text
//! use synctv_core::service::global_settings::*;
//!
//! // Initialize during app startup
//! let registry = SettingsRegistry::new(settings_service);
//! let cancel = tokio_util::sync::CancellationToken::new();
//! registry.init(cancel)?;
//!
//! // Read - type-safe, returns cached value
//! if registry.enable_password_signup.get()? {
//!     // Password signup is enabled
//! }
//!
//! // Write - auto-converts to string and persists
//! registry.enable_password_signup.set(true).await?;
//!
//! // Validate user input via storage
//! if registry.storage.validate("user.enable_password_signup", "true") {
//!     // Value is valid
//! }
//! ```

use crate::models::room_settings::MaxMembers;
use crate::service::email::{EmailConfig, EmailConfigProvider};
use crate::service::{
    settings::SettingsValidationContext,
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
    ConfiguredIceServer, CorsAllowedOrigins, IceServerList, OAuth2ProviderConfig,
    OAuth2ProviderConfigs, OAuth2SignupPolicy, PermissionSet, PublicSettings, RoomPasswordPolicy,
};

/// Maximum allowed value for `max_chat_messages` setting (0 = unlimited)
const MAX_CHAT_MESSAGES_LIMIT: u64 = 10_000;

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

/// Settings registry for runtime initialization
///
/// Use this to initialize and manage all settings during app startup
#[derive(Clone)]
pub struct SettingsRegistry {
    /// Storage for managing all settings
    pub storage: Arc<SettingsStorage>,
    /// Stable logical server identity, automatically initialized by the runtime.
    pub server_identity_id: Setting<String>,

    // Server settings
    pub allow_room_creation: Setting<bool>,
    pub max_rooms_per_user: Setting<i64>,
    pub max_members_per_room: Setting<i64>,
    pub max_chat_messages: Setting<u64>,

    // Permission settings - global defaults for each role
    pub admin_default_permissions: Setting<PermissionSet>,
    pub member_default_permissions: Setting<PermissionSet>,
    pub guest_default_permissions: Setting<PermissionSet>,

    // Room settings
    pub disable_create_room: Setting<bool>,
    pub create_room_need_review: Setting<bool>,
    pub room_password_policy: Setting<RoomPasswordPolicy>,

    // User settings
    pub enable_password_signup: Setting<bool>,
    pub password_signup_need_review: Setting<bool>,
    pub enable_email_signup: Setting<bool>,
    pub email_signup_need_review: Setting<bool>,
    pub enable_webauthn_signup: Setting<bool>,
    pub webauthn_signup_need_review: Setting<bool>,
    pub enable_guest: Setting<bool>,

    // OAuth2 settings
    pub oauth2_providers: Setting<OAuth2ProviderConfigs>,

    // Proxy settings
    pub movie_proxy: Setting<bool>,
    pub live_proxy: Setting<bool>,

    // RTMP settings
    pub custom_publish_host: Setting<String>,
    pub ts_disguised_as_png: Setting<bool>,

    // Email settings
    pub email_enabled: Setting<bool>,
    pub email_smtp_host: Setting<String>,
    pub email_smtp_port: Setting<u16>,
    pub email_smtp_username: Setting<String>,
    pub email_smtp_password: Setting<String>,
    pub email_use_tls: Setting<bool>,
    pub email_from_email: Setting<String>,
    pub email_from_name: Setting<String>,
    pub email_whitelist_enabled: Setting<bool>,
    pub email_whitelist: Setting<String>,

    // WebRTC settings
    /// External ICE servers exposed to native clients.
    pub external_ice_servers: Setting<IceServerList>,

    // Chat message retention settings
    /// Maximum number of messages to keep per room (0 = unlimited)
    pub max_chat_messages_per_room: Setting<u64>,
    /// Absolute retention cap in days for chat messages (default: 90)
    pub chat_message_retention_days: Setting<i64>,

    // CORS settings
    /// Allowed CORS origins for proxy endpoints (empty = no origins allowed)
    pub cors_allowed_origins: Setting<CorsAllowedOrigins>,
}

impl std::fmt::Debug for SettingsRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsRegistry").finish()
    }
}

pub struct RuntimeEmailConfigProvider {
    settings: Arc<SettingsRegistry>,
    changes: broadcast::Sender<()>,
}

impl RuntimeEmailConfigProvider {
    #[must_use]
    pub fn new(settings: &Arc<SettingsRegistry>) -> Self {
        let (changes, _) = broadcast::channel(64);

        let provider = Self {
            settings: Arc::clone(settings),
            changes: changes.clone(),
        };

        let _ = provider.current_config();

        let Some(mut subscriptions) = try_subscribe_email_settings(settings) else {
            tracing::warn!(
                "Runtime email config changes are disabled because settings storage has no service backend"
            );
            return provider;
        };

        crate::spawn::spawn_monitored("runtime_email_config_provider_changes", async move {
            loop {
                let event = tokio::select! {
                    event = recv_email_setting_change(&mut subscriptions.enabled) => event,
                    event = recv_email_setting_change(&mut subscriptions.smtp_host) => event,
                    event = recv_email_setting_change(&mut subscriptions.smtp_port) => event,
                    event = recv_email_setting_change(&mut subscriptions.smtp_username) => event,
                    event = recv_email_setting_change(&mut subscriptions.smtp_password) => event,
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
    smtp_host: SettingChangeReceiver<String>,
    smtp_port: SettingChangeReceiver<u16>,
    smtp_username: SettingChangeReceiver<String>,
    smtp_password: SettingChangeReceiver<String>,
    use_tls: SettingChangeReceiver<bool>,
    from_email: SettingChangeReceiver<String>,
    from_name: SettingChangeReceiver<String>,
}

fn try_subscribe_email_settings(
    settings: &SettingsRegistry,
) -> Option<RuntimeEmailSettingSubscriptions> {
    Some(RuntimeEmailSettingSubscriptions {
        enabled: settings.email_enabled.subscribe_changes().ok()?,
        smtp_host: settings.email_smtp_host.subscribe_changes().ok()?,
        smtp_port: settings.email_smtp_port.subscribe_changes().ok()?,
        smtp_username: settings.email_smtp_username.subscribe_changes().ok()?,
        smtp_password: settings.email_smtp_password.subscribe_changes().ok()?,
        use_tls: settings.email_use_tls.subscribe_changes().ok()?,
        from_email: settings.email_from_email.subscribe_changes().ok()?,
        from_name: settings.email_from_name.subscribe_changes().ok()?,
    })
}

impl EmailConfigProvider for RuntimeEmailConfigProvider {
    fn current_config(&self) -> crate::Result<Option<EmailConfig>> {
        if !self.settings.email_enabled.get()? {
            return Ok(None);
        }

        Ok(Some(EmailConfig {
            smtp_host: self.settings.email_smtp_host.get()?.trim().to_string(),
            smtp_port: self.settings.email_smtp_port.get()?,
            smtp_username: self.settings.email_smtp_username.get()?,
            smtp_password: self.settings.email_smtp_password.get()?,
            from_email: self.settings.email_from_email.get()?.trim().to_string(),
            from_name: self.settings.email_from_name.get()?.trim().to_string(),
            use_tls: self.settings.email_use_tls.get()?,
        }))
    }

    fn subscribe_changes(&self) -> Option<broadcast::Receiver<()>> {
        Some(self.changes.subscribe())
    }
}

#[derive(Clone)]
struct EmailSettings {
    enabled: Setting<bool>,
    smtp_host: Setting<String>,
    smtp_port: Setting<u16>,
    smtp_username: Setting<String>,
    smtp_password: Setting<String>,
    use_tls: Setting<bool>,
    from_email: Setting<String>,
    from_name: Setting<String>,
}

impl EmailSettings {
    fn validate(&self, context: &SettingsValidationContext) -> crate::Result<()> {
        if !context.get(&self.enabled)? {
            return Ok(());
        }

        EmailConfig {
            smtp_host: context.get(&self.smtp_host)?.trim().to_string(),
            smtp_port: context.get(&self.smtp_port)?,
            smtp_username: context.get(&self.smtp_username)?,
            smtp_password: context.get(&self.smtp_password)?,
            from_email: context.get(&self.from_email)?.trim().to_string(),
            from_name: context.get(&self.from_name)?.trim().to_string(),
            use_tls: context.get(&self.use_tls)?,
        }
        .validate()
    }
}

impl SettingsRegistry {
    /// Create a new settings registry with all setting instances
    #[must_use]
    pub fn new(settings_service: Arc<SettingsService>) -> Self {
        Self::new_with_ssrf_guard(
            settings_service,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    /// Create a new settings registry using the runtime SSRF policy for
    /// settings that validate outbound provider URLs.
    #[must_use]
    pub fn new_with_ssrf_guard(
        settings_service: Arc<SettingsService>,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        let storage = Arc::new(SettingsStorage::new(settings_service));
        Self::from_storage(storage, ssrf_guard)
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests() -> Self {
        Self::new_for_tests_with_ssrf_guard(&synctv_common::ssrf::SsrfGuard::strict_policy())
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests_with_ssrf_guard(
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        let providers = Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new()));
        let storage = Arc::new(SettingsStorage::new_with_provider_map(providers));
        Self::from_storage(storage, ssrf_guard)
    }

    fn from_storage(
        storage: Arc<SettingsStorage>,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        let oauth2_ssrf_guard = ssrf_guard.clone();

        let email_smtp_host = setting!(String, "email.smtp_host", storage.clone(), String::new());
        let email_smtp_port = setting!(
            u16,
            "email.smtp_port",
            storage.clone(),
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
        let email_smtp_username = setting!(
            String,
            "email.smtp_username",
            storage.clone(),
            String::new()
        );
        let email_smtp_password = setting!(
            String,
            "email.smtp_password",
            storage.clone(),
            String::new()
        )
        .hidden_from_user_projection();
        let email_use_tls = setting!(bool, "email.use_tls", storage.clone(), true);
        let email_from_email = setting!(
            String,
            "email.from_email",
            storage.clone(),
            String::new(),
            |value: &String| -> crate::Result<()> {
                if value.is_empty()
                    || (value.contains('@') && !value.starts_with('@') && !value.ends_with('@'))
                {
                    Ok(())
                } else {
                    Err(crate::Error::InvalidInput(
                        "email.from_email must be empty or a valid email address".into(),
                    ))
                }
            }
        );
        let email_from_name = setting!(
            String,
            "email.from_name",
            storage.clone(),
            "SyncTV".to_string()
        );
        let email_enabled = setting!(bool, "email.enabled", storage.clone(), false);

        let registry = Self {
            storage: storage.clone(),
            server_identity_id: setting!(
                String,
                "server.identity_id",
                storage.clone(),
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
                            "server.identity_id must be a generated srv_ prefixed UUID value"
                                .into(),
                        ))
                    }
                }
            )
            .with_user_updates_disabled()
            .hidden_from_user_projection(),

            // Server settings using the setting! macro
            // Each setting auto-registers its provider to storage
            allow_room_creation: setting!(
                bool,
                "server.allow_room_creation",
                storage.clone(),
                true
            ),
            max_rooms_per_user: setting!(
                i64,
                "server.max_rooms_per_user",
                storage.clone(),
                10,
                |v: &i64| -> crate::Result<()> {
                    if *v > 0 && *v <= 1000 {
                        Ok(())
                    } else {
                        Err(crate::Error::InvalidInput(
                            "max_rooms_per_user must be between 1 and 1000".into(),
                        ))
                    }
                }
            ),
            max_members_per_room: setting!(
                i64,
                "server.max_members_per_room",
                storage.clone(),
                100,
                |v: &i64| -> crate::Result<()> {
                    if *v > 0 {
                        Ok(())
                    } else {
                        Err(crate::Error::InvalidInput(format!(
                            "max_members_per_room must be between 1 and {}",
                            MaxMembers::MAX
                        )))
                    }
                }
            ),
            max_chat_messages: setting!(
                u64,
                "server.max_chat_messages",
                storage.clone(),
                500,
                |v: &u64| -> crate::Result<()> {
                    if *v <= MAX_CHAT_MESSAGES_LIMIT {
                        Ok(())
                    } else {
                        Err(crate::Error::InvalidInput(format!("max_chat_messages must be at most {MAX_CHAT_MESSAGES_LIMIT} (0 = unlimited)")))
                    }
                }
            ),

            // Permission settings - global defaults for each room role.
            // PermissionService reads these runtime settings as its base role permissions.
            admin_default_permissions: setting!(
                PermissionSet,
                "permissions.admin_default",
                storage.clone(),
                PermissionSet::admin_default()
            ),
            member_default_permissions: setting!(
                PermissionSet,
                "permissions.member_default",
                storage.clone(),
                PermissionSet::member_default()
            ),
            guest_default_permissions: setting!(
                PermissionSet,
                "permissions.guest_default",
                storage.clone(),
                PermissionSet::guest_default(),
                |permissions: &PermissionSet| permissions.validate_guest_default()
            ),

            // Room settings
            disable_create_room: setting!(bool, "room.disable_create_room", storage.clone(), false),
            create_room_need_review: setting!(
                bool,
                "room.create_room_need_review",
                storage.clone(),
                false
            ),
            room_password_policy: setting!(
                RoomPasswordPolicy,
                "room.password_policy",
                storage.clone(),
                RoomPasswordPolicy::Optional
            ),

            // User settings
            enable_password_signup: setting!(
                bool,
                "user.enable_password_signup",
                storage.clone(),
                false
            ),
            password_signup_need_review: setting!(
                bool,
                "user.password_signup_need_review",
                storage.clone(),
                false
            ),
            enable_email_signup: setting!(bool, "user.enable_email_signup", storage.clone(), false),
            email_signup_need_review: setting!(
                bool,
                "user.email_signup_need_review",
                storage.clone(),
                false
            ),
            enable_webauthn_signup: setting!(
                bool,
                "user.enable_webauthn_signup",
                storage.clone(),
                false
            ),
            webauthn_signup_need_review: setting!(
                bool,
                "user.webauthn_signup_need_review",
                storage.clone(),
                false
            ),
            enable_guest: setting!(bool, "user.enable_guest", storage.clone(), true),

            // OAuth2 settings
            oauth2_providers: setting!(
                OAuth2ProviderConfigs,
                "oauth2.providers",
                storage.clone(),
                OAuth2ProviderConfigs::default(),
                move |configs: &OAuth2ProviderConfigs| {
                    configs.validate_with_ssrf_guard(&oauth2_ssrf_guard)
                }
            ),

            // Proxy settings
            movie_proxy: setting!(bool, "proxy.movie_proxy", storage.clone(), true),
            live_proxy: setting!(bool, "proxy.live_proxy", storage.clone(), true),
            // RTMP settings
            custom_publish_host: setting!(
                String,
                "rtmp.custom_publish_host",
                storage.clone(),
                String::new()
            ),
            ts_disguised_as_png: setting!(bool, "rtmp.ts_disguised_as_png", storage.clone(), false),

            // Email settings
            email_enabled,
            email_smtp_host,
            email_smtp_port,
            email_smtp_username,
            email_smtp_password,
            email_use_tls,
            email_from_email,
            email_from_name,
            email_whitelist_enabled: setting!(
                bool,
                "email.whitelist_enabled",
                storage.clone(),
                false
            ),
            email_whitelist: setting!(
                String,
                "email.whitelist",
                storage.clone(),
                String::new(),
                |value: &String| validate_email_whitelist_domains(value)
            ),

            // WebRTC settings
            external_ice_servers: setting!(
                IceServerList,
                "webrtc.external_ice_servers",
                storage.clone(),
                IceServerList::new()
            ),

            // Chat message retention settings
            max_chat_messages_per_room: setting!(
                u64,
                "chat.max_messages_per_room",
                storage.clone(),
                500,
                |v: &u64| -> crate::Result<()> {
                    if *v <= 100_000 {
                        Ok(())
                    } else {
                        Err(crate::Error::InvalidInput(
                            "max_chat_messages_per_room must be <= 100000 (0 = unlimited)".into(),
                        ))
                    }
                }
            ),
            chat_message_retention_days: setting!(
                i64,
                "chat.message_retention_days",
                storage.clone(),
                90,
                |v: &i64| -> crate::Result<()> {
                    if *v >= 1 && *v <= 3650 {
                        Ok(())
                    } else {
                        Err(crate::Error::InvalidInput(
                            "chat_message_retention_days must be between 1 and 3650".into(),
                        ))
                    }
                }
            ),

            // CORS settings
            cors_allowed_origins: setting!(
                CorsAllowedOrigins,
                "cors.allowed_origins",
                storage,
                CorsAllowedOrigins::new()
            ),
        };

        let email_settings = registry.email_settings();
        if let Ok(settings_service) = registry.storage.settings_service() {
            settings_service.add_batch_validator(move |context| email_settings.validate(context));
        }
        registry
    }

    fn email_settings(&self) -> EmailSettings {
        EmailSettings {
            enabled: self.email_enabled.clone(),
            smtp_host: self.email_smtp_host.clone(),
            smtp_port: self.email_smtp_port.clone(),
            smtp_username: self.email_smtp_username.clone(),
            smtp_password: self.email_smtp_password.clone(),
            use_tls: self.email_use_tls.clone(),
            from_email: self.email_from_email.clone(),
            from_name: self.email_from_name.clone(),
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
        self.room_password_policy.set(policy).await?;
        Ok(())
    }

    pub async fn get_or_initialize_server_id(&self) -> crate::Result<String> {
        self.server_identity_id
            .get_or_initialize_with(|| format!("srv_{}", uuid::Uuid::new_v4().simple()))
            .await
    }

    /// Build a `PublicSettings` snapshot from the current registry values.
    pub fn to_public_settings(&self) -> crate::Result<PublicSettings> {
        let email_whitelist_enabled = self.email_whitelist_enabled.get()?;
        let email_whitelist_domains = if email_whitelist_enabled {
            Self::normalize_email_whitelist_domains(&self.email_whitelist.get()?)
        } else {
            Vec::new()
        };

        Ok(PublicSettings {
            allow_room_creation: self.allow_room_creation.get()?,
            max_rooms_per_user: self.max_rooms_per_user.get()?,
            max_members_per_room: self.max_members_per_room.get()?,
            disable_create_room: self.disable_create_room.get()?,
            create_room_need_review: self.create_room_need_review.get()?,
            room_password_policy: self.room_password_policy.get()?,
            enable_password_signup: self.enable_password_signup.get()?,
            password_signup_need_review: self.password_signup_need_review.get()?,
            enable_email_signup: self.enable_email_signup.get()?,
            email_signup_need_review: self.email_signup_need_review.get()?,
            enable_webauthn_signup: self.enable_webauthn_signup.get()?,
            webauthn_signup_need_review: self.webauthn_signup_need_review.get()?,
            enable_guest: self.enable_guest.get()?,
            enable_email: self.email_enabled.get()?,
            enable_webauthn: false,
            movie_proxy: self.movie_proxy.get()?,
            live_proxy: self.live_proxy.get()?,
            ts_disguised_as_png: self.ts_disguised_as_png.get()?,
            custom_publish_host: self.custom_publish_host.get()?,
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
        let allowed: PermissionSet = r#"["view_member_list","view_chat_history","use_webrtc"]"#
            .parse()
            .unwrap();
        assert!(allowed.validate_guest_default().is_ok());

        let empty: PermissionSet = "[]".parse().unwrap();
        assert!(empty.validate_guest_default().is_ok());

        let rejected: PermissionSet = r#"["view_media_resources","chat","create_media_resource"]"#
            .parse()
            .unwrap();
        let error = rejected
            .validate_guest_default()
            .expect_err("media-resource and chat permissions must not be guest defaults");
        assert!(error.to_string().contains("permissions.guest_default"));
        assert!(error.to_string().contains("view_media_resources"));
        assert!(error.to_string().contains("chat"));
        assert!(error.to_string().contains("create_media_resource"));
    }

    #[test]
    fn test_permission_set_uses_live_control_name_only() {
        let parsed: PermissionSet = r#"["live_control"]"#.parse().unwrap();
        assert!(parsed
            .bits()
            .has(crate::models::RoomPermission::LIVE_CONTROL));
        assert_eq!(parsed.to_string(), r#"["live_control"]"#);

        let error = r#"["start_live"]"#
            .parse::<PermissionSet>()
            .expect_err("start_live is not a supported permission setting name");
        assert!(
            error
                .to_string()
                .contains("unknown permission name 'start_live'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_permission_set_accepts_chat_name() {
        let parsed: PermissionSet = r#"["chat"]"#.parse().unwrap();
        assert!(parsed.bits().has(crate::models::RoomPermission::CHAT));
        assert_eq!(parsed.to_string(), r#"["chat"]"#);
    }

    #[tokio::test]
    async fn test_settings_registry_rejects_unsafe_guest_default_permissions() {
        let registry = SettingsRegistry::new_for_tests();

        let invalid = r#"["view_media_resources","chat"]"#;
        assert!(
            !registry
                .storage
                .validate("permissions.guest_default", invalid),
            "guest defaults must reject permissions outside GUEST_ASSIGNABLE"
        );

        let valid = r#"["view_member_list","use_webrtc"]"#;
        assert!(
            registry
                .storage
                .validate("permissions.guest_default", valid),
            "guest defaults should accept guest-safe permissions"
        );
    }

    #[tokio::test]
    async fn test_settings_registry_validates_email_whitelist_domains() {
        let registry = SettingsRegistry::new_for_tests();

        assert!(registry
            .storage
            .validate("email.whitelist", "example.com,@team.example.org"));
        assert!(!registry
            .storage
            .validate("email.whitelist", "alice@example.com"));
        assert!(!registry.storage.validate("email.whitelist", "example"));
    }

    #[test]
    fn test_settings_registry_wires_validation_providers() {
        let registry = SettingsRegistry::new_for_tests();

        assert!(registry.storage.validate("server.max_rooms_per_user", "10"));
        assert!(!registry.storage.validate("server.max_rooms_per_user", "0"));
        assert!(!registry
            .storage
            .validate("server.max_rooms_per_user", "1001"));

        assert!(registry
            .storage
            .validate("server.max_members_per_room", "100"));
        assert!(!registry
            .storage
            .validate("server.max_members_per_room", "0"));

        assert!(registry
            .storage
            .validate("user.enable_password_signup", "true"));
        assert!(!registry
            .storage
            .validate("user.enable_password_signup", "not_bool"));

        assert!(registry.storage.validate("server.max_chat_messages", "500"));
        assert!(!registry
            .storage
            .validate("server.max_chat_messages", "10001"));
    }

    #[tokio::test]
    async fn test_public_settings_hides_disabled_email_whitelist_domains() {
        let registry = SettingsRegistry::new_for_tests();

        registry
            .email_whitelist_enabled
            .set_for_test(&false)
            .unwrap();
        registry
            .email_whitelist
            .set_for_test(&"example.com,@team.example.org".to_string())
            .unwrap();

        let settings = registry.to_public_settings().unwrap();
        assert!(!settings.email_whitelist_enabled);
        assert!(settings.email_whitelist_domains.is_empty());
    }

    #[tokio::test]
    async fn test_public_settings_returns_enabled_email_whitelist_domains() {
        let registry = SettingsRegistry::new_for_tests();

        registry
            .email_whitelist_enabled
            .set_for_test(&true)
            .unwrap();
        registry
            .email_whitelist
            .set_for_test(&"Example.com,@team.example.org,example.com".to_string())
            .unwrap();

        let settings = registry.to_public_settings().unwrap();
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
        let configs: OAuth2ProviderConfigs = r#"{"github":{"type":"github","enable_signup":true,"config":{"client_id":"id","client_secret":"secret","redirect_url":"https://app.example.com/cb"}},"corp_oidc":{"type":"oidc","enable_signup":true,"signup_need_review":true,"config":{"client_id":"id","client_secret":"secret","redirect_url":"https://app.example.com/cb","issuer":"https://idp.example.com"}}}"#
            .parse()
            .unwrap();
        assert!(configs.policy_for("github").enable_signup);
        assert!(!configs.policy_for("github").signup_need_review);
        assert!(configs.policy_for("corp_oidc").enable_signup);
        assert!(configs.policy_for("corp_oidc").signup_need_review);
        assert!(!configs.policy_for("missing").enable_signup);
        assert_eq!(
            configs.0["github"].config["redirect_url"],
            "https://app.example.com/cb"
        );
    }

    #[test]
    fn test_oauth2_provider_configs_validate_instance_names() {
        let configs: OAuth2ProviderConfigs =
            r#"{"github_enterprise-1":{"type":"github","enable_signup":true,"config":{"client_id":"id","client_secret":"secret","redirect_url":"https://app.example.com/cb"}}}"#
                .parse()
                .unwrap();
        assert!(configs.validate().is_ok());

        let dotted: OAuth2ProviderConfigs =
            r#"{"github.enterprise-1":{"type":"github","enable_signup":true,"config":{"client_id":"id","client_secret":"secret","redirect_url":"https://app.example.com/cb"}}}"#
                .parse()
                .unwrap();
        assert!(dotted.validate().is_err());

        let invalid: OAuth2ProviderConfigs =
            r#"{"bad/name":{"type":"github","enable_signup":true,"config":{"client_id":"id","client_secret":"secret","redirect_url":"https://app.example.com/cb"}}}"#
                .parse()
                .unwrap();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn test_oauth2_provider_configs_validate_rejects_unimplemented_or_invalid_provider() {
        let unimplemented: OAuth2ProviderConfigs =
            r#"{"microsoft":{"type":"microsoft","enable_signup":true,"config":{"client_id":"id","client_secret":"secret","redirect_url":"https://app.example.com/cb"}}}"#
                .parse()
                .unwrap();
        assert!(unimplemented.validate().is_err());

        let invalid_config: OAuth2ProviderConfigs =
            r#"{"github":{"type":"github","enable_signup":true,"config":{"client_id":"id"}}}"#
                .parse()
                .unwrap();
        assert!(invalid_config.validate().is_err());
    }

    #[test]
    fn test_public_settings_includes_nonempty_custom_publish_host() {
        let mut settings = PublicSettings::defaults();
        settings.custom_publish_host = "rtmp://live.example.com".to_string();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("custom_publish_host"));
        assert!(json.contains("rtmp://live.example.com"));
    }
}
