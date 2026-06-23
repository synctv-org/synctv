use super::prelude::*;

#[derive(Debug, Args)]
pub struct SystemCommand {
    #[command(subcommand)]
    pub command: SystemSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SystemSubcommand {
    /// Show system statistics
    Stats(SystemStatsArgs),
    /// Active stream inspection and control
    Stream(SystemStreamCommand),
}

#[derive(Debug, Args)]
pub struct SystemStreamCommand {
    #[command(subcommand)]
    pub command: SystemStreamSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SystemStreamSubcommand {
    /// List active streams across the cluster
    List(SystemStreamListArgs),
    /// Kick an active stream
    Kick(SystemStreamKickArgs),
}

#[derive(Debug, Args)]
pub struct SystemStatsArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SystemStreamListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub room_id: Option<String>,

    #[command(flatten)]
    pub user: StreamUserFilterArgs,

    #[arg(long)]
    pub node_id: Option<String>,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliActiveStreamSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Desc)]
    pub sort_dir: CliSortDirection,
}

#[derive(Debug, Args)]
pub struct SystemStreamKickArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(long)]
    pub room_id: String,

    #[arg(long)]
    pub media_id: String,

    #[arg(long)]
    pub reason: Option<String>,
}
