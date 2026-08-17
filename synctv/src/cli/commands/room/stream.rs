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
    /// Create an RTMP publish key for a room media item
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

    #[arg(long, value_enum)]
    pub key_type: CliPublishKeyType,

    /// Unix timestamp. Required for single-use and expiring keys.
    #[arg(long)]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliPublishKeyType {
    SingleUse,
    Expiring,
    Permanent,
}

impl CliPublishKeyType {
    pub const fn to_proto(self) -> i32 {
        match self {
            Self::SingleUse => synctv_proto::client::PublishKeyType::SingleUse as i32,
            Self::Expiring => synctv_proto::client::PublishKeyType::Expiring as i32,
            Self::Permanent => synctv_proto::client::PublishKeyType::Permanent as i32,
        }
    }
}

#[derive(Debug, Args)]
pub struct RoomStreamGetArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long, allow_hyphen_values = true)]
    pub media_id: String,
}
