use super::super::prelude::*;
use super::{
    ProviderBoundCredentialArgs, ProviderCredentialCommandArgs, ProviderServiceInstanceArgs,
    ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderTruenasCommand {
    #[command(subcommand)]
    pub command: ProviderTruenasSubcommand,
}
#[derive(Debug, Subcommand)]
pub enum ProviderTruenasSubcommand {
    Login(ProviderTruenasLoginArgs),
    List(ProviderTruenasListArgs),
    Logout(ProviderCredentialCommandArgs),
    Binds(ProviderTruenasBindsArgs),
}
#[derive(Debug, Args)]
pub struct ProviderTruenasLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[arg(long)]
    pub server_endpoint: String,
    #[arg(long)]
    pub api_key: String,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
#[derive(Debug, Args)]
pub struct ProviderTruenasListArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long, default_value = "/")]
    pub path: String,
    #[arg(long, default_value_t = 1)]
    pub page: u64,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub page_size: u32,
    #[arg(long)]
    pub search: Option<String>,
}
#[derive(Debug, Args)]
pub struct ProviderTruenasBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
