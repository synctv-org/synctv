use super::*;
pub(crate) async fn execute_provider_cloudreve(command: ProviderCloudreveCommand) -> Result<()> {
    match command.command {
        ProviderCloudreveSubcommand::Login(args) => provider_call!(
            args,
            cloudreve_login,
            CloudreveLoginRequest,
            synctv_proto::providers::cloudreve::LoginRequest {
                host: args.server_endpoint,
                email: args.account_email,
                password: args.password,
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
        ProviderCloudreveSubcommand::List(args) => provider_call!(
            args,
            cloudreve_list,
            CloudreveListRequest,
            synctv_proto::providers::cloudreve::ListRequest {
                server_id: args.bind.server_id,
                path: args.path,
                pagination: match (args.page, args.cursor) {
                    (Some(page), None) => Some(
                        synctv_proto::providers::cloudreve::list_request::Pagination::Page(
                            synctv_proto::providers::cloudreve::PagePagination { page, total: 0 },
                        ),
                    ),
                    (None, Some(cursor)) => Some(
                        synctv_proto::providers::cloudreve::list_request::Pagination::Cursor(
                            synctv_proto::providers::cloudreve::CursorPagination { cursor },
                        ),
                    ),
                    (None, None) => None,
                    (Some(_), Some(_)) =>
                        unreachable!("clap prevents --page and --cursor together"),
                },
                per_page: args.per_page,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderCloudreveSubcommand::Search(args) => provider_call!(
            args,
            cloudreve_search,
            CloudreveSearchRequest,
            synctv_proto::providers::cloudreve::SearchRequest {
                server_id: args.bind.server_id,
                keywords: args.keywords,
                offset: args.offset,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderCloudreveSubcommand::Me(args) => provider_call!(
            args,
            cloudreve_get_me,
            CloudreveGetMeRequest,
            synctv_proto::providers::cloudreve::GetMeRequest {
                server_id: args.bind.server_id,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderCloudreveSubcommand::Logout(args) => provider_call!(
            args,
            cloudreve_logout,
            CloudreveLogoutRequest,
            synctv_proto::providers::cloudreve::LogoutRequest {
                server_id: args.server_id,
            }
        ),
        ProviderCloudreveSubcommand::Binds(args) => provider_call!(
            args,
            cloudreve_get_binds,
            CloudreveGetBindsRequest,
            synctv_proto::providers::cloudreve::GetBindsRequest {
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
    }
}
