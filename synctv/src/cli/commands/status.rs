use super::prelude::*;

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    /// Query runtime status on a specific cluster node through the connected management endpoint
    #[arg(long, value_name = "NODE_ID", conflicts_with = "all_nodes")]
    pub node_id: Option<String>,

    /// Query runtime status on the connected node and all reachable cluster nodes
    #[arg(long)]
    pub all_nodes: bool,
}
