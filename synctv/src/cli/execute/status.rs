use super::*;

pub(super) async fn execute_status(args: StatusArgs) -> Result<()> {
    let session = connect_remote_access(&args.remote).await?;
    let response = management_unary_call!(
        session,
        "get status",
        get_server_state,
        management_proto::GetServerStateRequest {
            node_id: args.node_id.unwrap_or_default(),
            all_nodes: args.all_nodes,
        }
    )?;
    args.remote.print_output(&response)
}
