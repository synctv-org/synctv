use super::super::prelude::*;

#[derive(Debug, Args)]
pub struct RoomStreamCommand {
    #[command(subcommand)]
    pub command: RoomStreamSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomStreamSubcommand {
    /// List active RTMP publish sessions in a room
    List(RoomStreamListArgs),
    /// Create a single-use RTMP publish key for a room media item
    PublishKey(RoomStreamPublishKeyArgs),
    /// Get the active RTMP stream state for one room media item
    Get(RoomStreamGetArgs),
    /// Kick an active RTMP publish session in a room
    Kick(RoomStreamKickArgs),
}

#[derive(Debug, Args)]
pub struct RoomStreamListArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliRoomStreamSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Asc)]
    pub sort_dir: CliSortDirection,
}

#[derive(Debug, Args)]
pub struct RoomStreamKickArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long, allow_hyphen_values = true)]
    pub media_id: String,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct RoomStreamPublishKeyArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    #[arg(long, allow_hyphen_values = true)]
    pub media_id: String,
}

#[derive(Debug, Args)]
pub struct RoomStreamGetArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long, allow_hyphen_values = true)]
    pub media_id: String,
}
