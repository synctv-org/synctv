use std::sync::Arc;

use super::{RemoteProviderManager, RemoteProviderRuntimeStatus};
use crate::provider::ProviderError;
use synctv_media_providers::remote_transport::RemoteProviderConnection;

impl RemoteProviderManager {
    pub(super) fn map_remote_resolution_error(err: crate::Error) -> ProviderError {
        match err {
            crate::Error::InvalidInput(msg) => ProviderError::InvalidConfig(msg),
            crate::Error::RangeNotSatisfiable { total_size } => ProviderError::InvalidConfig(
                format!("Range not satisfiable: total size {total_size}"),
            ),
            crate::Error::Timeout(msg) => ProviderError::NetworkError(msg),
            crate::Error::ServiceUnavailable(msg) | crate::Error::RateLimited(msg) => {
                ProviderError::ApiError(msg)
            }
            crate::Error::Database(error) => {
                tracing::warn!(error = %error, "Provider configuration store unavailable");
                ProviderError::ApiError(
                    "Provider configuration service is temporarily unavailable.".to_string(),
                )
            }
            crate::Error::Redis(error) => {
                tracing::warn!(error = %error, "Provider configuration cache unavailable");
                ProviderError::ApiError(
                    "Provider configuration service is temporarily unavailable.".to_string(),
                )
            }
            crate::Error::Serialization(err) => {
                ProviderError::Internal(format!("Serialization error: {err}"))
            }
            crate::Error::Deserialization { context } => {
                ProviderError::Internal(format!("Deserialization error: {context}"))
            }
            crate::Error::Authentication(msg) => ProviderError::Internal(format!(
                "Unexpected authentication error while resolving provider instance: {msg}"
            )),
            crate::Error::Authorization(msg) => ProviderError::Internal(format!(
                "Unexpected authorization error while resolving provider instance: {msg}"
            )),
            crate::Error::KickCooldownDenied => ProviderError::Internal(format!(
                "Unexpected authorization error while resolving provider instance: {}",
                crate::Error::kick_cooldown_denied_message()
            )),
            crate::Error::NotFound(msg) => ProviderError::Internal(format!(
                "Unexpected not found error while resolving provider instance: {msg}"
            )),
            crate::Error::AlreadyExists(msg) => ProviderError::Internal(format!(
                "Unexpected already exists error while resolving provider instance: {msg}"
            )),
            crate::Error::Conflict(msg) => ProviderError::Internal(format!(
                "Unexpected conflict while resolving provider instance: {msg}"
            )),
            crate::Error::Internal(msg) => ProviderError::Internal(msg),
            crate::Error::OptimisticLockConflict => {
                ProviderError::Internal("Optimistic lock conflict".to_string())
            }
            crate::Error::LockConflict(msg) => ProviderError::Internal(format!(
                "Distributed lock conflict while resolving provider instance: {msg}"
            )),
        }
    }

    fn provider_error_from_ref(error: &ProviderError) -> ProviderError {
        match error {
            ProviderError::InvalidUrl(message) => ProviderError::InvalidUrl(message.clone()),
            ProviderError::InvalidConfig(message) => ProviderError::InvalidConfig(message.clone()),
            ProviderError::MissingField(message) => ProviderError::MissingField(message.clone()),
            ProviderError::NetworkError(message) => ProviderError::NetworkError(message.clone()),
            ProviderError::AuthRequired => ProviderError::AuthRequired,
            ProviderError::CredentialRequired => ProviderError::CredentialRequired,
            ProviderError::InvalidCredentialType => ProviderError::InvalidCredentialType,
            ProviderError::Authentication(message) => {
                ProviderError::Authentication(message.clone())
            }
            ProviderError::NotFound => ProviderError::NotFound,
            ProviderError::ApiError(message) => ProviderError::ApiError(message.clone()),
            ProviderError::UpstreamHttp { status, url } => ProviderError::UpstreamHttp {
                status: *status,
                url: url.clone(),
            },
            ProviderError::UnsupportedFormat(format) => {
                ProviderError::UnsupportedFormat(format.clone())
            }
            ProviderError::ParseError(message) => ProviderError::ParseError(message.clone()),
            ProviderError::MissingInstance => ProviderError::MissingInstance,
            ProviderError::InstanceNotFound(name) => ProviderError::InstanceNotFound(name.clone()),
            ProviderError::CredentialNotFound(message) => {
                ProviderError::CredentialNotFound(message.clone())
            }
            ProviderError::CredentialExpired(message) => {
                ProviderError::CredentialExpired(message.clone())
            }
            ProviderError::EncryptionRequired(provider) => {
                ProviderError::EncryptionRequired(provider)
            }
            ProviderError::RouteRegistrationFailed(message) => {
                ProviderError::RouteRegistrationFailed(message.clone())
            }
            ProviderError::Internal(message) => ProviderError::Internal(message.clone()),
            ProviderError::IoError(error) => {
                ProviderError::IoError(std::io::Error::new(error.kind(), error.to_string()))
            }
            ProviderError::JsonError(error) => ProviderError::JsonError(serde_json::Error::io(
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()),
            )),
        }
    }

    pub(super) async fn get_connection_result(
        &self,
        name: &str,
    ) -> std::result::Result<Option<RemoteProviderConnection>, ProviderError> {
        enum LoadError {
            NotCacheable,
            Provider(ProviderError),
        }

        let loaded = self
            .connection_cache
            .try_get_with_by_ref(name, async {
                let Some(config) = self.repository.get_by_name(name).await.map_err(|error| {
                    LoadError::Provider(Self::map_remote_resolution_error(error))
                })?
                else {
                    return Err(LoadError::NotCacheable);
                };

                if !config.enabled || !Self::requires_remote_connection(&config) {
                    return Err(LoadError::NotCacheable);
                }

                let connection = self.create_remote_connection(&config).map_err(|error| {
                    LoadError::Provider(Self::map_remote_resolution_error(error))
                })?;

                tracing::debug!(
                    "Lazily created and cached remote provider connection for instance '{}'",
                    config.name
                );
                Ok(connection)
            })
            .await;

        match loaded {
            Ok(connection) => Ok(Some(connection)),
            Err(error) => match Arc::try_unwrap(error) {
                Ok(LoadError::NotCacheable) => Ok(None),
                Ok(LoadError::Provider(error)) => Err(error),
                Err(error) => match error.as_ref() {
                    LoadError::NotCacheable => Ok(None),
                    LoadError::Provider(error) => Err(Self::provider_error_from_ref(error)),
                },
            },
        }
    }

    /// Get a remote provider instance connection by name for best-effort probes.
    ///
    /// Checks the local moka cache first. On cache miss, loads the instance config
    /// from the database and creates a connection lazily. This ensures that provider
    /// instances added on other replicas are visible after the cache TTL expires
    /// (or immediately if a Redis invalidation notification was received).
    ///
    /// Returns:
    /// - `Some(connection)` if the instance exists and is enabled
    /// - `None` if not found, disabled, or temporarily unavailable
    pub async fn runtime_status(&self, name: &str) -> RemoteProviderRuntimeStatus {
        match self.get_connection_result(name).await {
            Ok(Some(connection)) => RemoteProviderRuntimeStatus {
                available: true,
                has_auth_secret: connection.auth_secret().is_some(),
            },
            Ok(None) => RemoteProviderRuntimeStatus {
                available: false,
                has_auth_secret: false,
            },
            Err(err) => {
                tracing::error!(
                    "Failed to resolve remote provider instance '{}' for runtime status: {}",
                    name,
                    err
                );
                RemoteProviderRuntimeStatus {
                    available: false,
                    has_auth_secret: false,
                }
            }
        }
    }

    pub(crate) async fn get(&self, name: &str) -> Option<RemoteProviderConnection> {
        match self.get_connection_result(name).await {
            Ok(connection) => connection,
            Err(err) => {
                tracing::error!(
                    "Failed to resolve remote provider instance '{}' for best-effort probe: {}",
                    name,
                    err
                );
                None
            }
        }
    }

    /// Resolve a provider client without silent fallback and attach a cooperative request context.
    ///
    /// Semantics:
    /// - `instance_name=None`: use the local singleton client.
    /// - `instance_name=Some(name)` and remote exists: use remote client.
    /// - `instance_name=Some(name)` and remote missing/disabled: return
    ///   [`ProviderError::InstanceNotFound`] instead of masking the issue by
    ///   falling back to the local singleton.
    /// - `instance_name=Some(name)` and remote resolution/config loading fails:
    ///   surface the underlying provider/config/internal error.
    pub(crate) async fn resolve_client_required_with_context<T>(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&crate::provider::ExecutionControl>,
        create_remote: impl FnOnce(RemoteProviderConnection) -> T,
        load_local: impl FnOnce() -> T,
    ) -> std::result::Result<T, ProviderError> {
        if let Some(request_context) = request_context {
            request_context
                .check_active()
                .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
        }

        match instance_name {
            Some(name) => self
                .get_connection_result(name)
                .await?
                .map(|connection| {
                    if let Some(request_context) = request_context {
                        request_context
                            .check_active()
                            .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
                    }
                    Ok::<T, ProviderError>(create_remote(
                        connection.with_request_context(request_context.cloned()),
                    ))
                })
                .transpose()?
                .ok_or_else(|| ProviderError::InstanceNotFound(name.to_string())),
            None => Ok(load_local()),
        }
    }
}
