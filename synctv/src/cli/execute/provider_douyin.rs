use super::*;

pub(super) async fn execute_provider_douyin(command: ProviderDouyinCommand) -> Result<()> {
    match command.command {
        ProviderDouyinSubcommand::Bind(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "Douyin bind",
                douyin_bind,
                management_proto::DouyinBindRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::douyin::BindRequest {
                        label: args.label,
                        cookie: args.cookie,
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderDouyinSubcommand::Binds(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "Douyin get binds",
                douyin_get_binds,
                management_proto::DouyinGetBindsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::douyin::GetBindsRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderDouyinSubcommand::Unbind(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "Douyin unbind",
                douyin_unbind,
                management_proto::DouyinUnbindRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::douyin::UnbindRequest {
                        server_id: args.server_id,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderDouyinSubcommand::Resolve(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "Douyin resolve",
                douyin_resolve,
                management_proto::DouyinResolveRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::douyin::ResolveRequest {
                        resource: args.resource,
                        instance_name: provider_service_instance_name(&args.instance),
                        shared: false,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderDouyinSubcommand::Posts(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "Douyin list user posts",
                douyin_list_user_posts,
                management_proto::DouyinListUserPostsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::douyin::ListUserPostsRequest {
                        sec_uid: args.sec_uid,
                        cursor: args.cursor,
                        page_size: args.page_size,
                        instance_name: provider_service_instance_name(&args.instance),
                        shared: false,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
    }
}
