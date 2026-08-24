use std::collections::{BTreeMap, BTreeSet};

use synctv_core::{
    models::PlayMode,
    service::{
        ConfiguredIceServer, OAuth2AppleProviderConfig, OAuth2CasdoorProviderConfig,
        OAuth2DiscordProviderConfig, OAuth2FeishuProviderConfig, OAuth2GiteeProviderConfig,
        OAuth2GithubProviderConfig, OAuth2GoogleProviderConfig, OAuth2LogtoProviderConfig,
        OAuth2MicrosoftProviderConfig, OAuth2OidcProviderConfig, OAuth2ProviderConfig,
        OAuth2ProviderConfigs, OAuth2ProviderPrivateConfig, OAuth2QqProviderConfig,
        RoomPasswordPolicy,
    },
};
use synctv_proto::{admin as admin_proto, client as client_proto};

use crate::impls::admin::settings::{
    ChatSettingsPatch, CorsSettingsPatch, EmailSettingsPatch, OAuth2SettingsPatch,
    OptionalConfigPatch, PermissionSettingsPatch, PlaybackHistorySettingsPatch,
    RoomAutoPlaySettingsPatch, RoomCreationSettingsPatch, RoomDefaultsSettingsPatch,
    RoomSettingsUpdatePatch, RtmpSettingsPatch, RuntimeSettingsPatch, ServerSettingsPatch,
    SmtpCredentialsInput, SmtpProxyInput, UserSettingsPatch, WebRtcSettingsPatch,
};
use crate::ApiError;

pub fn runtime_settings_patch_from_admin_proto(
    req: admin_proto::UpdateSettingsRequest,
) -> Result<RuntimeSettingsPatch, ApiError> {
    runtime_settings_patch_from_admin_proto_with_oauth2(req, None)
}

pub fn runtime_settings_patch_from_admin_proto_with_current(
    req: admin_proto::UpdateSettingsRequest,
    current_oauth2: &OAuth2ProviderConfigs,
) -> Result<RuntimeSettingsPatch, ApiError> {
    runtime_settings_patch_from_admin_proto_with_oauth2(req, Some(current_oauth2))
}

fn runtime_settings_patch_from_admin_proto_with_oauth2(
    req: admin_proto::UpdateSettingsRequest,
    current_oauth2: Option<&OAuth2ProviderConfigs>,
) -> Result<RuntimeSettingsPatch, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let settings = req
        .settings
        .ok_or_else(|| ApiError::InvalidInput("settings is required".to_string()))?;
    let paths = req
        .update_mask
        .ok_or_else(|| ApiError::InvalidInput("update_mask is required".to_string()))?
        .paths;
    let patch = RuntimeSettingsPatch {
        server: settings
            .server
            .map(|patch| ServerSettingsPatch { name: patch.name }),
        room_defaults: settings
            .room_defaults
            .map(|patch| RoomDefaultsSettingsPatch {
                default_max_members: patch.default_max_members,
                default_max_chat_messages: patch.default_max_chat_messages,
            }),
        permissions: settings.permissions.map(|patch| PermissionSettingsPatch {
            admin_default_permissions: patch.admin_default_permissions,
            member_default_permissions: patch.member_default_permissions,
            guest_default_permissions: patch.guest_default_permissions,
        }),
        room_creation: settings
            .room_creation
            .map(room_creation_settings_patch_from_admin_proto)
            .transpose()?,
        user: settings.user.map(|patch| UserSettingsPatch {
            enable_password_signup: patch.enable_password_signup,
            password_signup_need_review: patch.password_signup_need_review,
            enable_email_signup: patch.enable_email_signup,
            email_signup_need_review: patch.email_signup_need_review,
            enable_webauthn_signup: patch.enable_webauthn_signup,
            webauthn_signup_need_review: patch.webauthn_signup_need_review,
            enable_guest: patch.enable_guest,
        }),
        oauth2: settings
            .oauth2
            .map(|patch| oauth2_settings_patch_from_admin_proto(patch, current_oauth2))
            .transpose()?,
        rtmp: settings.rtmp.map(|patch| RtmpSettingsPatch {
            advertise_address: patch.advertise_address.map(OptionalConfigPatch::Set),
            ts_disguised_as_png: patch.ts_disguised_as_png,
        }),
        email: settings.email.map(email_settings_patch_from_admin_proto),
        webrtc: settings.webrtc.map(|patch| WebRtcSettingsPatch {
            external_ice_servers: Some(
                patch
                    .external_ice_servers
                    .into_iter()
                    .map(core_ice_server)
                    .collect(),
            ),
            max_voice_participants_per_room: patch.max_voice_participants_per_room,
        }),
        chat: settings.chat.map(|patch| ChatSettingsPatch {
            max_messages_per_room: patch.max_messages_per_room,
            max_pinned_messages_per_room: patch.max_pinned_messages_per_room,
            message_retention_days: patch.message_retention_days,
        }),
        playback_history: settings
            .playback_history
            .map(|patch| PlaybackHistorySettingsPatch {
                retention_days: patch.retention_days,
                max_entries_per_room: patch.max_entries_per_room,
            }),
        cors: settings.cors.map(|patch| CorsSettingsPatch {
            allowed_origins: Some(patch.allowed_origins),
        }),
    };
    select_runtime_settings_patch(patch, &paths)
}

fn required_runtime_snapshot_section<T>(value: Option<T>, path: &str) -> Result<T, ApiError> {
    value.ok_or_else(|| ApiError::InvalidInput(format!("{path} is required")))
}

fn runtime_snapshot_credentials(
    credentials: admin_proto::SmtpCredentials,
    path: &str,
) -> Result<SmtpCredentialsInput, ApiError> {
    let password = credentials.password.ok_or_else(|| {
        ApiError::InvalidInput(format!(
            "{path}.password is required in a runtime settings snapshot"
        ))
    })?;
    Ok(SmtpCredentialsInput {
        username: credentials.username,
        password: Some(password),
    })
}

pub fn runtime_settings_replacement_patch_from_admin_proto(
    settings: admin_proto::RuntimeSettings,
) -> Result<RuntimeSettingsPatch, ApiError> {
    crate::impls::validate_proto_request(&settings)?;

    let server = required_runtime_snapshot_section(settings.server, "settings.server")?;
    let room_defaults =
        required_runtime_snapshot_section(settings.room_defaults, "settings.room_defaults")?;
    let permissions =
        required_runtime_snapshot_section(settings.permissions, "settings.permissions")?;
    let room_creation =
        required_runtime_snapshot_section(settings.room_creation, "settings.room_creation")?;
    let user = required_runtime_snapshot_section(settings.user, "settings.user")?;
    let oauth2 = required_runtime_snapshot_section(settings.oauth2, "settings.oauth2")?;
    let rtmp = required_runtime_snapshot_section(settings.rtmp, "settings.rtmp")?;
    let email = required_runtime_snapshot_section(settings.email, "settings.email")?;
    let webrtc = required_runtime_snapshot_section(settings.webrtc, "settings.webrtc")?;
    let chat = required_runtime_snapshot_section(settings.chat, "settings.chat")?;
    let playback_history =
        required_runtime_snapshot_section(settings.playback_history, "settings.playback_history")?;
    let cors = required_runtime_snapshot_section(settings.cors, "settings.cors")?;

    let smtp_credentials = email
        .smtp_credentials
        .map(|credentials| {
            runtime_snapshot_credentials(credentials, "settings.email.smtp_credentials")
                .map(OptionalConfigPatch::Set)
        })
        .transpose()?
        .unwrap_or(OptionalConfigPatch::Clear);
    let smtp_proxy = email
        .smtp_proxy
        .map(|proxy| {
            let credentials = proxy
                .credentials
                .map(|credentials| {
                    runtime_snapshot_credentials(
                        credentials,
                        "settings.email.smtp_proxy.credentials",
                    )
                })
                .transpose()?;
            Ok::<_, ApiError>(OptionalConfigPatch::Set(SmtpProxyInput {
                url: proxy.url,
                credentials,
            }))
        })
        .transpose()?
        .unwrap_or(OptionalConfigPatch::Clear);

    Ok(RuntimeSettingsPatch {
        server: Some(ServerSettingsPatch {
            name: Some(server.name),
        }),
        room_defaults: Some(RoomDefaultsSettingsPatch {
            default_max_members: Some(room_defaults.default_max_members),
            default_max_chat_messages: Some(room_defaults.default_max_chat_messages),
        }),
        permissions: Some(PermissionSettingsPatch {
            admin_default_permissions: Some(permissions.admin_default_permissions),
            member_default_permissions: Some(permissions.member_default_permissions),
            guest_default_permissions: Some(permissions.guest_default_permissions),
        }),
        room_creation: Some(RoomCreationSettingsPatch {
            enabled: Some(room_creation.enabled),
            approval_required: Some(room_creation.approval_required),
            password_policy: Some(core_room_password_policy(room_creation.password_policy)?),
            max_rooms_per_user: Some(room_creation.max_rooms_per_user),
        }),
        user: Some(UserSettingsPatch {
            enable_password_signup: Some(user.enable_password_signup),
            password_signup_need_review: Some(user.password_signup_need_review),
            enable_email_signup: Some(user.enable_email_signup),
            email_signup_need_review: Some(user.email_signup_need_review),
            enable_webauthn_signup: Some(user.enable_webauthn_signup),
            webauthn_signup_need_review: Some(user.webauthn_signup_need_review),
            enable_guest: Some(user.enable_guest),
        }),
        oauth2: Some(OAuth2SettingsPatch {
            providers: Some(oauth2_provider_configs_from_admin_proto(
                oauth2.providers,
                None,
            )?),
            allowed_redirect_urls: Some(oauth2.allowed_redirect_urls),
        }),
        rtmp: Some(RtmpSettingsPatch {
            advertise_address: Some(match rtmp.advertise_address {
                Some(address) => OptionalConfigPatch::Set(address),
                None => OptionalConfigPatch::Clear,
            }),
            ts_disguised_as_png: Some(rtmp.ts_disguised_as_png),
        }),
        email: Some(EmailSettingsPatch {
            enabled: Some(email.enabled),
            smtp_host: Some(match email.smtp_host {
                Some(host) => OptionalConfigPatch::Set(host),
                None => OptionalConfigPatch::Clear,
            }),
            smtp_port: Some(email.smtp_port),
            smtp_credentials: Some(smtp_credentials),
            smtp_proxy: Some(smtp_proxy),
            use_tls: Some(email.use_tls),
            from_email: Some(match email.from_email {
                Some(from_email) => OptionalConfigPatch::Set(from_email),
                None => OptionalConfigPatch::Clear,
            }),
            from_name: Some(email.from_name),
            whitelist_enabled: Some(email.whitelist_enabled),
            whitelist_domains: Some(email.whitelist_domains),
        }),
        webrtc: Some(WebRtcSettingsPatch {
            external_ice_servers: Some(
                webrtc
                    .external_ice_servers
                    .into_iter()
                    .map(core_ice_server)
                    .collect(),
            ),
            max_voice_participants_per_room: Some(webrtc.max_voice_participants_per_room),
        }),
        chat: Some(ChatSettingsPatch {
            max_messages_per_room: Some(chat.max_messages_per_room),
            max_pinned_messages_per_room: Some(chat.max_pinned_messages_per_room),
            message_retention_days: Some(chat.message_retention_days),
        }),
        playback_history: Some(PlaybackHistorySettingsPatch {
            retention_days: Some(playback_history.retention_days),
            max_entries_per_room: Some(playback_history.max_entries_per_room),
        }),
        cors: Some(CorsSettingsPatch {
            allowed_origins: Some(cors.allowed_origins),
        }),
    })
}

fn required_mask_value<T>(value: Option<T>, path: &str) -> Result<T, ApiError> {
    value.ok_or_else(|| ApiError::InvalidInput(format!("{path} is required by update_mask")))
}

fn select_runtime_settings_patch(
    mut source: RuntimeSettingsPatch,
    paths: &[String],
) -> Result<RuntimeSettingsPatch, ApiError> {
    if paths.is_empty() {
        return Err(ApiError::InvalidInput(
            "update_mask.paths must not be empty".to_string(),
        ));
    }

    let mut seen = BTreeSet::new();
    let mut selected = RuntimeSettingsPatch::default();

    macro_rules! select_required {
        ($section:ident, $field:ident, $path:expr) => {{
            let value = source
                .$section
                .as_mut()
                .and_then(|section| section.$field.take());
            selected
                .$section
                .get_or_insert_with(Default::default)
                .$field = Some(required_mask_value(value, $path)?);
        }};
    }

    macro_rules! select_optional {
        ($section:ident, $field:ident) => {{
            let value = source
                .$section
                .as_mut()
                .and_then(|section| section.$field.take())
                .unwrap_or(OptionalConfigPatch::Clear);
            selected
                .$section
                .get_or_insert_with(Default::default)
                .$field = Some(value);
        }};
    }

    for path in paths {
        if !seen.insert(path.as_str()) {
            return Err(ApiError::InvalidInput(format!(
                "duplicate update_mask path '{path}'"
            )));
        }

        match path.as_str() {
            "server.name" => select_required!(server, name, path),
            "room_defaults.default_max_members" => {
                select_required!(room_defaults, default_max_members, path);
            }
            "room_defaults.default_max_chat_messages" => {
                select_required!(room_defaults, default_max_chat_messages, path);
            }
            "permissions.admin_default_permissions" => {
                select_required!(permissions, admin_default_permissions, path);
            }
            "permissions.member_default_permissions" => {
                select_required!(permissions, member_default_permissions, path);
            }
            "permissions.guest_default_permissions" => {
                select_required!(permissions, guest_default_permissions, path);
            }
            "room_creation.enabled" => select_required!(room_creation, enabled, path),
            "room_creation.approval_required" => {
                select_required!(room_creation, approval_required, path);
            }
            "room_creation.password_policy" => {
                select_required!(room_creation, password_policy, path);
            }
            "room_creation.max_rooms_per_user" => {
                select_required!(room_creation, max_rooms_per_user, path);
            }
            "user.enable_password_signup" => {
                select_required!(user, enable_password_signup, path);
            }
            "user.password_signup_need_review" => {
                select_required!(user, password_signup_need_review, path);
            }
            "user.enable_email_signup" => select_required!(user, enable_email_signup, path),
            "user.email_signup_need_review" => {
                select_required!(user, email_signup_need_review, path);
            }
            "user.enable_webauthn_signup" => {
                select_required!(user, enable_webauthn_signup, path);
            }
            "user.webauthn_signup_need_review" => {
                select_required!(user, webauthn_signup_need_review, path);
            }
            "user.enable_guest" => select_required!(user, enable_guest, path),
            "oauth2.providers" => select_required!(oauth2, providers, path),
            "oauth2.allowedRedirectUrls" | "oauth2.allowed_redirect_urls" => {
                select_required!(oauth2, allowed_redirect_urls, path);
            }
            "rtmp.advertise_address" => select_optional!(rtmp, advertise_address),
            "rtmp.ts_disguised_as_png" => {
                select_required!(rtmp, ts_disguised_as_png, path);
            }
            "email.enabled" => select_required!(email, enabled, path),
            "email.smtp_host" => select_optional!(email, smtp_host),
            "email.smtp_port" => select_required!(email, smtp_port, path),
            "email.smtp_credentials" => select_optional!(email, smtp_credentials),
            "email.smtp_proxy" => select_optional!(email, smtp_proxy),
            "email.use_tls" => select_required!(email, use_tls, path),
            "email.from_email" => select_optional!(email, from_email),
            "email.from_name" => select_required!(email, from_name, path),
            "email.whitelist_enabled" => {
                select_required!(email, whitelist_enabled, path);
            }
            "email.whitelist_domains" => {
                select_required!(email, whitelist_domains, path);
            }
            "webrtc.external_ice_servers" => {
                select_required!(webrtc, external_ice_servers, path);
            }
            "webrtc.max_voice_participants_per_room" => {
                select_required!(webrtc, max_voice_participants_per_room, path);
            }
            "chat.max_messages_per_room" => {
                select_required!(chat, max_messages_per_room, path);
            }
            "chat.max_pinned_messages_per_room" => {
                select_required!(chat, max_pinned_messages_per_room, path);
            }
            "chat.message_retention_days" => {
                select_required!(chat, message_retention_days, path);
            }
            "playback_history.retention_days" => {
                select_required!(playback_history, retention_days, path);
            }
            "playback_history.max_entries_per_room" => {
                select_required!(playback_history, max_entries_per_room, path);
            }
            "cors.allowed_origins" => select_required!(cors, allowed_origins, path),
            _ => {
                return Err(ApiError::InvalidInput(format!(
                    "unsupported update_mask path '{path}'"
                )))
            }
        }
    }

    Ok(selected)
}

pub fn room_settings_patch_from_admin_proto(
    req: &admin_proto::UpdateRoomSettingsRequest,
) -> Result<RoomSettingsUpdatePatch, ApiError> {
    crate::impls::validate_proto_request(req)?;
    let settings = req
        .settings
        .ok_or_else(|| ApiError::InvalidInput("settings is required".to_string()))?;
    let paths = &req
        .update_mask
        .as_ref()
        .ok_or_else(|| ApiError::InvalidInput("update_mask is required".to_string()))?
        .paths;
    let patch = crate::room_settings_mapping::select_room_settings_patch(settings, paths)?;
    room_settings_patch_from_client_proto(patch)
}

fn email_settings_patch_from_admin_proto(
    patch: admin_proto::EmailSettingsPatch,
) -> EmailSettingsPatch {
    EmailSettingsPatch {
        enabled: patch.enabled,
        smtp_host: patch.smtp_host.map(OptionalConfigPatch::Set),
        smtp_port: patch.smtp_port,
        smtp_credentials: patch.smtp_credentials.map(|credentials| {
            OptionalConfigPatch::Set(SmtpCredentialsInput {
                username: credentials.username,
                password: credentials.password,
            })
        }),
        smtp_proxy: patch.smtp_proxy.map(|proxy| {
            OptionalConfigPatch::Set(SmtpProxyInput {
                url: proxy.url,
                credentials: proxy.credentials.map(|credentials| SmtpCredentialsInput {
                    username: credentials.username,
                    password: credentials.password,
                }),
            })
        }),
        use_tls: patch.use_tls,
        from_email: patch.from_email.map(OptionalConfigPatch::Set),
        from_name: patch.from_name,
        whitelist_enabled: patch.whitelist_enabled,
        whitelist_domains: Some(patch.whitelist_domains),
    }
}

fn room_settings_patch_from_client_proto(
    patch: client_proto::RoomSettingsPatch,
) -> Result<RoomSettingsUpdatePatch, ApiError> {
    Ok(RoomSettingsUpdatePatch {
        allow_guest_join: patch.allow_guest_join,
        max_members: patch.max_members,
        require_approval: patch.require_approval,
        allow_auto_join: patch.allow_auto_join,
        chat_enabled: patch.chat_enabled,
        voice_chat_enabled: patch.voice_chat_enabled,
        p2p_media_enabled: patch.p2p_media_enabled,
        auto_play: patch
            .auto_play
            .map(auto_play_patch_from_client_proto)
            .transpose()?,
        admin_added_permissions: patch.admin_added_permissions,
        admin_removed_permissions: patch.admin_removed_permissions,
        member_added_permissions: patch.member_added_permissions,
        member_removed_permissions: patch.member_removed_permissions,
        guest_added_permissions: patch.guest_added_permissions,
        guest_removed_permissions: patch.guest_removed_permissions,
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
    current: Option<&OAuth2ProviderConfigs>,
) -> Result<OAuth2SettingsPatch, ApiError> {
    Ok(OAuth2SettingsPatch {
        providers: Some(oauth2_provider_configs_from_admin_proto(
            patch.providers,
            current,
        )?),
        allowed_redirect_urls: Some(patch.allowed_redirect_urls),
    })
}

fn oauth2_provider_configs_from_admin_proto(
    providers: Vec<admin_proto::OAuth2ProviderSettings>,
    current: Option<&OAuth2ProviderConfigs>,
) -> Result<OAuth2ProviderConfigs, ApiError> {
    let mut configs = BTreeMap::new();
    for provider in providers {
        let name = provider.name.clone();
        let config = oauth2_config_from_admin_proto(
            &name,
            &provider,
            current.and_then(|configs| configs.0.get(&name)),
        )?;
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
    current: Option<&OAuth2ProviderConfig>,
) -> Result<OAuth2ProviderPrivateConfig, ApiError> {
    use admin_proto::o_auth2_provider_settings::Config;
    Ok(
        match provider.config.clone().ok_or_else(|| {
            ApiError::InvalidInput(format!("OAuth2 provider '{name}' config is required"))
        })? {
            Config::Qq(config) => {
                let current_secret = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::Qq(current) => {
                        Some((current.client_id.as_str(), current.client_secret.as_str()))
                    }
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::Qq(OAuth2QqProviderConfig {
                    client_secret: resolve_oauth2_client_secret(
                        name,
                        "qq",
                        &config.client_id,
                        config.client_secret,
                        current_secret,
                    )?,
                    client_id: config.client_id,
                })
            }
            Config::Github(config) => {
                let current_secret = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::GitHub(current) => {
                        Some((current.client_id.as_str(), current.client_secret.as_str()))
                    }
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::GitHub(OAuth2GithubProviderConfig {
                    client_secret: resolve_oauth2_client_secret(
                        name,
                        "github",
                        &config.client_id,
                        config.client_secret,
                        current_secret,
                    )?,
                    client_id: config.client_id,
                })
            }
            Config::Google(config) => {
                let current_secret = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::Google(current) => {
                        Some((current.client_id.as_str(), current.client_secret.as_str()))
                    }
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::Google(OAuth2GoogleProviderConfig {
                    client_secret: resolve_oauth2_client_secret(
                        name,
                        "google",
                        &config.client_id,
                        config.client_secret,
                        current_secret,
                    )?,
                    client_id: config.client_id,
                })
            }
            Config::Microsoft(config) => {
                let current_secret = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::Microsoft(current) => {
                        Some((current.client_id.as_str(), current.client_secret.as_str()))
                    }
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::Microsoft(OAuth2MicrosoftProviderConfig {
                    client_secret: resolve_oauth2_client_secret(
                        name,
                        "microsoft",
                        &config.client_id,
                        config.client_secret,
                        current_secret,
                    )?,
                    client_id: config.client_id,
                    tenant: microsoft_tenant_or_default(config.tenant),
                })
            }
            Config::Discord(config) => {
                let current_secret = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::Discord(current) => {
                        Some((current.client_id.as_str(), current.client_secret.as_str()))
                    }
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::Discord(OAuth2DiscordProviderConfig {
                    client_secret: resolve_oauth2_client_secret(
                        name,
                        "discord",
                        &config.client_id,
                        config.client_secret,
                        current_secret,
                    )?,
                    client_id: config.client_id,
                })
            }
            Config::Logto(config) => {
                let current_secret = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::Logto(current) => {
                        Some((current.client_id.as_str(), current.client_secret.as_str()))
                    }
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::Logto(OAuth2LogtoProviderConfig {
                    client_secret: resolve_oauth2_client_secret(
                        name,
                        "logto",
                        &config.client_id,
                        config.client_secret,
                        current_secret,
                    )?,
                    client_id: config.client_id,
                    endpoint: config.endpoint,
                })
            }
            Config::Oidc(config) => {
                let current_secret = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::Oidc(current) => {
                        Some((current.client_id.as_str(), current.client_secret.as_str()))
                    }
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::Oidc(OAuth2OidcProviderConfig {
                    client_secret: resolve_oauth2_client_secret(
                        name,
                        "oidc",
                        &config.client_id,
                        config.client_secret,
                        current_secret,
                    )?,
                    client_id: config.client_id,
                    issuer: config.issuer,
                    auth_url: config.auth_url,
                    token_url: config.token_url,
                    userinfo_url: config.userinfo_url,
                    jwks_url: config.jwks_url,
                    scopes: config.scopes,
                })
            }
            Config::Casdoor(config) => {
                let current_secret = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::Casdoor(current) => {
                        Some((current.client_id.as_str(), current.client_secret.as_str()))
                    }
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::Casdoor(OAuth2CasdoorProviderConfig {
                    client_secret: resolve_oauth2_client_secret(
                        name,
                        "casdoor",
                        &config.client_id,
                        config.client_secret,
                        current_secret,
                    )?,
                    client_id: config.client_id,
                    issuer: config.issuer,
                    auth_url: config.auth_url,
                    token_url: config.token_url,
                    userinfo_url: config.userinfo_url,
                    jwks_url: config.jwks_url,
                })
            }
            Config::Apple(config) => {
                let current_credentials = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::Apple(current) => Some(current),
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::Apple(OAuth2AppleProviderConfig {
                    web_client_secret: resolve_oauth2_client_secret(
                        name,
                        "apple.web",
                        &config.web_client_id,
                        config.web_client_secret,
                        current_credentials.map(|current| {
                            (
                                current.web_client_id.as_str(),
                                current.web_client_secret.as_str(),
                            )
                        }),
                    )?,
                    web_client_id: config.web_client_id,
                    native_client_secret: resolve_oauth2_client_secret(
                        name,
                        "apple.native",
                        &config.native_client_id,
                        config.native_client_secret,
                        current_credentials.map(|current| {
                            (
                                current.native_client_id.as_str(),
                                current.native_client_secret.as_str(),
                            )
                        }),
                    )?,
                    native_client_id: config.native_client_id,
                })
            }
            Config::Feishu(config) => {
                let current_secret = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::Feishu(current) => {
                        Some((current.client_id.as_str(), current.client_secret.as_str()))
                    }
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::Feishu(OAuth2FeishuProviderConfig {
                    client_secret: resolve_oauth2_client_secret(
                        name,
                        "feishu",
                        &config.client_id,
                        config.client_secret,
                        current_secret,
                    )?,
                    client_id: config.client_id,
                    endpoint: config.endpoint,
                })
            }
            Config::Gitee(config) => {
                let current_secret = current.and_then(|provider| match &provider.config {
                    OAuth2ProviderPrivateConfig::Gitee(current) => {
                        Some((current.client_id.as_str(), current.client_secret.as_str()))
                    }
                    _ => None,
                });
                OAuth2ProviderPrivateConfig::Gitee(OAuth2GiteeProviderConfig {
                    client_secret: resolve_oauth2_client_secret(
                        name,
                        "gitee",
                        &config.client_id,
                        config.client_secret,
                        current_secret,
                    )?,
                    client_id: config.client_id,
                })
            }
        },
    )
}

fn resolve_oauth2_client_secret(
    name: &str,
    provider_type: &str,
    client_id: &str,
    supplied: Option<String>,
    current: Option<(&str, &str)>,
) -> Result<String, ApiError> {
    if client_id.trim().is_empty() {
        return Ok(String::new());
    }
    if let Some(secret) = supplied {
        return Ok(secret);
    }
    if let Some((current_client_id, current_secret)) = current {
        if current_client_id == client_id {
            return Ok(current_secret.to_string());
        }
    }
    Err(ApiError::InvalidInput(format!(
        "OAuth2 provider '{name}' {provider_type}.client_secret is required for a new provider, provider type change, or client_id change"
    )))
}

fn microsoft_tenant_or_default(tenant: String) -> String {
    if tenant.trim().is_empty() {
        "common".to_string()
    } else {
        tenant
    }
}

fn core_room_password_policy(value: i32) -> Result<RoomPasswordPolicy, ApiError> {
    match synctv_proto::common::RoomPasswordPolicy::try_from(value) {
        Ok(synctv_proto::common::RoomPasswordPolicy::Optional) => Ok(RoomPasswordPolicy::Optional),
        Ok(synctv_proto::common::RoomPasswordPolicy::Required) => Ok(RoomPasswordPolicy::Required),
        Ok(synctv_proto::common::RoomPasswordPolicy::Forbidden) => {
            Ok(RoomPasswordPolicy::Forbidden)
        }
        Ok(synctv_proto::common::RoomPasswordPolicy::Unspecified) | Err(_) => Err(
            ApiError::InvalidInput("room_creation password policy is required".to_string()),
        ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use admin_proto::o_auth2_provider_settings::Config;

    #[test]
    fn apple_native_only_config_skips_missing_web_secret() {
        let provider = admin_proto::OAuth2ProviderSettings {
            name: "apple-native".to_string(),
            enable_signup: true,
            signup_need_review: false,
            config: Some(Config::Apple(admin_proto::OAuth2AppleProviderConfig {
                web_client_id: String::new(),
                web_client_secret: None,
                native_client_id: "org.example.app".to_string(),
                native_client_secret: Some("native-secret".to_string()),
            })),
        };

        let mapped = oauth2_config_from_admin_proto("apple-native", &provider, None)
            .expect("native-only Apple config should map");
        let OAuth2ProviderPrivateConfig::Apple(mapped) = mapped else {
            panic!("expected Apple provider config");
        };

        assert!(mapped.web_client_secret.is_empty());
        assert_eq!(mapped.native_client_secret, "native-secret");
    }

    #[test]
    fn microsoft_empty_tenant_uses_common_default() {
        let provider = admin_proto::OAuth2ProviderSettings {
            name: "microsoft".to_string(),
            enable_signup: true,
            signup_need_review: false,
            config: Some(Config::Microsoft(
                admin_proto::OAuth2MicrosoftProviderConfig {
                    client_id: "client-id".to_string(),
                    client_secret: Some("client-secret".to_string()),
                    tenant: String::new(),
                },
            )),
        };

        let mapped = oauth2_config_from_admin_proto("microsoft", &provider, None)
            .expect("Microsoft config should map");
        let OAuth2ProviderPrivateConfig::Microsoft(mapped) = mapped else {
            panic!("expected Microsoft provider config");
        };

        assert_eq!(mapped.tenant, "common");
    }
}
