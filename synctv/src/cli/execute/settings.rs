use super::*;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::path::Path;

const MAX_RUNTIME_SETTINGS_SNAPSHOT_BYTES: u64 =
    synctv_core::service::MAX_RUNTIME_SETTINGS_SNAPSHOT_BYTES as u64;

#[derive(Debug, serde::Serialize)]
struct SettingsSectionOutput(serde_json::Value);

impl crate::cli::human_output::ToHuman for SettingsSectionOutput {
    type Human = serde_json::Value;

    fn to_human(&self) -> Self::Human {
        self.0.clone()
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsImportOutput {
    applied: bool,
    changed_sections: Vec<String>,
}

impl crate::cli::human_output::ToHuman for SettingsImportOutput {
    type Human = Self;

    fn to_human(&self) -> Self::Human {
        Self {
            applied: self.applied,
            changed_sections: self.changed_sections.clone(),
        }
    }
}

fn settings_json_value<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

#[derive(Debug, Clone, Copy)]
enum SettingsSection {
    Server,
    RoomDefaults,
    Permissions,
    RoomCreation,
    User,
    OAuth2,
    Rtmp,
    Email,
    WebRtc,
    Chat,
    PlaybackHistory,
    Cors,
}

impl SettingsSection {
    const NAMES: &'static [&'static str] = &[
        "server",
        "roomDefaults",
        "permissions",
        "roomCreation",
        "user",
        "oauth2",
        "rtmp",
        "email",
        "webrtc",
        "chat",
        "playbackHistory",
        "cors",
    ];

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "server" => Ok(Self::Server),
            "roomDefaults" => Ok(Self::RoomDefaults),
            "permissions" => Ok(Self::Permissions),
            "roomCreation" => Ok(Self::RoomCreation),
            "user" => Ok(Self::User),
            "oauth2" => Ok(Self::OAuth2),
            "rtmp" => Ok(Self::Rtmp),
            "email" => Ok(Self::Email),
            "webrtc" => Ok(Self::WebRtc),
            "chat" => Ok(Self::Chat),
            "playbackHistory" => Ok(Self::PlaybackHistory),
            "cors" => Ok(Self::Cors),
            _ => bail!(
                "unsupported settings group '{raw}'; expected one of: {}",
                Self::NAMES.join(", ")
            ),
        }
    }
}

fn select_admin_settings_section(
    settings: &synctv_proto::admin::RuntimeSettings,
    group: &str,
) -> Result<serde_json::Value> {
    match SettingsSection::parse(group)? {
        SettingsSection::Server => Ok(settings_json_value(&settings.server)),
        SettingsSection::RoomDefaults => Ok(settings_json_value(&settings.room_defaults)),
        SettingsSection::Permissions => Ok(settings_json_value(&settings.permissions)),
        SettingsSection::RoomCreation => Ok(settings_json_value(&settings.room_creation)),
        SettingsSection::User => Ok(settings_json_value(&settings.user)),
        SettingsSection::OAuth2 => Ok(settings_json_value(&settings.oauth2)),
        SettingsSection::Rtmp => Ok(settings_json_value(&settings.rtmp)),
        SettingsSection::Email => Ok(settings_json_value(&settings.email)),
        SettingsSection::WebRtc => Ok(settings_json_value(&settings.webrtc)),
        SettingsSection::Chat => Ok(settings_json_value(&settings.chat)),
        SettingsSection::PlaybackHistory => Ok(settings_json_value(&settings.playback_history)),
        SettingsSection::Cors => Ok(settings_json_value(&settings.cors)),
    }
}

fn runtime_settings_snapshot_json(
    snapshot: &synctv_proto::admin::RuntimeSettingsSnapshot,
) -> Result<Vec<u8>> {
    let mut json = serde_json::to_vec_pretty(snapshot)
        .context("failed to serialize runtime settings snapshot")?;
    json.push(b'\n');
    Ok(json)
}

fn write_runtime_settings_snapshot(path: Option<&Path>, data: &[u8], force: bool) -> Result<()> {
    let Some(path) = path else {
        let stdout = io::stdout();
        let mut output = stdout.lock();
        output
            .write_all(data)
            .context("failed to write runtime settings snapshot to stdout")?;
        output.flush().context("failed to flush stdout")?;
        return Ok(());
    };

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        bail!(
            "runtime settings export directory does not exist: {}",
            parent.display()
        );
    }
    if path.exists() && !force {
        bail!(
            "runtime settings export file already exists: {}; pass --force to replace it",
            path.display()
        );
    }

    let mut temporary = tempfile::Builder::new()
        .prefix(".synctv-runtime-settings-")
        .tempfile_in(parent)
        .with_context(|| {
            format!(
                "failed to create temporary runtime settings export in {}",
                parent.display()
            )
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .context("failed to restrict runtime settings export permissions")?;
    }
    temporary
        .write_all(data)
        .context("failed to write runtime settings export")?;
    temporary
        .as_file_mut()
        .flush()
        .context("failed to flush runtime settings export")?;
    temporary
        .as_file()
        .sync_all()
        .context("failed to sync runtime settings export")?;

    if force {
        temporary.persist(path)
    } else {
        temporary.persist_noclobber(path)
    }
    .map_err(|error| error.error)
    .with_context(|| {
        format!(
            "failed to persist runtime settings export: {}",
            path.display()
        )
    })?;

    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync export directory: {}", parent.display()))?;

    Ok(())
}

fn read_runtime_settings_snapshot(path: &Path) -> Result<String> {
    let input: Box<dyn io::Read> = if path == Path::new("-") {
        Box::new(io::stdin())
    } else {
        Box::new(File::open(path).with_context(|| {
            format!(
                "failed to open runtime settings snapshot: {}",
                path.display()
            )
        })?)
    };
    let mut json = String::new();
    input
        .take(MAX_RUNTIME_SETTINGS_SNAPSHOT_BYTES + 1)
        .read_to_string(&mut json)
        .with_context(|| {
            format!(
                "failed to read runtime settings snapshot: {}",
                path.display()
            )
        })?;
    if json.len() as u64 > MAX_RUNTIME_SETTINGS_SNAPSHOT_BYTES {
        bail!("runtime settings snapshot exceeds the {MAX_RUNTIME_SETTINGS_SNAPSHOT_BYTES} byte limit");
    }
    Ok(json)
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
            let request: synctv_proto::admin::UpdateSettingsRequest =
                parse_masked_settings_request(
                    "settings update request",
                    args.request_json.as_deref(),
                    &args.set,
                    &args.unset,
                )?;
            let session = connect_remote_access(&args.remote).await?;
            let response =
                management_unary_call!(session, "update settings", update_settings, request)?;
            args.remote.print_output(&response)
        }
        SettingsSubcommand::Export(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let snapshot = management_unary_call!(
                session,
                "export settings",
                export_settings,
                synctv_proto::admin::ExportSettingsRequest {}
            )?;
            let json = runtime_settings_snapshot_json(&snapshot)?;
            write_runtime_settings_snapshot(args.file.as_deref(), &json, args.force)?;
            if let Some(path) = args.file {
                eprintln!("Exported runtime settings to {}", path.display());
            }
            Ok(())
        }
        SettingsSubcommand::Import(args) => {
            let json = read_runtime_settings_snapshot(&args.file)?;
            let snapshot: synctv_proto::admin::RuntimeSettingsSnapshot =
                parse_cli_json("runtime settings snapshot", &json)?;
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "import settings",
                import_settings,
                synctv_proto::admin::ImportSettingsRequest {
                    snapshot: Some(snapshot),
                    dry_run: args.dry_run,
                }
            )?;
            args.remote.print_output(&SettingsImportOutput {
                applied: response.applied,
                changed_sections: response.changed_sections,
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_settings_snapshot_json_round_trips_proto_json() -> Result<()> {
        let snapshot = synctv_proto::admin::RuntimeSettingsSnapshot {
            format_version: 1,
            settings: Some(synctv_proto::admin::RuntimeSettings::default()),
        };

        let json = runtime_settings_snapshot_json(&snapshot)?;
        let value: serde_json::Value = serde_json::from_slice(&json)?;
        assert_eq!(value["formatVersion"], 1);
        let decoded: synctv_proto::admin::RuntimeSettingsSnapshot = serde_json::from_slice(&json)?;
        assert_eq!(decoded, snapshot);
        Ok(())
    }

    #[test]
    fn runtime_settings_snapshot_file_is_private_and_atomic() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime-settings.json");

        write_runtime_settings_snapshot(Some(&path), b"first\n", false)?;
        assert_eq!(std::fs::read(&path)?, b"first\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o600
            );
        }

        let error = write_runtime_settings_snapshot(Some(&path), b"second\n", false)
            .expect_err("existing export should require --force");
        assert!(error.to_string().contains("--force"));
        assert_eq!(std::fs::read(&path)?, b"first\n");

        write_runtime_settings_snapshot(Some(&path), b"second\n", true)?;
        assert_eq!(std::fs::read(&path)?, b"second\n");
        Ok(())
    }
}
