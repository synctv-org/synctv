use super::*;

pub(super) async fn execute_playlist_provider(command: PlaylistProviderCommand) -> Result<()> {
    match command.command {
        PlaylistProviderSubcommand::Alist(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "create alist dynamic playlist",
                create_alist_playlist,
                management_proto::CreateAlistPlaylistRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    name: args.name,
                    parent_id: normalized_optional_cli_value(args.parent_id.as_deref())
                        .unwrap_or_default(),
                    server_id: args.server_id,
                    path: args.path,
                    password: args.password.unwrap_or_default(),
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistProviderSubcommand::Emby(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "create emby dynamic playlist",
                create_emby_playlist,
                management_proto::CreateEmbyPlaylistRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    name: args.name,
                    parent_id: normalized_optional_cli_value(args.parent_id.as_deref())
                        .unwrap_or_default(),
                    server_id: args.server_id,
                    item_id: args.item_id,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                }
            )?;
            args.room.remote.print_output(&response)
        }
    }
}
