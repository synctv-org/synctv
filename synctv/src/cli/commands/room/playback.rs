use super::super::prelude::*;

#[derive(Debug, Args)]
pub struct RoomPlaybackCommand {
    #[command(subcommand)]
    pub command: RoomPlaybackSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomPlaybackSubcommand {
    /// Get the current playback state and signed pull URLs for the room's active item
    Get(RoomPlaybackGetArgs),
    /// Start playback for a static media item or dynamic playlist target
    Start(RoomPlaybackStartArgs),
    /// Resume playback for the room's current playback item
    Play(RoomPlaybackStateUpdateArgs),
    /// Pause playback for the room's current playback item
    Pause(RoomPlaybackStateUpdateArgs),
    /// Seek the room's current playback item to a position
    Seek(RoomPlaybackSeekArgs),
    /// Change playback speed for the room's current playback item
    Speed(RoomPlaybackSpeedArgs),
    /// Stop the room's current playback item
    Stop(RoomPlaybackStopArgs),
}

#[derive(Debug, Args)]
pub struct RoomPlaybackGetArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub playback_client_profile: PlaybackClientProfileArgs,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("playback_target")
        .args(["media_id", "playlist_id"])
        .required(true)
        .multiple(false)
))]
pub struct RoomPlaybackStartArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: OptionalActorUserArgs,

    #[arg(long, group = "playback_target")]
    pub media_id: Option<String>,

    #[arg(long, group = "playback_target")]
    pub playlist_id: Option<String>,

    #[arg(long)]
    pub target_json: Option<String>,
}

#[derive(Debug, Args)]
pub struct RoomPlaybackStopArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliPlaybackStateUpdateType {
    Play,
    Pause,
    Seek,
    Speed,
}

impl CliPlaybackStateUpdateType {
    pub(in crate::cli) const fn to_proto(self) -> i32 {
        match self {
            Self::Play => synctv_proto::client::PlaybackUpdateType::Play as i32,
            Self::Pause => synctv_proto::client::PlaybackUpdateType::Pause as i32,
            Self::Seek => synctv_proto::client::PlaybackUpdateType::Seek as i32,
            Self::Speed => synctv_proto::client::PlaybackUpdateType::Speed as i32,
        }
    }
}

#[derive(Debug, Args)]
pub struct RoomPlaybackStateUpdateArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    /// Final playing state to apply together with this update.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub playing: Option<bool>,

    /// Playback position in seconds. Required for `seek`.
    #[arg(long, value_name = "SECONDS")]
    pub position: Option<f64>,

    /// Playback speed multiplier, usually between 0.25 and 4.0
    #[arg(long)]
    pub speed: Option<f64>,

    /// Optional optimistic-lock playback state version
    #[arg(long)]
    pub version: Option<i64>,
}

#[derive(Debug, Args)]
pub struct RoomPlaybackSeekArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    /// Playback position in seconds.
    #[arg(long, value_name = "SECONDS")]
    pub position: f64,

    /// Final playing state to apply together with this update.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub playing: Option<bool>,

    /// Playback speed multiplier, usually between 0.25 and 4.0
    #[arg(long)]
    pub speed: Option<f64>,

    /// Optional optimistic-lock playback state version
    #[arg(long)]
    pub version: Option<i64>,
}

#[derive(Debug, Args)]
pub struct RoomPlaybackSpeedArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    /// Playback speed multiplier, usually between 0.25 and 4.0
    #[arg(long)]
    pub speed: f64,

    /// Final playing state to apply together with this update.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub playing: Option<bool>,

    /// Playback position in seconds.
    #[arg(long, value_name = "SECONDS")]
    pub position: Option<f64>,

    /// Optional optimistic-lock playback state version
    #[arg(long)]
    pub version: Option<i64>,
}
