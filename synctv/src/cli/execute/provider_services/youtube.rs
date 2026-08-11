use super::*;
pub(crate) async fn execute_provider_youtube(command: ProviderYoutubeCommand) -> Result<()> {
    match command.command {
        ProviderYoutubeSubcommand::Bind(args) => provider_call!(
            args,
            youtube_bind,
            YoutubeBindRequest,
            synctv_proto::providers::youtube::BindRequest {
                label: args.label,
                visitor_data: args.visitor_data,
                po_token: args.po_token,
                cookie: args.cookie,
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
        ProviderYoutubeSubcommand::Binds(args) => provider_call!(
            args,
            youtube_get_binds,
            YoutubeGetBindsRequest,
            synctv_proto::providers::youtube::GetBindsRequest {
                instance_name: provider_service_instance_name(&args.instance),
            }
        ),
        ProviderYoutubeSubcommand::Unbind(args) => provider_call!(
            args,
            youtube_unbind,
            YoutubeUnbindRequest,
            synctv_proto::providers::youtube::UnbindRequest {
                server_id: args.server_id,
            }
        ),
        ProviderYoutubeSubcommand::Resolve(args) => provider_call!(
            args,
            youtube_resolve,
            YoutubeResolveRequest,
            synctv_proto::providers::youtube::ResolveRequest {
                resource: args.resource,
                instance_name: provider_service_instance_name(&args.instance),
                shared: false,
            }
        ),
    }
}
