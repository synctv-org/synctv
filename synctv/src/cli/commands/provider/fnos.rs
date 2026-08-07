use super::super::prelude::*;
use super::{
    ProviderBoundCredentialArgs, ProviderBoundCredentialInstanceCommandArgs,
    ProviderCredentialCommandArgs, ProviderServiceInstanceArgs, ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderFnosCommand {
    #[command(subcommand)]
    pub command: ProviderFnosSubcommand,
}
#[derive(Debug, Subcommand)]
pub enum ProviderFnosSubcommand {
    Login(ProviderFnosLoginArgs),
    List(ProviderFnosListArgs),
    Libraries(ProviderFnosLibrariesArgs),
    Items(ProviderFnosItemsArgs),
    SetFavorite(ProviderFnosSetFavoriteArgs),
    SetWatched(ProviderFnosSetWatchedArgs),
    ServerInfo(ProviderBoundCredentialInstanceCommandArgs),
    Logout(ProviderCredentialCommandArgs),
    Binds(ProviderFnosBindsArgs),
}
#[derive(Debug, Args)]
pub struct ProviderFnosLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[arg(long)]
    pub server_endpoint: String,
    #[arg(long)]
    pub webdav_endpoint: Option<String>,
    #[arg(long)]
    pub media_endpoint: Option<String>,
    #[arg(long)]
    pub account_username: String,
    #[arg(long)]
    pub password: String,
    #[arg(long)]
    pub twofa_code: Option<String>,
    #[arg(long, default_value_t = false)]
    pub trust_device: bool,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
#[derive(Debug, Args)]
pub struct ProviderFnosListArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long, default_value = "/")]
    pub path: String,
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub page_size: u32,
    #[arg(long)]
    pub search: Option<String>,
}
#[derive(Debug, Args)]
pub struct ProviderFnosLibrariesArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProviderFnosCollection {
    Library,
    Favorites,
    History,
}
impl ProviderFnosCollection {
    pub const fn to_proto(self) -> i32 {
        match self {
            Self::Library => 1,
            Self::Favorites => 2,
            Self::History => 3,
        }
    }
}
#[derive(Debug, Args)]
pub struct ProviderFnosItemsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long, value_enum)]
    pub collection: ProviderFnosCollection,
    #[arg(long)]
    pub ancestor_guid: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub page_size: u32,
    #[arg(long, value_delimiter = ',')]
    pub media_types: Vec<String>,
    #[arg(long)]
    pub search: Option<String>,
}
#[derive(Debug, Args)]
pub struct ProviderFnosSetFavoriteArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long)]
    pub item_guid: String,
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub favorite: bool,
}
#[derive(Debug, Args)]
pub struct ProviderFnosSetWatchedArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long)]
    pub item_guid: String,
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub watched: bool,
}
#[derive(Debug, Args)]
pub struct ProviderFnosBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
