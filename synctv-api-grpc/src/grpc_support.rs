//! Shared gRPC transport helpers used by public API and management services.

/// Map a typed [`ApiError`](synctv_api_common::impls::ApiError) to a gRPC `Status`.
///
/// Shared across gRPC service implementations to avoid duplicating the
/// identical match block in every service file.
///
/// For internal errors, the details are logged server-side and a generic
/// message is returned to the client to avoid leaking sensitive information.
#[must_use]
pub fn map_api_error_ref(err: &synctv_api_common::impls::ApiError) -> tonic::Status {
    let sanitized = synctv_api_common::api_error_model::sanitized_api_error(err);
    synctv_api_common::api_error_model::GoogleApiError::from_api_error(&sanitized).to_tonic_status()
}

pub fn map_api_error(err: impl Into<synctv_api_common::impls::ApiError>) -> tonic::Status {
    let err = err.into();
    map_api_error_ref(&err)
}

#[must_use]
pub fn map_auth_authorization_error(err: &synctv_core::Error) -> tonic::Status {
    let api_error = match err {
        synctv_core::Error::Authorization(message) => {
            synctv_api_common::impls::ApiError::Authorization(message.clone())
        }
        synctv_core::Error::KickCooldownDenied => {
            synctv_api_common::impls::ApiError::Authorization(
                synctv_core::Error::kick_cooldown_denied_message().to_string(),
            )
        }
        other => {
            tracing::error!(error = %other, "Unexpected authorization-classified auth error");
            synctv_api_common::impls::ApiError::Authorization(
                "You do not have permission to perform this action".to_string(),
            )
        }
    };
    map_api_error_ref(&api_error)
}

/// Extract the effective client IP for gRPC requests.
///
/// Matches HTTP semantics: only trust forwarded headers when the direct peer is
/// a configured trusted proxy. Otherwise fall back to the socket peer address.
pub fn extract_client_ip<T>(
    request: &tonic::Request<T>,
    runtime_settings: &synctv_api_common::ApiRuntimeSettings,
) -> Result<Option<std::net::IpAddr>, tonic::Status> {
    synctv_adapter::grpc::extract_client_ip(request, |ip| {
        runtime_settings.server.is_trusted_proxy(ip)
    })
}

pub fn request_user_agent<T>(request: &tonic::Request<T>) -> Result<Option<String>, tonic::Status> {
    request
        .metadata()
        .get("user-agent")
        .map(|value| {
            value.to_str().map(str::to_owned).map_err(|_| {
                tonic::Status::invalid_argument("user-agent metadata must be valid ASCII")
            })
        })
        .transpose()
}

pub fn request_metadata<T>(
    request: &tonic::Request<T>,
    runtime_settings: &synctv_api_common::ApiRuntimeSettings,
    timeout: Option<std::time::Duration>,
) -> Result<synctv_api_common::impls::RequestMetadata, tonic::Status> {
    let authorization = request
        .metadata()
        .get("authorization")
        .map(|value| {
            value.to_str().map(str::to_owned).map_err(|_| {
                tonic::Status::unauthenticated(
                    synctv_common::messages::INVALID_AUTHORIZATION_HEADER_NON_UTF8,
                )
            })
        })
        .transpose()?;
    let user_agent = request_user_agent(request)?;

    Ok(synctv_api_common::impls::RequestMetadata::new(
        synctv_api_common::impls::TransportProtocol::Grpc,
    )
    .with_authorization(authorization)
    .with_client_ip(extract_client_ip(request, runtime_settings)?)
    .with_user_agent(user_agent)
    .with_timeout(timeout))
}

#[must_use]
pub const fn grpc_unary_request_timeout() -> std::time::Duration {
    synctv_core::resilience::timeout::REMOTE_TRANSPORT_CALL_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::request_metadata;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn request_metadata_extracts_ascii_authorization() -> TestResult {
        let config = synctv_api_common::ApiRuntimeSettings::default();
        let mut request = tonic::Request::new(());
        request.metadata_mut().insert(
            "authorization",
            tonic::metadata::MetadataValue::from_static("Bearer token"),
        );

        let metadata = request_metadata(&request, &config, None)?;

        assert_eq!(metadata.authorization.as_deref(), Some("Bearer token"));
        assert_eq!(
            metadata.transport,
            synctv_api_common::impls::TransportProtocol::Grpc
        );
        Ok(())
    }

    #[test]
    fn request_metadata_ignores_authorization_binary_metadata() -> TestResult {
        let config = synctv_api_common::ApiRuntimeSettings::default();
        let mut request = tonic::Request::new(());
        request.metadata_mut().insert_bin(
            "authorization-bin",
            tonic::metadata::MetadataValue::from_bytes(b"\xff"),
        );

        let metadata = request_metadata(&request, &config, None)?;

        assert!(metadata.authorization.is_none());
        Ok(())
    }

    #[test]
    fn request_metadata_extracts_ascii_user_agent() -> TestResult {
        let config = synctv_api_common::ApiRuntimeSettings::default();
        let mut request = tonic::Request::new(());
        request.metadata_mut().insert(
            "user-agent",
            tonic::metadata::MetadataValue::from_static("synctv-test/1.0"),
        );

        let metadata = request_metadata(&request, &config, None)?;

        assert_eq!(metadata.user_agent.as_deref(), Some("synctv-test/1.0"));
        Ok(())
    }

    #[test]
    fn request_metadata_allows_missing_user_agent() -> TestResult {
        let config = synctv_api_common::ApiRuntimeSettings::default();
        let request = tonic::Request::new(());

        let metadata = request_metadata(&request, &config, None)?;

        assert!(metadata.user_agent.is_none());
        Ok(())
    }
}
