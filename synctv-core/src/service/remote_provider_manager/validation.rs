use super::RemoteProviderManager;
use crate::models::{validate_provider_instance_name, ProviderInstance};
use crate::provider::provider_client::validate_auth_secret;

impl RemoteProviderManager {
    /// Validate endpoint URL structure and apply the configured runtime SSRF
    /// policy to hostnames and IP literals.
    ///
    /// Only validates hostnames and IP literals statically. DNS validation runs
    /// again inside the transport connector at connection time.
    pub(super) fn validate_endpoint_ssrf(
        endpoint: &str,
        guard: &synctv_common::ssrf::SsrfGuard,
    ) -> crate::Result<()> {
        let url = url::Url::parse(endpoint).map_err(|e| {
            crate::Error::InvalidInput(format!("SSRF validation: invalid URL: {e}"))
        })?;

        match url.scheme() {
            "http" | "https" => {}
            scheme => {
                return Err(crate::Error::InvalidInput(format!(
                    "Remote provider endpoint scheme '{scheme}' is not supported; use http:// for plaintext transport, or https:// for TLS"
                )))
            }
        }

        let host = url.host_str().ok_or_else(|| {
            crate::Error::InvalidInput("SSRF validation: missing host".to_string())
        })?;

        if guard.is_host_blocked(host) {
            return Err(crate::Error::InvalidInput(format!(
                "SSRF validation: host '{host}' is blocked (internal/reserved)"
            )));
        }

        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if guard.is_ip_blocked(&ip) {
                return Err(crate::Error::InvalidInput(format!(
                    "SSRF validation: IP '{ip}' is blocked (internal/private)"
                )));
            }
        }

        if let Some(port) = url.port() {
            if port == 0 {
                return Err(crate::Error::InvalidInput(
                    "SSRF validation: port 0 is not valid".to_string(),
                ));
            }
        }

        Ok(())
    }

    pub(super) fn normalized_transport_endpoint(
        config: &ProviderInstance,
    ) -> crate::Result<String> {
        let url = url::Url::parse(&config.endpoint).map_err(|e| {
            crate::Error::InvalidInput(format!("Remote provider endpoint is invalid: {e}"))
        })?;

        let normalized_scheme = match url.scheme() {
            "http" => "http",
            "https" => "https",
            scheme => {
                return Err(crate::Error::InvalidInput(format!(
                    "Remote provider endpoint scheme '{scheme}' is not supported; use http:// for plaintext transport, or https:// for TLS"
                )))
            }
        };

        let host = url.host_str().ok_or_else(|| {
            crate::Error::InvalidInput("Remote provider endpoint is missing host".to_string())
        })?;
        let mut normalized = format!("{normalized_scheme}://{host}");
        if let Some(port) = url.port() {
            normalized.push(':');
            normalized.push_str(&port.to_string());
        }
        let path = url.path();
        if !path.is_empty() && path != "/" {
            normalized.push_str(path);
        }
        if let Some(query) = url.query() {
            normalized.push('?');
            normalized.push_str(query);
        }

        Ok(normalized)
    }

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
            Self::validate_endpoint_ssrf(&config.endpoint, ssrf_guard)?;
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
            validate_auth_secret(Some(Self::required_auth_secret(config)?))
                .map_err(|e| crate::Error::InvalidInput(e.to_string()))?;
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

    pub(super) fn required_auth_secret(config: &ProviderInstance) -> crate::Result<&str> {
        let secret = config.jwt_secret.as_deref().ok_or_else(|| {
            crate::Error::InvalidInput(format!(
                "Remote provider instance '{}' requires a non-empty jwt_secret",
                config.name
            ))
        })?;
        let trimmed = secret.trim();
        if trimmed.is_empty() {
            return Err(crate::Error::InvalidInput(format!(
                "Remote provider instance '{}' requires a non-empty jwt_secret",
                config.name
            )));
        }
        Ok(trimmed)
    }
}
