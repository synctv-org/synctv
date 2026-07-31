use super::super::prelude::*;
use super::{
    ProviderBoundCredentialArgs, ProviderCredentialCommandArgs, ProviderServiceInstanceArgs,
    ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderSeafileCommand {
    #[command(subcommand)]
    pub command: ProviderSeafileSubcommand,
}
#[derive(Debug, Subcommand)]
pub enum ProviderSeafileSubcommand {
    Login(ProviderSeafileLoginArgs),
    UnlockLibrary(ProviderSeafileUnlockLibraryArgs),
    Repositories(ProviderSeafileRepositoriesArgs),
    List(ProviderSeafileListArgs),
    Starred(ProviderSeafileStarredArgs),
    Logout(ProviderCredentialCommandArgs),
    Binds(ProviderSeafileBindsArgs),
}
#[derive(Debug, Args)]
pub struct ProviderSeafileLoginArgs {
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
pub struct ProviderSeafileUnlockLibraryArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long)]
    pub repository_id: String,
    #[arg(long)]
    pub password: String,
}
#[derive(Debug, Args)]
pub struct ProviderSeafileRepositoriesArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long, default_value_t = 1)]
    pub page: u64,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub page_size: u32,
}
#[derive(Debug, Args)]
pub struct ProviderSeafileListArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long)]
    pub repository_id: String,
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
pub struct ProviderSeafileStarredArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long, default_value_t = 1)]
    pub page: u64,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub page_size: u32,
}
#[derive(Debug, Args)]
pub struct ProviderSeafileBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
