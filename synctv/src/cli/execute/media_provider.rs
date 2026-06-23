use super::*;

pub(super) async fn execute_media_provider(command: MediaProviderCommand) -> Result<()> {
    match command.command {
        MediaProviderSubcommand::Alist(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add alist media",
                add_alist_media,
                management_proto::AddAlistMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    server_id: args.server_id,
                    path: args.path,
                    password: args.password.unwrap_or_default(),
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaProviderSubcommand::Emby(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add emby media",
                add_emby_media,
                management_proto::AddEmbyMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    server_id: args.server_id,
                    item_id: args.item_id,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaProviderSubcommand::Bilibili(command) => {
            execute_media_provider_bilibili(command).await
        }
    }
}

pub(super) async fn execute_media_provider_bilibili(
    command: MediaProviderBilibiliCommand,
) -> Result<()> {
    match command.command {
        MediaProviderBilibiliSubcommand::Video(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add bilibili video media",
                add_bilibili_video_media,
                management_proto::AddBilibiliVideoMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    bvid: args.video.bvid.unwrap_or_default(),
                    aid: args.video.aid,
                    cid: args.cid,
                    shared: args.shared,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaProviderBilibiliSubcommand::Pgc(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add bilibili pgc media",
                add_bilibili_pgc_media,
                management_proto::AddBilibiliPgcMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    epid: args.epid,
                    cid: args.cid,
                    shared: args.shared,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaProviderBilibiliSubcommand::Live(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add bilibili live media",
                add_bilibili_live_media,
                management_proto::AddBilibiliLiveMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    room_live_id: args.room_live_id,
                    shared: args.shared,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
    }
}
