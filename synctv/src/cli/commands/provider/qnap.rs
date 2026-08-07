use super::super::prelude::*;
use super::{
    ProviderBoundCredentialArgs, ProviderBoundCredentialInstanceCommandArgs,
    ProviderCredentialCommandArgs, ProviderServiceInstanceArgs, ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderQnapCommand {
    #[command(subcommand)]
    pub command: ProviderQnapSubcommand,
}
#[derive(Debug, Subcommand)]
pub enum ProviderQnapSubcommand {
    Login(ProviderQnapLoginArgs),
    List(ProviderQnapListArgs),
    Capabilities(ProviderBoundCredentialInstanceCommandArgs),
    Logout(ProviderCredentialCommandArgs),
    Binds(ProviderQnapBindsArgs),
}
#[derive(Debug, Args)]
pub struct ProviderQnapLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[arg(long)]
    pub server_endpoint: String,
    #[arg(long)]
    pub account_username: String,
    #[arg(long)]
    pub password: String,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
#[derive(Debug, Args)]
pub struct ProviderQnapListArgs {
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
pub struct ProviderQnapBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
