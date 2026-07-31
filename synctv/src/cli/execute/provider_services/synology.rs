use super::*;
pub(crate) async fn execute_provider_synology(command: ProviderSynologyCommand) -> Result<()> {
    match command.command {
        ProviderSynologySubcommand::Login(args) => provider_call!(
            args,
            synology_login,
            SynologyLoginRequest,
            synctv_proto::providers::synology::LoginRequest {
                endpoint: args.server_endpoint,
                username: args.account_username,
                password: args.password,
                otp_code: args.otp_code,
                device_name: args.device_name,
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
        ProviderSynologySubcommand::Files(args) => provider_call!(
            args,
            synology_list_files,
            SynologyListFilesRequest,
            synctv_proto::providers::synology::ListFilesRequest {
                server_id: args.bind.server_id,
                path: args.path,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSynologySubcommand::Libraries(args) => provider_call!(
            args,
            synology_list_libraries,
            SynologyListLibrariesRequest,
            synctv_proto::providers::synology::ListLibrariesRequest {
                server_id: args.bind.server_id,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSynologySubcommand::Movies(args) => provider_call!(
            args,
            synology_list_movies,
            SynologyListMoviesRequest,
            synctv_proto::providers::synology::ListMoviesRequest {
                server_id: args.bind.server_id,
                library_id: args.library_id,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSynologySubcommand::TvShows(args) => provider_call!(
            args,
            synology_list_tv_shows,
            SynologyListTvShowsRequest,
            synctv_proto::providers::synology::ListTvShowsRequest {
                server_id: args.bind.server_id,
                library_id: args.library_id,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSynologySubcommand::Episodes(args) => provider_call!(
            args,
            synology_list_episodes,
            SynologyListEpisodesRequest,
            synctv_proto::providers::synology::ListEpisodesRequest {
                server_id: args.bind.server_id,
                library_id: args.library_id,
                tv_show_id: args.tv_show_id,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSynologySubcommand::HomeVideos(args) => provider_call!(
            args,
            synology_list_home_videos,
            SynologyListHomeVideosRequest,
            synctv_proto::providers::synology::ListHomeVideosRequest {
                server_id: args.bind.server_id,
                library_id: args.library_id,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSynologySubcommand::Recordings(args) => provider_call!(
            args,
            synology_list_tv_recordings,
            SynologyListTvRecordingsRequest,
            synctv_proto::providers::synology::ListTvRecordingsRequest {
                server_id: args.bind.server_id,
                library_id: args.library_id,
                page: args.page,
                page_size: args.page_size,
                search: args.search,
                instance_name: provider_service_instance_name(&args.bind.instance),
            }
        ),
        ProviderSynologySubcommand::Logout(args) => provider_call!(
            args,
            synology_logout,
            SynologyLogoutRequest,
            synctv_proto::providers::synology::LogoutRequest {
                server_id: args.server_id,
            }
        ),
        ProviderSynologySubcommand::Binds(args) => provider_call!(
            args,
            synology_get_binds,
            SynologyGetBindsRequest,
            synctv_proto::providers::synology::GetBindsRequest {
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
    }
}
