use super::*;

pub(super) async fn execute_provider(provider_command: ProviderCommand) -> Result<()> {
    let ProviderCommand { command } = provider_command;
    match command {
        ProviderSubcommand::Available(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list available provider instances",
                list_available_provider_instances,
                synctv_proto::providers::common::ListAvailableProviderInstancesRequest {
                    provider_type: optional_source_provider_to_proto_i32(args.provider_type),
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Backends(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list provider backends",
                list_provider_backends,
                synctv_proto::providers::common::ListProviderBackendsRequest {
                    provider_type: args.provider_type.to_proto_i32(),
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::List(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "list provider instances",
                list_provider_instances,
                synctv_proto::providers::common::ListProviderInstancesRequest {
                    page: args.page,
                    page_size: args.page_size,
                    provider_type: optional_source_provider_to_proto_i32(args.provider_type),
                    search: args.search.unwrap_or_default(),
                    enabled: args.enabled,
                    tls: args.tls,
                    sort_by: args.sort_by.map_or(
                        synctv_proto::providers::common::ProviderInstanceListSortBy::CreatedAt
                            as i32,
                        CliProviderSortField::to_proto,
                    ),
                    sort_direction: args.sort_dir.to_proto(),
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Create(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "add provider instance",
                add_provider_instance,
                synctv_proto::providers::common::AddProviderInstanceRequest {
                    name: args.name,
                    endpoint: args.provider_endpoint,
                    comment: args.comment.unwrap_or_default(),
                    timeout_seconds: args.timeout_seconds,
                    tls: args.tls,
                    insecure_tls: args.insecure_tls,
                    providers: normalized_provider_types(&args.providers),
                    jwt_secret: normalized_optional_cli_value(args.jwt_secret.as_deref()),
                    custom_ca: normalized_optional_cli_value(args.custom_ca.as_deref()),
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Update(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "update provider instance",
                update_provider_instance,
                synctv_proto::providers::common::UpdateProviderInstanceRequest {
                    name: args.name,
                    endpoint: args.provider_endpoint,
                    comment: args.comment,
                    clear_comment: args.clear_comment.then_some(true),
                    timeout_seconds: args.timeout_seconds,
                    tls: args.tls,
                    insecure_tls: args.insecure_tls,
                    providers: normalized_provider_types(&args.providers),
                    jwt_secret: normalized_optional_cli_value(args.jwt_secret.as_deref()),
                    custom_ca: normalized_optional_cli_value(args.custom_ca.as_deref()),
                    clear_jwt_secret: args.clear_jwt_secret.then_some(true),
                    clear_custom_ca: args.clear_custom_ca.then_some(true),
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "delete provider instance",
                delete_provider_instance,
                synctv_proto::providers::common::DeleteProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Reconnect(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "reconnect provider instance",
                reconnect_provider_instance,
                synctv_proto::providers::common::ReconnectProviderInstanceRequest {
                    name: args.name,
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Enable(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "enable provider instance",
                enable_provider_instance,
                synctv_proto::providers::common::EnableProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Disable(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "disable provider instance",
                disable_provider_instance,
                synctv_proto::providers::common::DisableProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderSubcommand::Alist(command) => execute_provider_alist(command).await,
        ProviderSubcommand::Emby(command) => execute_provider_emby(command).await,
        ProviderSubcommand::Bilibili(command) => execute_provider_bilibili(command).await,
        ProviderSubcommand::Douyin(command) => execute_provider_douyin(command).await,
        ProviderSubcommand::Tiktok(command) => execute_provider_tiktok(command).await,
        ProviderSubcommand::Twitch(command) => execute_provider_twitch(command).await,
        ProviderSubcommand::Rtmp(command) => execute_provider_rtmp(command).await,
    }
}
