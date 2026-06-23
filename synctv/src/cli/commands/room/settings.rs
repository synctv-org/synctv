use super::super::prelude::*;

#[derive(Debug, Args)]
pub struct RoomSettingsCommand {
    #[command(subcommand)]
    pub command: RoomSettingsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomSettingsSubcommand {
    /// Get room settings
    Get(RoomSettingsGetArgs),
    /// Patch room settings with a partial JSON object
    Update(RoomSettingsUpdateArgs),
    /// Reset room settings to defaults
    Reset(RoomSettingsResetArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("room_settings_scope")
        .args(["room_id", "room_id_flag"])
        .required(true)
        .multiple(false)
))]
pub struct RoomSettingsScopeArgs {
    #[arg(value_name = "ROOM_ID", allow_hyphen_values = true)]
    pub room_id: Option<String>,

    #[arg(long = "room-id", value_name = "ROOM_ID", allow_hyphen_values = true)]
    pub room_id_flag: Option<String>,
}

impl RoomSettingsScopeArgs {
    pub(in crate::cli) fn resolved_room_id(&self) -> Result<&str> {
        self.room_id
            .as_deref()
            .or(self.room_id_flag.as_deref())
            .ok_or_else(|| anyhow!("room settings requires ROOM_ID or --room-id"))
    }
}

#[derive(Debug, Args)]
pub struct RoomSettingsGetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub room: RoomSettingsScopeArgs,
}

#[derive(Debug, Args)]
pub struct RoomSettingsUpdateArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub room: RoomSettingsScopeArgs,

    /// Partial JSON object patch merged onto the current room settings before submission
    #[arg(long)]
    pub settings_json: String,
}

#[derive(Debug, Args)]
pub struct RoomSettingsResetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub room: RoomSettingsScopeArgs,
}
