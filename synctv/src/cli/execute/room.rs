use super::*;

pub(super) async fn execute_room(room_command: RoomCommand) -> Result<()> {
    let RoomCommand { command } = room_command;
    match command {
        RoomSubcommand::Create(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "create room",
                create_room,
                management_proto::CreateRoomRequest {
                    actor: Some(args.actor.to_management_proto()?),
                    name: args.name,
                    settings: parse_optional_room_settings_json(args.settings_json.as_deref())?,
                    description: args.description.unwrap_or_default(),
                    password: args.password.unwrap_or_default(),
                    category_id: args.category_id.unwrap_or_default(),
                    label_ids: args.label_ids,
                    is_public: args.private_room.then_some(false),
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list rooms",
                list_rooms,
                management_proto::ListRoomsRequest {
                    page: args.page,
                    page_size: args.page_size,
                    status: args.status.map_or(
                        synctv_proto::common::RoomStatus::Unspecified as i32,
                        CliRoomStatus::to_proto,
                    ),
                    search: args.search.unwrap_or_default(),
                    creator: args.creator.to_management_proto()?,
                    is_banned: args.is_banned,
                    sort_by: args.sort_by.map_or(
                        management_proto::RoomListSortBy::CreatedAt as i32,
                        CliRoomSortField::to_proto
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                    category_id: args.category_id.unwrap_or_default(),
                    label_ids: args.label_ids,
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::Get(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "get room",
                get_room,
                management_proto::GetRoomRequest {
                    room_id: args.room_id,
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::TransferOwner(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "transfer room ownership",
                transfer_room_ownership,
                management_proto::TransferRoomOwnershipRequest {
                    room_id: args.room_id,
                    actor: Some(args.actor.to_management_proto()?),
                    new_owner: Some(args.new_owner.to_management_proto()?),
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::Favorite(favorite_command) => match favorite_command.command {
            RoomFavoriteSubcommand::Add(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "favorite room",
                    favorite_room,
                    management_proto::FavoriteRoomRequest {
                        actor: Some(args.actor.to_management_proto()?),
                        request: Some(synctv_proto::client::FavoriteRoomRequest {
                            room_id: args.room_id,
                        }),
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomFavoriteSubcommand::Remove(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "unfavorite room",
                    unfavorite_room,
                    management_proto::UnfavoriteRoomRequest {
                        actor: Some(args.actor.to_management_proto()?),
                        request: Some(synctv_proto::client::UnfavoriteRoomRequest {
                            room_id: args.room_id,
                        }),
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomFavoriteSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list favorite rooms",
                    list_favorite_rooms,
                    management_proto::ListFavoriteRoomsRequest {
                        actor: Some(args.actor.to_management_proto()?),
                        request: Some(synctv_proto::client::ListFavoriteRoomsRequest {
                            page: args.page,
                            page_size: args.page_size,
                            search: args.search.unwrap_or_default(),
                        }),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        RoomSubcommand::Settings(settings_command) => match settings_command.command {
            RoomSettingsSubcommand::Get(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "get room settings",
                    get_room_settings,
                    management_proto::GetRoomSettingsRequest {
                        room_id: args.room.resolved_room_id().to_string(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomSettingsSubcommand::Update(args) => {
                let mut request: synctv_proto::admin::UpdateRoomSettingsRequest =
                    parse_masked_settings_request(
                        "room settings update request",
                        args.request_json.as_deref(),
                        &args.set,
                        &args.unset,
                    )?;
                request.room_id = args.room.resolved_room_id().to_string();
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "update room settings",
                    update_room_settings,
                    request
                )?;
                args.remote.print_output(&response)
            }
            RoomSettingsSubcommand::Reset(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "reset room settings",
                    reset_room_settings,
                    management_proto::ResetRoomSettingsRequest {
                        room_id: args.room.resolved_room_id().to_string(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        RoomSubcommand::Category(category_command) => match category_command.command {
            RoomCategorySubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list room categories",
                    list_room_categories,
                    synctv_proto::admin::ListRoomCategoriesRequest {
                        include_disabled: args.include_disabled,
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomCategorySubcommand::Upsert(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "upsert room category",
                    upsert_room_category,
                    synctv_proto::admin::UpsertRoomCategoryRequest {
                        key: args.key,
                        name: args.name,
                        description: args.description.unwrap_or_default(),
                        sort_order: args.sort_order,
                        is_enabled: args.enabled,
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomCategorySubcommand::Delete(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "delete room category",
                    delete_room_category,
                    synctv_proto::admin::DeleteRoomCategoryRequest {
                        category_id: args.category_id,
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        RoomSubcommand::Label(label_command) => match label_command.command {
            RoomLabelSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list room labels",
                    list_room_labels,
                    synctv_proto::admin::ListRoomLabelsRequest {
                        include_disabled: args.include_disabled,
                        category_id: args.category_id.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomLabelSubcommand::Upsert(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "upsert room label",
                    upsert_room_label,
                    synctv_proto::admin::UpsertRoomLabelRequest {
                        key: args.key,
                        name: args.name,
                        description: args.description.unwrap_or_default(),
                        color: args.color.unwrap_or_default(),
                        category_id: args.category_id.unwrap_or_default(),
                        sort_order: args.sort_order,
                        is_enabled: args.enabled,
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomLabelSubcommand::Delete(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "delete room label",
                    delete_room_label,
                    synctv_proto::admin::DeleteRoomLabelRequest {
                        label_id: args.label_id,
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        RoomSubcommand::Taxonomy(taxonomy_command) => match taxonomy_command.command {
            RoomTaxonomySubcommand::Set(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "update room taxonomy",
                    update_room_taxonomy,
                    synctv_proto::admin::UpdateRoomTaxonomyRequest {
                        room_id: args.room_id,
                        category_id: args.category_id,
                        label_ids: args.label_ids,
                        clear_category: args.clear_category,
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        RoomSubcommand::Chat(chat_command) => match chat_command.command {
            RoomChatSubcommand::Search(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let (user_id, username) = args.sender.to_management_selector();
                let response = management_unary_call!(
                    session,
                    "search chat messages",
                    search_chat_messages,
                    management_proto::SearchChatMessagesRequest {
                        room_id: args.room.room_id,
                        actor: Some(args.actor.to_management_proto()?),
                        query: args.query,
                        cursor: args.cursor.unwrap_or_default(),
                        limit: args.limit,
                        include_deleted: args.include_deleted,
                        user_id,
                        username,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
        },
        RoomSubcommand::Member(member_command) => match member_command.command {
            RoomMemberSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "get room members",
                    get_room_members,
                    management_proto::GetRoomMembersRequest {
                        room_id: args.resolved_room_id().to_string(),
                        page: args.page,
                        page_size: args.page_size,
                        search: args.search.unwrap_or_default(),
                        role: args.role.map_or(
                            synctv_proto::common::RoomMemberRole::Unspecified as i32,
                            CliRoomMemberRole::to_proto,
                        ),
                        sort_by: args.sort_by.map_or(
                            management_proto::RoomMemberListSortBy::JoinedAt as i32,
                            CliRoomMemberSortField::to_proto,
                        ),
                        sort_direction: args.sort_dir.to_proto(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomMemberSubcommand::Add(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let (user_id, username) = args.user.to_management_selector()?;
                let response = management_unary_call!(
                    session,
                    "add room member",
                    add_member,
                    management_proto::AddMemberRequest {
                        room_id: args.room.room_id,
                        user_id,
                        username,
                        role: args.role.to_proto(),
                        notify: args.notify,
                        remark_name: args.remark_name.unwrap_or_default(),
                        display_tag: args.display_tag.unwrap_or_default(),
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomMemberSubcommand::SetRemarkName(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let (user_id, username) = args.user.to_management_selector()?;
                let response = management_unary_call!(
                    session,
                    "update room member remark name",
                    update_member_remark_name,
                    management_proto::UpdateMemberRemarkNameRequest {
                        room_id: args.room.room_id,
                        user_id,
                        username,
                        remark_name: args.remark_name,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomMemberSubcommand::SetDisplayTag(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let (user_id, username) = args.user.to_management_selector()?;
                let response = management_unary_call!(
                    session,
                    "update room member display tag",
                    update_member_display_tag,
                    management_proto::UpdateMemberDisplayTagRequest {
                        room_id: args.room.room_id,
                        user_id,
                        username,
                        display_tag: args.display_tag,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomMemberSubcommand::SetPermissions(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let (user_id, username) = args.user.to_management_selector()?;
                let response = management_unary_call!(
                    session,
                    "update room member permissions",
                    update_member_permissions,
                    management_proto::UpdateMemberPermissionsRequest {
                        room_id: args.room.room_id,
                        user_id,
                        username,
                        role: args.role.map_or(
                            synctv_proto::common::RoomMemberRole::Unspecified as i32,
                            CliRoomMemberRole::to_proto,
                        ),
                        added_permissions: args.added_permissions.map_or(0, Into::into),
                        removed_permissions: args.removed_permissions.map_or(0, Into::into),
                        admin_added_permissions: args.admin_added_permissions.map_or(0, Into::into),
                        admin_removed_permissions: args
                            .admin_removed_permissions
                            .map_or(0, Into::into),
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomMemberSubcommand::Kick(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let (user_id, username) = args.user.to_management_selector()?;
                let response = management_unary_call!(
                    session,
                    "kick room member",
                    kick_member,
                    management_proto::KickMemberRequest {
                        room_id: args.room.room_id,
                        user_id,
                        username,
                        kick_cooldown_seconds: args.kick_cooldown_seconds,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
        },
        RoomSubcommand::Playback(playback_command) => match playback_command.command {
            RoomPlaybackSubcommand::Get(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "get room playback",
                    get_playback,
                    management_proto::GetPlaybackRequest {
                        room_id: args.room.room_id,
                        playback_client_profile: args.playback_client_profile.to_proto(),
                    }
                )?;
                let output = build_get_playback_cli_output(response, &args.room.remote.global);
                args.room.remote.print_output(&output)
            }
            RoomPlaybackSubcommand::Start(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let room_id = args.room.room_id;
                let media_id = args.media_id;
                let playlist_id = args.playlist_id;
                let target = parse_optional_provider_target_json(args.target_json.as_deref())?;
                let actor = args.actor.to_management_proto()?;
                management_unary_call!(
                    session,
                    "start room playback",
                    start_playback,
                    management_proto::StartPlaybackRequest {
                        actor,
                        room_id: room_id.clone(),
                        media_id: media_id.clone().unwrap_or_default(),
                        playlist_id: playlist_id.clone().unwrap_or_default(),
                        target,
                    }
                )?;
                args.room.remote.print_output(&PlaybackStartCliOutput {
                    success: true,
                    room_id,
                    media_id,
                    playlist_id,
                })
            }
            RoomPlaybackSubcommand::Play(args) => {
                let playing = Some(args.playing.unwrap_or(true));
                execute_room_playback_state_update(
                    args.room,
                    CliPlaybackStateUpdateType::Play,
                    playing,
                    args.position,
                    args.speed,
                    args.version,
                )
                .await
            }
            RoomPlaybackSubcommand::Pause(args) => {
                let playing = Some(args.playing.unwrap_or(false));
                execute_room_playback_state_update(
                    args.room,
                    CliPlaybackStateUpdateType::Pause,
                    playing,
                    args.position,
                    args.speed,
                    args.version,
                )
                .await
            }
            RoomPlaybackSubcommand::Seek(args) => {
                execute_room_playback_state_update(
                    args.room,
                    CliPlaybackStateUpdateType::Seek,
                    args.playing,
                    Some(args.position),
                    args.speed,
                    args.version,
                )
                .await
            }
            RoomPlaybackSubcommand::Speed(args) => {
                execute_room_playback_state_update(
                    args.room,
                    CliPlaybackStateUpdateType::Speed,
                    args.playing,
                    args.position,
                    Some(args.speed),
                    args.version,
                )
                .await
            }
            RoomPlaybackSubcommand::Stop(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let room_id = args.room.room_id;
                management_unary_call!(
                    session,
                    "stop room playback",
                    stop_playback,
                    management_proto::StopPlaybackRequest {
                        room_id: room_id.clone(),
                    }
                )?;
                args.room.remote.print_output(&PlaybackStopCliOutput {
                    success: true,
                    room_id,
                })
            }
        },
        RoomSubcommand::Stream(stream_command) => match stream_command.command {
            RoomStreamSubcommand::List(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list room streams",
                    list_room_streams,
                    management_proto::ListRoomStreamsRequest {
                        room_id: args.room.room_id,
                        page: args.page,
                        page_size: args.page_size,
                        search: args.search.unwrap_or_default(),
                        sort_by: args.sort_by.map_or(
                            management_proto::RoomStreamListSortBy::MediaId as i32,
                            CliRoomStreamSortField::to_proto,
                        ),
                        sort_direction: args.sort_dir.to_proto(),
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomStreamSubcommand::PublishKey(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "create room stream publish key",
                    create_publish_key,
                    management_proto::CreatePublishKeyRequest {
                        actor: Some(args.actor.to_management_proto()?),
                        room_id: args.room.room_id,
                        media_id: args.media_id,
                        r#type: args.key_type.to_proto(),
                        expires_at: args.expires_at,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomStreamSubcommand::Get(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "get room stream info",
                    get_stream_info,
                    management_proto::GetStreamInfoRequest {
                        room_id: args.room.room_id,
                        media_id: args.media_id,
                    }
                )?;
                args.room.remote.print_output(&response)
            }
            RoomStreamSubcommand::Kick(args) => {
                let session = connect_remote_access(&args.room.remote).await?;
                let response = management_unary_call!(
                    session,
                    "kick room stream",
                    kick_room_stream,
                    management_proto::KickRoomStreamRequest {
                        room_id: args.room.room_id,
                        media_id: args.media_id,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.room.remote.print_output(&response)
            }
        },
        RoomSubcommand::Batch(batch_command) => match batch_command.command {
            RoomBatchSubcommand::Ban(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "batch ban rooms",
                    batch_ban_rooms,
                    management_proto::BatchBanRoomsRequest {
                        room_ids: args.resolved_room_ids(),
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            RoomBatchSubcommand::Delete(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "batch delete rooms",
                    batch_delete_rooms,
                    management_proto::BatchDeleteRoomsRequest {
                        room_ids: args.resolved_room_ids(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        RoomSubcommand::SetPassword(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "update room password",
                update_room_password,
                management_proto::UpdateRoomPasswordRequest {
                    room_id: args.room_id,
                    new_password: args.new_password,
                    clear: args.clear,
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::Ban(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "ban room",
                ban_room,
                management_proto::BanRoomRequest {
                    room_id: args.room_id,
                    reason: args.reason.unwrap_or_default(),
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::Unban(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "unban room",
                unban_room,
                management_proto::UnbanRoomRequest {
                    room_id: args.room_id,
                }
            )?;
            args.remote.print_output(&response)
        }
        RoomSubcommand::Bans(command) => match command.command {
            RoomBansSubcommand::List(args) => {
                super::ban::execute_ban_records_list(
                    &args.remote,
                    synctv_proto::admin::BanTargetType::Room as i32,
                    args.active,
                    String::new(),
                    args.room_id.unwrap_or_default(),
                    args.page,
                    args.page_size,
                )
                .await
            }
        },
        RoomSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "delete room",
                delete_room,
                management_proto::DeleteRoomRequest {
                    room_id: args.room_id,
                }
            )?;
            args.remote.print_output(&response)
        }
    }
}
