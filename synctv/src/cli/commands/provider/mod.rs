use super::prelude::*;

mod alist;
mod bilibili;
mod douyin;
mod emby;
mod rtmp;
mod shared;
mod tiktok;
mod twitch;

pub use alist::*;
pub use bilibili::*;
pub use douyin::*;
pub use emby::*;
pub use rtmp::*;
pub use shared::*;
pub use tiktok::*;
pub use twitch::*;

#[derive(Debug, Args)]
pub struct ProviderCommand {
    #[command(subcommand)]
    pub command: ProviderSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderSubcommand {
    /// List enabled remote provider instance names available to app clients
    Available(ProviderAvailableArgs),
    /// List backends for one provider type, including the default backend when present
    Backends(ProviderBackendsArgs),
    /// List provider instances
    List(ProviderListArgs),
    /// Create a provider instance
    Create(ProviderAddArgs),
    /// Update a provider instance
    Update(ProviderUpdateArgs),
    /// Delete a provider instance
    Delete(ProviderDeleteArgs),
    /// Reconnect a provider instance
    Reconnect(ProviderReconnectArgs),
    /// Enable a provider instance
    Enable(ProviderEnableArgs),
    /// Disable a provider instance
    Disable(ProviderDisableArgs),
    /// Alist provider service operations
    Alist(ProviderAlistCommand),
    /// Emby provider service operations
    Emby(ProviderEmbyCommand),
    /// Bilibili provider service operations
    Bilibili(ProviderBilibiliCommand),
    /// Douyin provider service operations
    Douyin(ProviderDouyinCommand),
    /// TikTok provider service operations
    Tiktok(ProviderTikTokCommand),
    /// Twitch provider service operations
    Twitch(ProviderTwitchCommand),
    /// RTMP provider service operations
    Rtmp(ProviderRtmpCommand),
}

#[derive(Debug, Args)]
pub struct ProviderAvailableArgs {
    #[arg(long, value_enum)]
    pub provider_type: Option<CliSourceProvider>,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBackendsArgs {
    pub provider_type: CliSourceProvider,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderListArgs {
    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long, value_enum)]
    pub provider_type: Option<CliSourceProvider>,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub enabled: Option<bool>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub tls: Option<bool>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliProviderSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Desc)]
    pub sort_dir: CliSortDirection,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderAddArgs {
    pub name: String,
    #[arg(value_name = "PROVIDER_ENDPOINT")]
    pub provider_endpoint: String,

    #[arg(long)]
    pub comment: Option<String>,

    #[arg(long, default_value_t = 10)]
    pub timeout_seconds: u32,

    #[arg(long, default_value_t = false)]
    pub tls: bool,

    #[arg(long, default_value_t = false)]
    pub insecure_tls: bool,

    #[arg(long = "provider", value_name = "PROVIDER_TYPE", required = true, num_args = 1..)]
    pub providers: Vec<CliSourceProvider>,

    /// Shared secret used to authenticate against a remote provider server
    #[arg(long)]
    pub jwt_secret: Option<String>,

    /// Custom PEM CA bundle used when connecting to a TLS-enabled provider endpoint
    #[arg(long)]
    pub custom_ca: Option<String>,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderUpdateArgs {
    pub name: String,

    #[arg(long = "provider-endpoint")]
    pub provider_endpoint: Option<String>,

    #[arg(long)]
    pub comment: Option<String>,

    #[arg(long, default_value_t = false)]
    pub clear_comment: bool,

    #[arg(long)]
    pub timeout_seconds: Option<u32>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub tls: Option<bool>,

    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub insecure_tls: Option<bool>,

    #[arg(long = "provider", value_name = "PROVIDER_TYPE")]
    pub providers: Vec<CliSourceProvider>,

    /// Replace the shared secret used to authenticate against a remote provider server
    #[arg(long, conflicts_with = "clear_jwt_secret")]
    pub jwt_secret: Option<String>,

    /// Clear the stored remote provider shared secret
    #[arg(long, default_value_t = false, conflicts_with = "jwt_secret")]
    pub clear_jwt_secret: bool,

    /// Replace the custom PEM CA bundle for TLS provider endpoints
    #[arg(long, conflicts_with = "clear_custom_ca")]
    pub custom_ca: Option<String>,

    /// Clear the stored custom PEM CA bundle
    #[arg(long, default_value_t = false, conflicts_with = "custom_ca")]
    pub clear_custom_ca: bool,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderDeleteArgs {
    pub name: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderReconnectArgs {
    pub name: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderEnableArgs {
    pub name: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderDisableArgs {
    pub name: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}
