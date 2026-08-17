use super::*;

pub(super) async fn execute_provider(provider_command: ProviderCommand) -> Result<()> {
    match provider_command.command {
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
        ProviderSubcommand::Alist(command) => execute_provider_alist(command).await,
        ProviderSubcommand::Emby(command) => execute_provider_emby(command).await,
        ProviderSubcommand::Bilibili(command) => execute_provider_bilibili(command).await,
        ProviderSubcommand::Douyin(command) => execute_provider_douyin(command).await,
        ProviderSubcommand::Tiktok(command) => execute_provider_tiktok(command).await,
        ProviderSubcommand::Twitch(command) => execute_provider_twitch(command).await,
        ProviderSubcommand::Acfun(command) => execute_provider_acfun(command).await,
        ProviderSubcommand::Cctv(command) => execute_provider_cctv(command).await,
        ProviderSubcommand::Cloudreve(command) => execute_provider_cloudreve(command).await,
        ProviderSubcommand::Douyu(command) => execute_provider_douyu(command).await,
        ProviderSubcommand::Fnos(command) => execute_provider_fnos(command).await,
        ProviderSubcommand::Huya(command) => execute_provider_huya(command).await,
        ProviderSubcommand::Nextcloud(command) => execute_provider_nextcloud(command).await,
        ProviderSubcommand::Qnap(command) => execute_provider_qnap(command).await,
        ProviderSubcommand::Seafile(command) => execute_provider_seafile(command).await,
        ProviderSubcommand::Synology(command) => execute_provider_synology(command).await,
        ProviderSubcommand::Truenas(command) => execute_provider_truenas(command).await,
        ProviderSubcommand::Youtube(command) => execute_provider_youtube(command).await,
    }
}
