use super::*;

pub(super) async fn execute_provider_alist(command: ProviderAlistCommand) -> Result<()> {
    match command.command {
        ProviderAlistSubcommand::Login(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let credential = alist_login_credential(&args)?;
            let response = management_unary_call!(
                session,
                "alist login",
                alist_login,
                management_proto::AlistLoginRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::LoginRequest {
                        host: args.host,
                        username: args.account_username,
                        credential: Some(credential),
                        otp_code: args.otp_code.unwrap_or_default(),
                        otp_secret: args.otp_secret.unwrap_or_default(),
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderAlistSubcommand::List(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "alist list",
                alist_list,
                management_proto::AlistListRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::ListRequest {
                        server_id: args.bind.server_id,
                        path: args.path,
                        password: args.password.unwrap_or_default(),
                        page: args.page,
                        per_page: args.per_page,
                        refresh: args.refresh,
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderAlistSubcommand::Search(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "alist search",
                alist_search,
                management_proto::AlistSearchRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::SearchRequest {
                        server_id: args.bind.server_id,
                        parent: args.parent,
                        keywords: args.keywords,
                        scope: args.scope,
                        page: args.page,
                        per_page: args.per_page,
                        password: args.password.unwrap_or_default(),
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderAlistSubcommand::Me(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "alist me",
                alist_get_me,
                management_proto::AlistGetMeRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::GetMeRequest {
                        server_id: args.bind.server_id,
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderAlistSubcommand::Logout(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "alist logout",
                alist_logout,
                management_proto::AlistLogoutRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::LogoutRequest {
                        server_id: args.bind.server_id,
                        instance_name: provider_service_instance_name(&args.bind.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
        ProviderAlistSubcommand::Binds(args) => {
            let (session, actor_user_id) = connect_provider_actor_access(&args.access).await?;
            let response = management_unary_call!(
                session,
                "alist get binds",
                alist_get_binds,
                management_proto::AlistGetBindsRequest {
                    actor: Some(actor_user_id),
                    request: Some(synctv_proto::providers::alist::GetBindsRequest {
                        instance_name: provider_service_instance_name(&args.instance),
                    }),
                }
            )?;
            args.access.remote.print_output(&response)
        }
    }
}
