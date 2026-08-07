use super::*;

pub(super) async fn execute_provider_instance(command: ProviderInstanceCommand) -> Result<()> {
    match command.command {
        ProviderInstanceSubcommand::Available(args) => {
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
        ProviderInstanceSubcommand::List(args) => {
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
        ProviderInstanceSubcommand::Create(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "create provider instance",
                add_provider_instance,
                synctv_proto::providers::common::AddProviderInstanceRequest {
                    name: args.name,
                    endpoint: args.instance_endpoint,
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
        ProviderInstanceSubcommand::Update(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "update provider instance",
                update_provider_instance,
                synctv_proto::providers::common::UpdateProviderInstanceRequest {
                    name: args.name,
                    endpoint: args.instance_endpoint,
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
        ProviderInstanceSubcommand::Delete(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "delete provider instance",
                delete_provider_instance,
                synctv_proto::providers::common::DeleteProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderInstanceSubcommand::Reconnect(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "reconnect provider instance",
                reconnect_provider_instance,
                synctv_proto::providers::common::ReconnectProviderInstanceRequest {
                    name: args.name
                }
            )?;
            args.remote.print_output(&response)
        }
        ProviderInstanceSubcommand::Enable(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "enable provider instance",
                enable_provider_instance,
                synctv_proto::providers::common::EnableProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
        ProviderInstanceSubcommand::Disable(args) => {
            let session = connect_remote_access(&args.remote).await?;
            let response = management_unary_call!(
                session,
                "disable provider instance",
                disable_provider_instance,
                synctv_proto::providers::common::DisableProviderInstanceRequest { name: args.name }
            )?;
            args.remote.print_output(&response)
        }
    }
}
