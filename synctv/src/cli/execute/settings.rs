use super::*;
use std::str::FromStr;

fn typed_setting<T>(
    settings: &std::collections::HashMap<String, String>,
    key: &str,
    default: T,
    type_name: &str,
) -> Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    settings
        .get(key)
        .map(|value| {
            value
                .parse::<T>()
                .with_context(|| format!("invalid {key} {type_name}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn reject_unknown_settings(
    settings: &std::collections::HashMap<String, String>,
    allowed_keys: &[&str],
) -> Result<()> {
    for key in settings.keys() {
        if !allowed_keys.contains(&key.as_str()) {
            bail!("unknown settings field '{key}'");
        }
    }
    Ok(())
}

fn bool_setting(
    settings: &std::collections::HashMap<String, String>,
    key: &str,
    default: bool,
) -> Result<bool> {
    typed_setting(settings, key, default, "bool")
}

fn i64_setting(
    settings: &std::collections::HashMap<String, String>,
    key: &str,
    default: i64,
) -> Result<i64> {
    typed_setting(settings, key, default, "integer")
}

fn u64_setting(
    settings: &std::collections::HashMap<String, String>,
    key: &str,
    default: u64,
) -> Result<u64> {
    typed_setting(settings, key, default, "integer")
}

fn permission_bits_setting(
    settings: &std::collections::HashMap<String, String>,
    key: &str,
    default: u64,
    named_permissions: &[(&str, u64)],
) -> Result<u64> {
    settings
        .get(key)
        .map(|value| {
            value
                .parse::<u64>()
                .or_else(|_| parse_permission_names(value, named_permissions))
                .with_context(|| format!("invalid {key} permissions"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_permission_names(raw: &str, named_permissions: &[(&str, u64)]) -> Result<u64> {
    let mut bits = 0_u64;
    for raw_name in raw.split(',') {
        let name = raw_name.trim();
        if name.is_empty() {
            continue;
        }

        let bit = named_permissions
            .iter()
            .find_map(|(candidate, bit)| (*candidate == name).then_some(*bit))
            .ok_or_else(|| anyhow!("unknown permission name '{name}'"))?;
        bits |= bit;
    }
    Ok(bits)
}

fn u32_setting(
    settings: &std::collections::HashMap<String, String>,
    key: &str,
    default: u32,
) -> Result<u32> {
    typed_setting(settings, key, default, "integer")
}

fn string_setting(
    settings: &std::collections::HashMap<String, String>,
    key: &str,
    default: &str,
) -> String {
    settings
        .get(key)
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn repeated_string_setting(
    settings: &std::collections::HashMap<String, String>,
    key: &str,
) -> Vec<String> {
    settings
        .get(key)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn oauth2_provider_settings_from_proto_json(
    raw: Option<&String>,
) -> Result<Vec<synctv_proto::admin::OAuth2ProviderSettings>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let providers: Vec<synctv_proto::admin::OAuth2ProviderSettings> =
        serde_json::from_str(raw).context("invalid providers ProtoJSON")?;
    let mut seen = std::collections::BTreeSet::new();
    for provider in &providers {
        if !seen.insert(provider.instance_name.as_str()) {
            bail!(
                "duplicate oauth2 provider instanceName '{}'",
                provider.instance_name
            );
        }
    }
    Ok(providers)
}

pub(in crate::cli) fn parse_management_settings_update(
    group: &str,
    settings: &std::collections::HashMap<String, String>,
) -> Result<management_proto::update_settings_request::Settings> {
    use management_proto::update_settings_request::Settings;

    match group {
        "server" => {
            reject_unknown_settings(
                settings,
                &[
                    "allowRoomCreation",
                    "maxRoomsPerUser",
                    "maxMembersPerRoom",
                    "maxChatMessages",
                ],
            )?;
            Ok(Settings::Server(synctv_proto::admin::ServerSettings {
                allow_room_creation: bool_setting(settings, "allowRoomCreation", true)?,
                max_rooms_per_user: i64_setting(settings, "maxRoomsPerUser", 10)?,
                max_members_per_room: i64_setting(settings, "maxMembersPerRoom", 100)?,
                max_chat_messages: u64_setting(settings, "maxChatMessages", 500)?,
            }))
        }
        "permissions" => {
            reject_unknown_settings(
                settings,
                &[
                    "adminDefaultPermissions",
                    "memberDefaultPermissions",
                    "guestDefaultPermissions",
                ],
            )?;
            Ok(Settings::Permissions(
                synctv_proto::admin::PermissionSettings {
                    admin_default_permissions: permission_bits_setting(
                        settings,
                        "adminDefaultPermissions",
                        0,
                        CLI_ADMIN_NAMED_PERMISSIONS,
                    )?,
                    member_default_permissions: permission_bits_setting(
                        settings,
                        "memberDefaultPermissions",
                        0,
                        CLI_MEMBER_NAMED_PERMISSIONS,
                    )?,
                    guest_default_permissions: permission_bits_setting(
                        settings,
                        "guestDefaultPermissions",
                        0,
                        CLI_MEMBER_NAMED_PERMISSIONS,
                    )?,
                },
            ))
        }
        "room" => {
            reject_unknown_settings(
                settings,
                &[
                    "disableCreateRoom",
                    "createRoomNeedReview",
                    "passwordPolicy",
                ],
            )?;
            Ok(Settings::Room(synctv_proto::admin::RoomPolicySettings {
                disable_create_room: bool_setting(settings, "disableCreateRoom", false)?,
                create_room_need_review: bool_setting(settings, "createRoomNeedReview", false)?,
                password_policy: settings
                    .get("passwordPolicy")
                    .map(|value| match value.as_str() {
                        "optional" => Ok(synctv_proto::admin::RoomPasswordPolicy::Optional as i32),
                        "required" => Ok(synctv_proto::admin::RoomPasswordPolicy::Required as i32),
                        "forbidden" => {
                            Ok(synctv_proto::admin::RoomPasswordPolicy::Forbidden as i32)
                        }
                        _ => {
                            bail!(
                                "invalid passwordPolicy: expected optional, required, or forbidden"
                            )
                        }
                    })
                    .transpose()?
                    .unwrap_or(synctv_proto::admin::RoomPasswordPolicy::Optional as i32),
            }))
        }
        "user" => {
            reject_unknown_settings(
                settings,
                &[
                    "enablePasswordSignup",
                    "passwordSignupNeedReview",
                    "enableEmailSignup",
                    "emailSignupNeedReview",
                    "enableWebauthnSignup",
                    "webauthnSignupNeedReview",
                    "enableGuest",
                ],
            )?;
            Ok(Settings::User(synctv_proto::admin::UserSettings {
                enable_password_signup: bool_setting(settings, "enablePasswordSignup", false)?,
                password_signup_need_review: bool_setting(
                    settings,
                    "passwordSignupNeedReview",
                    false,
                )?,
                enable_email_signup: bool_setting(settings, "enableEmailSignup", false)?,
                email_signup_need_review: bool_setting(settings, "emailSignupNeedReview", false)?,
                enable_webauthn_signup: bool_setting(settings, "enableWebauthnSignup", false)?,
                webauthn_signup_need_review: bool_setting(
                    settings,
                    "webauthnSignupNeedReview",
                    false,
                )?,
                enable_guest: bool_setting(settings, "enableGuest", true)?,
            }))
        }
        "proxy" => {
            reject_unknown_settings(settings, &["movieProxy", "liveProxy"])?;
            Ok(Settings::Proxy(synctv_proto::admin::ProxySettings {
                movie_proxy: bool_setting(settings, "movieProxy", true)?,
                live_proxy: bool_setting(settings, "liveProxy", true)?,
            }))
        }
        "rtmp" => {
            reject_unknown_settings(settings, &["customPublishHost", "tsDisguisedAsPng"])?;
            Ok(Settings::Rtmp(synctv_proto::admin::RtmpSettings {
                custom_publish_host: string_setting(settings, "customPublishHost", ""),
                ts_disguised_as_png: bool_setting(settings, "tsDisguisedAsPng", false)?,
            }))
        }
        "email" => {
            reject_unknown_settings(
                settings,
                &[
                    "enabled",
                    "smtpHost",
                    "smtpPort",
                    "smtpUsername",
                    "smtpPassword",
                    "useTls",
                    "fromEmail",
                    "fromName",
                    "whitelistEnabled",
                    "whitelistDomains",
                ],
            )?;
            Ok(Settings::Email(synctv_proto::admin::EmailSettings {
                enabled: bool_setting(settings, "enabled", false)?,
                smtp_host: string_setting(settings, "smtpHost", ""),
                smtp_port: u32_setting(settings, "smtpPort", 587)?,
                smtp_username: string_setting(settings, "smtpUsername", ""),
                smtp_password: string_setting(settings, "smtpPassword", ""),
                use_tls: bool_setting(settings, "useTls", true)?,
                from_email: string_setting(settings, "fromEmail", ""),
                from_name: string_setting(settings, "fromName", "SyncTV"),
                whitelist_enabled: bool_setting(settings, "whitelistEnabled", false)?,
                whitelist_domains: repeated_string_setting(settings, "whitelistDomains"),
            }))
        }
        "oauth2" => {
            reject_unknown_settings(settings, &["providers"])?;
            Ok(Settings::Oauth2(synctv_proto::admin::OAuth2Settings {
                providers: oauth2_provider_settings_from_proto_json(settings.get("providers"))?,
            }))
        }
        "webrtc" => {
            reject_unknown_settings(settings, &["externalIceServers"])?;
            Ok(Settings::Webrtc(synctv_proto::admin::WebRtcSettings {
                external_ice_servers: settings
                    .get("externalIceServers")
                    .map(|raw| {
                        serde_json::from_str(raw).context("invalid externalIceServers ProtoJSON")
                    })
                    .transpose()?
                    .unwrap_or_default(),
            }))
        }
        "chat" => {
            reject_unknown_settings(
                settings,
                &[
                    "maxMessagesPerRoom",
                    "maxPinnedMessagesPerRoom",
                    "messageRetentionDays",
                ],
            )?;
            Ok(Settings::Chat(synctv_proto::admin::ChatSettings {
                max_messages_per_room: u64_setting(settings, "maxMessagesPerRoom", 500)?,
                max_pinned_messages_per_room: u64_setting(
                    settings,
                    "maxPinnedMessagesPerRoom",
                    20,
                )?,
                message_retention_days: i64_setting(settings, "messageRetentionDays", 90)?,
            }))
        }
        "cors" => {
            reject_unknown_settings(settings, &["allowedOrigins"])?;
            Ok(Settings::Cors(synctv_proto::admin::CorsSettings {
                allowed_origins: repeated_string_setting(settings, "allowedOrigins"),
            }))
        }
        _ => bail!("unsupported settings group '{group}'"),
    }
}

pub(super) async fn execute_settings(settings_command: SettingsCommand) -> Result<()> {
    let SettingsCommand { command } = settings_command;
    match command {
        SettingsSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get settings",
                get_settings,
                management_proto::GetSettingsRequest {}
            )?;
            args.remote.print_output(&response)
        }
        SettingsSubcommand::Get(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get settings group",
                get_settings_group,
                management_proto::GetSettingsGroupRequest { group: args.group }
            )?;
            let group = response.group.ok_or_else(|| {
                anyhow!("management settings group response did not include group")
            })?;
            args.remote.print_output(&group)
        }
        SettingsSubcommand::Update(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let raw_settings = parse_setting_entries(&args.entries)?;
            let settings = parse_management_settings_update(&args.group, &raw_settings)?;
            let response = management_unary_call!(
                session,
                "update settings",
                update_settings,
                management_proto::UpdateSettingsRequest {
                    group: args.group,
                    settings: Some(settings),
                }
            )?;
            let group = response.group.ok_or_else(|| {
                anyhow!("management update settings response did not include group")
            })?;
            args.remote.print_output(&group)
        }
        SettingsSubcommand::TestEmail(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "send test email",
                send_test_email,
                management_proto::SendTestEmailRequest { to: args.to }
            )?;
            args.remote.print_output(&response)
        }
    }
}
