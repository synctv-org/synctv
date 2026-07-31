use super::*;

pub(crate) async fn execute_provider_douyu(command: ProviderDouyuCommand) -> Result<()> {
    resolve_provider!(
        command,
        ProviderDouyuSubcommand::Resolve,
        douyu,
        DouyuResolveRequest,
        douyu_resolve
    )
}
