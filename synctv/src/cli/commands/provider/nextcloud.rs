use super::super::prelude::*;
use super::{
    ProviderBoundCredentialArgs, ProviderCredentialCommandArgs, ProviderServiceInstanceArgs,
    ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderNextcloudCommand {
    #[command(subcommand)]
    pub command: ProviderNextcloudSubcommand,
}
#[derive(Debug, Subcommand)]
pub enum ProviderNextcloudSubcommand {
    Login(ProviderNextcloudLoginArgs),
    StartLoginFlow(ProviderNextcloudStartLoginFlowArgs),
    PollLoginFlow(ProviderNextcloudPollLoginFlowArgs),
    List(ProviderNextcloudListArgs),
    Favorites(ProviderNextcloudFavoritesArgs),
    Logout(ProviderCredentialCommandArgs),
    Binds(ProviderNextcloudBindsArgs),
}
#[derive(Debug, Args)]
pub struct ProviderNextcloudLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[arg(long)]
    pub server_endpoint: String,
    #[arg(long)]
    pub account_username: String,
    #[arg(long)]
    pub app_password: String,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
#[derive(Debug, Args)]
pub struct ProviderNextcloudStartLoginFlowArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[arg(long)]
    pub server_endpoint: String,
}
#[derive(Debug, Args)]
pub struct ProviderNextcloudPollLoginFlowArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[arg(long)]
    pub server_endpoint: String,
    #[arg(long)]
    pub poll_endpoint: String,
    #[arg(long)]
    pub poll_token: String,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
#[derive(Debug, Args)]
pub struct ProviderNextcloudListArgs {
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
pub struct ProviderNextcloudFavoritesArgs {
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
pub struct ProviderNextcloudBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
