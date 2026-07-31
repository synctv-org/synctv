use super::super::prelude::*;
use super::{
    ProviderCredentialCommandArgs, ProviderServiceInstanceArgs, ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderTikTokCommand {
    #[command(subcommand)]
    pub command: ProviderTikTokSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderTikTokSubcommand {
    /// Save a TikTok Cookie for an app user
    Bind(ProviderTikTokBindArgs),
    /// List saved TikTok binds for an app user
    Binds(ProviderTikTokBindsArgs),
    /// Remove a saved TikTok bind
    Unbind(ProviderCredentialCommandArgs),
    /// Resolve a TikTok video, live room, or short link
    Resolve(ProviderTikTokResolveArgs),
    /// Resolve a TikTok profile name to its stable secUid
    User(ProviderTikTokUserArgs),
    /// List an author's video posts using TikTok cursor pagination
    Posts(ProviderTikTokPostsArgs),
}

#[derive(Debug, Args)]
pub struct ProviderTikTokUserArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    pub unique_id: String,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderTikTokBindArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[arg(long)]
    pub label: String,

    #[arg(long)]
    pub cookie: String,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderTikTokBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderTikTokResolveArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    pub resource: String,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderTikTokPostsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[arg(long)]
    pub sec_uid: String,

    #[arg(long)]
    pub cursor: Option<String>,

    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=35))]
    pub page_size: u32,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
