use super::super::prelude::*;
use super::{
    ProviderCredentialCommandArgs, ProviderServiceInstanceArgs, ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderTwitchCommand {
    #[command(subcommand)]
    pub command: ProviderTwitchSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderTwitchSubcommand {
    /// Save a Twitch OAuth token for an app user
    Bind(ProviderTwitchBindArgs),
    /// List saved Twitch binds for an app user
    Binds(ProviderTwitchBindsArgs),
    /// Remove a saved Twitch bind
    Unbind(ProviderCredentialCommandArgs),
    /// Resolve a Twitch live channel, VOD, or clip
    Resolve(ProviderTwitchResolveArgs),
    /// List a channel's videos, highlights, uploads, or clips
    Items(ProviderTwitchItemsArgs),
    /// List followed live channels
    FollowedLive(ProviderTwitchFollowedLiveArgs),
    /// List live streams in a category
    CategoryStreams(ProviderTwitchCategoryStreamsArgs),
    /// List top Twitch categories
    TopCategories(ProviderTwitchTopCategoriesArgs),
    /// Search live channels
    SearchLive(ProviderTwitchSearchLiveArgs),
    /// List a broadcaster schedule
    Schedule(ProviderTwitchScheduleArgs),
}

#[derive(Debug, Args)]
pub struct ProviderTwitchBindArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[arg(long)]
    pub oauth_token: String,

    #[arg(long)]
    pub device_id: Option<String>,

    #[arg(long)]
    pub client_integrity: Option<String>,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderTwitchBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderTwitchResolveArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    pub resource: String,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProviderTwitchContent {
    Videos,
    Highlights,
    Uploads,
    Clips,
}

impl ProviderTwitchContent {
    pub(crate) const fn to_proto(self) -> i32 {
        use synctv_proto::source_config::TwitchPlaylistContent;
        match self {
            Self::Videos => TwitchPlaylistContent::Videos as i32,
            Self::Highlights => TwitchPlaylistContent::Highlights as i32,
            Self::Uploads => TwitchPlaylistContent::Uploads as i32,
            Self::Clips => TwitchPlaylistContent::Clips as i32,
        }
    }
}

#[derive(Debug, Args)]
pub struct ProviderTwitchItemsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    pub channel: String,

    #[arg(long, value_enum, default_value_t = ProviderTwitchContent::Videos)]
    pub content: ProviderTwitchContent,

    #[arg(long)]
    pub cursor: Option<String>,

    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub page_size: u32,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderTwitchFollowedLiveArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub page_size: u32,
}
#[derive(Debug, Args)]
pub struct ProviderTwitchCategoryStreamsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
    #[arg(long)]
    pub category_id: String,
    #[arg(long, default_value = "")]
    pub category_name: String,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub page_size: u32,
}
#[derive(Debug, Args)]
pub struct ProviderTwitchTopCategoriesArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub page_size: u32,
}
#[derive(Debug, Args)]
pub struct ProviderTwitchSearchLiveArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
    #[arg(long)]
    pub query: String,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub page_size: u32,
}
#[derive(Debug, Args)]
pub struct ProviderTwitchScheduleArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
    #[arg(long)]
    pub broadcaster_id: String,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=25))]
    pub page_size: u32,
}
