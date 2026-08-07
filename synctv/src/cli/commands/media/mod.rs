use super::prelude::*;

mod provider;

pub use provider::*;

#[derive(Debug, Args)]
pub struct MediaCommand {
    #[command(subcommand)]
    pub command: MediaSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum MediaSubcommand {
    /// List media and child playlists under the room root or a playlist
    List(MediaListArgs),
    /// Add media as a specific real user using any configured provider instance or direct-url source config
    Add(MediaAddArgs),
    /// Add a direct HTTP(S) media URL
    AddUrl(MediaAddUrlArgs),
    /// Update media name
    Update(MediaEditArgs),
    /// Delete a media item
    Delete(MediaDeleteArgs),
    /// Reorder media in place or move media into another static playlist
    Move(MediaMoveArgs),
    /// Add provider-backed media with typed provider arguments
    Provider(MediaProviderCommand),
}

#[derive(Debug, Args)]
pub struct MediaListArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(long)]
    pub playlist_id: Option<String>,

    #[arg(long)]
    pub target_json: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: u32,

    #[arg(long)]
    pub cursor: Option<String>,

    #[arg(long, default_value_t = 50)]
    pub page_size: u32,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub source_provider: Option<CliSourceProvider>,

    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliMediaSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Asc)]
    pub sort_dir: CliSortDirection,

    /// Force upstream provider directory cache refresh when listing a dynamic playlist
    #[arg(long, default_value_t = false)]
    pub refresh: bool,

    #[arg(long, value_enum, default_value_t = CliResourceAvailabilityFilter::All)]
    pub availability: CliResourceAvailabilityFilter,
}

#[derive(Debug, Args)]
pub struct MediaAddUrlArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    pub url: String,

    #[arg(long)]
    pub playlist_id: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaAddArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    #[arg(long)]
    pub playlist_id: Option<String>,

    #[arg(long, value_enum)]
    pub source_provider: CliSourceProvider,

    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub source_config_json: String,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaEditArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub media_id: String,

    #[arg(long)]
    pub name: String,
}

#[derive(Debug, Args)]
pub struct MediaDeleteArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[arg(allow_hyphen_values = true)]
    pub media_id: String,

    #[arg(long, default_value_t = false)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct MediaMoveArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    /// Media ID to move. Repeat this flag to move multiple media items in order.
    #[arg(
        long = "media-id",
        value_name = "MEDIA_ID",
        allow_hyphen_values = true,
        action = ArgAction::Append,
        required_unless_present = "all_from_scope"
    )]
    pub media_ids: Vec<String>,

    /// Move every media item from the room root or the source playlist scope.
    #[arg(long, default_value_t = false)]
    pub all_from_scope: bool,

    /// Source static playlist when using --all-from-scope. Omit for the room root scope.
    #[arg(long, requires = "all_from_scope")]
    pub from_playlist_id: Option<String>,

    /// Target static playlist. Omit to keep media in the current scope.
    #[arg(long)]
    pub to_playlist_id: Option<String>,

    /// Insert before this media in the target scope. Omit both anchors to append to the target scope.
    #[arg(long, conflicts_with = "after_media_id")]
    pub before_media_id: Option<String>,

    /// Insert after this media in the target scope. Omit both anchors to append to the target scope.
    #[arg(long, conflicts_with = "before_media_id")]
    pub after_media_id: Option<String>,
}
