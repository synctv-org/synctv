use super::super::prelude::*;
use super::{
    ProviderBoundCredentialArgs, ProviderCredentialCommandArgs, ProviderServiceInstanceArgs,
    ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderSynologyCommand {
    #[command(subcommand)]
    pub command: ProviderSynologySubcommand,
}
#[derive(Debug, Subcommand)]
pub enum ProviderSynologySubcommand {
    Login(ProviderSynologyLoginArgs),
    Files(ProviderSynologyFilesArgs),
    Libraries(ProviderSynologyLibrariesArgs),
    Movies(ProviderSynologyMoviesArgs),
    TvShows(ProviderSynologyTvShowsArgs),
    Episodes(ProviderSynologyEpisodesArgs),
    HomeVideos(ProviderSynologyHomeVideosArgs),
    Recordings(ProviderSynologyRecordingsArgs),
    Logout(ProviderCredentialCommandArgs),
    Binds(ProviderSynologyBindsArgs),
}
#[derive(Debug, Args)]
pub struct ProviderSynologyLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[arg(long)]
    pub server_endpoint: String,
    #[arg(long)]
    pub account_username: String,
    #[arg(long)]
    pub password: String,
    #[arg(long)]
    pub otp_code: Option<String>,
    #[arg(long)]
    pub device_name: Option<String>,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
#[derive(Debug, Args)]
pub struct ProviderSynologyFilesArgs {
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
pub struct ProviderSynologyLibrariesArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
}
macro_rules! synology_video_args { ($name:ident) => { #[derive(Debug, Args)] pub struct $name { #[command(flatten)] pub access: ProviderServiceRemoteActorArgs, #[command(flatten)] pub bind: ProviderBoundCredentialArgs, #[arg(long, default_value_t = 0)] pub library_id: i64, #[arg(long, default_value_t = 1)] pub page: u64, #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))] pub page_size: u32, #[arg(long)] pub search: Option<String> } }; }
synology_video_args!(ProviderSynologyMoviesArgs);
synology_video_args!(ProviderSynologyTvShowsArgs);
synology_video_args!(ProviderSynologyHomeVideosArgs);
synology_video_args!(ProviderSynologyRecordingsArgs);
#[derive(Debug, Args)]
pub struct ProviderSynologyEpisodesArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
    #[arg(long, default_value_t = 0)]
    pub library_id: i64,
    #[arg(long)]
    pub tv_show_id: i64,
    #[arg(long, default_value_t = 1)]
    pub page: u64,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub page_size: u32,
    #[arg(long)]
    pub search: Option<String>,
}
#[derive(Debug, Args)]
pub struct ProviderSynologyBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,
    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
