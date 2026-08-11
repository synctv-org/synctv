use super::*;

pub(super) async fn execute_provider_twitch(command: ProviderTwitchCommand) -> Result<()> {
    match command.command {
        ProviderTwitchSubcommand::Bind(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "Twitch bind",
                twitch_bind,
                management_proto::TwitchBindRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::twitch::BindRequest {
                        auth_token: args.oauth_token,
                        device_id: args.device_id,
                        client_integrity: args.client_integrity,
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderTwitchSubcommand::Binds(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "Twitch get binds",
                twitch_get_binds,
                management_proto::TwitchGetBindsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::twitch::GetBindsRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderTwitchSubcommand::Unbind(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "Twitch unbind",
                twitch_unbind,
                management_proto::TwitchUnbindRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::twitch::UnbindRequest {
                        server_id: args.server_id,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderTwitchSubcommand::Resolve(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "Twitch resolve",
                twitch_resolve,
                management_proto::TwitchResolveRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::twitch::ResolveRequest {
                        resource: args.resource,
                        instance_name: provider_service_instance_name(&args.instance),
                        shared: false,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderTwitchSubcommand::Items(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "Twitch list channel items",
                twitch_list_channel_items,
                management_proto::TwitchListChannelItemsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::twitch::ListChannelItemsRequest {
                        resource: args.channel,
                        content: args.content.to_proto(),
                        cursor: args.cursor,
                        page_size: args.page_size,
                        instance_name: provider_service_instance_name(&args.instance),
                        shared: false,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderTwitchSubcommand::FollowedLive(args) => execute_twitch_followed_live(args).await,
        ProviderTwitchSubcommand::CategoryStreams(args) => {
            execute_twitch_category_streams(args).await
        }
        ProviderTwitchSubcommand::TopCategories(args) => execute_twitch_top_categories(args).await,
        ProviderTwitchSubcommand::SearchLive(args) => execute_twitch_search_live(args).await,
        ProviderTwitchSubcommand::Schedule(args) => execute_twitch_schedule(args).await,
    }
}

pub(crate) async fn execute_twitch_followed_live(
    args: ProviderTwitchFollowedLiveArgs,
) -> Result<()> {
    provider_call!(
        args,
        twitch_list_followed_live,
        TwitchListFollowedLiveRequest,
        synctv_proto::providers::twitch::ListFollowedLiveRequest {
            cursor: args.cursor,
            page_size: args.page_size,
            instance_name: provider_service_instance_name(&args.instance),
            shared: false
        }
    )
}

pub(crate) async fn execute_twitch_category_streams(
    args: ProviderTwitchCategoryStreamsArgs,
) -> Result<()> {
    provider_call!(
        args,
        twitch_list_category_streams,
        TwitchListCategoryStreamsRequest,
        synctv_proto::providers::twitch::ListCategoryStreamsRequest {
            category_id: args.category_id,
            category_name: args.category_name,
            cursor: args.cursor,
            page_size: args.page_size,
            instance_name: provider_service_instance_name(&args.instance),
            shared: false
        }
    )
}

pub(crate) async fn execute_twitch_top_categories(
    args: ProviderTwitchTopCategoriesArgs,
) -> Result<()> {
    provider_call!(
        args,
        twitch_list_top_categories,
        TwitchListTopCategoriesRequest,
        synctv_proto::providers::twitch::ListTopCategoriesRequest {
            cursor: args.cursor,
            page_size: args.page_size,
            instance_name: provider_service_instance_name(&args.instance),
            shared: false
        }
    )
}

pub(crate) async fn execute_twitch_search_live(args: ProviderTwitchSearchLiveArgs) -> Result<()> {
    provider_call!(
        args,
        twitch_search_live_channels,
        TwitchSearchLiveChannelsRequest,
        synctv_proto::providers::twitch::SearchLiveChannelsRequest {
            query: args.query,
            cursor: args.cursor,
            page_size: args.page_size,
            instance_name: provider_service_instance_name(&args.instance),
            shared: false
        }
    )
}

pub(crate) async fn execute_twitch_schedule(args: ProviderTwitchScheduleArgs) -> Result<()> {
    provider_call!(
        args,
        twitch_list_schedule,
        TwitchListScheduleRequest,
        synctv_proto::providers::twitch::ListScheduleRequest {
            broadcaster_id: args.broadcaster_id,
            cursor: args.cursor,
            page_size: args.page_size,
            instance_name: provider_service_instance_name(&args.instance),
            shared: false
        }
    )
}
