use super::prelude::*;

mod provider;

pub use provider::*;

#[derive(Debug, Args)]
pub struct PlaylistCommand {
    #[command(subcommand)]
    pub command: PlaylistSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum PlaylistSubcommand {
    /// List playlists under a room or parent playlist
    List(PlaylistListArgs),
    /// Get a playlist by ID
    Get(PlaylistGetArgs),
    /// Create a playlist as a specific real user
    Create(PlaylistCreateArgs),
    /// Update playlist name
    Update(PlaylistUpdateArgs),
    /// Move a playlist before or after a sibling
    Move(PlaylistMoveArgs),
    /// Delete a playlist
    Delete(PlaylistDeleteArgs),
    /// Create provider-backed dynamic playlists with typed provider arguments
    Provider(PlaylistProviderCommand),
}

#[derive(Debug, Args)]
pub struct PlaylistListArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long)]
    pub parent_id: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub source_provider: Option<CliSourceProvider>,

    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub dynamic_only: Option<bool>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliPlaylistSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Asc)]
    pub sort_dir: CliSortDirection,

    #[arg(long, value_enum, default_value_t = CliResourceAvailabilityFilter::All)]
    pub availability: CliResourceAvailabilityFilter,
}

#[derive(Debug, Args)]
pub struct PlaylistGetArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub playlist_id: String,
}

#[derive(Debug, Args)]
pub struct PlaylistCreateArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    pub name: String,

    #[arg(long)]
    pub parent_id: Option<String>,

    #[arg(long, value_enum)]
    pub source_provider: Option<CliSourceProvider>,

    #[arg(long)]
    pub source_config_json: Option<String>,

    #[arg(long)]
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Args)]
pub struct PlaylistUpdateArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub playlist_id: String,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct PlaylistMoveArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub playlist_id: String,

    #[arg(long, conflicts_with = "after_playlist_id")]
    pub before_playlist_id: Option<String>,

    #[arg(long, conflicts_with = "before_playlist_id")]
    pub after_playlist_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct PlaylistDeleteArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub playlist_id: String,

    #[arg(long, default_value_t = false)]
    pub force: bool,
}
