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
//! if registry.signup_enabled.get().unwrap() {
//!     // Signup is enabled
//! }
//!
//! // Write - auto-converts to string and persists
//! registry.signup_enabled.set(false).await?;
//!
//! // Validate user input via storage
//! if registry.storage.validate("server.signup_enabled", "true") {
//!     // Value is valid
//! }
//! ```

use crate::models::room_settings::MaxMembers;
use crate::service::{
    settings_vars::{Setting, SettingsStorage},
    SettingsService,
};
use crate::setting;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Maximum allowed value for `max_chat_messages` setting (0 = unlimited)
const MAX_CHAT_MESSAGES_LIMIT: u64 = 10_000;

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
    pub fn with_auth(
        mut self,
        username: impl Into<String>,
        credential: impl Into<String>,
    ) -> Self {
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

/// A snapshot of all client-visible settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicSettings {
    pub signup_enabled: bool,
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
    pub signup_need_review: bool,
    pub enable_password_signup: bool,
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
            signup_enabled: true,
            allow_room_creation: true,
            max_rooms_per_user: 10,
            max_members_per_room: 100,
            disable_create_room: false,
            create_room_need_review: false,
            room_ttl: 172_800,
            room_must_need_pwd: false,
            room_must_no_need_pwd: false,
            signup_need_review: false,
            enable_password_signup: true,
            enable_guest: true,
            movie_proxy: true,
            live_proxy: true,
            ts_disguised_as_png: true,
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
    pub signup_enabled: Setting<bool>,
    pub allow_room_creation: Setting<bool>,
    pub max_rooms_per_user: Setting<i64>,
    pub max_members_per_room: Setting<i64>,
    pub max_chat_messages: Setting<u64>,

    // Permission settings - global defaults for each role
    pub admin_default_permissions: Setting<u64>,
    pub member_default_permissions: Setting<u64>,
    pub guest_default_permissions: Setting<u64>,

    // Room settings
    pub disable_create_room: Setting<bool>,
    pub create_room_need_review: Setting<bool>,
    pub room_ttl: Setting<i64>,
    pub room_must_need_pwd: Setting<bool>,
    pub room_must_no_need_pwd: Setting<bool>,

    // User settings
    pub signup_need_review: Setting<bool>,
    pub enable_password_signup: Setting<bool>,
    pub password_signup_need_review: Setting<bool>,
    pub enable_guest: Setting<bool>,

    // Proxy settings
    pub movie_proxy: Setting<bool>,
    pub live_proxy: Setting<bool>,
    pub allow_proxy_to_local: Setting<bool>,
    pub proxy_cache_enable: Setting<bool>,

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
        let storage = Arc::new(SettingsStorage::new(settings_service));

        Self {
            storage: storage.clone(),

            // Server settings using the setting! macro
            // Each setting auto-registers its provider to storage
            signup_enabled: setting!(bool, "server.signup_enabled", storage.clone(), true),
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

            // Permission settings - global defaults for each role
            // These are base permissions that rooms can override with added/removed permissions
            // Admin default: All permissions except System::ADMIN (1073741823 = 0x3FFFFFFF)
            admin_default_permissions: setting!(
                u64,
                "permissions.admin_default",
                storage.clone(),
                1_073_741_823
            ),
            // Member default: Basic member permissions (262143 = 0x3FFFF)
            member_default_permissions: setting!(
                u64,
                "permissions.member_default",
                storage.clone(),
                262_143
            ),
            // Guest default: Read-only permissions (511 = 0x1FF)
            guest_default_permissions: setting!(
                u64,
                "permissions.guest_default",
                storage.clone(),
                511
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
            signup_need_review: setting!(bool, "user.signup_need_review", storage.clone(), false),
            enable_password_signup: setting!(
                bool,
                "user.enable_password_signup",
                storage.clone(),
                true
            ),
            password_signup_need_review: setting!(
                bool,
                "user.password_signup_need_review",
                storage.clone(),
                false
            ),
            enable_guest: setting!(bool, "user.enable_guest", storage.clone(), true),

            // Proxy settings
            movie_proxy: setting!(bool, "proxy.movie_proxy", storage.clone(), true),
            live_proxy: setting!(bool, "proxy.live_proxy", storage.clone(), true),
            allow_proxy_to_local: setting!(
                bool,
                "proxy.allow_proxy_to_local",
                storage.clone(),
                false
            ),
            proxy_cache_enable: setting!(bool, "proxy.proxy_cache_enable", storage.clone(), false),

            // RTMP settings
            custom_publish_host: setting!(
                String,
                "rtmp.custom_publish_host",
                storage.clone(),
                String::new()
            ),
            ts_disguised_as_png: setting!(bool, "rtmp.ts_disguised_as_png", storage.clone(), true),

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
            signup_enabled: Self::get_or_warn("signup_enabled", &self.signup_enabled, true),
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
            signup_need_review: Self::get_or_warn(
                "signup_need_review",
                &self.signup_need_review,
                false,
            ),
            enable_password_signup: Self::get_or_warn(
                "enable_password_signup",
                &self.enable_password_signup,
                true,
            ),
            enable_guest: Self::get_or_warn("enable_guest", &self.enable_guest, true),
            movie_proxy: Self::get_or_warn("movie_proxy", &self.movie_proxy, true),
            live_proxy: Self::get_or_warn("live_proxy", &self.live_proxy, true),
            ts_disguised_as_png: Self::get_or_warn(
                "ts_disguised_as_png",
                &self.ts_disguised_as_png,
                true,
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
        assert_eq!(deserialized.signup_enabled, settings.signup_enabled);
        assert_eq!(deserialized.max_rooms_per_user, settings.max_rooms_per_user);
        assert_eq!(deserialized.room_ttl, settings.room_ttl);
        assert_eq!(deserialized.custom_publish_host, "rtmp://live.example.com");
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
