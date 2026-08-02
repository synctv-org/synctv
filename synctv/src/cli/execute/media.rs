use super::*;

pub(super) async fn execute_media(media_command: MediaCommand) -> Result<()> {
    let MediaCommand { command } = media_command;
    match command {
        MediaSubcommand::List(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "list media",
                list_media,
                management_proto::ListMediaRequest {
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    target: parse_optional_provider_target_json(args.target_json.as_deref())?,
                    pagination: Some(match args.cursor {
                        Some(cursor) => management_proto::list_media_request::Pagination::Cursor(
                            synctv_proto::client::CursorPagination { cursor },
                        ),
                        None => management_proto::list_media_request::Pagination::Page(
                            synctv_proto::client::PagePagination { page: args.page },
                        ),
                    }),
                    page_size: args.page_size,
                    search: args.search.unwrap_or_default(),
                    source_provider: optional_source_provider_to_proto_i32(args.source_provider),
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref(),
                    ),
                    sort_by: args.sort_by.map_or(
                        management_proto::MediaListSortBy::Position as i32,
                        CliMediaSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                    refresh: args.refresh,
                    availability: args.availability.to_proto(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Add(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let source_config =
                parse_media_source_config_json(args.source_provider, &args.source_config_json)?;
            let response = management_unary_call!(
                session,
                "add media",
                add_media,
                management_proto::AddMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    source_provider: args.source_provider.to_proto_i32(),
                    provider_instance_name: provider_instance_name_string(
                        args.provider_instance_name.as_deref()
                    ),
                    source_config: Some(source_config),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::AddUrl(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "add direct url media",
                add_direct_url_media,
                management_proto::AddDirectUrlMediaRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    room_id: args.room.room_id,
                    source_config: Some(synctv_proto::source_config::DirectUrlMediaSourceConfig {
                        medias: vec![synctv_proto::source_config::DirectUrlMediaResourceConfig {
                            name: String::new(),
                            url: args.url,
                            headers: Default::default(),
                            format: String::new(),
                        }],
                        default_media_index: Some(0),
                        subtitles: Vec::new(),
                        default_subtitle_index: None,
                        danmakus: Vec::new(),
                        default_danmaku_index: None,
                        playback_kind: Some(
                            synctv_proto::source_config::PlaybackKind::Regular as i32,
                        ),
                        duration_seconds: None,
                        prefer_proxy: Some(false),
                        proxy_only: None,
                    }),
                    playlist_id: normalized_optional_cli_value(args.playlist_id.as_deref())
                        .unwrap_or_default(),
                    name: args.name.unwrap_or_default(),
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Update(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "edit media",
                edit_media,
                management_proto::EditMediaRequest {
                    room_id: args.room.room_id,
                    media_id: args.media_id,
                    name: args.name,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let response = management_unary_call!(
                session,
                "delete media",
                delete_media,
                management_proto::DeleteMediaRequest {
                    room_id: args.room.room_id,
                    media_id: args.media_id,
                    force: args.force,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Move(args) => {
            let session = connect_remote_access(&args.room.remote).await?;
            let anchor = match (args.before_media_id, args.after_media_id) {
                (Some(id), None) if !id.trim().is_empty() => Some(
                    management_proto::move_media_request::Anchor::BeforeMediaId(id),
                ),
                (None, Some(id)) if !id.trim().is_empty() => Some(
                    management_proto::move_media_request::Anchor::AfterMediaId(id),
                ),
                _ => None,
            };
            let response = management_unary_call!(
                session,
                "move media",
                move_media,
                management_proto::MoveMediaRequest {
                    room_id: args.room.room_id,
                    media_ids: args.media_ids,
                    source_playlist_id: normalized_optional_cli_value(
                        args.from_playlist_id.as_deref(),
                    ),
                    target_playlist_id: normalized_optional_cli_value(
                        args.to_playlist_id.as_deref(),
                    ),
                    all_from_scope: args.all_from_scope,
                    anchor,
                }
            )?;
            args.room.remote.print_output(&response)
        }
        MediaSubcommand::Provider(command) => execute_media_provider(command).await,
    }
}
