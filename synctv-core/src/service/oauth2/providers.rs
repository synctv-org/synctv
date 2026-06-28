use std::collections::HashMap;
use std::sync::Arc;

use tracing::info;

use crate::{
    models::oauth2_client::OAuth2Provider,
    oauth2::Provider as OAuth2ProviderTrait,
    service::{oauth2::OAuth2ProviderEntry, oauth2::OAuth2Service, OAuth2SignupPolicy},
    Error, Result,
};

impl OAuth2Service {
    pub async fn register_provider(
        &self,
        instance_name: String,
        provider_type: OAuth2Provider,
        provider: Box<dyn OAuth2ProviderTrait>,
    ) {
        let mut providers = self.providers.write().await;

        info!(
            "Registered OAuth2 provider: {} (type: {})",
            instance_name,
            provider_type.as_str()
        );
        providers.insert(
            instance_name,
            OAuth2ProviderEntry {
                provider: Arc::from(provider),
                provider_type,
                signup_policy: OAuth2SignupPolicy::default(),
            },
        );
    }

    pub(super) async fn sync_runtime_providers(&self) -> Result<()> {
        let Some(settings_registry) = self.settings_registry.as_ref() else {
            return Ok(());
        };

        let configs = settings_registry.oauth2_providers.get()?;
        configs.validate_with_ssrf_guard(&self.ssrf_guard)?;
        let fingerprint = configs.to_string();
        {
            let cached = self.providers_fingerprint.read().await;
            if cached.as_deref() == Some(fingerprint.as_str()) {
                return Ok(());
            }
        }

        let mut rebuilt = HashMap::new();
        for (instance_name, provider_config) in configs.0 {
            let provider_type_name = provider_config.provider_type_name();
            let provider_type =
                OAuth2Provider::from_str_name(provider_type_name).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "OAuth2 provider '{instance_name}' uses unsupported type '{provider_type_name}'"
                    ))
                })?;
            let provider = self
                .provider_registry
                .create_provider(provider_type_name, &provider_config.config)?;

            rebuilt.insert(
                instance_name,
                OAuth2ProviderEntry {
                    provider: Arc::from(provider),
                    provider_type,
                    signup_policy: provider_config.signup_policy(),
                },
            );
        }

        *self.providers.write().await = rebuilt;
        *self.providers_fingerprint.write().await = Some(fingerprint);

        Ok(())
    }

    pub(super) async fn provider_entry(&self, instance_name: &str) -> Result<OAuth2ProviderEntry> {
        self.sync_runtime_providers().await?;
        let providers = self.providers.read().await;
        providers.get(instance_name).cloned().ok_or_else(|| {
            Error::InvalidInput(format!(
                "OAuth2 provider instance not found: {instance_name}"
            ))
        })
    }

    pub async fn signup_policy_for(&self, instance_name: &str) -> Result<OAuth2SignupPolicy> {
        Ok(self.provider_entry(instance_name).await?.signup_policy)
    }
}
