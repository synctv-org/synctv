use std::collections::BTreeMap;

use synctv_core::{
    models::PlayMode,
    service::{
        ConfiguredIceServer, OAuth2GithubProviderConfig, OAuth2GoogleProviderConfig,
        OAuth2LogtoProviderConfig, OAuth2OidcProviderConfig, OAuth2ProviderConfig,
        OAuth2ProviderConfigs, OAuth2ProviderPrivateConfig, RoomPasswordPolicy,
    },
};
use synctv_proto::{admin as admin_proto, client as client_proto};

use crate::impls::admin::settings::{
    ChatSettingsPatch, CorsSettingsPatch, EmailSettingsPatch, OAuth2SettingsPatch,
    PermissionSettingsPatch, ProxySettingsPatch, RoomAutoPlaySettingsPatch,
    RoomCreationSettingsPatch, RoomDefaultsSettingsPatch, RoomSettingsUpdatePatch,
    RtmpSettingsPatch, RuntimeSettingsPatch, UserSettingsPatch, WebRtcSettingsPatch,
};
use crate::ApiError;

pub fn runtime_settings_patch_from_admin_proto(
    req: admin_proto::UpdateSettingsRequest,
) -> Result<RuntimeSettingsPatch, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    runtime_settings_patch_from_admin_proto_parts(AdminUpdateSettingsProtoParts {
        room_defaults: req.room_defaults,
        permissions: req.permissions,
        room_creation: req.room_creation,
        user: req.user,
        oauth2: req.oauth2,
        proxy: req.proxy,
        rtmp: req.rtmp,
        email: req.email,
        webrtc: req.webrtc,
        chat: req.chat,
        cors: req.cors,
    })
}

#[derive(Debug, Clone, Default)]
pub struct AdminUpdateSettingsProtoParts {
    pub room_defaults: Option<admin_proto::RoomDefaultsSettingsPatch>,
    pub permissions: Option<admin_proto::PermissionSettingsPatch>,
    pub room_creation: Option<admin_proto::RoomCreationSettingsPatch>,
    pub user: Option<admin_proto::UserSettingsPatch>,
    pub oauth2: Option<admin_proto::OAuth2SettingsPatch>,
    pub proxy: Option<admin_proto::ProxySettingsPatch>,
    pub rtmp: Option<admin_proto::RtmpSettingsPatch>,
    pub email: Option<admin_proto::EmailSettingsPatch>,
    pub webrtc: Option<admin_proto::WebRtcSettingsPatch>,
    pub chat: Option<admin_proto::ChatSettingsPatch>,
    pub cors: Option<admin_proto::CorsSettingsPatch>,
}

pub fn runtime_settings_patch_from_admin_proto_parts(
    parts: AdminUpdateSettingsProtoParts,
) -> Result<RuntimeSettingsPatch, ApiError> {
    Ok(RuntimeSettingsPatch {
        room_defaults: parts.room_defaults.map(|patch| RoomDefaultsSettingsPatch {
            default_max_members: patch.default_max_members,
            default_max_chat_messages: patch.default_max_chat_messages,
        }),
        permissions: parts.permissions.map(|patch| PermissionSettingsPatch {
            admin_default_permissions: patch.admin_default_permissions,
            member_default_permissions: patch.member_default_permissions,
            guest_default_permissions: patch.guest_default_permissions,
        }),
        room_creation: parts
            .room_creation
            .map(room_creation_settings_patch_from_admin_proto)
            .transpose()?,
        user: parts.user.map(|patch| UserSettingsPatch {
            enable_password_signup: patch.enable_password_signup,
            password_signup_need_review: patch.password_signup_need_review,
            enable_email_signup: patch.enable_email_signup,
            email_signup_need_review: patch.email_signup_need_review,
            enable_webauthn_signup: patch.enable_webauthn_signup,
            webauthn_signup_need_review: patch.webauthn_signup_need_review,
            enable_guest: patch.enable_guest,
        }),
        oauth2: parts
            .oauth2
            .map(oauth2_settings_patch_from_admin_proto)
            .transpose()?,
        proxy: parts.proxy.map(|patch| ProxySettingsPatch {
            movie_proxy: patch.movie_proxy,
            live_proxy: patch.live_proxy,
        }),
        rtmp: parts.rtmp.map(|patch| RtmpSettingsPatch {
            custom_publish_host: patch.custom_publish_host,
            ts_disguised_as_png: patch.ts_disguised_as_png,
        }),
        email: parts.email.map(|patch| EmailSettingsPatch {
            enabled: patch.enabled,
            smtp_host: patch.smtp_host,
            smtp_port: patch.smtp_port,
            smtp_username: patch.smtp_username,
            smtp_password: patch.smtp_password,
            use_tls: patch.use_tls,
            from_email: patch.from_email,
            from_name: patch.from_name,
            whitelist_enabled: patch.whitelist_enabled,
            whitelist_domains: patch.whitelist_domains.map(|domains| domains.values),
        }),
        webrtc: parts.webrtc.map(|patch| WebRtcSettingsPatch {
            external_ice_servers: patch
                .external_ice_servers
                .map(|servers| servers.values.into_iter().map(core_ice_server).collect()),
        }),
        chat: parts.chat.map(|patch| ChatSettingsPatch {
            max_messages_per_room: patch.max_messages_per_room,
            max_pinned_messages_per_room: patch.max_pinned_messages_per_room,
            message_retention_days: patch.message_retention_days,
        }),
        cors: parts.cors.map(|patch| CorsSettingsPatch {
            allowed_origins: patch.allowed_origins.map(|origins| origins.values),
        }),
    })
}

pub fn room_settings_patch_from_admin_proto(
    req: &admin_proto::UpdateRoomSettingsRequest,
) -> Result<RoomSettingsUpdatePatch, ApiError> {
    crate::impls::validate_proto_request(req)?;
    room_settings_patch_from_admin_proto_parts(&AdminUpdateRoomSettingsProtoParts {
        allow_guest_join: req.allow_guest_join,
        max_members: req.max_members,
        require_approval: req.require_approval,
        allow_auto_join: req.allow_auto_join,
        chat_enabled: req.chat_enabled,
        auto_play: req.auto_play,
        admin_added_permissions: req.admin_added_permissions,
        admin_removed_permissions: req.admin_removed_permissions,
        member_added_permissions: req.member_added_permissions,
        member_removed_permissions: req.member_removed_permissions,
        guest_added_permissions: req.guest_added_permissions,
        guest_removed_permissions: req.guest_removed_permissions,
    })
}

#[derive(Debug, Clone, Default)]
pub struct AdminUpdateRoomSettingsProtoParts {
    pub allow_guest_join: Option<bool>,
    pub max_members: Option<u64>,
    pub require_approval: Option<bool>,
    pub allow_auto_join: Option<bool>,
    pub chat_enabled: Option<bool>,
    pub auto_play: Option<client_proto::AutoPlaySettingsPatch>,
    pub admin_added_permissions: Option<u64>,
    pub admin_removed_permissions: Option<u64>,
    pub member_added_permissions: Option<u64>,
    pub member_removed_permissions: Option<u64>,
    pub guest_added_permissions: Option<u64>,
    pub guest_removed_permissions: Option<u64>,
}

pub fn room_settings_patch_from_admin_proto_parts(
    parts: &AdminUpdateRoomSettingsProtoParts,
) -> Result<RoomSettingsUpdatePatch, ApiError> {
    Ok(RoomSettingsUpdatePatch {
        allow_guest_join: parts.allow_guest_join,
        max_members: parts.max_members,
        require_approval: parts.require_approval,
        allow_auto_join: parts.allow_auto_join,
        chat_enabled: parts.chat_enabled,
        auto_play: parts
            .auto_play
            .map(auto_play_patch_from_client_proto)
            .transpose()?,
        admin_added_permissions: parts.admin_added_permissions,
        admin_removed_permissions: parts.admin_removed_permissions,
        member_added_permissions: parts.member_added_permissions,
        member_removed_permissions: parts.member_removed_permissions,
        guest_added_permissions: parts.guest_added_permissions,
        guest_removed_permissions: parts.guest_removed_permissions,
    })
}

fn room_creation_settings_patch_from_admin_proto(
    patch: admin_proto::RoomCreationSettingsPatch,
) -> Result<RoomCreationSettingsPatch, ApiError> {
    Ok(RoomCreationSettingsPatch {
        enabled: patch.enabled,
        approval_required: patch.approval_required,
        password_policy: patch
            .password_policy
            .map(core_room_password_policy)
            .transpose()?,
        max_rooms_per_user: patch.max_rooms_per_user,
    })
}

fn oauth2_settings_patch_from_admin_proto(
    patch: admin_proto::OAuth2SettingsPatch,
) -> Result<OAuth2SettingsPatch, ApiError> {
    Ok(OAuth2SettingsPatch {
        providers: patch
            .providers
            .map(|providers| oauth2_provider_configs_from_admin_proto(providers.providers))
            .transpose()?,
    })
}

fn oauth2_provider_configs_from_admin_proto(
    providers: Vec<admin_proto::OAuth2ProviderSettings>,
) -> Result<OAuth2ProviderConfigs, ApiError> {
    let mut configs = BTreeMap::new();
    for provider in providers {
        let name = provider.name.clone();
        let config = oauth2_config_from_admin_proto(&name, &provider)?;
        if configs
            .insert(
                name.clone(),
                OAuth2ProviderConfig {
                    enable_signup: provider.enable_signup,
                    signup_need_review: provider.signup_need_review,
                    config,
                },
            )
            .is_some()
        {
            return Err(ApiError::InvalidInput(format!(
                "Duplicate OAuth2 provider name '{name}'"
            )));
        }
    }
    Ok(OAuth2ProviderConfigs(configs))
}

fn oauth2_config_from_admin_proto(
    name: &str,
    provider: &admin_proto::OAuth2ProviderSettings,
) -> Result<OAuth2ProviderPrivateConfig, ApiError> {
    use admin_proto::o_auth2_provider_settings::Config;
    Ok(
        match provider.config.clone().ok_or_else(|| {
            ApiError::InvalidInput(format!("OAuth2 provider '{name}' config is required"))
        })? {
            Config::Github(config) => {
                OAuth2ProviderPrivateConfig::GitHub(OAuth2GithubProviderConfig {
                    client_id: config.client_id,
                    client_secret: config.client_secret,
                    redirect_url: config.redirect_url,
                })
            }
            Config::Google(config) => {
                OAuth2ProviderPrivateConfig::Google(OAuth2GoogleProviderConfig {
                    client_id: config.client_id,
                    client_secret: config.client_secret,
                    redirect_url: config.redirect_url,
                })
            }
            Config::Logto(config) => {
                OAuth2ProviderPrivateConfig::Logto(OAuth2LogtoProviderConfig {
                    client_id: config.client_id,
                    client_secret: config.client_secret,
                    redirect_url: config.redirect_url,
                    endpoint: config.endpoint,
                })
            }
            Config::Oidc(config) => OAuth2ProviderPrivateConfig::Oidc(OAuth2OidcProviderConfig {
                client_id: config.client_id,
                client_secret: config.client_secret,
                redirect_url: config.redirect_url,
                issuer: config.issuer,
                auth_url: config.auth_url,
                token_url: config.token_url,
                userinfo_url: config.userinfo_url,
                jwks_url: config.jwks_url,
            }),
            Config::Casdoor(config) => {
                OAuth2ProviderPrivateConfig::Casdoor(OAuth2OidcProviderConfig {
                    client_id: config.client_id,
                    client_secret: config.client_secret,
                    redirect_url: config.redirect_url,
                    issuer: config.issuer,
                    auth_url: config.auth_url,
                    token_url: config.token_url,
                    userinfo_url: config.userinfo_url,
                    jwks_url: config.jwks_url,
                })
            }
        },
    )
}

fn core_room_password_policy(value: i32) -> Result<RoomPasswordPolicy, ApiError> {
    match admin_proto::RoomPasswordPolicy::try_from(value) {
        Ok(admin_proto::RoomPasswordPolicy::Optional) => Ok(RoomPasswordPolicy::Optional),
        Ok(admin_proto::RoomPasswordPolicy::Required) => Ok(RoomPasswordPolicy::Required),
        Ok(admin_proto::RoomPasswordPolicy::Forbidden) => Ok(RoomPasswordPolicy::Forbidden),
        Ok(admin_proto::RoomPasswordPolicy::Unspecified) | Err(_) => Err(ApiError::InvalidInput(
            "room_creation password policy is required".to_string(),
        )),
    }
}

fn auto_play_patch_from_client_proto(
    patch: client_proto::AutoPlaySettingsPatch,
) -> Result<RoomAutoPlaySettingsPatch, ApiError> {
    Ok(RoomAutoPlaySettingsPatch {
        enabled: patch.enabled,
        mode: patch.mode.map(core_play_mode).transpose()?,
        delay: patch.delay,
    })
}

fn core_play_mode(value: i32) -> Result<PlayMode, ApiError> {
    match client_proto::PlayMode::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported auto_play.mode".to_string()))?
    {
        client_proto::PlayMode::Unspecified | client_proto::PlayMode::Sequential => {
            Ok(PlayMode::Sequential)
        }
        client_proto::PlayMode::RepeatOne => Ok(PlayMode::RepeatOne),
        client_proto::PlayMode::RepeatAll => Ok(PlayMode::RepeatAll),
        client_proto::PlayMode::Shuffle => Ok(PlayMode::Shuffle),
    }
}

fn core_ice_server(value: client_proto::IceServer) -> ConfiguredIceServer {
    ConfiguredIceServer {
        urls: value.urls,
        username: value.username,
        credential: value.credential,
    }
}
