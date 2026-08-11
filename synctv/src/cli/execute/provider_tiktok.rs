use super::*;

pub(super) async fn execute_provider_tiktok(command: ProviderTikTokCommand) -> Result<()> {
    match command.command {
        ProviderTikTokSubcommand::Bind(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "TikTok bind",
                tik_tok_bind,
                management_proto::TikTokBindRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::tiktok::BindRequest {
                        label: args.label,
                        cookie: args.cookie,
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderTikTokSubcommand::Binds(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "TikTok get binds",
                tik_tok_get_binds,
                management_proto::TikTokGetBindsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::tiktok::GetBindsRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderTikTokSubcommand::Unbind(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "TikTok unbind",
                tik_tok_unbind,
                management_proto::TikTokUnbindRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::tiktok::UnbindRequest {
                        server_id: args.server_id,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderTikTokSubcommand::Resolve(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "TikTok resolve",
                tik_tok_resolve,
                management_proto::TikTokResolveRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::tiktok::ResolveRequest {
                        resource: args.resource,
                        instance_name: provider_service_instance_name(&args.instance),
                        shared: false,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderTikTokSubcommand::User(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "TikTok get user",
                tik_tok_get_user,
                management_proto::TikTokGetUserRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::tiktok::GetUserRequest {
                        resource: args.unique_id,
                        instance_name: provider_service_instance_name(&args.instance),
                        shared: false,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderTikTokSubcommand::Posts(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "TikTok list user posts",
                tik_tok_list_user_posts,
                management_proto::TikTokListUserPostsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::tiktok::ListUserPostsRequest {
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
