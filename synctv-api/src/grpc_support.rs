//! Shared gRPC transport helpers used by public API and management services.

pub const ERROR_CODE_METADATA_KEY: &str = "x-synctv-error-code";
pub const RETRY_AFTER_METADATA_KEY: &str = "retry-after";

/// Map a typed [`ApiError`](crate::impls::ApiError) to a gRPC `Status`.
///
/// Shared across gRPC service implementations to avoid duplicating the
/// identical match block in every service file.
///
/// For internal errors, the details are logged server-side and a generic
/// message is returned to the client to avoid leaking sensitive information.
#[must_use]
pub fn map_api_error_ref(err: &crate::impls::ApiError) -> tonic::Status {
    use crate::impls::ErrorKind;
    let msg = err.message().to_string();
    let mut status = match err.classify() {
        ErrorKind::NotFound => tonic::Status::not_found(msg),
        ErrorKind::Unauthenticated => tonic::Status::unauthenticated(msg),
        ErrorKind::PermissionDenied => tonic::Status::permission_denied(msg),
        ErrorKind::AlreadyExists => tonic::Status::already_exists(msg),
        ErrorKind::Conflict => tonic::Status::aborted(msg),
        ErrorKind::InvalidArgument => tonic::Status::invalid_argument(msg),
        ErrorKind::RateLimited => tonic::Status::resource_exhausted(msg),
        ErrorKind::ServiceUnavailable => tonic::Status::unavailable(msg),
        ErrorKind::Timeout => tonic::Status::deadline_exceeded(msg),
        ErrorKind::Internal => {
            tracing::error!("API internal error: {msg}");
            tonic::Status::internal("Internal error")
        }
    };

    if let Ok(value) = err.code().to_string().parse() {
        status.metadata_mut().insert(ERROR_CODE_METADATA_KEY, value);
    }
    if let Some(retry_after_seconds) = err.retry_after_seconds() {
        if let Ok(value) = retry_after_seconds.to_string().parse() {
            status
                .metadata_mut()
                .insert(RETRY_AFTER_METADATA_KEY, value);
        }
    }

    status
}

pub fn map_api_error(err: impl Into<crate::impls::ApiError>) -> tonic::Status {
    let err = err.into();
    map_api_error_ref(&err)
}

#[must_use]
pub fn map_auth_authorization_error(err: &synctv_core::Error) -> tonic::Status {
    match err {
        synctv_core::Error::Authorization(message) => {
            tonic::Status::permission_denied(message.clone())
        }
        other => {
            tracing::error!(error = %other, "Unexpected authorization-classified auth error");
            tonic::Status::permission_denied("You do not have permission to perform this action")
        }
    }
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
    synctv_core::resilience::timeout::GRPC_CALL_TIMEOUT
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
