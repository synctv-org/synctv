use super::prelude::*;

#[derive(Debug, Args)]
pub struct SliceCacheCommand {
    #[command(subcommand)]
    pub command: SliceCacheSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SliceCacheSubcommand {
    /// Show proxy slice cache runtime statistics and configuration
    Stats(SliceCacheStatsArgs),
    /// Remove all cached slice entries and metadata
    Purge(SliceCachePurgeArgs),
    /// Remove only expired cached slice entries
    EvictExpired(SliceCacheEvictExpiredArgs),
}

#[derive(Debug, Args)]
pub struct SliceCacheStatsArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub target: SliceCacheTargetArgs,
}

#[derive(Debug, Args)]
pub struct SliceCachePurgeArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub target: SliceCacheTargetArgs,
}

#[derive(Debug, Args)]
pub struct SliceCacheEvictExpiredArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub target: SliceCacheTargetArgs,
}

#[derive(Debug, Clone, Default, Args)]
pub struct SliceCacheTargetArgs {
    /// Query or manage slice cache on a specific cluster node through the connected management endpoint
    #[arg(long, value_name = "NODE_ID", conflicts_with = "all_nodes")]
    pub node_id: Option<String>,

    /// Query or manage slice cache on the connected node and all reachable cluster nodes
    #[arg(long)]
    pub all_nodes: bool,
}
