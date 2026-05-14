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
    use crate::impls::ErrorKind;
    let msg = err.message().to_string();
    match err.classify() {
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
    }
}

#[allow(clippy::needless_pass_by_value)]
pub fn map_api_error(err: crate::impls::ApiError) -> tonic::Status {
    map_api_error_ref(&err)
}

#[must_use]
pub fn map_auth_authorization_error(err: &synctv_core::Error) -> tonic::Status {
    match err {
        synctv_core::Error::Authorization(message) => {
            tonic::Status::permission_denied(message.clone())
        }
        synctv_core::Error::EmailNotVerified => tonic::Status::permission_denied(
            "Email not verified. Please verify your email to continue.",
        ),
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
#[must_use]
pub fn extract_client_ip<T>(
    request: &tonic::Request<T>,
    config: &synctv_core::Config,
) -> Option<std::net::IpAddr> {
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
            if let Some(value) = request.metadata().get(header_name).and_then(|v| {
                v.to_str()
                    .ok()
                    .and_then(|s| s.parse::<axum::http::HeaderValue>().ok())
            }) {
                headers.insert(header_name, value);
            }
        }
        return Some(crate::client_ip::extract_client_ip_from_headers(
            config, peer_ip, &headers,
        ));
    }

    remote_addr
}

#[must_use]
pub fn request_metadata<T>(
    request: &tonic::Request<T>,
    config: &synctv_core::Config,
    timeout: Option<std::time::Duration>,
) -> crate::impls::RequestMetadata {
    let authorization = request
        .metadata()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let user_agent = request
        .metadata()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    crate::impls::RequestMetadata::new(crate::impls::TransportProtocol::Grpc)
        .with_authorization(authorization)
        .with_client_ip(extract_client_ip(request, config))
        .with_user_agent(user_agent)
        .with_timeout(timeout)
}

#[must_use]
pub const fn grpc_unary_request_timeout() -> std::time::Duration {
    synctv_core::resilience::timeout::GRPC_CALL_TIMEOUT
}
