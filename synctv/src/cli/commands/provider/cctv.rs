use super::super::prelude::*;
use super::ProviderServiceRemoteActorArgs;

#[derive(Debug, Args)]
pub struct ProviderCctvCommand {
    #[command(subcommand)]
    pub command: ProviderCctvSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderCctvSubcommand {
    Resolve(ProviderCctvResolveArgs),
}

#[derive(Debug, Args)]
pub struct ProviderCctvResolveArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    pub resource: String,
}
