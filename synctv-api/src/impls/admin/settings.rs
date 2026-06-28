use synctv_core::{
    models::{AuditDetails, UserId},
    provider::ExecutionControl,
    Error as CoreError,
};

use crate::impls::client::convert::{apply_room_settings_patch_from_proto, room_settings_to_proto};

use super::{AdminApiImpl, ApiError, RequestContext};

type SettingsMap = std::collections::BTreeMap<String, String>;

fn required_setting<'a>(effective: &'a SettingsMap, key: &str) -> Result<&'a str, ApiError> {
    effective
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| ApiError::Internal(format!("Missing effective setting '{key}'")))
}

fn parse_setting<T>(effective: &SettingsMap, key: &str) -> Result<T, ApiError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    required_setting(effective, key)?
        .parse::<T>()
        .map_err(|error| ApiError::Internal(format!("Invalid effective setting '{key}': {error}")))
}

fn string_setting(effective: &SettingsMap, key: &str) -> Result<String, ApiError> {
    Ok(required_setting(effective, key)?.to_string())
}

fn permission_bits_setting(effective: &SettingsMap, key: &str) -> Result<u64, ApiError> {
    let permissions: synctv_core::service::global_settings::PermissionSet =
        parse_setting(effective, key)?;
    Ok(permissions.bits().bits())
}

fn permission_bits_to_setting_value(bits: u64) -> Result<String, ApiError> {
    const NAMED_PERMISSIONS: &[(&str, u64)] = &[
        ("chat", synctv_core::models::RoomAdminPermissionBits::CHAT),
        (
            "create_media_resource",
            synctv_core::models::RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE,
        ),
        (
            "view_media_resources",
            synctv_core::models::RoomAdminPermissionBits::VIEW_MEDIA_RESOURCES,
        ),
        (
            "view_member_list",
            synctv_core::models::RoomAdminPermissionBits::VIEW_MEMBER_LIST,
        ),
        (
            "view_chat_history",
            synctv_core::models::RoomAdminPermissionBits::VIEW_CHAT_HISTORY,
        ),
        (
            "use_webrtc",
            synctv_core::models::RoomAdminPermissionBits::USE_WEBRTC,
        ),
        (
            "delete_media_resource_any",
            synctv_core::models::RoomAdminPermissionBits::DELETE_MEDIA_RESOURCE_ANY,
        ),
        (
            "reorder_media_resources",
            synctv_core::models::RoomAdminPermissionBits::REORDER_MEDIA_RESOURCES,
        ),
        (
            "clear_media_resources",
            synctv_core::models::RoomAdminPermissionBits::CLEAR_MEDIA_RESOURCES,
        ),
        (
            "live_control",
            synctv_core::models::RoomAdminPermissionBits::LIVE_CONTROL,
        ),
        (
            "play_control",
            synctv_core::models::RoomAdminPermissionBits::PLAY_CONTROL,
        ),
        (
            "change_current_media",
            synctv_core::models::RoomAdminPermissionBits::CHANGE_CURRENT_MEDIA,
        ),
        (
            "change_playback_rate",
            synctv_core::models::RoomAdminPermissionBits::CHANGE_PLAYBACK_RATE,
        ),
        (
            "approve_member",
            synctv_core::models::RoomAdminPermissionBits::APPROVE_MEMBER,
        ),
        (
            "kick_member",
            synctv_core::models::RoomAdminPermissionBits::KICK_MEMBER,
        ),
        (
            "set_member_permissions",
            synctv_core::models::RoomAdminPermissionBits::SET_MEMBER_PERMISSIONS,
        ),
        (
            "add_member",
            synctv_core::models::RoomAdminPermissionBits::ADD_MEMBER,
        ),
        (
            "set_room_settings",
            synctv_core::models::RoomAdminPermissionBits::SET_ROOM_SETTINGS,
        ),
        (
            "delete_chat",
            synctv_core::models::RoomAdminPermissionBits::DELETE_CHAT,
        ),
        (
            "delete_room",
            synctv_core::models::RoomAdminPermissionBits::DELETE_ROOM,
        ),
    ];

    let names = NAMED_PERMISSIONS
        .iter()
        .filter_map(|(name, bit)| ((bits & *bit) != 0).then_some(*name))
        .collect::<Vec<_>>();
    serde_json::to_string(&names).map_err(|error| {
        ApiError::Internal(format!(
            "Failed to encode permission setting value: {error}"
        ))
    })
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

fn core_room_password_policy(
    value: i32,
) -> Result<synctv_core::service::RoomPasswordPolicy, ApiError> {
    match synctv_proto::admin::RoomPasswordPolicy::try_from(value) {
        Ok(synctv_proto::admin::RoomPasswordPolicy::Optional) => {
            Ok(synctv_core::service::RoomPasswordPolicy::Optional)
        }
        Ok(synctv_proto::admin::RoomPasswordPolicy::Required) => {
            Ok(synctv_core::service::RoomPasswordPolicy::Required)
        }
        Ok(synctv_proto::admin::RoomPasswordPolicy::Forbidden) => {
            Ok(synctv_core::service::RoomPasswordPolicy::Forbidden)
        }
        Ok(synctv_proto::admin::RoomPasswordPolicy::Unspecified) | Err(_) => Err(
            ApiError::InvalidInput("room password policy is required".to_string()),
        ),
    }
}

fn proto_ice_server(
    server: synctv_core::service::ConfiguredIceServer,
) -> synctv_proto::client::IceServer {
    synctv_proto::client::IceServer {
        urls: server.urls,
        username: server.username,
        credential: server.credential,
    }
}

fn core_ice_server(
    server: synctv_proto::client::IceServer,
) -> synctv_core::service::ConfiguredIceServer {
    synctv_core::service::ConfiguredIceServer {
        urls: server.urls,
        username: server.username,
        credential: server.credential,
    }
}

fn comma_join(values: &[String]) -> String {
    values.join(",")
}

fn oauth2_basic_config_to_proto(
    config: &synctv_core::service::OAuth2BasicProviderConfig,
) -> synctv_proto::admin::OAuth2BasicProviderConfig {
    synctv_proto::admin::OAuth2BasicProviderConfig {
        client_id: config.client_id.clone(),
        client_secret: config.client_secret.clone(),
        redirect_url: config.redirect_url.clone(),
    }
}

fn oauth2_basic_config_from_proto(
    config: synctv_proto::admin::OAuth2BasicProviderConfig,
) -> synctv_core::service::OAuth2BasicProviderConfig {
    synctv_core::service::OAuth2BasicProviderConfig {
        client_id: config.client_id,
        client_secret: config.client_secret,
        redirect_url: config.redirect_url,
    }
}

fn oauth2_config_to_proto(
    config: &synctv_core::service::OAuth2ProviderPrivateConfig,
) -> synctv_proto::admin::o_auth2_provider_settings::Config {
    use synctv_core::service::OAuth2ProviderPrivateConfig as CoreConfig;
    use synctv_proto::admin::o_auth2_provider_settings::Config;

    match config {
        CoreConfig::GitHub(config) => Config::Github(oauth2_basic_config_to_proto(config)),
        CoreConfig::Google(config) => Config::Google(oauth2_basic_config_to_proto(config)),
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
        }),
        CoreConfig::Casdoor(config) => {
            Config::Casdoor(synctv_proto::admin::OAuth2OidcProviderConfig {
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
    }
}

fn oauth2_config_from_proto(
    instance_name: &str,
    config: Option<synctv_proto::admin::o_auth2_provider_settings::Config>,
) -> Result<synctv_core::service::OAuth2ProviderPrivateConfig, ApiError> {
    use synctv_core::service::OAuth2ProviderPrivateConfig as CoreConfig;
    use synctv_proto::admin::o_auth2_provider_settings::Config;

    let config = config.ok_or_else(|| {
        ApiError::InvalidInput(format!(
            "OAuth2 provider '{instance_name}' config is required"
        ))
    })?;
    Ok(match config {
        Config::Github(config) => CoreConfig::GitHub(oauth2_basic_config_from_proto(config)),
        Config::Google(config) => CoreConfig::Google(oauth2_basic_config_from_proto(config)),
        Config::Logto(config) => {
            CoreConfig::Logto(synctv_core::service::OAuth2LogtoProviderConfig {
                client_id: config.client_id,
                client_secret: config.client_secret,
                redirect_url: config.redirect_url,
                endpoint: config.endpoint,
            })
        }
        Config::Oidc(config) => CoreConfig::Oidc(synctv_core::service::OAuth2OidcProviderConfig {
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
            CoreConfig::Casdoor(synctv_core::service::OAuth2OidcProviderConfig {
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
    })
}

impl AdminApiImpl {
    fn effective_settings_by_key(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, ApiError> {
        let mut effective = std::collections::BTreeMap::new();
        let mut registered_keys = None;
        let mut visible_keys = None;

        if let Some(registry) = &self.settings_registry {
            let defaults = registry
                .storage
                .registered_defaults()
                .map_err(ApiError::from)?;
            visible_keys = Some(
                defaults
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect::<std::collections::HashSet<_>>(),
            );
            effective.extend(defaults);
            registered_keys = Some(
                registry
                    .storage
                    .registered_keys()
                    .into_iter()
                    .collect::<std::collections::HashSet<_>>(),
            );
        }

        for setting in self.settings_service.get_all().map_err(ApiError::from)? {
            if let Some(registered_keys) = &registered_keys {
                if visible_keys
                    .as_ref()
                    .is_some_and(|visible_keys| visible_keys.contains(&setting.key))
                {
                    effective.insert(setting.key, setting.value);
                    continue;
                }
                if registered_keys.contains(&setting.key) {
                    continue;
                }
                tracing::warn!(
                    key = %setting.key,
                    group = %setting.group_name,
                    "Ignoring unsupported persisted setting during admin settings projection"
                );
                continue;
            }
            effective.insert(setting.key, setting.value);
        }

        Ok(effective)
    }

    fn settings_group(
        name: &str,
        settings: synctv_proto::admin::settings_group::Settings,
    ) -> synctv_proto::admin::SettingsGroup {
        synctv_proto::admin::SettingsGroup {
            name: name.to_string(),
            settings: Some(settings),
        }
    }

    fn project_settings_groups(
        effective: &SettingsMap,
    ) -> Result<Vec<synctv_proto::admin::SettingsGroup>, ApiError> {
        use synctv_proto::admin::settings_group::Settings;

        let oauth2_providers: synctv_core::service::OAuth2ProviderConfigs =
            parse_setting(effective, "oauth2.providers")?;
        let external_ice_servers: synctv_core::service::IceServerList =
            parse_setting(effective, "webrtc.external_ice_servers")?;
        let cors_allowed_origins: synctv_core::service::global_settings::CorsAllowedOrigins =
            parse_setting(effective, "cors.allowed_origins")?;

        Ok(vec![
            Self::settings_group(
                "server",
                Settings::Server(synctv_proto::admin::ServerSettings {
                    allow_room_creation: parse_setting(effective, "server.allow_room_creation")?,
                    max_rooms_per_user: parse_setting(effective, "server.max_rooms_per_user")?,
                    max_members_per_room: parse_setting(effective, "server.max_members_per_room")?,
                    max_chat_messages: parse_setting(effective, "server.max_chat_messages")?,
                }),
            ),
            Self::settings_group(
                "permissions",
                Settings::Permissions(synctv_proto::admin::PermissionSettings {
                    admin_default_permissions: permission_bits_setting(
                        effective,
                        "permissions.admin_default",
                    )?,
                    member_default_permissions: permission_bits_setting(
                        effective,
                        "permissions.member_default",
                    )?,
                    guest_default_permissions: permission_bits_setting(
                        effective,
                        "permissions.guest_default",
                    )?,
                }),
            ),
            Self::settings_group(
                "room",
                Settings::Room(synctv_proto::admin::RoomPolicySettings {
                    disable_create_room: parse_setting(effective, "room.disable_create_room")?,
                    create_room_need_review: parse_setting(
                        effective,
                        "room.create_room_need_review",
                    )?,
                    password_policy: proto_room_password_policy(parse_setting(
                        effective,
                        "room.password_policy",
                    )?) as i32,
                }),
            ),
            Self::settings_group(
                "user",
                Settings::User(synctv_proto::admin::UserSettings {
                    enable_password_signup: parse_setting(
                        effective,
                        "user.enable_password_signup",
                    )?,
                    password_signup_need_review: parse_setting(
                        effective,
                        "user.password_signup_need_review",
                    )?,
                    enable_email_signup: parse_setting(effective, "user.enable_email_signup")?,
                    email_signup_need_review: parse_setting(
                        effective,
                        "user.email_signup_need_review",
                    )?,
                    enable_webauthn_signup: parse_setting(
                        effective,
                        "user.enable_webauthn_signup",
                    )?,
                    webauthn_signup_need_review: parse_setting(
                        effective,
                        "user.webauthn_signup_need_review",
                    )?,
                    enable_guest: parse_setting(effective, "user.enable_guest")?,
                }),
            ),
            Self::settings_group(
                "oauth2",
                Settings::Oauth2(synctv_proto::admin::OAuth2Settings {
                    providers: oauth2_providers
                        .0
                        .into_iter()
                        .map(|(instance_name, provider)| {
                            synctv_proto::admin::OAuth2ProviderSettings {
                                instance_name,
                                enable_signup: provider.enable_signup,
                                signup_need_review: provider.signup_need_review,
                                config: Some(oauth2_config_to_proto(&provider.config)),
                            }
                        })
                        .collect(),
                }),
            ),
            Self::settings_group(
                "proxy",
                Settings::Proxy(synctv_proto::admin::ProxySettings {
                    movie_proxy: parse_setting(effective, "proxy.movie_proxy")?,
                    live_proxy: parse_setting(effective, "proxy.live_proxy")?,
                }),
            ),
            Self::settings_group(
                "rtmp",
                Settings::Rtmp(synctv_proto::admin::RtmpSettings {
                    custom_publish_host: string_setting(effective, "rtmp.custom_publish_host")?,
                    ts_disguised_as_png: parse_setting(effective, "rtmp.ts_disguised_as_png")?,
                }),
            ),
            Self::settings_group(
                "email",
                Settings::Email(synctv_proto::admin::EmailSettings {
                    enabled: parse_setting(effective, "email.enabled")?,
                    smtp_host: string_setting(effective, "email.smtp_host")?,
                    smtp_port: parse_setting::<u16>(effective, "email.smtp_port")?.into(),
                    smtp_username: string_setting(effective, "email.smtp_username")?,
                    smtp_password: String::new(),
                    use_tls: parse_setting(effective, "email.use_tls")?,
                    from_email: string_setting(effective, "email.from_email")?,
                    from_name: string_setting(effective, "email.from_name")?,
                    whitelist_enabled: parse_setting(effective, "email.whitelist_enabled")?,
                    whitelist_domains:
                        synctv_core::service::SettingsRegistry::normalize_email_whitelist_domains(
                            required_setting(effective, "email.whitelist")?,
                        ),
                }),
            ),
            Self::settings_group(
                "webrtc",
                Settings::Webrtc(synctv_proto::admin::WebRtcSettings {
                    external_ice_servers: external_ice_servers
                        .0
                        .into_iter()
                        .map(proto_ice_server)
                        .collect(),
                }),
            ),
            Self::settings_group(
                "chat",
                Settings::Chat(synctv_proto::admin::ChatSettings {
                    max_messages_per_room: parse_setting(effective, "chat.max_messages_per_room")?,
                    max_pinned_messages_per_room: parse_setting(
                        effective,
                        "chat.max_pinned_messages_per_room",
                    )?,
                    message_retention_days: parse_setting(
                        effective,
                        "chat.message_retention_days",
                    )?,
                }),
            ),
            Self::settings_group(
                "cors",
                Settings::Cors(synctv_proto::admin::CorsSettings {
                    allowed_origins: cors_allowed_origins.0,
                }),
            ),
        ])
    }

    fn fully_qualified_setting_updates(
        group_name: &str,
        settings: synctv_proto::admin::update_settings_request::Settings,
    ) -> Result<Vec<(String, String)>, ApiError> {
        if group_name.trim().is_empty() {
            return Err(ApiError::InvalidInput(
                "settings group must not be empty".to_string(),
            ));
        }

        let mut updates = match settings {
            synctv_proto::admin::update_settings_request::Settings::Server(settings) => vec![
                (
                    "server.allow_room_creation".to_string(),
                    settings.allow_room_creation.to_string(),
                ),
                (
                    "server.max_rooms_per_user".to_string(),
                    settings.max_rooms_per_user.to_string(),
                ),
                (
                    "server.max_members_per_room".to_string(),
                    settings.max_members_per_room.to_string(),
                ),
                (
                    "server.max_chat_messages".to_string(),
                    settings.max_chat_messages.to_string(),
                ),
            ],
            synctv_proto::admin::update_settings_request::Settings::Permissions(settings) => vec![
                (
                    "permissions.admin_default".to_string(),
                    permission_bits_to_setting_value(settings.admin_default_permissions)?,
                ),
                (
                    "permissions.member_default".to_string(),
                    permission_bits_to_setting_value(settings.member_default_permissions)?,
                ),
                (
                    "permissions.guest_default".to_string(),
                    permission_bits_to_setting_value(settings.guest_default_permissions)?,
                ),
            ],
            synctv_proto::admin::update_settings_request::Settings::Room(settings) => vec![
                (
                    "room.disable_create_room".to_string(),
                    settings.disable_create_room.to_string(),
                ),
                (
                    "room.create_room_need_review".to_string(),
                    settings.create_room_need_review.to_string(),
                ),
                (
                    "room.password_policy".to_string(),
                    core_room_password_policy(settings.password_policy)?.to_string(),
                ),
            ],
            synctv_proto::admin::update_settings_request::Settings::User(settings) => vec![
                (
                    "user.enable_password_signup".to_string(),
                    settings.enable_password_signup.to_string(),
                ),
                (
                    "user.password_signup_need_review".to_string(),
                    settings.password_signup_need_review.to_string(),
                ),
                (
                    "user.enable_email_signup".to_string(),
                    settings.enable_email_signup.to_string(),
                ),
                (
                    "user.email_signup_need_review".to_string(),
                    settings.email_signup_need_review.to_string(),
                ),
                (
                    "user.enable_webauthn_signup".to_string(),
                    settings.enable_webauthn_signup.to_string(),
                ),
                (
                    "user.webauthn_signup_need_review".to_string(),
                    settings.webauthn_signup_need_review.to_string(),
                ),
                (
                    "user.enable_guest".to_string(),
                    settings.enable_guest.to_string(),
                ),
            ],
            synctv_proto::admin::update_settings_request::Settings::Oauth2(settings) => {
                let providers = settings
                    .providers
                    .into_iter()
                    .map(|provider| {
                        let config =
                            oauth2_config_from_proto(&provider.instance_name, provider.config)?;
                        Ok((
                            provider.instance_name,
                            synctv_core::service::OAuth2ProviderConfig {
                                enable_signup: provider.enable_signup,
                                signup_need_review: provider.signup_need_review,
                                config,
                            },
                        ))
                    })
                    .collect::<Result<std::collections::BTreeMap<_, _>, ApiError>>()?;
                let configs = synctv_core::service::OAuth2ProviderConfigs(providers);
                vec![("oauth2.providers".to_string(), configs.to_string())]
            }
            synctv_proto::admin::update_settings_request::Settings::Proxy(settings) => vec![
                (
                    "proxy.movie_proxy".to_string(),
                    settings.movie_proxy.to_string(),
                ),
                (
                    "proxy.live_proxy".to_string(),
                    settings.live_proxy.to_string(),
                ),
            ],
            synctv_proto::admin::update_settings_request::Settings::Rtmp(settings) => vec![
                (
                    "rtmp.custom_publish_host".to_string(),
                    settings.custom_publish_host,
                ),
                (
                    "rtmp.ts_disguised_as_png".to_string(),
                    settings.ts_disguised_as_png.to_string(),
                ),
            ],
            synctv_proto::admin::update_settings_request::Settings::Email(settings) => vec![
                ("email.enabled".to_string(), settings.enabled.to_string()),
                ("email.smtp_host".to_string(), settings.smtp_host),
                (
                    "email.smtp_port".to_string(),
                    settings.smtp_port.to_string(),
                ),
                ("email.smtp_username".to_string(), settings.smtp_username),
                ("email.smtp_password".to_string(), settings.smtp_password),
                ("email.use_tls".to_string(), settings.use_tls.to_string()),
                ("email.from_email".to_string(), settings.from_email),
                ("email.from_name".to_string(), settings.from_name),
                (
                    "email.whitelist_enabled".to_string(),
                    settings.whitelist_enabled.to_string(),
                ),
                (
                    "email.whitelist".to_string(),
                    comma_join(&settings.whitelist_domains),
                ),
            ],
            synctv_proto::admin::update_settings_request::Settings::Webrtc(settings) => {
                let servers = synctv_core::service::IceServerList(
                    settings
                        .external_ice_servers
                        .into_iter()
                        .map(core_ice_server)
                        .collect(),
                );
                vec![(
                    "webrtc.external_ice_servers".to_string(),
                    servers.to_string(),
                )]
            }
            synctv_proto::admin::update_settings_request::Settings::Chat(settings) => vec![
                (
                    "chat.max_messages_per_room".to_string(),
                    settings.max_messages_per_room.to_string(),
                ),
                (
                    "chat.max_pinned_messages_per_room".to_string(),
                    settings.max_pinned_messages_per_room.to_string(),
                ),
                (
                    "chat.message_retention_days".to_string(),
                    settings.message_retention_days.to_string(),
                ),
            ],
            synctv_proto::admin::update_settings_request::Settings::Cors(settings) => {
                let origins = synctv_core::service::global_settings::CorsAllowedOrigins(
                    settings.allowed_origins,
                );
                vec![("cors.allowed_origins".to_string(), origins.to_string())]
            }
        };

        if !updates.iter().all(|(key, _)| {
            key.split_once('.')
                .is_some_and(|(key_group, _)| key_group == group_name)
        }) {
            return Err(ApiError::InvalidInput(format!(
                "settings body does not match group '{group_name}'"
            )));
        }
        updates.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(updates)
    }

    pub async fn get_settings(
        &self,
        _req: synctv_proto::admin::GetSettingsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::GetSettingsResponse, ApiError> {
        let group_list = Self::project_settings_groups(&self.effective_settings_by_key()?)?;
        let group_names: Vec<String> = group_list.iter().map(|g| g.name.clone()).collect();

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::SettingsViewed,
            synctv_core::models::AuditTargetType::Settings,
            None,
            AuditDetails {
                group_count: Some(group_names.len()),
                groups: group_names,
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::GetSettingsResponse { groups: group_list })
    }

    pub async fn get_settings_group(
        &self,
        req: synctv_proto::admin::GetSettingsGroupRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::GetSettingsGroupResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let requested_group = req.group.trim();

        let group = Self::project_settings_groups(&self.effective_settings_by_key()?)?
            .into_iter()
            .find(|group| group.name == requested_group)
            .ok_or_else(|| {
                ApiError::NotFound(format!("Settings group '{requested_group}' not found"))
            })?;

        let group_name = group.name.clone();

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::SettingsGroupViewed,
            synctv_core::models::AuditTargetType::Settings,
            None,
            AuditDetails {
                group: Some(group_name),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::GetSettingsGroupResponse { group: Some(group) })
    }

    pub async fn update_settings(
        &self,
        req: synctv_proto::admin::UpdateSettingsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::UpdateSettingsResponse, ApiError> {
        let group_name = req.group.trim().to_string();
        let settings = req
            .settings
            .ok_or_else(|| ApiError::InvalidInput("settings body is required".to_string()))?;
        let updates = Self::fully_qualified_setting_updates(&group_name, settings)?;
        let changed_keys: Vec<String> = updates.iter().map(|(key, _)| key.clone()).collect();

        self.settings_service
            .update_batch(updates)
            .await
            .map_err(ApiError::from)?;

        if !self.room_cache_fanout.try_publish_all_invalidation().await {
            tracing::warn!(
                group = %group_name,
                changed_keys = ?changed_keys,
                "Failed to publish global room cache invalidation after settings update"
            );
        }

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::SettingsUpdated,
            synctv_core::models::AuditTargetType::Settings,
            None,
            AuditDetails {
                changed_keys,
                ..Default::default()
            },
            ctx,
        )
        .await;

        let group = Self::project_settings_groups(&self.effective_settings_by_key()?)?
            .into_iter()
            .find(|group| group.name == group_name)
            .ok_or_else(|| {
                ApiError::NotFound(format!("Settings group '{group_name}' not found"))
            })?;

        Ok(synctv_proto::admin::UpdateSettingsResponse { group: Some(group) })
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
        let rid = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let (settings, version) = self
            .room_service
            .get_room_settings_with_version(&rid)
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
    ) -> Result<synctv_proto::admin::UpdateRoomSettingsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid =
            crate::impls::proto_validated_room_id(req.room_id.clone(), &self.public_id_codec)?;
        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let admin_username = admin_actor.username;
        let (current_settings, current_version) = self
            .room_service
            .get_room_settings_with_version(&rid)
            .await
            .map_err(ApiError::from)?;
        let settings = apply_room_settings_patch_from_proto(current_settings, req.settings)?;
        let prepared_settings_fanout = self.room_settings_fanout.prepare_settings_changed(
            &rid,
            admin_user_id,
            &admin_username,
            settings.clone(),
            current_version + 1,
        )?;
        let snapshot = self
            .room_service
            .set_room_settings_with_outbox(
                &rid,
                &settings,
                Some(prepared_settings_fanout.settings_outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        self.room_settings_fanout
            .publish_prepared_after_outbox_commit(
                prepared_settings_fanout.with_version(snapshot.version)?,
            );
        self.publish_room_cache_invalidation(&rid);

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::UpdateRoomSettingsResponse {
            room: Some(
                self.load_admin_room_proto(&room, Some(&snapshot.settings))
                    .await?,
            ),
        })
    }

    pub async fn reset_room_settings(
        &self,
        req: synctv_proto::admin::ResetRoomSettingsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::ResetRoomSettingsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let default_settings = synctv_core::models::RoomSettings::default();
        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let admin_username = admin_actor.username;
        let (_, current_version) = self
            .room_service
            .get_room_settings_with_version(&rid)
            .await
            .map_err(ApiError::from)?;
        let prepared_settings_fanout = self.room_settings_fanout.prepare_settings_changed(
            &rid,
            admin_user_id,
            &admin_username,
            default_settings.clone(),
            current_version + 1,
        )?;
        let snapshot = self
            .room_service
            .set_room_settings_with_outbox(
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
                prepared_settings_fanout.with_version(snapshot.version)?,
            );
        self.publish_room_cache_invalidation(&rid);

        Ok(synctv_proto::admin::ResetRoomSettingsResponse {
            room: Some(
                self.load_admin_room_proto(&room, Some(&snapshot.settings))
                    .await?,
            ),
        })
    }
}
