use super::super::prelude::*;

#[derive(Debug, Args)]
pub struct PlaylistProviderCommand {
    #[command(subcommand)]
    pub command: PlaylistProviderSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum PlaylistProviderSubcommand {
    /// Create an Alist-backed dynamic playlist
    Alist(PlaylistProviderAlistArgs),
    /// Create an Emby-compatible dynamic playlist
    Emby(PlaylistProviderEmbyArgs),
}

#[derive(Debug, Args)]
pub struct PlaylistProviderAlistArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    pub name: String,

    /// Alist folder path used as the dynamic playlist root
    #[arg(long)]
    pub path: String,

    #[arg(long)]
    pub parent_id: Option<String>,

    /// Saved Alist credential server identifier
    #[arg(long)]
    pub server_id: String,

    /// Optional Alist directory password
    #[arg(long)]
    pub password: Option<String>,

    /// Explicit provider instance name to store alongside the playlist
    #[arg(long)]
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Args)]
pub struct PlaylistProviderEmbyArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    pub name: String,

    /// Root Emby-compatible item identifier used as the dynamic playlist source
    #[arg(long)]
    pub item_id: String,

    #[arg(long)]
    pub parent_id: Option<String>,

    /// Saved Emby-compatible credential server identifier
    #[arg(long)]
    pub server_id: String,

    /// Explicit provider instance name to store alongside the playlist
    #[arg(long)]
    pub provider_instance_name: Option<String>,
}
