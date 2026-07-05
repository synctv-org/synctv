use super::*;

pub(super) async fn execute_system(system_command: SystemCommand) -> Result<()> {
    let SystemCommand { command } = system_command;
    match command {
        SystemSubcommand::Stats(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get system stats",
                get_system_stats,
                management_proto::GetSystemStatsRequest {}
            )?;
            args.remote.print_output(&response)
        }
        SystemSubcommand::Stream(stream_command) => match stream_command.command {
            SystemStreamSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let (user_id, username) = args.user.to_management_selector();
                let response = management_unary_call!(
                    session,
                    "list active streams",
                    list_active_streams,
                    management_proto::ListActiveStreamsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        room_id: args.room_id.unwrap_or_default(),
                        user_id,
                        username,
                        node_id: args.node_id.unwrap_or_default(),
                        search: args.search.unwrap_or_default(),
                        sort_by: args.sort_by.map_or(
                            management_proto::ActiveStreamListSortBy::StartedAt as i32,
                            CliActiveStreamSortField::to_proto,
                        ),
                        sort_direction: args.sort_dir.to_proto(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            SystemStreamSubcommand::Kick(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let room_id = args.room_id;
                let media_id = args.media_id;
                let reason = args.reason;
                management_unary_call!(
                    session,
                    "kick active stream",
                    kick_stream,
                    management_proto::KickStreamRequest {
                        room_id: room_id.clone(),
                        media_id: media_id.clone(),
                        reason: reason.clone().unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&KickStreamCliOutput {
                    success: true,
                    room_id,
                    media_id,
                    reason,
                })
            }
        },
    }
}
