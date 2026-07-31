use super::*;
pub(crate) async fn execute_provider_qnap(command: ProviderQnapCommand) -> Result<()> {
    match command.command {
        ProviderQnapSubcommand::Login(args) => provider_call!(
            args,
            qnap_login,
            QnapLoginRequest,
            synctv_proto::providers::qnap::LoginRequest {
                endpoint: args.server_endpoint,
                username: args.account_username,
                password: args.password,
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
        ProviderQnapSubcommand::List(args) => provider_call!(
            args,
            qnap_list,
            QnapListRequest,
            synctv_proto::providers::qnap::ListRequest {
                server_id: args.bind.server_id,
                path: args.path,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderQnapSubcommand::Capabilities(args) => provider_call!(
            args,
            qnap_get_capabilities,
            QnapGetCapabilitiesRequest,
            synctv_proto::providers::qnap::GetCapabilitiesRequest {
                server_id: args.bind.server_id,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderQnapSubcommand::Logout(args) => provider_call!(
            args,
            qnap_logout,
            QnapLogoutRequest,
            synctv_proto::providers::qnap::LogoutRequest {
                server_id: args.server_id,
            }
        ),
        ProviderQnapSubcommand::Binds(args) => provider_call!(
            args,
            qnap_get_binds,
            QnapGetBindsRequest,
            synctv_proto::providers::qnap::GetBindsRequest {
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
    }
}
