use super::*;
pub(crate) async fn execute_provider_seafile(command: ProviderSeafileCommand) -> Result<()> {
    match command.command {
        ProviderSeafileSubcommand::Login(args) => provider_call!(
            args,
            seafile_login,
            SeafileLoginRequest,
            synctv_proto::providers::seafile::LoginRequest {
                endpoint: args.server_endpoint,
                username: args.account_username,
                password: args.password,
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
        ProviderSeafileSubcommand::UnlockLibrary(args) => provider_call!(
            args,
            seafile_unlock_library,
            SeafileUnlockLibraryRequest,
            synctv_proto::providers::seafile::UnlockLibraryRequest {
                server_id: args.bind.server_id,
                repository_id: args.repository_id,
                password: args.password,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSeafileSubcommand::Repositories(args) => provider_call!(
            args,
            seafile_list_repositories,
            SeafileListRepositoriesRequest,
            synctv_proto::providers::seafile::ListRepositoriesRequest {
                server_id: args.bind.server_id,
                page: args.page,
                page_size: args.page_size,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSeafileSubcommand::List(args) => provider_call!(
            args,
            seafile_list,
            SeafileListRequest,
            synctv_proto::providers::seafile::ListRequest {
                server_id: args.bind.server_id,
                repository_id: args.repository_id,
                path: args.path,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSeafileSubcommand::Starred(args) => provider_call!(
            args,
            seafile_list_starred,
            SeafileListStarredRequest,
            synctv_proto::providers::seafile::ListStarredRequest {
                server_id: args.bind.server_id,
                page: args.page,
                page_size: args.page_size,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSeafileSubcommand::Logout(args) => provider_call!(
            args,
            seafile_logout,
            SeafileLogoutRequest,
            synctv_proto::providers::seafile::LogoutRequest {
                server_id: args.server_id,
            }
        ),
        ProviderSeafileSubcommand::Binds(args) => provider_call!(
            args,
            seafile_get_binds,
            SeafileGetBindsRequest,
            synctv_proto::providers::seafile::GetBindsRequest {
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
    }
}
