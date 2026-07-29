use crate::models::{RoomAdminPermissionBits, RoomPermissionSet};
use crate::service::email::{EmailConfig, SmtpCredentials, SmtpProxyConfig};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OptionalRuntimeConfig<T>(pub Option<T>);

impl<T> Default for OptionalRuntimeConfig<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T: Serialize> fmt::Display for OptionalRuntimeConfig<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let json = serde_json::to_string(&self.0).map_err(|_| fmt::Error)?;
        f.write_str(&json)
    }
}

impl<T: DeserializeOwned> std::str::FromStr for OptionalRuntimeConfig<T> {
    type Err = serde_json::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(value).map(Self)
    }
}

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

    #[must_use]
    pub const fn from_bits(bits: RoomPermissionSet) -> Self {
        Self(bits)
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
    (
        "send_chat_messages",
        RoomAdminPermissionBits::SEND_CHAT_MESSAGES,
    ),
    (
        "manage_own_media",
        RoomAdminPermissionBits::MANAGE_OWN_MEDIA,
    ),
    ("browse_library", RoomAdminPermissionBits::BROWSE_LIBRARY),
    ("view_members", RoomAdminPermissionBits::VIEW_MEMBERS),
    (
        "view_chat_history",
        RoomAdminPermissionBits::VIEW_CHAT_HISTORY,
    ),
    ("use_voice_chat", RoomAdminPermissionBits::USE_VOICE_CHAT),
    ("use_p2p_media", RoomAdminPermissionBits::USE_P2P_MEDIA),
    ("delete_media", RoomAdminPermissionBits::DELETE_MEDIA),
    ("reorder_media", RoomAdminPermissionBits::REORDER_MEDIA),
    ("clear_media", RoomAdminPermissionBits::CLEAR_MEDIA),
    (
        "manage_live_streams",
        RoomAdminPermissionBits::MANAGE_LIVE_STREAMS,
    ),
    (
        "control_playback_state",
        RoomAdminPermissionBits::CONTROL_PLAYBACK_STATE,
    ),
    (
        "navigate_playback",
        RoomAdminPermissionBits::NAVIGATE_PLAYBACK,
    ),
    (
        "review_join_requests",
        RoomAdminPermissionBits::REVIEW_JOIN_REQUESTS,
    ),
    ("remove_members", RoomAdminPermissionBits::REMOVE_MEMBERS),
    (
        "manage_member_permissions",
        RoomAdminPermissionBits::MANAGE_MEMBER_PERMISSIONS,
    ),
    ("add_members", RoomAdminPermissionBits::ADD_MEMBERS),
    (
        "manage_room_settings",
        RoomAdminPermissionBits::MANAGE_ROOM_SETTINGS,
    ),
    (
        "delete_chat_messages",
        RoomAdminPermissionBits::DELETE_CHAT_MESSAGES,
    ),
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
            let Some((_, bit)) = NAMED_PERMISSIONS
                .iter()
                .find(|(permission_name, _)| *permission_name == name)
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

#[cfg(test)]
mod tests {
    use super::{validate_webrtc_settings, IceServerList, PermissionSet, WebRtcRuntimeSettings};
    use crate::models::RoomAdminPermissionBits;
    use std::str::FromStr;

    #[test]
    fn permission_set_accepts_exact_canonical_names() {
        let permissions = PermissionSet::from_str(r#"["manage_live_streams","view_members"]"#)
            .expect("canonical permission names should parse");

        assert_eq!(
            permissions.bits().0,
            RoomAdminPermissionBits::MANAGE_LIVE_STREAMS | RoomAdminPermissionBits::VIEW_MEMBERS
        );
    }

    #[test]
    fn voice_participant_limit_accepts_mesh_operating_range() {
        for max_voice_participants_per_room in [2, 8, 32] {
            assert!(validate_webrtc_settings(&WebRtcRuntimeSettings {
                external_ice_servers: IceServerList::new(),
                max_voice_participants_per_room,
            })
            .is_ok());
        }
        for max_voice_participants_per_room in [1, 33] {
            assert!(validate_webrtc_settings(&WebRtcRuntimeSettings {
                external_ice_servers: IceServerList::new(),
                max_voice_participants_per_room,
            })
            .is_err());
        }
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
                "room_creation.password_policy must be one of: optional, required, forbidden; got '{value}'"
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
        Self(Vec::new())
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
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuth2SignupPolicy {
    pub enable_signup: bool,
    pub signup_need_review: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct OAuth2ProviderConfig {
    pub enable_signup: bool,
    pub signup_need_review: bool,
    #[serde(flatten)]
    pub config: OAuth2ProviderPrivateConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum OAuth2ProviderPrivateConfig {
    #[serde(rename = "github")]
    GitHub(OAuth2GithubProviderConfig),
    #[serde(rename = "google")]
    Google(OAuth2GoogleProviderConfig),
    #[serde(rename = "logto")]
    Logto(OAuth2LogtoProviderConfig),
    #[serde(rename = "casdoor")]
    Casdoor(OAuth2CasdoorProviderConfig),
    #[serde(rename = "oidc")]
    Oidc(OAuth2OidcProviderConfig),
    #[serde(rename = "apple")]
    Apple(OAuth2AppleProviderConfig),
}

impl Default for OAuth2ProviderPrivateConfig {
    fn default() -> Self {
        Self::GitHub(OAuth2GithubProviderConfig::default())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuth2GithubProviderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuth2GoogleProviderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuth2LogtoProviderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuth2OidcProviderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    #[serde(default)]
    pub issuer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub userinfo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuth2CasdoorProviderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub issuer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub userinfo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuth2AppleProviderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
}

impl OAuth2ProviderPrivateConfig {
    #[must_use]
    pub const fn provider_type_name(&self) -> &'static str {
        match self {
            Self::GitHub(_) => "github",
            Self::Google(_) => "google",
            Self::Logto(_) => "logto",
            Self::Casdoor(_) => "casdoor",
            Self::Oidc(_) => "oidc",
            Self::Apple(_) => "apple",
        }
    }
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
    pub fn provider_type_name(&self) -> &'static str {
        self.config.provider_type_name()
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

    pub fn validate(
        &self,
        ctx: &crate::models::SettingsValidationContext<'_>,
    ) -> crate::Result<()> {
        for (instance_name, provider_config) in &self.0 {
            validate_oauth2_instance_name(instance_name)?;
            let provider_type = provider_config.provider_type_name();
            if crate::models::oauth2_client::OAuth2Provider::from_str_name(provider_type).is_none()
            {
                return Err(crate::Error::InvalidInput(format!(
                    "OAuth2 provider '{instance_name}' uses unsupported type '{provider_type}'"
                )));
            }
            crate::oauth2::providers::provider_registry(ctx.ssrf_guard.clone())
                .create_provider(provider_type, &provider_config.config)
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

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeSettings {
    pub server: ServerRuntimeSettings,
    pub room_defaults: RoomDefaultsRuntimeSettings,
    pub permissions: PermissionRuntimeSettings,
    pub room_creation: RoomCreationRuntimeSettings,
    pub user: UserRuntimeSettings,
    pub oauth2: OAuth2RuntimeSettings,
    pub proxy: ProxyRuntimeSettings,
    pub rtmp: RtmpRuntimeSettings,
    pub email: EmailRuntimeSettings,
    pub webrtc: WebRtcRuntimeSettings,
    pub chat: ChatRuntimeSettings,
    pub playback_history: PlaybackHistoryRuntimeSettings,
    pub cors: CorsRuntimeSettings,
}

impl RuntimeSettings {
    pub fn validate(
        &self,
        ctx: &crate::models::SettingsValidationContext<'_>,
    ) -> crate::Result<()> {
        validate_all_runtime_settings(self, ctx)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeSettingsUpdateMask {
    pub server: ServerRuntimeSettingsUpdateMask,
    pub room_defaults: RoomDefaultsRuntimeSettingsUpdateMask,
    pub permissions: PermissionRuntimeSettingsUpdateMask,
    pub room_creation: RoomCreationRuntimeSettingsUpdateMask,
    pub user: UserRuntimeSettingsUpdateMask,
    pub oauth2: OAuth2RuntimeSettingsUpdateMask,
    pub proxy: ProxyRuntimeSettingsUpdateMask,
    pub rtmp: RtmpRuntimeSettingsUpdateMask,
    pub email: EmailRuntimeSettingsUpdateMask,
    pub webrtc: WebRtcRuntimeSettingsUpdateMask,
    pub chat: ChatRuntimeSettingsUpdateMask,
    pub playback_history: PlaybackHistoryRuntimeSettingsUpdateMask,
    pub cors: CorsRuntimeSettingsUpdateMask,
}

impl RuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            server: ServerRuntimeSettingsUpdateMask::all(),
            room_defaults: RoomDefaultsRuntimeSettingsUpdateMask::all(),
            permissions: PermissionRuntimeSettingsUpdateMask::all(),
            room_creation: RoomCreationRuntimeSettingsUpdateMask::all(),
            user: UserRuntimeSettingsUpdateMask::all(),
            oauth2: OAuth2RuntimeSettingsUpdateMask::all(),
            proxy: ProxyRuntimeSettingsUpdateMask::all(),
            rtmp: RtmpRuntimeSettingsUpdateMask::all(),
            email: EmailRuntimeSettingsUpdateMask::all(),
            webrtc: WebRtcRuntimeSettingsUpdateMask::all(),
            chat: ChatRuntimeSettingsUpdateMask::all(),
            playback_history: PlaybackHistoryRuntimeSettingsUpdateMask::all(),
            cors: CorsRuntimeSettingsUpdateMask::all(),
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.server.is_empty()
            && self.room_defaults.is_empty()
            && self.permissions.is_empty()
            && self.room_creation.is_empty()
            && self.user.is_empty()
            && self.oauth2.is_empty()
            && self.proxy.is_empty()
            && self.rtmp.is_empty()
            && self.email.is_empty()
            && self.webrtc.is_empty()
            && self.chat.is_empty()
            && self.playback_history.is_empty()
            && self.cors.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerRuntimeSettingsUpdateMask {
    pub name: bool,
}

impl ServerRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self { name: true }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.name
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoomDefaultsRuntimeSettingsUpdateMask {
    pub default_max_members: bool,
    pub default_max_chat_messages: bool,
}

impl RoomDefaultsRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            default_max_members: true,
            default_max_chat_messages: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.default_max_members && !self.default_max_chat_messages
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PermissionRuntimeSettingsUpdateMask {
    pub admin_default_permissions: bool,
    pub member_default_permissions: bool,
    pub guest_default_permissions: bool,
}

impl PermissionRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            admin_default_permissions: true,
            member_default_permissions: true,
            guest_default_permissions: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.admin_default_permissions
            && !self.member_default_permissions
            && !self.guest_default_permissions
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoomCreationRuntimeSettingsUpdateMask {
    pub enabled: bool,
    pub approval_required: bool,
    pub password_policy: bool,
    pub max_rooms_per_user: bool,
}

impl RoomCreationRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            enabled: true,
            approval_required: true,
            password_policy: true,
            max_rooms_per_user: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.enabled
            && !self.approval_required
            && !self.password_policy
            && !self.max_rooms_per_user
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserRuntimeSettingsUpdateMask {
    pub enable_password_signup: bool,
    pub password_signup_need_review: bool,
    pub enable_email_signup: bool,
    pub email_signup_need_review: bool,
    pub enable_webauthn_signup: bool,
    pub webauthn_signup_need_review: bool,
    pub enable_guest: bool,
}

impl UserRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            enable_password_signup: true,
            password_signup_need_review: true,
            enable_email_signup: true,
            email_signup_need_review: true,
            enable_webauthn_signup: true,
            webauthn_signup_need_review: true,
            enable_guest: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.enable_password_signup
            && !self.password_signup_need_review
            && !self.enable_email_signup
            && !self.email_signup_need_review
            && !self.enable_webauthn_signup
            && !self.webauthn_signup_need_review
            && !self.enable_guest
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OAuth2RuntimeSettingsUpdateMask {
    pub providers: bool,
}

impl OAuth2RuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self { providers: true }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.providers
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProxyRuntimeSettingsUpdateMask {
    pub movie_proxy: bool,
    pub live_proxy: bool,
}

impl ProxyRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            movie_proxy: true,
            live_proxy: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.movie_proxy && !self.live_proxy
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RtmpRuntimeSettingsUpdateMask {
    pub custom_publish_host: bool,
    pub ts_disguised_as_png: bool,
}

impl RtmpRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            custom_publish_host: true,
            ts_disguised_as_png: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.custom_publish_host && !self.ts_disguised_as_png
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmailRuntimeSettingsUpdateMask {
    pub enabled: bool,
    pub smtp_host: bool,
    pub smtp_port: bool,
    pub smtp_credentials: bool,
    pub smtp_proxy: bool,
    pub use_tls: bool,
    pub from_email: bool,
    pub from_name: bool,
    pub whitelist_enabled: bool,
    pub whitelist_domains: bool,
}

impl EmailRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            enabled: true,
            smtp_host: true,
            smtp_port: true,
            smtp_credentials: true,
            smtp_proxy: true,
            use_tls: true,
            from_email: true,
            from_name: true,
            whitelist_enabled: true,
            whitelist_domains: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.enabled
            && !self.smtp_host
            && !self.smtp_port
            && !self.smtp_credentials
            && !self.smtp_proxy
            && !self.use_tls
            && !self.from_email
            && !self.from_name
            && !self.whitelist_enabled
            && !self.whitelist_domains
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WebRtcRuntimeSettingsUpdateMask {
    pub external_ice_servers: bool,
    pub max_voice_participants_per_room: bool,
}

impl WebRtcRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            external_ice_servers: true,
            max_voice_participants_per_room: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.external_ice_servers && !self.max_voice_participants_per_room
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChatRuntimeSettingsUpdateMask {
    pub max_messages_per_room: bool,
    pub max_pinned_messages_per_room: bool,
    pub message_retention_days: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaybackHistoryRuntimeSettingsUpdateMask {
    pub retention_days: bool,
    pub max_entries_per_room: bool,
}

impl PlaybackHistoryRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            retention_days: true,
            max_entries_per_room: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.retention_days && !self.max_entries_per_room
    }
}

impl ChatRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            max_messages_per_room: true,
            max_pinned_messages_per_room: true,
            message_retention_days: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.max_messages_per_room
            && !self.max_pinned_messages_per_room
            && !self.message_retention_days
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CorsRuntimeSettingsUpdateMask {
    pub allowed_origins: bool,
}

impl CorsRuntimeSettingsUpdateMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            allowed_origins: true,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.allowed_origins
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRuntimeSettings {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomDefaultsRuntimeSettings {
    pub default_max_members: i64,
    pub default_max_chat_messages: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRuntimeSettings {
    pub admin_default_permissions: PermissionSet,
    pub member_default_permissions: PermissionSet,
    pub guest_default_permissions: PermissionSet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomCreationRuntimeSettings {
    pub enabled: bool,
    pub approval_required: bool,
    pub password_policy: RoomPasswordPolicy,
    pub max_rooms_per_user: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRuntimeSettings {
    pub enable_password_signup: bool,
    pub password_signup_need_review: bool,
    pub enable_email_signup: bool,
    pub email_signup_need_review: bool,
    pub enable_webauthn_signup: bool,
    pub webauthn_signup_need_review: bool,
    pub enable_guest: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OAuth2RuntimeSettings {
    pub providers: OAuth2ProviderConfigs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyRuntimeSettings {
    pub movie_proxy: bool,
    pub live_proxy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtmpRuntimeSettings {
    pub custom_publish_host: Option<String>,
    pub ts_disguised_as_png: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailRuntimeSettings {
    pub enabled: bool,
    pub smtp_host: Option<String>,
    pub smtp_port: u16,
    pub smtp_credentials: Option<SmtpCredentials>,
    pub smtp_proxy: Option<SmtpProxyConfig>,
    pub use_tls: bool,
    pub from_email: Option<String>,
    pub from_name: String,
    pub whitelist_enabled: bool,
    pub whitelist_domains: Vec<String>,
}

impl EmailRuntimeSettings {
    #[must_use]
    pub fn whitelist_raw(&self) -> String {
        self.whitelist_domains.join(",")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebRtcRuntimeSettings {
    pub external_ice_servers: IceServerList,
    pub max_voice_participants_per_room: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRuntimeSettings {
    pub max_messages_per_room: u64,
    pub max_pinned_messages_per_room: u64,
    pub message_retention_days: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackHistoryRuntimeSettings {
    pub retention_days: u32,
    pub max_entries_per_room: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorsRuntimeSettings {
    pub allowed_origins: CorsAllowedOrigins,
}

fn validate_all_runtime_settings(
    settings: &RuntimeSettings,
    ctx: &crate::models::SettingsValidationContext<'_>,
) -> crate::Result<()> {
    validate_server_name(&settings.server.name)?;
    validate_room_defaults_settings(&settings.room_defaults)?;
    validate_permission_settings(&settings.permissions)?;
    validate_room_policy_settings(&settings.room_creation)?;
    validate_user_settings(&settings.user, &settings.email)?;
    settings.oauth2.providers.validate(ctx)?;
    validate_proxy_settings(&settings.proxy);
    validate_rtmp_settings(&settings.rtmp)?;
    validate_email_settings(&settings.email)?;
    validate_webrtc_settings(&settings.webrtc)?;
    validate_chat_settings(&settings.chat)?;
    validate_playback_history_settings(&settings.playback_history)?;
    validate_cors_settings(&settings.cors)
}

pub(super) fn validate_server_name(name: &str) -> crate::Result<()> {
    if name.trim() != name
        || name.is_empty()
        || name.chars().count() > 128
        || name.chars().any(char::is_control)
    {
        return Err(crate::Error::InvalidInput(
            "server.name must be 1 to 128 characters without surrounding whitespace or control characters"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_room_defaults_settings(settings: &RoomDefaultsRuntimeSettings) -> crate::Result<()> {
    if settings.default_max_members <= 0 {
        return Err(crate::Error::InvalidInput(
            "room_defaults.default_max_members must be greater than 0".to_string(),
        ));
    }
    if settings.default_max_chat_messages > 10_000 {
        return Err(crate::Error::InvalidInput(
            "room_defaults.default_max_chat_messages must be at most 10000".to_string(),
        ));
    }
    Ok(())
}

fn validate_permission_settings(settings: &PermissionRuntimeSettings) -> crate::Result<()> {
    let _ = &settings.admin_default_permissions;
    let _ = &settings.member_default_permissions;
    settings.guest_default_permissions.validate_guest_default()
}

fn validate_room_policy_settings(settings: &RoomCreationRuntimeSettings) -> crate::Result<()> {
    let _ = settings.enabled;
    let _ = settings.approval_required;
    let _ = settings.password_policy;
    if !(1..=1000).contains(&settings.max_rooms_per_user) {
        return Err(crate::Error::InvalidInput(
            "room_creation.max_rooms_per_user must be between 1 and 1000".to_string(),
        ));
    }
    Ok(())
}

fn validate_user_settings(
    settings: &UserRuntimeSettings,
    email: &EmailRuntimeSettings,
) -> crate::Result<()> {
    let _ = settings.enable_password_signup;
    let _ = settings.password_signup_need_review;
    if settings.enable_email_signup && !email.enabled {
        return Err(crate::Error::InvalidInput(
            "user.enable_email_signup requires email.enabled".to_string(),
        ));
    }
    let _ = settings.email_signup_need_review;
    let _ = settings.enable_webauthn_signup;
    let _ = settings.webauthn_signup_need_review;
    let _ = settings.enable_guest;
    Ok(())
}

fn validate_proxy_settings(settings: &ProxyRuntimeSettings) {
    let _ = settings.movie_proxy;
    let _ = settings.live_proxy;
}

fn validate_rtmp_settings(settings: &RtmpRuntimeSettings) -> crate::Result<()> {
    if settings
        .custom_publish_host
        .as_ref()
        .is_some_and(|host| host.trim().is_empty())
    {
        return Err(crate::Error::InvalidInput(
            "rtmp.custom_publish_host must be non-empty when configured".to_string(),
        ));
    }
    let _ = settings.ts_disguised_as_png;
    Ok(())
}

fn validate_email_settings(settings: &EmailRuntimeSettings) -> crate::Result<()> {
    validate_enabled_email_config(settings)?;
    let _ = settings.whitelist_enabled;
    for domain in &settings.whitelist_domains {
        validate_email_domain(domain)?;
    }
    Ok(())
}

fn validate_enabled_email_config(settings: &EmailRuntimeSettings) -> crate::Result<()> {
    if !settings.enabled {
        return Ok(());
    }
    let smtp_host = settings.smtp_host.as_deref().ok_or_else(|| {
        crate::Error::InvalidInput(
            "email.smtp_host is required when email.enabled is true".to_string(),
        )
    })?;
    let from_email = settings.from_email.as_deref().ok_or_else(|| {
        crate::Error::InvalidInput(
            "email.from_email is required when email.enabled is true".to_string(),
        )
    })?;
    EmailConfig {
        smtp_host: smtp_host.trim().to_string(),
        smtp_port: settings.smtp_port,
        smtp_credentials: settings.smtp_credentials.clone(),
        smtp_proxy: settings.smtp_proxy.clone(),
        from_email: from_email.trim().to_string(),
        from_name: settings.from_name.trim().to_string(),
        use_tls: settings.use_tls,
    }
    .validate()
}

fn validate_webrtc_settings(settings: &WebRtcRuntimeSettings) -> crate::Result<()> {
    if !(2..=32).contains(&settings.max_voice_participants_per_room) {
        return Err(crate::Error::InvalidInput(
            "webrtc.max_voice_participants_per_room must be between 2 and 32".to_string(),
        ));
    }
    for room_defaults in &settings.external_ice_servers.0 {
        if room_defaults.urls.is_empty() {
            return Err(crate::Error::InvalidInput(
                "webrtc.external_ice_servers entries must include at least one URL".to_string(),
            ));
        }
        for url in &room_defaults.urls {
            if !(url.starts_with("stun:")
                || url.starts_with("stuns:")
                || url.starts_with("turn:")
                || url.starts_with("turns:"))
            {
                return Err(crate::Error::InvalidInput(format!(
                    "webrtc.external_ice_servers URL must use stun, stuns, turn, or turns scheme: {url}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_chat_settings(settings: &ChatRuntimeSettings) -> crate::Result<()> {
    if settings.max_messages_per_room > 100_000 {
        return Err(crate::Error::InvalidInput(
            "chat.max_messages_per_room must be <= 100000".to_string(),
        ));
    }
    if settings.max_pinned_messages_per_room > 1_000 {
        return Err(crate::Error::InvalidInput(
            "chat.max_pinned_messages_per_room must be <= 1000".to_string(),
        ));
    }
    if !(1..=3650).contains(&settings.message_retention_days) {
        return Err(crate::Error::InvalidInput(
            "chat.message_retention_days must be between 1 and 3650".to_string(),
        ));
    }
    Ok(())
}

fn validate_playback_history_settings(
    settings: &PlaybackHistoryRuntimeSettings,
) -> crate::Result<()> {
    if settings.retention_days > 3_650 {
        return Err(crate::Error::InvalidInput(
            "playback_history.retention_days must be between 0 and 3650".to_string(),
        ));
    }
    if !(0..=100_000).contains(&settings.max_entries_per_room) {
        return Err(crate::Error::InvalidInput(
            "playback_history.max_entries_per_room must be between 0 and 100000".to_string(),
        ));
    }
    Ok(())
}

fn validate_cors_settings(settings: &CorsRuntimeSettings) -> crate::Result<()> {
    for origin in &settings.allowed_origins.0 {
        if origin == "*" {
            continue;
        }
        if !(origin.starts_with("http://") || origin.starts_with("https://")) {
            return Err(crate::Error::InvalidInput(format!(
                "cors.allowed_origins must contain HTTP(S) origins or '*': {origin}"
            )));
        }
    }
    Ok(())
}

fn validate_email_domain(entry: &str) -> crate::Result<()> {
    let domain = entry.trim().trim_start_matches('@');
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
            "email.whitelist_domains must contain email domains only".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicSettings {
    pub server_name: String,
    pub room_creation_enabled: bool,
    pub max_rooms_per_user: i64,
    pub default_max_members: i64,
    pub max_pinned_chat_messages_per_room: u64,
    pub approval_required: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_publish_host: Option<String>,
    pub email_whitelist_enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub email_whitelist_domains: Vec<String>,
}

impl PublicSettings {
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            server_name: "SyncTV".to_string(),
            room_creation_enabled: true,
            max_rooms_per_user: 10,
            default_max_members: 100,
            max_pinned_chat_messages_per_room: 20,
            approval_required: false,
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
            custom_publish_host: None,
            email_whitelist_enabled: false,
            email_whitelist_domains: Vec::new(),
        }
    }
}
