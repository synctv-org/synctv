use synctv_core::{
    models::{room_settings::RoomSettingsPatch, AuditDetails, PlayMode, UserId},
    provider::ExecutionControl,
    Error as CoreError,
};

use crate::impls::client::convert::room_settings_to_proto;

use super::{AdminApiImpl, ApiError, RequestContext};

#[derive(Debug, Clone, Default)]
pub struct RuntimeSettingsPatch {
    pub server: Option<ServerSettingsPatch>,
    pub room_defaults: Option<RoomDefaultsSettingsPatch>,
    pub permissions: Option<PermissionSettingsPatch>,
    pub room_creation: Option<RoomCreationSettingsPatch>,
    pub user: Option<UserSettingsPatch>,
    pub oauth2: Option<OAuth2SettingsPatch>,
    pub proxy: Option<ProxySettingsPatch>,
    pub rtmp: Option<RtmpSettingsPatch>,
    pub email: Option<EmailSettingsPatch>,
    pub webrtc: Option<WebRtcSettingsPatch>,
    pub chat: Option<ChatSettingsPatch>,
    pub playback_history: Option<PlaybackHistorySettingsPatch>,
    pub cors: Option<CorsSettingsPatch>,
}

#[derive(Debug, Clone, Default)]
pub struct ServerSettingsPatch {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RoomDefaultsSettingsPatch {
    pub default_max_members: Option<i64>,
    pub default_max_chat_messages: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct PermissionSettingsPatch {
    pub admin_default_permissions: Option<u64>,
    pub member_default_permissions: Option<u64>,
    pub guest_default_permissions: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct RoomCreationSettingsPatch {
    pub enabled: Option<bool>,
    pub approval_required: Option<bool>,
    pub password_policy: Option<synctv_core::service::RoomPasswordPolicy>,
    pub max_rooms_per_user: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct UserSettingsPatch {
    pub enable_password_signup: Option<bool>,
    pub password_signup_need_review: Option<bool>,
    pub enable_email_signup: Option<bool>,
    pub email_signup_need_review: Option<bool>,
    pub enable_webauthn_signup: Option<bool>,
    pub webauthn_signup_need_review: Option<bool>,
    pub enable_guest: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct OAuth2SettingsPatch {
    pub providers: Option<synctv_core::service::OAuth2ProviderConfigs>,
    pub allowed_redirect_urls: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct ProxySettingsPatch {
    pub movie_proxy: Option<bool>,
    pub live_proxy: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct RtmpSettingsPatch {
    pub custom_publish_host: Option<OptionalConfigPatch<String>>,
    pub ts_disguised_as_png: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct EmailSettingsPatch {
    pub enabled: Option<bool>,
    pub smtp_host: Option<OptionalConfigPatch<String>>,
    pub smtp_port: Option<u32>,
    pub smtp_credentials: Option<OptionalConfigPatch<SmtpCredentialsInput>>,
    pub smtp_proxy: Option<OptionalConfigPatch<SmtpProxyInput>>,
    pub use_tls: Option<bool>,
    pub from_email: Option<OptionalConfigPatch<String>>,
    pub from_name: Option<String>,
    pub whitelist_enabled: Option<bool>,
    pub whitelist_domains: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub enum OptionalConfigPatch<T> {
    Set(T),
    Clear,
}

#[derive(Debug, Clone)]
pub struct SmtpCredentialsInput {
    pub username: String,
    pub password: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SmtpProxyInput {
    pub url: String,
    pub credentials: Option<SmtpCredentialsInput>,
}

fn apply_smtp_credentials_patch(
    current: Option<synctv_core::service::SmtpCredentials>,
    patch: OptionalConfigPatch<SmtpCredentialsInput>,
    field: &str,
) -> Result<Option<synctv_core::service::SmtpCredentials>, ApiError> {
    match patch {
        OptionalConfigPatch::Clear => Ok(None),
        OptionalConfigPatch::Set(input) => {
            let password = match input.password {
                Some(password) => password,
                None => current
                    .filter(|credentials| credentials.username == input.username)
                    .map(|credentials| credentials.password)
                    .ok_or_else(|| {
                        ApiError::InvalidInput(format!(
                            "{field}.set.password is required for new credentials or a username change"
                        ))
                    })?,
            };
            Ok(Some(synctv_core::service::SmtpCredentials {
                username: input.username,
                password,
            }))
        }
    }
}

fn apply_smtp_proxy_patch(
    current: Option<synctv_core::service::SmtpProxyConfig>,
    patch: OptionalConfigPatch<SmtpProxyInput>,
) -> Result<Option<synctv_core::service::SmtpProxyConfig>, ApiError> {
    match patch {
        OptionalConfigPatch::Clear => Ok(None),
        OptionalConfigPatch::Set(input) => {
            let current_credentials = current.and_then(|proxy| proxy.credentials);
            let credentials = input
                .credentials
                .map(|credentials| {
                    apply_smtp_credentials_patch(
                        current_credentials,
                        OptionalConfigPatch::Set(credentials),
                        "email.smtp_proxy.credentials",
                    )
                })
                .transpose()?
                .flatten();
            Ok(Some(synctv_core::service::SmtpProxyConfig {
                url: input.url,
                credentials,
            }))
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WebRtcSettingsPatch {
    pub external_ice_servers: Option<Vec<synctv_core::service::ConfiguredIceServer>>,
    pub max_voice_participants_per_room: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatSettingsPatch {
    pub max_messages_per_room: Option<u64>,
    pub max_pinned_messages_per_room: Option<u64>,
    pub message_retention_days: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct PlaybackHistorySettingsPatch {
    pub retention_days: Option<u32>,
    pub max_entries_per_room: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct CorsSettingsPatch {
    pub allowed_origins: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct RoomSettingsUpdatePatch {
    pub allow_guest_join: Option<bool>,
    pub max_members: Option<u64>,
    pub require_approval: Option<bool>,
    pub allow_auto_join: Option<bool>,
    pub chat_enabled: Option<bool>,
    pub voice_chat_enabled: Option<bool>,
    pub p2p_media_enabled: Option<bool>,
    pub auto_play: Option<RoomAutoPlaySettingsPatch>,
    pub admin_added_permissions: Option<u64>,
    pub admin_removed_permissions: Option<u64>,
    pub member_added_permissions: Option<u64>,
    pub member_removed_permissions: Option<u64>,
    pub guest_added_permissions: Option<u64>,
    pub guest_removed_permissions: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct RoomAutoPlaySettingsPatch {
    pub enabled: Option<bool>,
    pub mode: Option<PlayMode>,
    pub delay: Option<u32>,
}

fn proto_room_password_policy(
    value: synctv_core::service::RoomPasswordPolicy,
) -> synctv_proto::admin::RoomPasswordPolicy {
    match value {
        synctv_core::service::RoomPasswordPolicy::Optional => {
            synctv_proto::admin::RoomPasswordPolicy::Optional
        }
        synctv_core::service::RoomPasswordPolicy::Required => {
            synctv_proto::admin::RoomPasswordPolicy::Required
        }
        synctv_core::service::RoomPasswordPolicy::Forbidden => {
            synctv_proto::admin::RoomPasswordPolicy::Forbidden
        }
    }
}

fn permission_set_from_bits(bits: u64) -> synctv_core::service::PermissionSet {
    let permissions = synctv_core::models::RoomPermissionSet(bits);
    synctv_core::service::PermissionSet::from_bits(permissions)
}

fn guest_permission_set_from_bits(
    bits: u64,
) -> Result<synctv_core::service::PermissionSet, ApiError> {
    let setting = permission_set_from_bits(bits);
    setting.validate_guest_default().map_err(ApiError::from)?;
    Ok(setting)
}

pub struct RuntimeSettingsPatchResult {
    pub settings: synctv_core::service::RuntimeSettings,
    pub update_mask: synctv_core::service::RuntimeSettingsUpdateMask,
}

fn changed_runtime_settings_sections(patch: &RuntimeSettingsPatch) -> Vec<String> {
    let mut sections = Vec::new();
    if patch.server.is_some() {
        sections.push("server".to_string());
    }
    if patch.room_defaults.is_some() {
        sections.push("roomDefaults".to_string());
    }
    if patch.permissions.is_some() {
        sections.push("permissions".to_string());
    }
    if patch.room_creation.is_some() {
        sections.push("roomCreation".to_string());
    }
    if patch.user.is_some() {
        sections.push("user".to_string());
    }
    if patch.oauth2.is_some() {
        sections.push("oauth2".to_string());
    }
    if patch.proxy.is_some() {
        sections.push("proxy".to_string());
    }
    if patch.rtmp.is_some() {
        sections.push("rtmp".to_string());
    }
    if patch.email.is_some() {
        sections.push("email".to_string());
    }
    if patch.webrtc.is_some() {
        sections.push("webrtc".to_string());
    }
    if patch.chat.is_some() {
        sections.push("chat".to_string());
    }
    if patch.cors.is_some() {
        sections.push("cors".to_string());
    }
    sections
}

fn proto_ice_server(
    room_defaults: synctv_core::service::ConfiguredIceServer,
) -> synctv_proto::client::IceServer {
    synctv_proto::client::IceServer {
        urls: room_defaults.urls,
        username: room_defaults.username,
        credential: room_defaults.credential,
    }
}

fn proto_smtp_credentials(
    credentials: synctv_core::service::SmtpCredentials,
) -> synctv_proto::admin::SmtpCredentials {
    synctv_proto::admin::SmtpCredentials {
        username: credentials.username,
        password: None,
    }
}

fn proto_smtp_proxy(
    proxy: synctv_core::service::SmtpProxyConfig,
) -> synctv_proto::admin::SmtpProxy {
    synctv_proto::admin::SmtpProxy {
        url: proxy.url,
        credentials: proxy.credentials.map(proto_smtp_credentials),
    }
}

fn oauth2_github_config_to_proto(
    config: &synctv_core::service::OAuth2GithubProviderConfig,
) -> synctv_proto::admin::OAuth2GithubProviderConfig {
    synctv_proto::admin::OAuth2GithubProviderConfig {
        client_id: config.client_id.clone(),
        client_secret: config.client_secret.clone(),
        redirect_url: config.redirect_url.clone(),
    }
}

fn oauth2_config_to_proto(
    config: &synctv_core::service::OAuth2ProviderPrivateConfig,
) -> synctv_proto::admin::o_auth2_provider_settings::Config {
    use synctv_core::service::OAuth2ProviderPrivateConfig as CoreConfig;
    use synctv_proto::admin::o_auth2_provider_settings::Config;

    match config {
        CoreConfig::GitHub(config) => Config::Github(oauth2_github_config_to_proto(config)),
        CoreConfig::Google(config) => {
            Config::Google(synctv_proto::admin::OAuth2GoogleProviderConfig {
                client_id: config.client_id.clone(),
                client_secret: config.client_secret.clone(),
                redirect_url: config.redirect_url.clone(),
            })
        }
        CoreConfig::Logto(config) => {
            Config::Logto(synctv_proto::admin::OAuth2LogtoProviderConfig {
                client_id: config.client_id.clone(),
                client_secret: config.client_secret.clone(),
                redirect_url: config.redirect_url.clone(),
                endpoint: config.endpoint.clone(),
            })
        }
        CoreConfig::Oidc(config) => Config::Oidc(synctv_proto::admin::OAuth2OidcProviderConfig {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            redirect_url: config.redirect_url.clone(),
            issuer: config.issuer.clone(),
            auth_url: config.auth_url.clone(),
            token_url: config.token_url.clone(),
            userinfo_url: config.userinfo_url.clone(),
            jwks_url: config.jwks_url.clone(),
            scopes: config.scopes.clone(),
        }),
        CoreConfig::Casdoor(config) => {
            Config::Casdoor(synctv_proto::admin::OAuth2CasdoorProviderConfig {
                client_id: config.client_id.clone(),
                client_secret: config.client_secret.clone(),
                redirect_url: config.redirect_url.clone(),
                issuer: config.issuer.clone(),
                auth_url: config.auth_url.clone(),
                token_url: config.token_url.clone(),
                userinfo_url: config.userinfo_url.clone(),
                jwks_url: config.jwks_url.clone(),
            })
        }
        CoreConfig::Apple(config) => {
            Config::Apple(synctv_proto::admin::OAuth2AppleProviderConfig {
                client_id: config.client_id.clone(),
                client_secret: config.client_secret.clone(),
                redirect_url: config.redirect_url.clone(),
            })
        }
    }
}

impl AdminApiImpl {
    fn runtime_settings_store(
        &self,
    ) -> Result<&synctv_core::service::RuntimeSettingsStore, ApiError> {
        self.runtime_settings_store
            .as_deref()
            .ok_or_else(|| ApiError::Internal("runtime settings store is unavailable".to_string()))
    }

    pub fn project_admin_settings(
        settings: synctv_core::service::RuntimeSettings,
    ) -> Result<synctv_proto::admin::RuntimeSettings, ApiError> {
        Ok(synctv_proto::admin::RuntimeSettings {
            server: Some(synctv_proto::admin::ServerSettings {
                name: settings.server.name,
            }),
            room_defaults: Some(synctv_proto::admin::RoomDefaultsSettings {
                default_max_members: settings.room_defaults.default_max_members,
                default_max_chat_messages: settings.room_defaults.default_max_chat_messages,
            }),
            permissions: Some(synctv_proto::admin::PermissionSettings {
                admin_default_permissions: settings
                    .permissions
                    .admin_default_permissions
                    .bits()
                    .bits(),
                member_default_permissions: settings
                    .permissions
                    .member_default_permissions
                    .bits()
                    .bits(),
                guest_default_permissions: settings
                    .permissions
                    .guest_default_permissions
                    .bits()
                    .bits(),
            }),
            room_creation: Some(synctv_proto::admin::RoomCreationSettings {
                enabled: settings.room_creation.enabled,
                approval_required: settings.room_creation.approval_required,
                password_policy: proto_room_password_policy(settings.room_creation.password_policy)
                    as i32,
                max_rooms_per_user: settings.room_creation.max_rooms_per_user,
            }),
            user: Some(synctv_proto::admin::UserSettings {
                enable_password_signup: settings.user.enable_password_signup,
                password_signup_need_review: settings.user.password_signup_need_review,
                enable_email_signup: settings.user.enable_email_signup,
                email_signup_need_review: settings.user.email_signup_need_review,
                enable_webauthn_signup: settings.user.enable_webauthn_signup,
                webauthn_signup_need_review: settings.user.webauthn_signup_need_review,
                enable_guest: settings.user.enable_guest,
            }),
            oauth2: Some(synctv_proto::admin::OAuth2Settings {
                allowed_redirect_urls: settings.oauth2.allowed_redirect_urls,
                providers: settings
                    .oauth2
                    .providers
                    .0
                    .into_iter()
                    .map(
                        |(name, provider)| synctv_proto::admin::OAuth2ProviderSettings {
                            name,
                            enable_signup: provider.enable_signup,
                            signup_need_review: provider.signup_need_review,
                            config: Some(oauth2_config_to_proto(&provider.config)),
                        },
                    )
                    .collect(),
            }),
            proxy: Some(synctv_proto::admin::ProxySettings {
                movie_proxy: settings.proxy.movie_proxy,
                live_proxy: settings.proxy.live_proxy,
            }),
            rtmp: Some(synctv_proto::admin::RtmpSettings {
                custom_publish_host: settings.rtmp.custom_publish_host,
                ts_disguised_as_png: settings.rtmp.ts_disguised_as_png,
            }),
            email: Some(synctv_proto::admin::EmailSettings {
                enabled: settings.email.enabled,
                smtp_host: settings.email.smtp_host,
                smtp_port: settings.email.smtp_port.into(),
                smtp_credentials: settings.email.smtp_credentials.map(proto_smtp_credentials),
                smtp_proxy: settings.email.smtp_proxy.map(proto_smtp_proxy),
                use_tls: settings.email.use_tls,
                from_email: settings.email.from_email,
                from_name: settings.email.from_name,
                whitelist_enabled: settings.email.whitelist_enabled,
                whitelist_domains: settings.email.whitelist_domains,
            }),
            webrtc: Some(synctv_proto::admin::WebRtcSettings {
                external_ice_servers: settings
                    .webrtc
                    .external_ice_servers
                    .0
                    .into_iter()
                    .map(proto_ice_server)
                    .collect(),
                max_voice_participants_per_room: settings.webrtc.max_voice_participants_per_room,
            }),
            chat: Some(synctv_proto::admin::ChatSettings {
                max_messages_per_room: settings.chat.max_messages_per_room,
                max_pinned_messages_per_room: settings.chat.max_pinned_messages_per_room,
                message_retention_days: settings.chat.message_retention_days,
            }),
            playback_history: Some(synctv_proto::admin::PlaybackHistorySettings {
                retention_days: settings.playback_history.retention_days,
                max_entries_per_room: settings.playback_history.max_entries_per_room,
            }),
            cors: Some(synctv_proto::admin::CorsSettings {
                allowed_origins: settings.cors.allowed_origins.0,
            }),
        })
    }

    pub fn apply_runtime_settings_patch(
        mut current: synctv_core::service::RuntimeSettings,
        patch: RuntimeSettingsPatch,
    ) -> Result<RuntimeSettingsPatchResult, ApiError> {
        let mut update_mask = synctv_core::service::RuntimeSettingsUpdateMask::default();

        if let Some(server) = patch.server {
            if let Some(value) = server.name {
                current.server.name = value;
                update_mask.server.name = true;
            }
        }

        if let Some(room_defaults) = patch.room_defaults {
            if let Some(value) = room_defaults.default_max_members {
                current.room_defaults.default_max_members = value;
                update_mask.room_defaults.default_max_members = true;
            }
            if let Some(value) = room_defaults.default_max_chat_messages {
                current.room_defaults.default_max_chat_messages = value;
                update_mask.room_defaults.default_max_chat_messages = true;
            }
        }

        if let Some(permissions) = patch.permissions {
            if let Some(value) = permissions.admin_default_permissions {
                current.permissions.admin_default_permissions = permission_set_from_bits(value);
                update_mask.permissions.admin_default_permissions = true;
            }
            if let Some(value) = permissions.member_default_permissions {
                current.permissions.member_default_permissions = permission_set_from_bits(value);
                update_mask.permissions.member_default_permissions = true;
            }
            if let Some(value) = permissions.guest_default_permissions {
                current.permissions.guest_default_permissions =
                    guest_permission_set_from_bits(value)?;
                update_mask.permissions.guest_default_permissions = true;
            }
        }

        if let Some(room_creation) = patch.room_creation {
            if let Some(value) = room_creation.enabled {
                current.room_creation.enabled = value;
                update_mask.room_creation.enabled = true;
            }
            if let Some(value) = room_creation.approval_required {
                current.room_creation.approval_required = value;
                update_mask.room_creation.approval_required = true;
            }
            if let Some(value) = room_creation.password_policy {
                current.room_creation.password_policy = value;
                update_mask.room_creation.password_policy = true;
            }
            if let Some(value) = room_creation.max_rooms_per_user {
                current.room_creation.max_rooms_per_user = value;
                update_mask.room_creation.max_rooms_per_user = true;
            }
        }

        if let Some(user) = patch.user {
            if let Some(value) = user.enable_password_signup {
                current.user.enable_password_signup = value;
                update_mask.user.enable_password_signup = true;
            }
            if let Some(value) = user.password_signup_need_review {
                current.user.password_signup_need_review = value;
                update_mask.user.password_signup_need_review = true;
            }
            if let Some(value) = user.enable_email_signup {
                current.user.enable_email_signup = value;
                update_mask.user.enable_email_signup = true;
            }
            if let Some(value) = user.email_signup_need_review {
                current.user.email_signup_need_review = value;
                update_mask.user.email_signup_need_review = true;
            }
            if let Some(value) = user.enable_webauthn_signup {
                current.user.enable_webauthn_signup = value;
                update_mask.user.enable_webauthn_signup = true;
            }
            if let Some(value) = user.webauthn_signup_need_review {
                current.user.webauthn_signup_need_review = value;
                update_mask.user.webauthn_signup_need_review = true;
            }
            if let Some(value) = user.enable_guest {
                current.user.enable_guest = value;
                update_mask.user.enable_guest = true;
            }
        }

        if let Some(oauth2) = patch.oauth2 {
            if let Some(providers) = oauth2.providers {
                current.oauth2.providers = providers;
                update_mask.oauth2.providers = true;
            }
            if let Some(urls) = oauth2.allowed_redirect_urls {
                current.oauth2.allowed_redirect_urls = urls;
                update_mask.oauth2.allowed_redirect_urls = true;
            }
        }

        if let Some(proxy) = patch.proxy {
            if let Some(value) = proxy.movie_proxy {
                current.proxy.movie_proxy = value;
                update_mask.proxy.movie_proxy = true;
            }
            if let Some(value) = proxy.live_proxy {
                current.proxy.live_proxy = value;
                update_mask.proxy.live_proxy = true;
            }
        }

        if let Some(rtmp) = patch.rtmp {
            if let Some(patch) = rtmp.custom_publish_host {
                current.rtmp.custom_publish_host = match patch {
                    OptionalConfigPatch::Set(value) => Some(value),
                    OptionalConfigPatch::Clear => None,
                };
                update_mask.rtmp.custom_publish_host = true;
            }
            if let Some(value) = rtmp.ts_disguised_as_png {
                current.rtmp.ts_disguised_as_png = value;
                update_mask.rtmp.ts_disguised_as_png = true;
            }
        }

        if let Some(email) = patch.email {
            if let Some(value) = email.enabled {
                current.email.enabled = value;
                update_mask.email.enabled = true;
            }
            if let Some(patch) = email.smtp_host {
                current.email.smtp_host = match patch {
                    OptionalConfigPatch::Set(value) => Some(value),
                    OptionalConfigPatch::Clear => None,
                };
                update_mask.email.smtp_host = true;
            }
            if let Some(value) = email.smtp_port {
                current.email.smtp_port = value.try_into().map_err(|_| {
                    ApiError::InvalidInput(
                        "email.smtp_port must be between 1 and 65535".to_string(),
                    )
                })?;
                update_mask.email.smtp_port = true;
            }
            if let Some(patch) = email.smtp_credentials {
                current.email.smtp_credentials = apply_smtp_credentials_patch(
                    current.email.smtp_credentials,
                    patch,
                    "email.smtp_credentials",
                )?;
                update_mask.email.smtp_credentials = true;
            }
            if let Some(patch) = email.smtp_proxy {
                current.email.smtp_proxy = apply_smtp_proxy_patch(current.email.smtp_proxy, patch)?;
                update_mask.email.smtp_proxy = true;
            }
            if let Some(value) = email.use_tls {
                current.email.use_tls = value;
                update_mask.email.use_tls = true;
            }
            if let Some(patch) = email.from_email {
                current.email.from_email = match patch {
                    OptionalConfigPatch::Set(value) => Some(value),
                    OptionalConfigPatch::Clear => None,
                };
                update_mask.email.from_email = true;
            }
            if let Some(value) = email.from_name {
                current.email.from_name = value;
                update_mask.email.from_name = true;
            }
            if let Some(value) = email.whitelist_enabled {
                current.email.whitelist_enabled = value;
                update_mask.email.whitelist_enabled = true;
            }
            if let Some(domains) = email.whitelist_domains {
                current.email.whitelist_domains = domains;
                update_mask.email.whitelist_domains = true;
            }
        }

        if let Some(webrtc) = patch.webrtc {
            if let Some(servers) = webrtc.external_ice_servers {
                current.webrtc.external_ice_servers = synctv_core::service::IceServerList(servers);
                update_mask.webrtc.external_ice_servers = true;
            }
            if let Some(value) = webrtc.max_voice_participants_per_room {
                current.webrtc.max_voice_participants_per_room = value;
                update_mask.webrtc.max_voice_participants_per_room = true;
            }
        }

        if let Some(chat) = patch.chat {
            if let Some(value) = chat.max_messages_per_room {
                current.chat.max_messages_per_room = value;
                update_mask.chat.max_messages_per_room = true;
            }
            if let Some(value) = chat.max_pinned_messages_per_room {
                current.chat.max_pinned_messages_per_room = value;
                update_mask.chat.max_pinned_messages_per_room = true;
            }
            if let Some(value) = chat.message_retention_days {
                current.chat.message_retention_days = value;
                update_mask.chat.message_retention_days = true;
            }
        }

        if let Some(playback_history) = patch.playback_history {
            if let Some(value) = playback_history.retention_days {
                current.playback_history.retention_days = value;
                update_mask.playback_history.retention_days = true;
            }
            if let Some(value) = playback_history.max_entries_per_room {
                current.playback_history.max_entries_per_room = value;
                update_mask.playback_history.max_entries_per_room = true;
            }
        }

        if let Some(cors) = patch.cors {
            if let Some(origins) = cors.allowed_origins {
                current.cors.allowed_origins = synctv_core::service::CorsAllowedOrigins(origins);
                update_mask.cors.allowed_origins = true;
            }
        }

        Ok(RuntimeSettingsPatchResult {
            settings: current,
            update_mask,
        })
    }

    pub async fn get_settings(
        &self,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::RuntimeSettings, ApiError> {
        let settings = Self::project_admin_settings(
            self.runtime_settings_store()?
                .runtime_settings()
                .map_err(ApiError::from)?,
        )?;
        let sections = [
            "server",
            "roomDefaults",
            "permissions",
            "roomCreation",
            "user",
            "oauth2",
            "proxy",
            "rtmp",
            "email",
            "webrtc",
            "chat",
            "cors",
        ];

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::SettingsViewed,
            synctv_core::models::AuditTargetType::Settings,
            None,
            AuditDetails {
                group_count: Some(sections.len()),
                groups: sections
                    .iter()
                    .map(|section| (*section).to_string())
                    .collect(),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(settings)
    }

    pub async fn update_settings(
        &self,
        req: synctv_proto::admin::UpdateSettingsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::RuntimeSettings, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let patch = crate::admin_settings_mapping::runtime_settings_patch_from_admin_proto(req)?;
        let runtime_settings_store = self.runtime_settings_store()?;
        let current = runtime_settings_store
            .runtime_settings()
            .map_err(ApiError::from)?;
        let changed_sections = changed_runtime_settings_sections(&patch);
        let patch_result = Self::apply_runtime_settings_patch(current, patch)?;

        runtime_settings_store
            .persist_runtime_settings_patch(&patch_result.settings, &patch_result.update_mask)
            .await
            .map_err(ApiError::from)?;

        if !self.room_cache_fanout.try_publish_all_invalidation().await {
            tracing::warn!(
                changed_sections = ?changed_sections,
                "Failed to publish runtime settings cache invalidation after settings update"
            );
        }

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::SettingsUpdated,
            synctv_core::models::AuditTargetType::Settings,
            None,
            AuditDetails {
                groups: changed_sections,
                ..Default::default()
            },
            ctx,
        )
        .await;

        Self::project_admin_settings(patch_result.settings)
    }

    pub(in crate::impls::admin) fn map_send_test_email_result(
        to: &str,
        result: Result<(), CoreError>,
    ) -> synctv_proto::admin::SendTestEmailResponse {
        match result {
            Ok(()) => synctv_proto::admin::SendTestEmailResponse {
                message: format!("Test email sent successfully to {to}"),
                success: true,
            },
            Err(error) => {
                tracing::error!(email = %to, error = %error, "Failed to send test email");
                synctv_proto::admin::SendTestEmailResponse {
                    message: "Failed to send test email. Please verify the email configuration and try again.".to_string(),
                    success: false,
                }
            }
        }
    }

    pub async fn send_test_email(
        &self,
        req: synctv_proto::admin::SendTestEmailRequest,
    ) -> Result<synctv_proto::admin::SendTestEmailResponse, ApiError> {
        self.send_test_email_with_control(req, None).await
    }

    pub async fn send_test_email_with_control(
        &self,
        req: synctv_proto::admin::SendTestEmailRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<synctv_proto::admin::SendTestEmailResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        Ok(Self::map_send_test_email_result(
            &req.to,
            self.email_service
                .send_test_email_with_control(&req.to, control)
                .await,
        ))
    }

    pub async fn get_room_settings(
        &self,
        req: synctv_proto::admin::GetRoomSettingsRequest,
    ) -> Result<synctv_proto::admin::GetRoomSettingsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let room_id = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let (settings, version) = self
            .room_service
            .get_room_settings_with_version(&room_id)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::GetRoomSettingsResponse {
            settings: Some(room_settings_to_proto(&settings)),
            version,
        })
    }

    pub async fn update_room_settings(
        &self,
        req: synctv_proto::admin::UpdateRoomSettingsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::Room, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid =
            crate::impls::proto_validated_room_id(req.room_id.clone(), &self.public_id_codec)?;
        let patch = crate::admin_settings_mapping::room_settings_patch_from_admin_proto(&req)?;
        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let admin_username = admin_actor.username;
        let mut settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let mut core_patch = RoomSettingsPatch::default();
        if let Some(value) = patch.allow_guest_join {
            core_patch.allow_guest_join = Some(
                synctv_core::models::room_settings::AllowGuestJoin::new(value),
            );
        }
        if let Some(value) = patch.max_members {
            core_patch.max_members =
                Some(synctv_core::models::room_settings::MaxMembers::new(value));
        }
        if let Some(value) = patch.require_approval {
            core_patch.require_approval = Some(
                synctv_core::models::room_settings::RequireApproval::new(value),
            );
        }
        if let Some(value) = patch.allow_auto_join {
            core_patch.allow_auto_join = Some(
                synctv_core::models::room_settings::AllowAutoJoin::new(value),
            );
        }
        if let Some(value) = patch.chat_enabled {
            core_patch.chat_enabled =
                Some(synctv_core::models::room_settings::ChatEnabled::new(value));
        }
        if let Some(value) = patch.voice_chat_enabled {
            core_patch.voice_chat_enabled = Some(
                synctv_core::models::room_settings::VoiceChatEnabled::new(value),
            );
        }
        if let Some(value) = patch.p2p_media_enabled {
            core_patch.p2p_media_enabled = Some(
                synctv_core::models::room_settings::P2pMediaEnabled::new(value),
            );
        }
        if let Some(auto_play) = patch.auto_play {
            let mut value = settings.auto_play.value.clone();
            if let Some(enabled) = auto_play.enabled {
                value.enabled = enabled;
            }
            if let Some(mode) = auto_play.mode {
                value.mode = mode;
            }
            if let Some(delay) = auto_play.delay {
                value.delay = delay;
            }
            core_patch.auto_play = Some(synctv_core::models::room_settings::AutoPlay::new(value));
        }
        if let Some(value) = patch.admin_added_permissions {
            core_patch.admin_added_permissions =
                Some(synctv_core::models::room_settings::AdminAddedPermissions::new(value));
        }
        if let Some(value) = patch.admin_removed_permissions {
            core_patch.admin_removed_permissions =
                Some(synctv_core::models::room_settings::AdminRemovedPermissions::new(value));
        }
        if let Some(value) = patch.member_added_permissions {
            core_patch.member_added_permissions =
                Some(synctv_core::models::room_settings::MemberAddedPermissions::new(value));
        }
        if let Some(value) = patch.member_removed_permissions {
            core_patch.member_removed_permissions =
                Some(synctv_core::models::room_settings::MemberRemovedPermissions::new(value));
        }
        if let Some(value) = patch.guest_added_permissions {
            core_patch.guest_added_permissions =
                Some(synctv_core::models::room_settings::GuestAddedPermissions::new(value));
        }
        if let Some(value) = patch.guest_removed_permissions {
            core_patch.guest_removed_permissions =
                Some(synctv_core::models::room_settings::GuestRemovedPermissions::new(value));
        }
        settings.merge_patch(core_patch);
        let prepared_settings_fanout = self.room_settings_fanout.prepare_settings_changed(
            &rid,
            admin_user_id,
            &admin_username,
        )?;
        let snapshot = self
            .room_service
            .manage_room_settings_with_outbox(
                &rid,
                &settings,
                Some(prepared_settings_fanout.settings_outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        self.room_settings_fanout
            .publish_prepared_after_outbox_commit(
                prepared_settings_fanout
                    .with_settings_and_version(&snapshot.settings, snapshot.version)?,
            );
        self.publish_room_cache_invalidation(&rid);

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        self.load_admin_room_proto(&room, Some(&snapshot.settings))
            .await
    }

    pub async fn reset_room_settings(
        &self,
        req: synctv_proto::admin::ResetRoomSettingsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::Room, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let default_settings = synctv_core::models::RoomSettings::default();
        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let admin_username = admin_actor.username;
        let prepared_settings_fanout = self.room_settings_fanout.prepare_settings_changed(
            &rid,
            admin_user_id,
            &admin_username,
        )?;
        let snapshot = self
            .room_service
            .manage_room_settings_with_outbox(
                &rid,
                &default_settings,
                Some(prepared_settings_fanout.settings_outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        self.room_settings_fanout
            .publish_prepared_after_outbox_commit(
                prepared_settings_fanout
                    .with_settings_and_version(&snapshot.settings, snapshot.version)?,
            );
        self.publish_room_cache_invalidation(&rid);

        self.load_admin_room_proto(&room, Some(&snapshot.settings))
            .await
    }
}
