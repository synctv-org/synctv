use super::*;

pub(super) async fn execute_provider_emby(command: ProviderEmbyCommand) -> Result<()> {
    match command.command {
        ProviderEmbySubcommand::Login(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let credential = emby_login_credential(&args)?;
            let response = management_unary_call!(
                session,
                "emby login",
                emby_login,
                management_proto::EmbyLoginRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::emby::LoginRequest {
                        host: args.server_endpoint,
                        username: args.account_username,
                        credential: Some(credential),
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderEmbySubcommand::List(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "emby list",
                emby_list,
                management_proto::EmbyListRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::emby::ListRequest {
                        server_id: args.bind.server_id,
                        path: args.path,
                        start_index: args.start_index,
                        limit: args.limit,
                        search_term: args.search_term.unwrap_or_default(),
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderEmbySubcommand::Me(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "emby me",
                emby_get_me,
                management_proto::EmbyGetMeRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::emby::GetMeRequest {
                        server_id: args.bind.server_id,
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderEmbySubcommand::Logout(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "emby logout",
                emby_logout,
                management_proto::EmbyLogoutRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::emby::LogoutRequest {
                        server_id: args.server_id,
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderEmbySubcommand::Binds(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "emby get binds",
                emby_get_binds,
                management_proto::EmbyGetBindsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::emby::GetBindsRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
    }
}
