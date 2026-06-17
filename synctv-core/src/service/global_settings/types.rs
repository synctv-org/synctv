use crate::models::{RoomAdminPermissionBits, RoomPermissionSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSet(RoomPermissionSet);

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
        Self(RoomPermissionSet::default_admin())
    }

    #[must_use]
    pub const fn member_default() -> Self {
        Self(RoomPermissionSet::default_member())
    }

    #[must_use]
    pub const fn guest_default() -> Self {
        Self(RoomPermissionSet::default_guest())
    }

    #[must_use]
    pub const fn bits(&self) -> RoomPermissionSet {
        self.0
    }

    fn names_for_bits(bits: u64) -> Vec<&'static str> {
        NAMED_PERMISSIONS
            .iter()
            .filter_map(|(name, bit)| ((bits & *bit) != 0).then_some(*name))
            .collect()
    }

    pub fn validate_guest_default(&self) -> crate::Result<()> {
        let invalid = self.bits().0 & !RoomPermissionSet::guest_assignable().0;
        if invalid == 0 {
            return Ok(());
        }

        let invalid_names = Self::names_for_bits(invalid);
        let allowed_names = Self::names_for_bits(RoomPermissionSet::guest_assignable().0);
        Err(crate::Error::InvalidInput(format!(
            "permissions.guest_default may only include guest-safe permissions: {}; invalid permissions: {}",
            allowed_names.join(", "),
            invalid_names.join(", "),
        )))
    }
}

const NAMED_PERMISSIONS: &[(&str, u64)] = &[
    ("chat", RoomAdminPermissionBits::CHAT),
    (
        "create_media_resource",
        RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE,
    ),
    (
        "view_media_resources",
        RoomAdminPermissionBits::VIEW_MEDIA_RESOURCES,
    ),
    (
        "view_member_list",
        RoomAdminPermissionBits::VIEW_MEMBER_LIST,
    ),
    (
        "view_chat_history",
        RoomAdminPermissionBits::VIEW_CHAT_HISTORY,
    ),
    ("use_webrtc", RoomAdminPermissionBits::USE_WEBRTC),
    (
        "delete_media_resource_any",
        RoomAdminPermissionBits::DELETE_MEDIA_RESOURCE_ANY,
    ),
    (
        "reorder_media_resources",
        RoomAdminPermissionBits::REORDER_MEDIA_RESOURCES,
    ),
    (
        "clear_media_resources",
        RoomAdminPermissionBits::CLEAR_MEDIA_RESOURCES,
    ),
    ("live_control", RoomAdminPermissionBits::LIVE_CONTROL),
    ("play_control", RoomAdminPermissionBits::PLAY_CONTROL),
    (
        "change_current_media",
        RoomAdminPermissionBits::CHANGE_CURRENT_MEDIA,
    ),
    (
        "change_playback_rate",
        RoomAdminPermissionBits::CHANGE_PLAYBACK_RATE,
    ),
    ("approve_member", RoomAdminPermissionBits::APPROVE_MEMBER),
    ("kick_member", RoomAdminPermissionBits::KICK_MEMBER),
    (
        "set_member_permissions",
        RoomAdminPermissionBits::SET_MEMBER_PERMISSIONS,
    ),
    ("add_member", RoomAdminPermissionBits::ADD_MEMBER),
    (
        "set_room_settings",
        RoomAdminPermissionBits::SET_ROOM_SETTINGS,
    ),
    ("delete_chat", RoomAdminPermissionBits::DELETE_CHAT),
    ("delete_room", RoomAdminPermissionBits::DELETE_ROOM),
];

impl fmt::Display for PermissionSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = NAMED_PERMISSIONS
            .iter()
            .filter_map(|(name, bit)| ((self.0.bits() & *bit) != 0).then_some(*name))
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

        let mut bits = RoomPermissionSet::empty();
        for name in names {
            let canonical = name.replace('-', "_").to_ascii_lowercase();
            let Some((_, bit)) = NAMED_PERMISSIONS
                .iter()
                .find(|(permission_name, _)| *permission_name == canonical)
            else {
                return Err(PermissionSetParseError(format!(
                    "unknown permission name '{name}'"
                )));
            };
            bits |= *bit;
        }

        Ok(Self(bits))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomPasswordPolicy {
    #[default]
    Optional,
    Required,
    Forbidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomPasswordPolicyParseError(String);

impl fmt::Display for RoomPasswordPolicyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RoomPasswordPolicyParseError {}

impl fmt::Display for RoomPasswordPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Optional => "optional",
            Self::Required => "required",
            Self::Forbidden => "forbidden",
        })
    }
}

impl std::str::FromStr for RoomPasswordPolicy {
    type Err = RoomPasswordPolicyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "optional" => Ok(Self::Optional),
            "required" => Ok(Self::Required),
            "forbidden" => Ok(Self::Forbidden),
            _ => Err(RoomPasswordPolicyParseError(format!(
                "room.password_policy must be one of: optional, required, forbidden; got '{value}'"
            ))),
        }
    }
}

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
        let json = serde_json::to_string(&self.0).map_err(|_| fmt::Error)?;
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CorsAllowedOrigins(pub Vec<String>);

impl CorsAllowedOrigins {
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
        let json = serde_json::to_string(&self.0).map_err(|_| fmt::Error)?;
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct OAuth2SignupPolicy {
    pub enable_signup: bool,
    pub signup_need_review: bool,
}

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
        let json = serde_json::to_string(&self.0).map_err(|_| fmt::Error)?;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicSettings {
    pub allow_room_creation: bool,
    pub max_rooms_per_user: i64,
    pub max_members_per_room: i64,
    pub max_pinned_chat_messages_per_room: u64,
    pub disable_create_room: bool,
    pub create_room_need_review: bool,
    pub room_password_policy: RoomPasswordPolicy,
    pub enable_password_signup: bool,
    pub password_signup_need_review: bool,
    pub enable_email_signup: bool,
    pub email_signup_need_review: bool,
    pub enable_webauthn_signup: bool,
    pub webauthn_signup_need_review: bool,
    pub enable_guest: bool,
    pub enable_email: bool,
    pub enable_webauthn: bool,
    pub movie_proxy: bool,
    pub live_proxy: bool,
    pub ts_disguised_as_png: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub custom_publish_host: String,
    pub email_whitelist_enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub email_whitelist_domains: Vec<String>,
}

impl PublicSettings {
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            allow_room_creation: true,
            max_rooms_per_user: 10,
            max_members_per_room: 100,
            max_pinned_chat_messages_per_room: 20,
            disable_create_room: false,
            create_room_need_review: false,
            room_password_policy: RoomPasswordPolicy::Optional,
            enable_password_signup: false,
            password_signup_need_review: false,
            enable_email_signup: false,
            email_signup_need_review: false,
            enable_webauthn_signup: false,
            webauthn_signup_need_review: false,
            enable_guest: true,
            enable_email: false,
            enable_webauthn: false,
            movie_proxy: true,
            live_proxy: true,
            ts_disguised_as_png: false,
            custom_publish_host: String::new(),
            email_whitelist_enabled: false,
            email_whitelist_domains: Vec::new(),
        }
    }
}
