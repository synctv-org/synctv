use super::RemoteProviderManager;
use crate::models::{validate_provider_instance_name, ProviderInstance};
use synctv_media_providers::remote_transport::{
    required_auth_secret, validate_auth_secret, validate_endpoint_ssrf,
};

impl RemoteProviderManager {
    /// Validate endpoint and timeout without creating or connecting a remote transport.
    pub(super) fn validate_config_with_ssrf_guard(
        config: &ProviderInstance,
        ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    ) -> crate::Result<()> {
        validate_provider_instance_name(&config.name).map_err(crate::Error::InvalidInput)?;
        config.parse_timeout().map_err(crate::Error::Internal)?;
        for provider in &config.providers {
            if !Self::is_supported_remote_provider(provider.as_str()) {
                return Err(crate::Error::InvalidInput(format!(
                    "Remote provider instance '{}' declares unsupported provider '{}'; supported providers are: {}",
                    config.name,
                    provider,
                    Self::SUPPORTED_REMOTE_PROVIDERS.join(", ")
                )));
            }
        }
        if Self::requires_remote_connection(config) {
            validate_endpoint_ssrf(&config.endpoint, ssrf_guard)
                .map_err(|error| Self::map_remote_transport_validation_error(&error))?;
            let endpoint = url::Url::parse(&config.endpoint).map_err(|e| {
                crate::Error::InvalidInput(format!("Remote provider endpoint is invalid: {e}"))
            })?;

            match (endpoint.scheme(), config.tls) {
                ("https", false) => {
                    return Err(crate::Error::InvalidInput(format!(
                        "Remote provider endpoint '{}' requires tls=true to match its https:// scheme",
                        config.endpoint
                    )));
                }
                ("http", true) => {
                    return Err(crate::Error::InvalidInput(format!(
                        "Remote provider endpoint '{}' requires tls=false to match its {}:// scheme",
                        config.endpoint,
                        endpoint.scheme()
                    )));
                }
                _ => {}
            }

            if config.insecure_tls && !config.tls {
                return Err(crate::Error::InvalidInput(
                    "insecure_tls=true requires tls=true for remote provider instances".to_string(),
                ));
            }

            if config.custom_ca.is_some() && !config.tls {
                return Err(crate::Error::InvalidInput(
                    "custom_ca requires tls=true for remote provider instances".to_string(),
                ));
            }

            if config.insecure_tls && config.custom_ca.is_some() {
                return Err(crate::Error::InvalidInput(
                    "insecure_tls cannot be combined with custom_ca for remote provider instances"
                        .to_string(),
                ));
            }
            validate_auth_secret(Some(
                required_auth_secret(&config.name, config.jwt_secret.as_deref())
                    .map_err(|error| Self::map_remote_transport_validation_error(&error))?,
            ))
            .map_err(|error| Self::map_remote_transport_validation_error(&error))?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn validate_config(config: &ProviderInstance) -> crate::Result<()> {
        Self::validate_config_with_ssrf_guard(
            config,
            &synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    pub(super) fn requires_remote_connection(config: &ProviderInstance) -> bool {
        config
            .providers
            .iter()
            .any(|provider| Self::is_supported_remote_provider(provider.as_str()))
    }

    fn is_supported_remote_provider(provider: &str) -> bool {
        let trimmed = provider.trim();
        Self::SUPPORTED_REMOTE_PROVIDERS.contains(&trimmed)
    }
}
