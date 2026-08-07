use super::*;

pub(super) async fn execute_provider_rtmp(command: ProviderRtmpCommand) -> Result<()> {
    match command.command {
        ProviderRtmpSubcommand::CreatePublishKey(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let room_id = args.room_id.clone();
            let media_id = args.resolved_media_id()?.to_string();
            let response = management_unary_call!(
                session,
                "create rtmp publish key",
                create_publish_key,
                management_proto::CreatePublishKeyRequest {
                    actor: Some(actor_user_id),
                    room_id,
                    media_id,
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderRtmpSubcommand::GetStreamInfo(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let media_id = args.resolved_media_id()?.to_string();
            let response = management_unary_call!(
                session,
                "get rtmp stream info",
                get_stream_info,
                management_proto::GetStreamInfoRequest {
                    room_id: args.room_id,
                    media_id,
                }
            )?;
            args.remote.print_output(&response)
        }
    }
}
