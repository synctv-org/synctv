use std::sync::Arc;

use crate::{
    models::{resolve_provider_instance_binding, CredentialProviderInstanceName},
    provider::{MediaProvider, ProviderContext, SourceConfig},
    repository::UserProviderCredentialRepository,
    Error, Result,
};

/// Resolve the effective provider instance for source configs that depend on
/// stored provider credentials.
///
/// Provider source-config parsing remains owned by the provider. This helper
/// only applies the shared policy: credential rows are authoritative for the
/// provider instance they were created through, omitted request bindings adopt
/// that stored binding, and different explicit bindings are rejected.
pub(crate) async fn resolve_credential_provider_instance_binding(
    provider: &dyn MediaProvider,
    credential_repo: Option<&Arc<UserProviderCredentialRepository>>,
    ctx: &ProviderContext<'_>,
    source_config: SourceConfig<'_>,
    explicit_provider_instance_name: Option<&str>,
) -> Result<Option<String>> {
    let Some(credential_repo) = credential_repo else {
        return resolve_provider_instance_binding(
            explicit_provider_instance_name,
            CredentialProviderInstanceName::NotCredentialBacked,
        )
        .map_err(|error| Error::InvalidInput(error.to_string()));
    };

    let dependencies = provider
        .credential_dependencies(ctx, source_config)
        .map_err(|error| Error::InvalidInput(format!("Invalid source_config: {error}")))?;
    let mut credential_binding: Option<Option<String>> = None;

    for dependency in dependencies
        .into_iter()
        .filter(|dependency| dependency.provider.as_str() == provider.name())
        .filter(|dependency| dependency.requirement.is_required())
    {
        let credential = credential_repo
            .get_by_provider_and_server(
                dependency.user_id,
                dependency.provider.as_str(),
                &dependency.server_id,
            )
            .await?
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "source_config depends on missing credential for provider '{}' server '{}'",
                    dependency.provider, dependency.server_id
                ))
            })?;

        let record_binding = resolve_provider_instance_binding(
            None,
            CredentialProviderInstanceName::CredentialBacked(
                credential.provider_instance_name.as_deref(),
            ),
        )
        .map_err(|error| Error::InvalidInput(error.to_string()))?;

        match &credential_binding {
            None => credential_binding = Some(record_binding),
            Some(existing) if *existing == record_binding => {}
            Some(_) => {
                return Err(Error::InvalidInput(
                    "source_config depends on credentials from different provider instances"
                        .to_string(),
                ));
            }
        }
    }

    resolve_provider_instance_binding(
        explicit_provider_instance_name,
        credential_binding.as_ref().map_or(
            CredentialProviderInstanceName::NotCredentialBacked,
            |binding| CredentialProviderInstanceName::CredentialBacked(binding.as_deref()),
        ),
    )
    .map_err(|error| Error::InvalidInput(error.to_string()))
}
