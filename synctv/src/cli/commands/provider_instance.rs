use super::prelude::*;

#[derive(Debug, Args)]
pub struct ProviderInstanceCommand {
    #[command(subcommand)]
    pub command: ProviderInstanceSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderInstanceSubcommand {
    /// List enabled instances available to app clients
    Available(ProviderInstanceAvailableArgs),
    /// List configured instances
    List(ProviderInstanceListArgs),
    /// Create an instance
    Create(ProviderInstanceCreateArgs),
    /// Update an instance
    Update(ProviderInstanceUpdateArgs),
    /// Delete an instance
    Delete(ProviderInstanceNameArgs),
    /// Reconnect an instance
    Reconnect(ProviderInstanceNameArgs),
    /// Enable an instance
    Enable(ProviderInstanceNameArgs),
    /// Disable an instance
    Disable(ProviderInstanceNameArgs),
}

#[derive(Debug, Args)]
pub struct ProviderInstanceAvailableArgs {
    #[arg(long, value_enum)]
    pub provider_type: Option<CliSourceProvider>,
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderInstanceListArgs {
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
pub struct ProviderInstanceCreateArgs {
    pub name: String,
    #[arg(value_name = "ENDPOINT")]
    pub instance_endpoint: String,
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
    #[arg(long)]
    pub jwt_secret: Option<String>,
    #[arg(long)]
    pub custom_ca: Option<String>,
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderInstanceUpdateArgs {
    pub name: String,
    #[arg(long = "instance-endpoint")]
    pub instance_endpoint: Option<String>,
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
    #[arg(long, conflicts_with = "clear_jwt_secret")]
    pub jwt_secret: Option<String>,
    #[arg(long, default_value_t = false, conflicts_with = "jwt_secret")]
    pub clear_jwt_secret: bool,
    #[arg(long, conflicts_with = "clear_custom_ca")]
    pub custom_ca: Option<String>,
    #[arg(long, default_value_t = false, conflicts_with = "custom_ca")]
    pub clear_custom_ca: bool,
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct ProviderInstanceNameArgs {
    pub name: String,
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}
