use super::*;
pub(crate) async fn execute_provider_fnos(command: ProviderFnosCommand) -> Result<()> {
    match command.command {
        ProviderFnosSubcommand::Login(args) => provider_call!(
            args,
            fnos_login,
            FnosLoginRequest,
            synctv_proto::providers::fnos::LoginRequest {
                endpoint: args.server_endpoint,
                webdav_endpoint: args.webdav_endpoint,
                media_endpoint: args.media_endpoint,
                username: args.account_username,
                password: args.password,
                twofa_code: args.twofa_code,
                trust_device: args.trust_device,
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
        ProviderFnosSubcommand::List(args) => provider_call!(
            args,
            fnos_list,
            FnosListRequest,
            synctv_proto::providers::fnos::ListRequest {
                server_id: args.bind.server_id,
                path: args.path,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderFnosSubcommand::Libraries(args) => provider_call!(
            args,
            fnos_list_media_libraries,
            FnosListMediaLibrariesRequest,
            synctv_proto::providers::fnos::ListMediaLibrariesRequest {
                server_id: args.bind.server_id,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderFnosSubcommand::Items(args) => provider_call!(
            args,
            fnos_list_media_items,
            FnosListMediaItemsRequest,
            synctv_proto::providers::fnos::ListMediaItemsRequest {
                server_id: args.bind.server_id,
                collection: args.collection.to_proto(),
                library_guid: args.library_guid,
                page: args.page,
                page_size: args.page_size,
                media_types: args.media_types,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
                parent_guid: args.parent_guid,
            }
        ),
        ProviderFnosSubcommand::SetFavorite(args) => provider_call!(
            args,
            fnos_set_favorite,
            FnosSetFavoriteRequest,
            synctv_proto::providers::fnos::SetFavoriteRequest {
                server_id: args.bind.server_id,
                item_guid: args.item_guid,
                favorite: args.favorite,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderFnosSubcommand::SetWatched(args) => provider_call!(
            args,
            fnos_set_watched,
            FnosSetWatchedRequest,
            synctv_proto::providers::fnos::SetWatchedRequest {
                server_id: args.bind.server_id,
                item_guid: args.item_guid,
                watched: args.watched,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderFnosSubcommand::ServerInfo(args) => provider_call!(
            args,
            fnos_get_server_info,
            FnosGetServerInfoRequest,
            synctv_proto::providers::fnos::GetServerInfoRequest {
                server_id: args.bind.server_id,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderFnosSubcommand::Logout(args) => provider_call!(
            args,
            fnos_logout,
            FnosLogoutRequest,
            synctv_proto::providers::fnos::LogoutRequest {
                server_id: args.server_id,
            }
        ),
        ProviderFnosSubcommand::Binds(args) => provider_call!(
            args,
            fnos_get_binds,
            FnosGetBindsRequest,
            synctv_proto::providers::fnos::GetBindsRequest {
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
    }
}
