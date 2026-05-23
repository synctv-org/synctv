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
//! registry.init(cancel).unwrap();
//!
//! // Read - type-safe, returns cached value
//! if registry.enable_password_signup.get().unwrap() {
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

use crate::models::{room_settings::MaxMembers, PermissionBits};
use crate::service::{
    settings_vars::{Setting, SettingsStorage},
    SettingsService,
};
use crate::setting;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

/// Maximum allowed value for `max_chat_messages` setting (0 = unlimited)
const MAX_CHAT_MESSAGES_LIMIT: u64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSet(PermissionBits);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSetParseError(String);

impl fmt::Display for PermissionSetParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PermissionSetParseError {}

impl PermissionSet {
    #[must_use]
    pub const fn admin_default() -> Self {
        Self(PermissionBits(PermissionBits::DEFAULT_ADMIN))
    }

    #[must_use]
    pub const fn member_default() -> Self {
        Self(PermissionBits(PermissionBits::DEFAULT_MEMBER))
    }

    #[must_use]
    pub const fn guest_default() -> Self {
        Self(PermissionBits(PermissionBits::DEFAULT_GUEST))
    }

    #[must_use]
    pub const fn bits(&self) -> PermissionBits {
        self.0
    }

    fn names_for_bits(bits: u64) -> Vec<&'static str> {
        NAMED_PERMISSIONS
            .iter()
            .filter_map(|(name, bit)| ((bits & *bit) != 0).then_some(*name))
            .collect()
    }

    pub fn validate_guest_default(&self) -> crate::Result<()> {
        let invalid = self.bits().0 & !PermissionBits::GUEST_ASSIGNABLE;
        if invalid == 0 {
            return Ok(());
        }

        let invalid_names = Self::names_for_bits(invalid);
        let allowed_names = Self::names_for_bits(PermissionBits::GUEST_ASSIGNABLE);
        Err(crate::Error::InvalidInput(format!(
            "permissions.guest_default may only include guest-safe permissions: {}; invalid permissions: {}",
            allowed_names.join(", "),
            invalid_names.join(", "),
        )))
    }
}

const NAMED_PERMISSIONS: &[(&str, u64)] = &[
    ("send_chat", PermissionBits::SEND_CHAT),
    (
        "create_media_resource",
        PermissionBits::CREATE_MEDIA_RESOURCE,
    ),
    (
        "delete_media_resource_any",
        PermissionBits::DELETE_MEDIA_RESOURCE_ANY,
    ),
    (
        "reorder_media_resources",
        PermissionBits::REORDER_MEDIA_RESOURCES,
    ),
    (
        "clear_media_resources",
        PermissionBits::CLEAR_MEDIA_RESOURCES,
    ),
    ("live_control", PermissionBits::LIVE_CONTROL),
    ("play_control", PermissionBits::PLAY_CONTROL),
    ("change_current_media", PermissionBits::CHANGE_CURRENT_MEDIA),
    ("change_playback_rate", PermissionBits::CHANGE_PLAYBACK_RATE),
    ("approve_member", PermissionBits::APPROVE_MEMBER),
    ("kick_member", PermissionBits::KICK_MEMBER),
    (
        "set_member_permissions",
        PermissionBits::SET_MEMBER_PERMISSIONS,
    ),
    ("add_member", PermissionBits::ADD_MEMBER),
    ("set_room_settings", PermissionBits::SET_ROOM_SETTINGS),
    ("delete_chat", PermissionBits::DELETE_CHAT),
    ("delete_room", PermissionBits::DELETE_ROOM),
    ("view_media_resources", PermissionBits::VIEW_MEDIA_RESOURCES),
    ("view_member_list", PermissionBits::VIEW_MEMBER_LIST),
    ("view_chat_history", PermissionBits::VIEW_CHAT_HISTORY),
    ("use_webrtc", PermissionBits::USE_WEBRTC),
];

impl fmt::Display for PermissionSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = NAMED_PERMISSIONS
            .iter()
            .filter_map(|(name, bit)| self.0.has(*bit).then_some(*name))
            .collect::<Vec<_>>();
        let json = serde_json::to_string(&names).map_err(|_| fmt::Error)?;
        f.write_str(&json)
    }
}

impl std::str::FromStr for PermissionSet {
    type Err = PermissionSetParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let names = serde_json::from_str::<Vec<String>>(s).map_err(|error| {
            PermissionSetParseError(format!(
                "permission set must be a JSON array of permission names: {error}"
            ))
        })?;

        let mut bits = PermissionBits::empty();
        for name in names {
            let Some((_, bit)) = NAMED_PERMISSIONS
                .iter()
                .find(|(permission_name, _)| *permission_name == name)
            else {
                return Err(PermissionSetParseError(format!(
                    "unknown permission name '{name}'"
                )));
            };
            bits.grant(*bit);
        }

        Ok(Self(bits))
    }
}

/// A statically configured ICE server entry exposed to native clients.
///
/// Supports STUN-only entries and TURN/TURNS entries with optional credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfiguredIceServer {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

impl ConfiguredIceServer {
    #[must_use]
    pub fn new(urls: Vec<String>) -> Self {
        Self {
            urls,
            username: None,
            credential: None,
        }
    }

    #[must_use]
    pub fn with_auth(mut self, username: impl Into<String>, credential: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self.credential = Some(credential.into());
        self
    }
}

/// A list of user-configured external ICE servers stored as JSON in the settings database.
///
/// Implements `Display` (→ JSON) and `FromStr` (← JSON) so it can be used
/// directly with `Setting<IceServerList>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct IceServerList(pub Vec<ConfiguredIceServer>);

impl IceServerList {
    #[must_use]
    pub fn new() -> Self {
        Self(vec![
            ConfiguredIceServer::new(vec!["stun:stun.l.google.com:19302".to_string()]),
            ConfiguredIceServer::new(vec!["stun:stun1.l.google.com:19302".to_string()]),
        ])
    }
}

impl Default for IceServerList {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for IceServerList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let json = serde_json::to_string(&self.0).unwrap_or_else(|_| "[]".to_string());
        f.write_str(&json)
    }
}

impl std::str::FromStr for IceServerList {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Ok(Self(Vec::new()));
        }
        let servers: Vec<ConfiguredIceServer> = serde_json::from_str(s)?;
        Ok(Self(servers))
    }
}

/// A list of allowed CORS origins, stored as a JSON array in the settings database.
///
/// Each entry is an origin URL string, e.g. `"https://example.com"`.
///
/// Implements `Display` (→ JSON) and `FromStr` (← JSON) so it can be used
/// directly with `Setting<CorsAllowedOrigins>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CorsAllowedOrigins(pub Vec<String>);

impl CorsAllowedOrigins {
    /// Create an empty list of allowed origins (secure default).
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }
}

impl Default for CorsAllowedOrigins {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for CorsAllowedOrigins {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let json = serde_json::to_string(&self.0).unwrap_or_else(|_| "[]".to_string());
        f.write_str(&json)
    }
}

impl std::str::FromStr for CorsAllowedOrigins {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Ok(Self(Vec::new()));
        }
        let origins: Vec<String> = serde_json::from_str(s)?;
        Ok(Self(origins))
    }
}

/// Runtime OAuth2 signup policy for one provider instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct OAuth2SignupPolicy {
    pub enable_signup: bool,
    pub signup_need_review: bool,
}

/// Runtime configuration for one OAuth2 provider instance.
///
/// Only the common envelope is modeled here. Provider-specific fields live
/// under `config` and are parsed by the selected provider factory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct OAuth2ProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: String,
    pub enable_signup: bool,
    pub signup_need_review: bool,
    pub config: serde_json::Map<String, serde_json::Value>,
}

impl OAuth2ProviderConfig {
    #[must_use]
    pub const fn signup_policy(&self) -> OAuth2SignupPolicy {
        OAuth2SignupPolicy {
            enable_signup: self.enable_signup,
            signup_need_review: self.signup_need_review,
        }
    }

    #[must_use]
    pub fn provider_config_value(&self) -> serde_json::Value {
        serde_json::Value::Object(self.config.clone())
    }
}

/// Dynamic OAuth2 provider registry stored as one JSON runtime setting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(transparent)]
pub struct OAuth2ProviderConfigs(pub BTreeMap<String, OAuth2ProviderConfig>);

impl OAuth2ProviderConfigs {
    #[must_use]
    pub fn policy_for(&self, instance_name: &str) -> OAuth2SignupPolicy {
        self.0
            .get(instance_name)
            .map(OAuth2ProviderConfig::signup_policy)
            .unwrap_or_default()
    }

    pub fn validate(&self) -> crate::Result<()> {
        self.validate_with_ssrf_guard(&synctv_common::ssrf::SsrfGuard::strict_policy())
    }

    pub fn validate_with_ssrf_guard(
        &self,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> crate::Result<()> {
        for (instance_name, provider_config) in &self.0 {
            validate_oauth2_instance_name(instance_name)?;
            let provider_type = provider_config.provider_type.trim();
            if provider_type.is_empty() {
                return Err(crate::Error::InvalidInput(format!(
                    "OAuth2 provider '{instance_name}' must set a non-empty type"
                )));
            }
            if crate::models::oauth2_client::OAuth2Provider::from_str_name(provider_type).is_none()
            {
                return Err(crate::Error::InvalidInput(format!(
                    "OAuth2 provider '{instance_name}' uses unsupported type '{provider_type}'"
                )));
            }
            let provider_config = provider_config.provider_config_value();
            crate::oauth2::providers::provider_registry(ssrf_guard.clone())
                .create_provider(provider_type, &provider_config)
                .map_err(|error| {
                    crate::Error::InvalidInput(format!(
                        "OAuth2 provider '{instance_name}' has invalid {provider_type} config: {error}"
                    ))
                })?;
        }
        Ok(())
    }
}

fn validate_oauth2_instance_name(instance_name: &str) -> crate::Result<()> {
    if instance_name.is_empty() {
        return Err(crate::Error::InvalidInput(
            "OAuth2 provider instance name must not be empty".to_string(),
        ));
    }
    if instance_name.len() > 64 {
        return Err(crate::Error::InvalidInput(
            "OAuth2 provider instance name must be at most 64 bytes".to_string(),
        ));
    }
    if !instance_name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(crate::Error::InvalidInput(
            "OAuth2 provider instance name may only contain ASCII letters, digits, '_' and '-'"
                .to_string(),
        ));
    }
    Ok(())
}

impl fmt::Display for OAuth2ProviderConfigs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let json = serde_json::to_string(&self.0).unwrap_or_else(|_| "{}".to_string());
        f.write_str(&json)
    }
}

impl std::str::FromStr for OAuth2ProviderConfigs {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Ok(Self::default());
        }
        let configs: BTreeMap<String, OAuth2ProviderConfig> = serde_json::from_str(s)?;
        Ok(Self(configs))
    }
}

/// A snapshot of all client-visible settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicSettings {
    pub allow_room_creation: bool,
    pub max_rooms_per_user: i64,
    pub max_members_per_room: i64,

    // Room settings
    pub disable_create_room: bool,
    pub create_room_need_review: bool,
    pub room_ttl: i64,
    pub room_must_need_pwd: bool,
    pub room_must_no_need_pwd: bool,

    // User settings
    pub enable_password_signup: bool,
    pub password_signup_need_review: bool,
    pub enable_email_signup: bool,
    pub email_signup_need_review: bool,
    pub enable_webauthn_signup: bool,
    pub webauthn_signup_need_review: bool,
    pub enable_guest: bool,

    // Proxy settings
    pub movie_proxy: bool,
    pub live_proxy: bool,

    // RTMP settings
    pub ts_disguised_as_png: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub custom_publish_host: String,

    // Email settings
    pub email_whitelist_enabled: bool,
}

impl PublicSettings {
    /// Default settings when the settings registry is not configured.
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            allow_room_creation: true,
            max_rooms_per_user: 10,
            max_members_per_room: 100,
            disable_create_room: false,
            create_room_need_review: false,
            room_ttl: 172_800,
            room_must_need_pwd: false,
            room_must_no_need_pwd: false,
            enable_password_signup: false,
            password_signup_need_review: false,
            enable_email_signup: false,
            email_signup_need_review: false,
            enable_webauthn_signup: false,
            webauthn_signup_need_review: false,
            enable_guest: true,
            movie_proxy: true,
            live_proxy: true,
            ts_disguised_as_png: false,
            custom_publish_host: String::new(),
            email_whitelist_enabled: false,
        }
    }
}

/// Settings registry for runtime initialization
///
/// Use this to initialize and manage all settings during app startup
#[derive(Clone)]
pub struct SettingsRegistry {
    /// Storage for managing all settings
    pub storage: Arc<SettingsStorage>,

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
    pub room_ttl: Setting<i64>,
    pub room_must_need_pwd: Setting<bool>,
    pub room_must_no_need_pwd: Setting<bool>,

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
        let oauth2_ssrf_guard = ssrf_guard.clone();

        Self {
            storage: storage.clone(),

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
                    if *v > 0 && *v <= MaxMembers::MAX.cast_signed() {
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
            // These must stay aligned with PermissionBits::DEFAULT_* because
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
            room_ttl: setting!(
                i64,
                "room.room_ttl",
                storage.clone(),
                172_800, // 48 hours in seconds
                |v: &i64| -> crate::Result<()> {
                    if *v >= 0 {
                        Ok(())
                    } else {
                        Err(crate::Error::InvalidInput(
                            "room_ttl must be non-negative (0 = never expire)".into(),
                        ))
                    }
                }
            ),
            room_must_need_pwd: setting!(bool, "room.room_must_need_pwd", storage.clone(), false),
            room_must_no_need_pwd: setting!(
                bool,
                "room.room_must_no_need_pwd",
                storage.clone(),
                false
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
            email_whitelist_enabled: setting!(
                bool,
                "email.whitelist_enabled",
                storage.clone(),
                false
            ),
            email_whitelist: setting!(String, "email.whitelist", storage.clone(), String::new()),

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

    /// Set `room_must_need_pwd` with cross-validation against `room_must_no_need_pwd`.
    ///
    /// Routes through the typed setting so the transactional cross-validation
    /// and the local `SettingsStorage` snapshot stay in sync immediately.
    pub async fn set_room_must_need_pwd(&self, value: bool) -> crate::Result<()> {
        self.room_must_need_pwd.set(value).await?;
        Ok(())
    }

    /// Set `room_must_no_need_pwd` with cross-validation against `room_must_need_pwd`.
    ///
    /// Routes through the typed setting so the transactional cross-validation
    /// and the local `SettingsStorage` snapshot stay in sync immediately.
    pub async fn set_room_must_no_need_pwd(&self, value: bool) -> crate::Result<()> {
        self.room_must_no_need_pwd.set(value).await?;
        Ok(())
    }

    /// Build a `PublicSettings` snapshot from the current registry values.
    #[must_use]
    pub fn to_public_settings(&self) -> PublicSettings {
        PublicSettings {
            allow_room_creation: Self::get_or_warn(
                "allow_room_creation",
                &self.allow_room_creation,
                true,
            ),
            max_rooms_per_user: Self::get_or_warn(
                "max_rooms_per_user",
                &self.max_rooms_per_user,
                10,
            ),
            max_members_per_room: Self::get_or_warn(
                "max_members_per_room",
                &self.max_members_per_room,
                100,
            ),
            disable_create_room: Self::get_or_warn(
                "disable_create_room",
                &self.disable_create_room,
                false,
            ),
            create_room_need_review: Self::get_or_warn(
                "create_room_need_review",
                &self.create_room_need_review,
                false,
            ),
            room_ttl: Self::get_or_warn("room_ttl", &self.room_ttl, 172_800),
            room_must_need_pwd: Self::get_or_warn(
                "room_must_need_pwd",
                &self.room_must_need_pwd,
                false,
            ),
            room_must_no_need_pwd: Self::get_or_warn(
                "room_must_no_need_pwd",
                &self.room_must_no_need_pwd,
                false,
            ),
            enable_password_signup: Self::get_or_warn(
                "enable_password_signup",
                &self.enable_password_signup,
                false,
            ),
            password_signup_need_review: Self::get_or_warn(
                "password_signup_need_review",
                &self.password_signup_need_review,
                false,
            ),
            enable_email_signup: Self::get_or_warn(
                "enable_email_signup",
                &self.enable_email_signup,
                false,
            ),
            email_signup_need_review: Self::get_or_warn(
                "email_signup_need_review",
                &self.email_signup_need_review,
                false,
            ),
            enable_webauthn_signup: Self::get_or_warn(
                "enable_webauthn_signup",
                &self.enable_webauthn_signup,
                false,
            ),
            webauthn_signup_need_review: Self::get_or_warn(
                "webauthn_signup_need_review",
                &self.webauthn_signup_need_review,
                false,
            ),
            enable_guest: Self::get_or_warn("enable_guest", &self.enable_guest, true),
            movie_proxy: Self::get_or_warn("movie_proxy", &self.movie_proxy, true),
            live_proxy: Self::get_or_warn("live_proxy", &self.live_proxy, true),
            ts_disguised_as_png: Self::get_or_warn(
                "ts_disguised_as_png",
                &self.ts_disguised_as_png,
                false,
            ),
            custom_publish_host: self.custom_publish_host.get().unwrap_or_else(|e| {
                tracing::warn!(
                    setting = "custom_publish_host",
                    error = %e,
                    "Failed to read setting for public settings, using default"
                );
                String::new()
            }),
            email_whitelist_enabled: Self::get_or_warn(
                "email_whitelist_enabled",
                &self.email_whitelist_enabled,
                false,
            ),
        }
    }

    /// Helper to get a setting value with a warning log on failure.
    fn get_or_warn<T>(name: &str, setting: &Setting<T>, default: T) -> T
    where
        T: Clone + std::fmt::Display + std::str::FromStr + Send + Sync + 'static,
        <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
    {
        setting.get().unwrap_or_else(|e| {
            tracing::warn!(
                setting = name,
                error = %e,
                "Failed to read setting for public settings, using default"
            );
            default
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ice_server_list_display() {
        let list = IceServerList(vec![
            ConfiguredIceServer::new(vec!["stun:example.com:19302".to_string()]),
            ConfiguredIceServer::new(vec!["turn:turn.example.com:3478".to_string()])
                .with_auth("alice", "secret"),
        ]);
        let json = list.to_string();
        let parsed: Vec<ConfiguredIceServer> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, list.0);
    }

    #[test]
    fn test_ice_server_list_from_str_empty_string() {
        let list: IceServerList = "".parse().unwrap();
        assert!(list.0.is_empty());
    }

    #[test]
    fn test_ice_server_list_from_str_valid_json() {
        let json = r#"[{"urls":["stun:a.com:19302"]},{"urls":["turn:b.com:3478"],"username":"bob","credential":"secret"}]"#;
        let list: IceServerList = json.parse().unwrap();
        assert_eq!(list.0.len(), 2);
        assert_eq!(list.0[0].urls, vec!["stun:a.com:19302"]);
        assert_eq!(list.0[1].username.as_deref(), Some("bob"));
        assert_eq!(list.0[1].credential.as_deref(), Some("secret"));
    }

    #[test]
    fn test_ice_server_list_from_str_invalid_json() {
        let result = "not json".parse::<IceServerList>();
        assert!(result.is_err());
    }

    #[test]
    fn test_ice_server_list_roundtrip() {
        let original = IceServerList(vec![
            ConfiguredIceServer::new(vec!["stun:a.com:19302".to_string()]),
            ConfiguredIceServer::new(vec!["turn:b.com:3478".to_string()])
                .with_auth("bob", "secret"),
        ]);
        let serialized = original.to_string();
        let deserialized: IceServerList = serialized.parse().unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_public_settings_serialization_roundtrip() {
        // Use non-empty custom_publish_host so skip_serializing_if doesn't omit it
        let mut settings = PublicSettings::defaults();
        settings.custom_publish_host = "rtmp://live.example.com".to_string();
        let json = serde_json::to_string(&settings).unwrap();
        let deserialized: PublicSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.enable_password_signup,
            settings.enable_password_signup
        );
        assert_eq!(deserialized.max_rooms_per_user, settings.max_rooms_per_user);
        assert_eq!(deserialized.room_ttl, settings.room_ttl);
        assert_eq!(deserialized.custom_publish_host, "rtmp://live.example.com");
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
        let allowed: PermissionSet = r#"["view_member_list","view_chat_history","use_webrtc"]"#
            .parse()
            .unwrap();
        assert!(allowed.validate_guest_default().is_ok());

        let empty: PermissionSet = "[]".parse().unwrap();
        assert!(empty.validate_guest_default().is_ok());

        let rejected: PermissionSet =
            r#"["view_media_resources","send_chat","create_media_resource"]"#
                .parse()
                .unwrap();
        let error = rejected
            .validate_guest_default()
            .expect_err("media-resource and chat permissions must not be guest defaults");
        assert!(error.to_string().contains("permissions.guest_default"));
        assert!(error.to_string().contains("view_media_resources"));
        assert!(error.to_string().contains("send_chat"));
        assert!(error.to_string().contains("create_media_resource"));
    }

    #[test]
    fn test_permission_set_uses_live_control_name_only() {
        let parsed: PermissionSet = r#"["live_control"]"#.parse().unwrap();
        assert!(parsed.bits().has(PermissionBits::LIVE_CONTROL));
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

    #[tokio::test]
    async fn test_settings_registry_rejects_unsafe_guest_default_permissions() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://fake:fake@localhost/fake")
            .unwrap();
        let service = Arc::new(SettingsService::new(
            crate::repository::SettingsRepository::new(pool.clone()),
            pool,
        ));
        let registry = SettingsRegistry::new(service);

        let invalid = r#"["view_media_resources","send_chat"]"#;
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
    fn test_public_settings_skips_empty_custom_publish_host() {
        let defaults = PublicSettings::defaults();
        let json = serde_json::to_string(&defaults).unwrap();
        // custom_publish_host is empty, should be omitted via skip_serializing_if
        assert!(!json.contains("custom_publish_host"));
    }

    #[test]
    fn test_public_settings_includes_nonempty_custom_publish_host() {
        let mut settings = PublicSettings::defaults();
        settings.custom_publish_host = "rtmp://live.example.com".to_string();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("custom_publish_host"));
        assert!(json.contains("rtmp://live.example.com"));
    }

    #[test]
    fn test_ice_server_list_from_str_empty_array() {
        let list: IceServerList = "[]".parse().unwrap();
        assert!(list.0.is_empty());
    }

    #[test]
    fn test_cors_allowed_origins_display_with_origins() {
        let origins = CorsAllowedOrigins(vec![
            "https://example.com".to_string(),
            "https://app.example.com".to_string(),
        ]);
        let json = origins.to_string();
        let parsed: Vec<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], "https://example.com");
        assert_eq!(parsed[1], "https://app.example.com");
    }

    #[test]
    fn test_cors_allowed_origins_from_str_empty_string() {
        let origins: CorsAllowedOrigins = "".parse().unwrap();
        assert!(origins.0.is_empty());
    }

    #[test]
    fn test_cors_allowed_origins_from_str_valid_json() {
        let json = r#"["https://example.com","https://app.example.com"]"#;
        let origins: CorsAllowedOrigins = json.parse().unwrap();
        assert_eq!(origins.0.len(), 2);
        assert_eq!(origins.0[0], "https://example.com");
        assert_eq!(origins.0[1], "https://app.example.com");
    }

    #[test]
    fn test_cors_allowed_origins_from_str_invalid_json() {
        let result = "not valid json".parse::<CorsAllowedOrigins>();
        assert!(result.is_err());
    }

    #[test]
    fn test_cors_allowed_origins_roundtrip() {
        let original = CorsAllowedOrigins(vec![
            "https://a.com".to_string(),
            "https://b.com".to_string(),
            "https://c.com".to_string(),
        ]);
        let serialized = original.to_string();
        let deserialized: CorsAllowedOrigins = serialized.parse().unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_cors_allowed_origins_from_str_empty_array() {
        let origins: CorsAllowedOrigins = "[]".parse().unwrap();
        assert!(origins.0.is_empty());
    }
}
