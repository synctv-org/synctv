use super::RemoteProviderManager;
use crate::models::ProviderInstance;
use std::sync::Arc;
use synctv_media_providers::remote_transport::{
    create_remote_connection as create_remote_provider_connection, RemoteProviderConnection,
    RemoteProviderConnectionOptions, RemoteProviderTransportConfig,
};
use synctv_media_providers::ProviderClientError;

impl RemoteProviderManager {
    pub(super) fn map_remote_transport_error(error: ProviderClientError) -> crate::Error {
        match error {
            ProviderClientError::InvalidConfig(message)
            | ProviderClientError::InvalidHeader(message)
            | ProviderClientError::Auth(message)
            | ProviderClientError::Parse(message) => crate::Error::InvalidInput(message),
            ProviderClientError::Network(message) | ProviderClientError::Api { message, .. } => {
                crate::Error::ServiceUnavailable(message)
            }
            ProviderClientError::Http { status, url, .. } => crate::Error::ServiceUnavailable(
                format!("Remote provider transport HTTP error {status} for {url}"),
            ),
            ProviderClientError::ResponseTooLarge { size } => crate::Error::ServiceUnavailable(
                format!("Remote provider transport response too large ({size} bytes)"),
            ),
        }
    }

    pub(super) fn map_remote_transport_validation_error(
        error: &ProviderClientError,
    ) -> crate::Error {
        crate::Error::InvalidInput(error.to_string())
    }

    pub(super) fn create_remote_connection(
        &self,
        config: &ProviderInstance,
    ) -> crate::Result<RemoteProviderConnection> {
        // Keep database/domain records out of the transport layer. The gRPC
        // connector only receives the fields required to establish a remote
        // provider connection.
        let options = RemoteProviderConnectionOptions {
            instance_name: config.name.clone(),
            endpoint: config.endpoint.clone(),
            jwt_secret: config.jwt_secret.clone(),
            custom_ca: config.custom_ca.clone(),
            timeout: config.parse_timeout().map_err(crate::Error::Internal)?,
            tls: config.tls,
            insecure_tls: config.insecure_tls,
        };
        let transport_config = RemoteProviderTransportConfig::new(
            Arc::clone(&self.address_overrides),
            self.ssrf_guard.clone(),
            self.transport_compression_enabled,
        );
        create_remote_provider_connection(&options, &transport_config)
            .map_err(Self::map_remote_transport_error)
    }
}
