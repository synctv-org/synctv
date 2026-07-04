use synctv_core::{
    models::{AuditDetails, UserId},
    provider::ExecutionControl,
    Error as CoreError,
};

use crate::impls::client::convert::{apply_room_settings_patch_from_proto, room_settings_to_proto};

use super::{AdminApiImpl, ApiError, RequestContext};

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
            ApiError::InvalidInput("room_creation password policy is required".to_string()),
        ),
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

pub(crate) struct RuntimeSettingsPatchResult {
    pub settings: synctv_core::service::RuntimeSettings,
    pub update_mask: synctv_core::service::RuntimeSettingsUpdateMask,
}

fn changed_runtime_settings_sections(
    req: &synctv_proto::admin::UpdateSettingsRequest,
) -> Vec<String> {
    let mut sections = Vec::new();
    if req.room_defaults.is_some() {
        sections.push("roomDefaults".to_string());
    }
    if req.permissions.is_some() {
        sections.push("permissions".to_string());
    }
    if req.room_creation.is_some() {
        sections.push("roomCreation".to_string());
    }
    if req.user.is_some() {
        sections.push("user".to_string());
    }
    if req.oauth2.is_some() {
        sections.push("oauth2".to_string());
    }
    if req.proxy.is_some() {
        sections.push("proxy".to_string());
    }
    if req.rtmp.is_some() {
        sections.push("rtmp".to_string());
    }
    if req.email.is_some() {
        sections.push("email".to_string());
    }
    if req.webrtc.is_some() {
        sections.push("webrtc".to_string());
    }
    if req.chat.is_some() {
        sections.push("chat".to_string());
    }
    if req.cors.is_some() {
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

fn core_ice_server(
    room_defaults: synctv_proto::client::IceServer,
) -> synctv_core::service::ConfiguredIceServer {
    synctv_core::service::ConfiguredIceServer {
        urls: room_defaults.urls,
        username: room_defaults.username,
        credential: room_defaults.credential,
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

fn oauth2_github_config_from_proto(
    client_id: String,
    client_secret: String,
    redirect_url: String,
) -> synctv_core::service::OAuth2GithubProviderConfig {
    synctv_core::service::OAuth2GithubProviderConfig {
        client_id,
        client_secret,
        redirect_url,
    }
}

fn oauth2_google_config_from_proto(
    client_id: String,
    client_secret: String,
    redirect_url: String,
) -> synctv_core::service::OAuth2GoogleProviderConfig {
    synctv_core::service::OAuth2GoogleProviderConfig {
        client_id,
        client_secret,
        redirect_url,
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
    }
}

fn oauth2_config_from_proto(
    name: &str,
    provider: &synctv_proto::admin::OAuth2ProviderSettings,
) -> Result<synctv_core::service::OAuth2ProviderPrivateConfig, ApiError> {
    use synctv_core::service::OAuth2ProviderPrivateConfig as CoreConfig;
    use synctv_proto::admin::o_auth2_provider_settings::Config;

    Ok(
        match provider.config.clone().ok_or_else(|| {
            ApiError::InvalidInput(format!("OAuth2 provider '{name}' config is required"))
        })? {
            Config::Github(config) => CoreConfig::GitHub(oauth2_github_config_from_proto(
                config.client_id,
                config.client_secret,
                config.redirect_url,
            )),
            Config::Google(config) => CoreConfig::Google(oauth2_google_config_from_proto(
                config.client_id,
                config.client_secret,
                config.redirect_url,
            )),
            Config::Logto(config) => {
                CoreConfig::Logto(synctv_core::service::OAuth2LogtoProviderConfig {
                    client_id: config.client_id,
                    client_secret: config.client_secret,
                    redirect_url: config.redirect_url,
                    endpoint: config.endpoint,
                })
            }
            Config::Oidc(config) => {
                CoreConfig::Oidc(synctv_core::service::OAuth2OidcProviderConfig {
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
        },
    )
}

impl AdminApiImpl {
    fn runtime_settings_store(
        &self,
    ) -> Result<&synctv_core::service::RuntimeSettingsStore, ApiError> {
        self.runtime_settings_store
            .as_deref()
            .ok_or_else(|| ApiError::Internal("runtime settings store is unavailable".to_string()))
    }

    fn project_admin_settings(
        settings: synctv_core::service::RuntimeSettings,
    ) -> Result<synctv_proto::admin::RuntimeSettings, ApiError> {
        Ok(synctv_proto::admin::RuntimeSettings {
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
                smtp_username: settings.email.smtp_username,
                smtp_password: None,
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
            }),
            chat: Some(synctv_proto::admin::ChatSettings {
                max_messages_per_room: settings.chat.max_messages_per_room,
                max_pinned_messages_per_room: settings.chat.max_pinned_messages_per_room,
                message_retention_days: settings.chat.message_retention_days,
            }),
            cors: Some(synctv_proto::admin::CorsSettings {
                allowed_origins: settings.cors.allowed_origins.0,
            }),
        })
    }

    fn oauth2_provider_configs_from_proto(
        providers: Vec<synctv_proto::admin::OAuth2ProviderSettings>,
    ) -> Result<synctv_core::service::OAuth2ProviderConfigs, ApiError> {
        let mut configs = std::collections::BTreeMap::new();
        for provider in providers {
            let name = provider.name.clone();
            let config = oauth2_config_from_proto(&name, &provider)?;
            if configs
                .insert(
                    name.clone(),
                    synctv_core::service::OAuth2ProviderConfig {
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
        Ok(synctv_core::service::OAuth2ProviderConfigs(configs))
    }

    pub(crate) fn apply_runtime_settings_patch(
        mut current: synctv_core::service::RuntimeSettings,
        patch: synctv_proto::admin::UpdateSettingsRequest,
    ) -> Result<RuntimeSettingsPatchResult, ApiError> {
        let mut update_mask = synctv_core::service::RuntimeSettingsUpdateMask::default();

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
                current.room_creation.password_policy = core_room_password_policy(value)?;
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
                current.oauth2.providers =
                    Self::oauth2_provider_configs_from_proto(providers.providers)?;
                update_mask.oauth2.providers = true;
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
            if let Some(value) = rtmp.custom_publish_host {
                current.rtmp.custom_publish_host = value;
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
            if let Some(value) = email.smtp_host {
                current.email.smtp_host = value;
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
            if let Some(value) = email.smtp_username {
                current.email.smtp_username = value;
                update_mask.email.smtp_username = true;
            }
            if let Some(value) = email.smtp_password {
                current.email.smtp_password = value;
                update_mask.email.smtp_password = true;
            }
            if let Some(value) = email.use_tls {
                current.email.use_tls = value;
                update_mask.email.use_tls = true;
            }
            if let Some(value) = email.from_email {
                current.email.from_email = value;
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
                current.email.whitelist_domains = domains.values;
                update_mask.email.whitelist_domains = true;
            }
        }

        if let Some(webrtc) = patch.webrtc {
            if let Some(servers) = webrtc.external_ice_servers {
                current.webrtc.external_ice_servers = synctv_core::service::IceServerList(
                    servers.values.into_iter().map(core_ice_server).collect(),
                );
                update_mask.webrtc.external_ice_servers = true;
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

        if let Some(cors) = patch.cors {
            if let Some(origins) = cors.allowed_origins {
                current.cors.allowed_origins =
                    synctv_core::service::CorsAllowedOrigins(origins.values);
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
        _req: synctv_proto::admin::GetSettingsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::RuntimeSettings, ApiError> {
        let settings = Self::project_admin_settings(
            self.runtime_settings_store()?
                .runtime_settings()
                .map_err(ApiError::from)?,
        )?;
        let sections = [
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
        let runtime_settings_store = self.runtime_settings_store()?;
        let current = runtime_settings_store
            .runtime_settings()
            .map_err(ApiError::from)?;
        let changed_sections = changed_runtime_settings_sections(&req);
        let patch_result = Self::apply_runtime_settings_patch(current, req)?;

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
    ) -> Result<synctv_proto::admin::Room, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid =
            crate::impls::proto_validated_room_id(req.room_id.clone(), &self.public_id_codec)?;
        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let admin_username = admin_actor.username;
        let current_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let settings = apply_room_settings_patch_from_proto(
            current_settings,
            synctv_proto::client::UpdateRoomSettingsRequest {
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
            },
        )?;
        let prepared_settings_fanout = self.room_settings_fanout.prepare_settings_changed(
            &rid,
            admin_user_id,
            &admin_username,
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
    ) -> Result<synctv_proto::admin::ResetRoomSettingsResponse, ApiError> {
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
                prepared_settings_fanout
                    .with_settings_and_version(&snapshot.settings, snapshot.version)?,
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
