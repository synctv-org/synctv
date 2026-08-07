use super::super::prelude::*;
use super::{
    ProviderCredentialCommandArgs, ProviderServiceInstanceArgs, ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderYoutubeCommand {
    #[command(subcommand)]
    pub command: ProviderYoutubeSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderYoutubeSubcommand {
    Bind(ProviderYoutubeBindArgs),
    Binds(ProviderYoutubeBindsArgs),
    Unbind(ProviderCredentialCommandArgs),
    Resolve(ProviderYoutubeResolveArgs),
}

#[derive(Debug, Args)]
pub struct ProviderYoutubeBindArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[arg(long)]
    pub label: String,
    #[arg(long)]
    pub visitor_data: Option<String>,
    #[arg(long)]
    pub po_token: Option<String>,
    #[arg(long)]
    pub cookie: Option<String>,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderYoutubeBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderYoutubeResolveArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    pub resource: String,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
