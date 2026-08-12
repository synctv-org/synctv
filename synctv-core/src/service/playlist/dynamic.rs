use std::sync::Arc;

use crate::{
    models::{
        normalize_provider_instance_name_owned, PlaylistSourceConfig, RoomId, SourceProvider,
        UserId,
    },
    provider::{provider_requires_credential_repo, ProviderContext, SourceConfig},
    repository::UserProviderCredentialRepository,
    service::{
        provider_binding::resolve_credential_provider_instance_binding,
        source_config::validate_source_config_size, ProvidersManager,
    },
    Error, Result,
};

use super::PlaylistService;

pub(super) fn normalize_dynamic_playlist_fields(
    source_provider: Option<SourceProvider>,
    source_config: Option<PlaylistSourceConfig>,
    provider_instance_name: Option<String>,
) -> Result<(
    Option<SourceProvider>,
    Option<PlaylistSourceConfig>,
    Option<String>,
)> {
    let normalized_provider = source_provider;
    let normalized_instance = normalize_provider_instance_name_owned(provider_instance_name);

    if let Some(provider) = normalized_provider {
        let source_config = source_config.ok_or_else(|| {
            Error::InvalidInput("source_config is required for dynamic playlists".to_string())
        })?;

        Ok((Some(provider), Some(source_config), normalized_instance))
    } else {
        if source_config.is_some() || normalized_instance.is_some() {
            return Err(Error::InvalidInput(
                "source_provider is required when setting dynamic playlist fields".to_string(),
            ));
        }

        Ok((None, None, None))
    }
}

pub(super) struct DynamicPlaylistValidationDeps<'a> {
    pub(super) providers_manager: &'a ProvidersManager,
    pub(super) credential_encryption:
        Option<&'a crate::credential_encryption::CredentialEncryption>,
    pub(super) credential_repo: Option<&'a Arc<UserProviderCredentialRepository>>,
}

pub(super) async fn validate_dynamic_playlist_source_with_dependencies(
    deps: DynamicPlaylistValidationDeps<'_>,
    room_id: &RoomId,
    user_id: &UserId,
    source_provider: SourceProvider,
    source_config: PlaylistSourceConfig,
    provider_instance_name: Option<String>,
) -> Result<(SourceProvider, PlaylistSourceConfig, Option<String>)> {
    let config_provider = source_config.provider();
    let source_config = source_config
        .ensure_provider(source_provider)
        .map_err(Error::InvalidInput)?;
    let trimmed_instance = normalize_provider_instance_name_owned(provider_instance_name);
    validate_source_config_size(&source_config)?;

    let provider = deps
        .providers_manager
        .resolve_provider(config_provider, trimmed_instance.as_deref())
        .await?;
    let provider_name = config_provider.as_str();

    if provider.as_dynamic_playlist_provider().is_none() {
        return Err(Error::InvalidInput(format!(
            "Provider {provider_name} does not support dynamic playlists"
        )));
    }
    ensure_provider_credential_repo_available(provider_name, deps.credential_repo)?;

    // ProviderContext building is repeated because it borrows the instance name.
    let mut ctx = ProviderContext::new("synctv", crate::provider::ProviderActor::User(*user_id))
        .with_room_id(*room_id)
        .with_credential_owner_id(*user_id);
    if let Some(provider_instance_name) = trimmed_instance.as_deref() {
        ctx = ctx.with_provider_instance_name(provider_instance_name);
    }
    if let Some(repo) = deps.credential_repo {
        ctx = ctx.with_credential_repo(repo);
    }
    if let Some(enc) = deps.credential_encryption {
        ctx = ctx.with_credential_encryption(enc);
    }

    provider
        .validate_source_config(&ctx, SourceConfig::dynamic_playlist(&source_config))
        .await
        .map_err(|e| Error::InvalidInput(format!("Invalid source_config: {e}")))?;

    let bound_instance = resolve_credential_provider_instance_binding(
        provider.as_ref(),
        deps.credential_repo,
        &ctx,
        SourceConfig::dynamic_playlist(&source_config),
        trimmed_instance.as_deref(),
    )
    .await?;
    let provider_instance_changed = bound_instance != trimmed_instance;
    let provider = if provider_instance_changed {
        let provider = deps
            .providers_manager
            .resolve_provider(config_provider, bound_instance.as_deref())
            .await?;
        if provider.as_dynamic_playlist_provider().is_none() {
            return Err(Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic playlists"
            )));
        }
        provider
    } else {
        provider
    };

    let mut ctx = ProviderContext::new("synctv", crate::provider::ProviderActor::User(*user_id))
        .with_room_id(*room_id)
        .with_credential_owner_id(*user_id);
    if let Some(provider_instance_name) = bound_instance.as_deref() {
        ctx = ctx.with_provider_instance_name(provider_instance_name);
    }
    if let Some(repo) = deps.credential_repo {
        ctx = ctx.with_credential_repo(repo);
    }
    if let Some(enc) = deps.credential_encryption {
        ctx = ctx.with_credential_encryption(enc);
    }

    if provider_instance_changed {
        provider
            .validate_source_config(&ctx, SourceConfig::dynamic_playlist(&source_config))
            .await
            .map_err(|e| Error::InvalidInput(format!("Invalid source_config: {e}")))?;
    }

    let prepared_source_config = provider
        .prepare_source_config(&ctx, SourceConfig::dynamic_playlist(&source_config))
        .await
        .map_err(|e| Error::Internal(format!("Failed to prepare source_config: {e}")))?
        .into_dynamic_playlist()
        .map_err(|e| Error::InvalidInput(format!("Invalid prepared source_config: {e}")))?;

    Ok((config_provider, prepared_source_config, bound_instance))
}

fn ensure_provider_credential_repo_available(
    provider_name: &str,
    credential_repo: Option<&Arc<UserProviderCredentialRepository>>,
) -> Result<()> {
    if provider_requires_credential_repo(provider_name) && credential_repo.is_none() {
        return Err(Error::ServiceUnavailable(format!(
            "Provider '{provider_name}' requires credential repository wiring"
        )));
    }

    Ok(())
}

impl PlaylistService {
    pub(super) async fn validate_dynamic_playlist_source(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        source_provider: SourceProvider,
        source_config: PlaylistSourceConfig,
        provider_instance_name: Option<String>,
    ) -> Result<(SourceProvider, PlaylistSourceConfig, Option<String>)> {
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &self.providers_manager,
                credential_encryption: self.credential_encryption.as_ref(),
                credential_repo: self.credential_repo.as_ref(),
            },
            room_id,
            user_id,
            source_provider,
            source_config,
            provider_instance_name,
        )
        .await
    }
}
