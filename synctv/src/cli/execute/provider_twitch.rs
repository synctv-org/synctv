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
                        server_id: args.bind.server_id,
                        instance_name: provider_service_instance_name(&args.bind.instance),
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
                        channel: args.channel,
                        content: args.content.to_proto(),
                        cursor: args.cursor,
                        page_size: args.page_size,
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
    }
}
