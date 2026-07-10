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
    /// Update room settings
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

    /// Set a room settings leaf using PATH=VALUE; may be repeated.
    #[arg(long, value_name = "PATH=VALUE", conflicts_with = "request_json")]
    pub set: Vec<String>,

    /// Restore a room settings leaf to its server default; may be repeated.
    #[arg(long, value_name = "PATH", conflicts_with = "request_json")]
    pub unset: Vec<String>,

    /// Admin UpdateRoomSettingsRequest encoded as ProtoJSON; roomId is taken from ROOM_ID.
    #[arg(
        long = "request-json",
        value_name = "JSON",
        conflicts_with_all = ["set", "unset"]
    )]
    pub request_json: Option<String>,
}

#[derive(Debug, Args)]
pub struct RoomSettingsResetArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub room: RoomSettingsScopeArgs,
}
