use super::*;

pub(crate) async fn execute_provider_cctv(command: ProviderCctvCommand) -> Result<()> {
    resolve_provider!(
        command,
        ProviderCctvSubcommand::Resolve,
        cctv,
        CctvResolveRequest,
        cctv_resolve
    )
}
