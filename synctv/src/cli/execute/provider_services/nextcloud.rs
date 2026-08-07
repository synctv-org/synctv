use super::*;
pub(crate) async fn execute_provider_nextcloud(command: ProviderNextcloudCommand) -> Result<()> {
    match command.command {
        ProviderNextcloudSubcommand::Login(args) => provider_call!(
            args,
            nextcloud_login,
            NextcloudLoginRequest,
            synctv_proto::providers::nextcloud::LoginRequest {
                endpoint: args.server_endpoint,
                username: args.account_username,
                app_password: args.app_password,
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
        ProviderNextcloudSubcommand::StartLoginFlow(args) => provider_call!(
            args,
            nextcloud_start_login_flow,
            NextcloudStartLoginFlowRequest,
            synctv_proto::providers::nextcloud::StartLoginFlowRequest {
                endpoint: args.server_endpoint
            }
        ),
        ProviderNextcloudSubcommand::PollLoginFlow(args) => provider_call!(
            args,
            nextcloud_poll_login_flow,
            NextcloudPollLoginFlowRequest,
            synctv_proto::providers::nextcloud::PollLoginFlowRequest {
                endpoint: args.server_endpoint,
                poll_endpoint: args.poll_endpoint,
                poll_token: args.poll_token,
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
        ProviderNextcloudSubcommand::List(args) => provider_call!(
            args,
            nextcloud_list,
            NextcloudListRequest,
            synctv_proto::providers::nextcloud::ListRequest {
                server_id: args.bind.server_id,
                path: args.path,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderNextcloudSubcommand::Favorites(args) => provider_call!(
            args,
            nextcloud_list_favorites,
            NextcloudListFavoritesRequest,
            synctv_proto::providers::nextcloud::ListFavoritesRequest {
                server_id: args.bind.server_id,
                page: args.page,
                page_size: args.page_size,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderNextcloudSubcommand::Logout(args) => provider_call!(
            args,
            nextcloud_logout,
            NextcloudLogoutRequest,
            synctv_proto::providers::nextcloud::LogoutRequest {
                server_id: args.server_id,
            }
        ),
        ProviderNextcloudSubcommand::Binds(args) => provider_call!(
            args,
            nextcloud_get_binds,
            NextcloudGetBindsRequest,
            synctv_proto::providers::nextcloud::GetBindsRequest {
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
    }
}
