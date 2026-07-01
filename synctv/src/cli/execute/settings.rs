use super::*;

#[derive(Debug, serde::Serialize)]
struct SettingsSectionOutput(serde_json::Value);

impl crate::cli::human_output::ToHuman for SettingsSectionOutput {
    type Human = serde_json::Value;

    fn to_human(&self) -> Self::Human {
        self.0.clone()
    }
}

fn settings_json_value<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_else(|_| serde_json::Value::Null)
}

#[derive(Debug, Clone, Copy)]
enum SettingsSection {
    RoomDefaults,
    Permissions,
    RoomCreation,
    User,
    OAuth2,
    Proxy,
    Rtmp,
    Email,
    WebRtc,
    Chat,
    Cors,
}

impl SettingsSection {
    const NAMES: &'static [&'static str] = &[
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

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "roomDefaults" => Ok(Self::RoomDefaults),
            "permissions" => Ok(Self::Permissions),
            "roomCreation" => Ok(Self::RoomCreation),
            "user" => Ok(Self::User),
            "oauth2" => Ok(Self::OAuth2),
            "proxy" => Ok(Self::Proxy),
            "rtmp" => Ok(Self::Rtmp),
            "email" => Ok(Self::Email),
            "webrtc" => Ok(Self::WebRtc),
            "chat" => Ok(Self::Chat),
            "cors" => Ok(Self::Cors),
            _ => bail!(
                "unsupported settings group '{raw}'; expected one of: {}",
                Self::NAMES.join(", ")
            ),
        }
    }

    fn patch_label(self) -> &'static str {
        match self {
            Self::RoomDefaults => "roomDefaults settings patch",
            Self::Permissions => "permissions settings patch",
            Self::RoomCreation => "roomCreation settings patch",
            Self::User => "user settings patch",
            Self::OAuth2 => "oauth2 settings patch",
            Self::Proxy => "proxy settings patch",
            Self::Rtmp => "rtmp settings patch",
            Self::Email => "email settings patch",
            Self::WebRtc => "webrtc settings patch",
            Self::Chat => "chat settings patch",
            Self::Cors => "cors settings patch",
        }
    }
}

pub(in crate::cli) fn parse_management_settings_patch_json(
    group: &str,
    patch_json: &str,
) -> Result<synctv_proto::admin::UpdateSettingsRequest> {
    let mut request = synctv_proto::admin::UpdateSettingsRequest::default();
    let section = SettingsSection::parse(group)?;
    match section {
        SettingsSection::RoomDefaults => {
            request.room_defaults = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
        SettingsSection::Permissions => {
            request.permissions = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
        SettingsSection::RoomCreation => {
            request.room_creation = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
        SettingsSection::User => {
            request.user = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
        SettingsSection::OAuth2 => {
            request.oauth2 = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
        SettingsSection::Proxy => {
            request.proxy = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
        SettingsSection::Rtmp => {
            request.rtmp = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
        SettingsSection::Email => {
            request.email = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
        SettingsSection::WebRtc => {
            request.webrtc = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
        SettingsSection::Chat => {
            request.chat = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
        SettingsSection::Cors => {
            request.cors = Some(parse_cli_json(section.patch_label(), patch_json)?);
        }
    }
    Ok(request)
}

fn select_admin_settings_section(
    settings: &synctv_proto::admin::RuntimeSettings,
    group: &str,
) -> Result<serde_json::Value> {
    match SettingsSection::parse(group)? {
        SettingsSection::RoomDefaults => Ok(settings_json_value(&settings.room_defaults)),
        SettingsSection::Permissions => Ok(settings_json_value(&settings.permissions)),
        SettingsSection::RoomCreation => Ok(settings_json_value(&settings.room_creation)),
        SettingsSection::User => Ok(settings_json_value(&settings.user)),
        SettingsSection::OAuth2 => Ok(settings_json_value(&settings.oauth2)),
        SettingsSection::Proxy => Ok(settings_json_value(&settings.proxy)),
        SettingsSection::Rtmp => Ok(settings_json_value(&settings.rtmp)),
        SettingsSection::Email => Ok(settings_json_value(&settings.email)),
        SettingsSection::WebRtc => Ok(settings_json_value(&settings.webrtc)),
        SettingsSection::Chat => Ok(settings_json_value(&settings.chat)),
        SettingsSection::Cors => Ok(settings_json_value(&settings.cors)),
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
                "get settings",
                get_settings,
                management_proto::GetSettingsRequest {}
            )?;
            let section =
                SettingsSectionOutput(select_admin_settings_section(&response, &args.group)?);
            args.remote.print_output(&section)
        }
        SettingsSubcommand::Update(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let request = parse_management_settings_patch_json(&args.group, &args.patch_json)?;
            let response =
                management_unary_call!(session, "update settings", update_settings, request)?;
            let section =
                SettingsSectionOutput(select_admin_settings_section(&response, &args.group)?);
            args.remote.print_output(&section)
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
