use super::*;
pub(crate) async fn execute_provider_truenas(command: ProviderTruenasCommand) -> Result<()> {
    match command.command {
        ProviderTruenasSubcommand::Login(args) => provider_call!(
            args,
            truenas_login,
            TruenasLoginRequest,
            synctv_proto::providers::truenas::LoginRequest {
                endpoint: args.server_endpoint,
                api_key: args.api_key,
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
        ProviderTruenasSubcommand::List(args) => provider_call!(
            args,
            truenas_list,
            TruenasListRequest,
            synctv_proto::providers::truenas::ListRequest {
                server_id: args.bind.server_id,
                path: args.path,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderTruenasSubcommand::Logout(args) => provider_call!(
            args,
            truenas_logout,
            TruenasLogoutRequest,
            synctv_proto::providers::truenas::LogoutRequest {
                server_id: args.server_id,
            }
        ),
        ProviderTruenasSubcommand::Binds(args) => provider_call!(
            args,
            truenas_get_binds,
            TruenasGetBindsRequest,
            synctv_proto::providers::truenas::GetBindsRequest {
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
    }
}
