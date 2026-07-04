//! Shared gRPC transport helpers used by public API and management services.

/// Map a typed [`ApiError`](crate::impls::ApiError) to a gRPC `Status`.
///
/// Shared across gRPC service implementations to avoid duplicating the
/// identical match block in every service file.
///
/// For internal errors, the details are logged server-side and a generic
/// message is returned to the client to avoid leaking sensitive information.
#[must_use]
pub fn map_api_error_ref(err: &crate::impls::ApiError) -> tonic::Status {
    let sanitized = crate::api_error_model::sanitized_api_error(err);
    crate::api_error_model::GoogleApiError::from_api_error(&sanitized).to_tonic_status()
}

pub fn map_api_error(err: impl Into<crate::impls::ApiError>) -> tonic::Status {
    let err = err.into();
    map_api_error_ref(&err)
}

#[must_use]
pub fn map_auth_authorization_error(err: &synctv_core::Error) -> tonic::Status {
    let api_error = match err {
        synctv_core::Error::Authorization(message) => {
            crate::impls::ApiError::Authorization(message.clone())
        }
        other => {
            tracing::error!(error = %other, "Unexpected authorization-classified auth error");
            crate::impls::ApiError::Authorization(
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
    config: &synctv_core::Config,
) -> Result<Option<std::net::IpAddr>, tonic::Status> {
    let remote_addr = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip())
        .or_else(|| {
            request
                .extensions()
                .get::<tonic::transport::server::TcpConnectInfo>()
                .and_then(tonic::transport::server::TcpConnectInfo::remote_addr)
                .map(|addr| addr.ip())
        });

    if let Some(peer_ip) = remote_addr {
        let mut headers = axum::http::HeaderMap::new();
        for header_name in ["x-forwarded-for", "x-real-ip"] {
            if let Some(value) = request.metadata().get(header_name) {
                let value = value
                    .to_str()
                    .map_err(|_| {
                        tonic::Status::invalid_argument(format!(
                            "{header_name} metadata must be valid ASCII"
                        ))
                    })?
                    .parse::<axum::http::HeaderValue>()
                    .map_err(|_| {
                        tonic::Status::invalid_argument(format!(
                            "{header_name} metadata must be a valid HTTP header value"
                        ))
                    })?;
                headers.insert(header_name, value);
            }
        }
        return crate::client_ip::extract_client_ip_from_headers(config, peer_ip, &headers)
            .map(Some)
            .map_err(|error| tonic::Status::invalid_argument(error.to_string()));
    }

    Ok(remote_addr)
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
    config: &synctv_core::Config,
    timeout: Option<std::time::Duration>,
) -> Result<crate::impls::RequestMetadata, tonic::Status> {
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

    Ok(
        crate::impls::RequestMetadata::new(crate::impls::TransportProtocol::Grpc)
            .with_authorization(authorization)
            .with_client_ip(extract_client_ip(request, config)?)
            .with_user_agent(user_agent)
            .with_timeout(timeout),
    )
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
        let config = synctv_core::Config::default();
        let mut request = tonic::Request::new(());
        request.metadata_mut().insert(
            "authorization",
            tonic::metadata::MetadataValue::from_static("Bearer token"),
        );

        let metadata = request_metadata(&request, &config, None)?;

        assert_eq!(metadata.authorization.as_deref(), Some("Bearer token"));
        assert_eq!(metadata.transport, crate::impls::TransportProtocol::Grpc);
        Ok(())
    }

    #[test]
    fn request_metadata_ignores_authorization_binary_metadata() -> TestResult {
        let config = synctv_core::Config::default();
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
        let config = synctv_core::Config::default();
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
        let config = synctv_core::Config::default();
        let request = tonic::Request::new(());

        let metadata = request_metadata(&request, &config, None)?;

        assert!(metadata.user_agent.is_none());
        Ok(())
    }
}
