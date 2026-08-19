use super::*;

pub(super) async fn execute_playlist(playlist_command: PlaylistCommand) -> Result<()> {
    let PlaylistCommand { command } = playlist_command;
    match command {
        PlaylistSubcommand::List(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "list playlists",
                list_playlists,
                management_proto::ListPlaylistsRequest {
                    room_id: args.room.room_id,
                    parent_id: normalized_optional_cli_value(args.parent_id.as_deref())
                        .unwrap_or_default(),
                    page: args.page,
                    page_size: args.page_size,
                    search: args.search.unwrap_or_default(),
                    source_provider: optional_source_provider_to_proto_i32(args.source_provider),
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    dynamic_only: args.dynamic_only,
                    sort_by: args.sort_by.map_or(
                        management_proto::PlaylistListSortBy::Position as i32,
                        CliPlaylistSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                    availability: args.availability.to_proto(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Get(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "get playlist",
                get_playlist,
                management_proto::GetPlaylistRequest {
                    room_id: args.room.room_id,
                    playlist_id: args.playlist_id,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Create(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let source_config = parse_optional_playlist_source_config_json(
                args.source_provider,
                args.source_config_json.as_deref(),
            )?;
            let response = management_unary_call!(
                session,
                "create playlist",
                create_playlist,
                management_proto::CreatePlaylistRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    name: args.name,
                    parent_id: normalized_optional_cli_value(args.parent_id.as_deref())
                        .unwrap_or_default(),
                    source_provider: optional_source_provider_to_proto_i32(args.source_provider),
                    source_config,
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    browse_access_mode: args
                        .browse_access_mode
                        .map_or(0, CliPlaylistBrowseAccessMode::to_proto),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Update(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "update playlist",
                update_playlist,
                management_proto::UpdatePlaylistRequest {
                    room_id: args.room.room_id,
                    playlist_id: args.playlist_id,
                    name: args.name,
                    browse_access_mode: args
                        .browse_access_mode
                        .map(CliPlaylistBrowseAccessMode::to_proto),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Move(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let anchor = match (args.before_playlist_id, args.after_playlist_id) {
                (Some(id), None) if !id.trim().is_empty() => {
                    Some(management_proto::move_playlist_request::Anchor::BeforePlaylistId(id))
                }
                (None, Some(id)) if !id.trim().is_empty() => {
                    Some(management_proto::move_playlist_request::Anchor::AfterPlaylistId(id))
                }
                _ => None,
            };
            let response = management_unary_call!(
                session,
                "move playlist",
                move_playlist,
                management_proto::MovePlaylistRequest {
                    room_id: args.room.room_id,
                    playlist_id: args.playlist_id,
                    anchor,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "delete playlist",
                delete_playlist,
                management_proto::DeletePlaylistRequest {
                    room_id: args.room.room_id,
                    playlist_id: args.playlist_id,
                    force: args.force,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        PlaylistSubcommand::Provider(command) => execute_playlist_provider(command).await,
    }
}
