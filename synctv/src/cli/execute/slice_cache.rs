use super::*;

pub(super) async fn execute_slice_cache(slice_cache_command: SliceCacheCommand) -> Result<()> {
    let SliceCacheCommand { command } = slice_cache_command;
    match command {
        SliceCacheSubcommand::Stats(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get slice cache stats",
                get_slice_cache_stats,
                management_proto::GetSliceCacheStatsRequest {
                    node_id: args.target.node_id.unwrap_or_default(),
                    all_nodes: args.target.all_nodes,
                }
            )?;
            args.remote.print_output(&response)
        }
        SliceCacheSubcommand::Purge(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "purge slice cache",
                purge_slice_cache,
                management_proto::PurgeSliceCacheRequest {
                    node_id: args.target.node_id.unwrap_or_default(),
                    all_nodes: args.target.all_nodes,
                }
            )?;
            args.remote.print_output(&response)
        }
        SliceCacheSubcommand::EvictExpired(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "evict expired slice cache entries",
                evict_expired_slice_cache,
                management_proto::EvictExpiredSliceCacheRequest {
                    node_id: args.target.node_id.unwrap_or_default(),
                    all_nodes: args.target.all_nodes,
                }
            )?;
            args.remote.print_output(&response)
        }
    }
}
