use super::super::prelude::*;
use super::ProviderServiceRemoteActorArgs;

#[derive(Debug, Args)]
pub struct ProviderHuyaCommand {
    #[command(subcommand)]
    pub command: ProviderHuyaSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderHuyaSubcommand {
    Resolve(ProviderHuyaResolveArgs),
}

#[derive(Debug, Args)]
pub struct ProviderHuyaResolveArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    pub resource: String,
}
