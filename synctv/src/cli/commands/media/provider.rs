use super::super::prelude::*;

#[derive(Debug, Args)]
pub struct MediaProviderCommand {
    #[command(subcommand)]
    pub command: MediaProviderSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum MediaProviderSubcommand {
    /// Add an Alist-backed media item
    Alist(MediaProviderAlistArgs),
    /// Add an Emby-compatible media item
    Emby(MediaProviderEmbyArgs),
    /// Add a Bilibili-backed media item
    Bilibili(MediaProviderBilibiliCommand),
}

#[derive(Debug, Args)]
pub struct MediaProviderBilibiliCommand {
    #[command(subcommand)]
    pub command: MediaProviderBilibiliSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum MediaProviderBilibiliSubcommand {
    /// Add a regular Bilibili video or multi-part page
    Video(MediaProviderBilibiliVideoArgs),
    /// Add a Bilibili PGC episode
    Pgc(MediaProviderBilibiliPgcArgs),
    /// Add a Bilibili live room
    Live(MediaProviderBilibiliLiveArgs),
}

#[derive(Debug, Args)]
pub struct MediaProviderAlistArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    /// Alist file path
    #[arg(long)]
    pub path: String,

    #[arg(long)]
    pub playlist_id: Option<String>,

    /// Saved Alist credential server identifier
    #[arg(long)]
    pub server_id: String,

    /// Optional Alist directory password
    #[arg(long)]
    pub password: Option<String>,

    /// Explicit provider instance name to store alongside the media item
    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaProviderEmbyArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    /// Emby-compatible media item identifier
    #[arg(long)]
    pub item_id: String,

    #[arg(long)]
    pub playlist_id: Option<String>,

    /// Saved Emby-compatible credential server identifier
    #[arg(long)]
    pub server_id: String,

    /// Explicit provider instance name to store alongside the media item
    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("bilibili_video_ref")
        .args(["bvid", "aid"])
        .required(true)
        .multiple(false)
))]
pub struct BilibiliVideoRefArgs {
    #[arg(long, group = "bilibili_video_ref")]
    pub bvid: Option<String>,

    #[arg(long, group = "bilibili_video_ref")]
    pub aid: Option<u64>,
}

#[derive(Debug, Args)]
pub struct MediaProviderBilibiliVideoArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    #[command(flatten)]
    pub video: BilibiliVideoRefArgs,

    /// Bilibili content page `cid`
    #[arg(long)]
    pub cid: u64,

    #[arg(long)]
    pub playlist_id: Option<String>,

    /// Share creator's Bilibili login for playback instead of each viewer's own login
    #[arg(long)]
    pub shared: bool,

    /// Explicit provider instance name to store alongside the media item
    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaProviderBilibiliPgcArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    /// Bilibili PGC episode identifier
    #[arg(long)]
    pub epid: u64,

    /// Bilibili content page `cid`
    #[arg(long)]
    pub cid: u64,

    #[arg(long)]
    pub playlist_id: Option<String>,

    /// Share creator's Bilibili login for playback instead of each viewer's own login
    #[arg(long)]
    pub shared: bool,

    /// Explicit provider instance name to store alongside the media item
    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct MediaProviderBilibiliLiveArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,

    /// Bilibili live room identifier
    #[arg(long)]
    pub room_live_id: u64,

    #[arg(long)]
    pub playlist_id: Option<String>,

    /// Share creator's Bilibili login for playback instead of each viewer's own login
    #[arg(long)]
    pub shared: bool,

    /// Explicit provider instance name to store alongside the media item
    #[arg(long)]
    pub provider_instance_name: Option<String>,

    #[arg(long)]
    pub name: Option<String>,
}
