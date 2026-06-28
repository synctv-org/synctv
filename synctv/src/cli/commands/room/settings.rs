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
pub struct RoomSettingsScopeArgs {
    #[arg(value_name = "ROOM_ID", allow_hyphen_values = true)]
    pub room_id: String,
}

impl RoomSettingsScopeArgs {
    pub(in crate::cli) fn resolved_room_id(&self) -> &str {
        &self.room_id
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
