use super::super::prelude::*;
use super::ProviderServiceRemoteActorArgs;

#[derive(Debug, Args)]
pub struct ProviderDouyuCommand {
    #[command(subcommand)]
    pub command: ProviderDouyuSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderDouyuSubcommand {
    Resolve(ProviderDouyuResolveArgs),
}

#[derive(Debug, Args)]
pub struct ProviderDouyuResolveArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    pub resource: String,
}
