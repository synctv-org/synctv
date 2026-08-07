use super::*;
pub(crate) async fn execute_provider_acfun(command: ProviderAcfunCommand) -> Result<()> {
    resolve_provider!(
        command,
        ProviderAcfunSubcommand::Resolve,
        acfun,
        AcfunResolveRequest,
        acfun_resolve
    )
}
