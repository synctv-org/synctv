use super::super::prelude::*;
use super::ProviderServiceRemoteActorArgs;

#[derive(Debug, Args)]
pub struct ProviderAcfunCommand {
    #[command(subcommand)]
    pub command: ProviderAcfunSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderAcfunSubcommand {
    Resolve(ProviderAcfunResolveArgs),
}

#[derive(Debug, Args)]
pub struct ProviderAcfunResolveArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    pub resource: String,
}
