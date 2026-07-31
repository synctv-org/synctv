use super::super::prelude::*;
use super::{
    ProviderCredentialCommandArgs, ProviderServiceInstanceArgs, ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderDouyinCommand {
    #[command(subcommand)]
    pub command: ProviderDouyinSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderDouyinSubcommand {
    /// Save a Douyin Cookie for an app user
    Bind(ProviderDouyinBindArgs),
    /// List saved Douyin binds for an app user
    Binds(ProviderDouyinBindsArgs),
    /// Remove a saved Douyin bind
    Unbind(ProviderCredentialCommandArgs),
    /// Resolve a Douyin video, live room, or short link
    Resolve(ProviderDouyinResolveArgs),
    /// List an author's video posts using Douyin cursor pagination
    Posts(ProviderDouyinPostsArgs),
}

#[derive(Debug, Args)]
pub struct ProviderDouyinBindArgs {
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
pub struct ProviderDouyinBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderDouyinResolveArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    pub resource: String,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderDouyinPostsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[arg(long)]
    pub sec_uid: String,

    #[arg(long)]
    pub cursor: Option<String>,

    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=50))]
    pub page_size: u32,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
