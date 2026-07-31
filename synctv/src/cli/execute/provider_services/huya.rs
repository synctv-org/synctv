use super::*;

pub(crate) async fn execute_provider_huya(command: ProviderHuyaCommand) -> Result<()> {
    resolve_provider!(
        command,
        ProviderHuyaSubcommand::Resolve,
        huya,
        HuyaResolveRequest,
        huya_resolve
    )
}
