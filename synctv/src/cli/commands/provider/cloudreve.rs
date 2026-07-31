use super::super::prelude::*;
use super::{
    ProviderBoundCredentialArgs, ProviderBoundCredentialInstanceCommandArgs,
    ProviderCredentialCommandArgs, ProviderServiceInstanceArgs, ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderCloudreveCommand {
    #[command(subcommand)]
    pub command: ProviderCloudreveSubcommand,
}
#[derive(Debug, Subcommand)]
pub enum ProviderCloudreveSubcommand {
    Login(ProviderCloudreveLoginArgs),
    List(ProviderCloudreveListArgs),
    Search(ProviderCloudreveSearchArgs),
    Me(ProviderBoundCredentialInstanceCommandArgs),
    Logout(ProviderCredentialCommandArgs),
    Binds(ProviderCloudreveBindsArgs),
}
#[derive(Debug, Args)]
pub struct ProviderCloudreveLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[arg(long)]
    pub server_endpoint: String,
    #[arg(long)]
    pub account_email: String,
    #[arg(long)]
    pub password: String,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
#[derive(Debug, Args)]
pub struct ProviderCloudreveListArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long, default_value = "/")]
    pub path: String,
    #[arg(long, conflicts_with = "cursor")]
    pub page: Option<u32>,
    #[arg(long, conflicts_with = "page")]
    pub cursor: Option<String>,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub per_page: u32,
}
#[derive(Debug, Args)]
pub struct ProviderCloudreveSearchArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long)]
    pub keywords: String,
    #[arg(long, default_value_t = 0)]
    pub offset: u64,
}
#[derive(Debug, Args)]
pub struct ProviderCloudreveBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
