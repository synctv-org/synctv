//! Remote provider transport helpers.
//!
//! This module owns the wire-level request wrapper, timeout handling, and
//! transport-status mapping for remote provider calls.

use super::{upstream_transport, ExecutionControl, ProviderError};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tonic::body::Body as TransportBody;
use tonic::client::GrpcService as TransportService;
use tonic::codec::CompressionEncoding as TransportCompression;
use tonic::codegen::{
    Body as TransportResponseBody, Bytes as TransportBytes, StdError as TransportStdError,
};
use tonic::metadata::MetadataValue as TransportMetadataValue;
use tonic::{Code, Request as TransportRequest, Status as TransportStatus};
use tonic_health::pb::{health_client::HealthClient, HealthCheckRequest};

type TransportChannel = tonic::transport::Channel;

/// Default per-request timeout for remote provider calls.
///
/// Reduced from 30s to 10s: hung requests under load consume threads.
/// Providers that genuinely need longer should use explicit deadlines.
const REMOTE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub(crate) struct RemoteProviderConnection {
    channel: TransportChannel,
    auth_secret: Option<Arc<str>>,
    request_context: Option<ExecutionControl>,
    transport_compression_enabled: bool,
}

impl RemoteProviderConnection {
    #[must_use]
    pub(crate) fn new_with_transport_compression(
        channel: TransportChannel,
        auth_secret: Option<impl Into<String>>,
        transport_compression_enabled: bool,
    ) -> Self {
        Self {
            channel,
            auth_secret: auth_secret.map(|secret| Arc::<str>::from(secret.into())),
            request_context: None,
            transport_compression_enabled,
        }
    }

    #[must_use]
    pub(crate) fn build_provider_client<T>(&self, create: impl FnOnce(TransportChannel) -> T) -> T {
        create(self.channel.clone())
    }

    #[must_use]
    pub(crate) fn auth_secret(&self) -> Option<&str> {
        self.auth_secret.as_deref()
    }

    #[must_use]
    pub(crate) const fn transport_compression_enabled(&self) -> bool {
        self.transport_compression_enabled
    }

    #[must_use]
    pub(crate) fn with_request_context(
        mut self,
        request_context: Option<ExecutionControl>,
    ) -> Self {
        self.request_context = request_context;
        self
    }

    #[must_use]
    pub(crate) const fn request_context(&self) -> Option<&ExecutionControl> {
        self.request_context.as_ref()
    }

    #[must_use]
    pub(crate) fn effective_request_timeout(&self) -> Duration {
        self.request_context
            .as_ref()
            .and_then(ExecutionControl::remaining_timeout)
            .unwrap_or(REMOTE_REQUEST_TIMEOUT)
    }
}

pub(crate) fn apply_provider_client_compression<T>(
    client: T,
    transport_compression_enabled: bool,
) -> T
where
    T: ProviderTransportClientCompression,
{
    if transport_compression_enabled {
        client
            .accept_provider_compression(TransportCompression::Gzip)
            .send_provider_compression(TransportCompression::Gzip)
    } else {
        client
    }
}

pub(crate) trait ProviderTransportClientCompression: Sized {
    fn accept_provider_compression(self, encoding: TransportCompression) -> Self;
    fn send_provider_compression(self, encoding: TransportCompression) -> Self;
}

impl<T> ProviderTransportClientCompression
    for upstream_transport::alist::alist_client::AlistClient<T>
where
    T: TransportService<TransportBody>,
    T::ResponseBody: TransportResponseBody<Data = TransportBytes> + Send + 'static,
    <T::ResponseBody as TransportResponseBody>::Error: Into<TransportStdError> + Send,
{
    fn accept_provider_compression(self, encoding: TransportCompression) -> Self {
        self.accept_compressed(encoding)
    }

    fn send_provider_compression(self, encoding: TransportCompression) -> Self {
        self.send_compressed(encoding)
    }
}

impl<T> ProviderTransportClientCompression
    for upstream_transport::bilibili::bilibili_client::BilibiliClient<T>
where
    T: TransportService<TransportBody>,
    T::ResponseBody: TransportResponseBody<Data = TransportBytes> + Send + 'static,
    <T::ResponseBody as TransportResponseBody>::Error: Into<TransportStdError> + Send,
{
    fn accept_provider_compression(self, encoding: TransportCompression) -> Self {
        self.accept_compressed(encoding)
    }

    fn send_provider_compression(self, encoding: TransportCompression) -> Self {
        self.send_compressed(encoding)
    }
}

impl<T> ProviderTransportClientCompression for upstream_transport::emby::emby_client::EmbyClient<T>
where
    T: TransportService<TransportBody>,
    T::ResponseBody: TransportResponseBody<Data = TransportBytes> + Send + 'static,
    <T::ResponseBody as TransportResponseBody>::Error: Into<TransportStdError> + Send,
{
    fn accept_provider_compression(self, encoding: TransportCompression) -> Self {
        self.accept_compressed(encoding)
    }

    fn send_provider_compression(self, encoding: TransportCompression) -> Self {
        self.send_compressed(encoding)
    }
}

pub(crate) async fn execute_remote_call<T, E, F>(
    connection: &RemoteProviderConnection,
    context: &str,
    future: F,
) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
    E: From<synctv_media_providers::ProviderClientError>,
{
    // This is an intentional cancellation boundary around outbound remote I/O.
    // Upper-layer business logic remains cooperatively cancellable; only the
    // remote transport wait itself is aborted here.
    let request_timeout = connection.effective_request_timeout();
    let run = async move {
        tokio::time::timeout(request_timeout, future)
            .await
            .map_err(|_| {
                E::from(synctv_media_providers::ProviderClientError::Network(
                    format!(
                        "remote transport request timeout ({}s) for {context}",
                        request_timeout.as_secs_f64(),
                    ),
                ))
            })?
    };

    match connection.request_context() {
        Some(request_context) => {
            let cancellation = request_context.cancellation_token();
            tokio::select! {
                () = cancellation.cancelled() => Err(E::from(
                    synctv_media_providers::ProviderClientError::Network(format!(
                        "remote transport request cancelled for {context}"
                    ))
                )),
                result = run => result,
            }
        }
        None => run.await,
    }
}

pub(crate) fn build_remote_request<T>(
    auth_secret: Option<&str>,
    payload: T,
) -> Result<TransportRequest<T>, synctv_media_providers::ProviderClientError> {
    let mut request = TransportRequest::new(payload);
    let Some(auth_secret) = auth_secret
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
    else {
        return Ok(request);
    };

    let metadata_value = auth_secret.parse().map_err(|e| {
        synctv_media_providers::ProviderClientError::InvalidHeader(format!(
            "invalid x-provider-secret metadata value: {e}"
        ))
    })?;

    request
        .metadata_mut()
        .insert("x-provider-secret", metadata_value);
    Ok(request)
}

fn build_health_check_request(
    instance_name: &str,
    auth_secret: Option<&str>,
) -> crate::Result<TransportRequest<HealthCheckRequest>> {
    let mut request = TransportRequest::new(HealthCheckRequest {
        service: String::new(),
    });
    let secret = auth_secret.ok_or_else(|| {
        crate::Error::InvalidInput(format!(
            "Remote provider instance '{instance_name}' requires a non-empty jwt_secret for health checks"
        ))
    })?;
    let metadata_value = secret.parse().map_err(|e| {
        crate::Error::InvalidInput(format!(
            "Remote provider instance '{instance_name}' jwt_secret must be valid ASCII remote transport metadata: {e}"
        ))
    })?;
    request
        .metadata_mut()
        .insert("x-provider-secret", metadata_value);
    Ok(request)
}

pub(crate) async fn execute_health_check(
    instance_name: &str,
    connection: &RemoteProviderConnection,
    control: &ExecutionControl,
    timeout: Duration,
) -> crate::Result<i32> {
    let mut client = connection.build_provider_client(HealthClient::new);
    let request = build_health_check_request(instance_name, connection.auth_secret())?;

    let response = control
        .run(client.check(request))
        .await
        .map_err(|err| match err {
            synctv_common::ExecutionControlError::DeadlineExceeded => {
                crate::Error::InvalidInput(format!(
                    "Remote provider instance '{instance_name}' connectivity validation timed out after {}s",
                    timeout.as_secs()
                ))
            }
            other => crate::Error::from(other),
        })?
        .map_err(|status| {
            crate::Error::InvalidInput(format!(
                "Remote provider instance '{instance_name}' health check failed: {status}"
            ))
        })?;

    Ok(response.into_inner().status)
}

pub(crate) fn validate_auth_secret(
    auth_secret: Option<&str>,
) -> Result<Option<&str>, ProviderError> {
    match auth_secret.map(str::trim) {
        Some("") => Err(ProviderError::InvalidConfig(
            "remote provider auth secret must not be empty".to_string(),
        )),
        Some(secret) => {
            if !secret.is_ascii() {
                return Err(ProviderError::InvalidConfig(
                    "remote provider auth secret must be valid ASCII remote transport metadata"
                        .to_string(),
                ));
            }
            TransportMetadataValue::try_from(secret).map_err(|_| {
                ProviderError::InvalidConfig(
                    "remote provider auth secret must be valid ASCII remote transport metadata"
                        .to_string(),
                )
            })?;
            Ok(Some(secret))
        }
        None => Ok(None),
    }
}

const fn remote_status_to_http_status(code: Code) -> Option<reqwest::StatusCode> {
    match code {
        Code::NotFound => Some(reqwest::StatusCode::NOT_FOUND),
        Code::PermissionDenied => Some(reqwest::StatusCode::FORBIDDEN),
        Code::ResourceExhausted => Some(reqwest::StatusCode::TOO_MANY_REQUESTS),
        Code::FailedPrecondition | Code::AlreadyExists => Some(reqwest::StatusCode::CONFLICT),
        _ => None,
    }
}

pub(crate) fn map_remote_status(
    context: &str,
    status: &TransportStatus,
) -> synctv_media_providers::ProviderClientError {
    let message = status.message().to_string();
    match status.code() {
        Code::Unauthenticated => synctv_media_providers::ProviderClientError::Auth(message),
        Code::InvalidArgument => {
            synctv_media_providers::ProviderClientError::InvalidConfig(message)
        }
        Code::Unimplemented => synctv_media_providers::ProviderClientError::Api {
            code: 501,
            message: format!("remote transport {context}: {message}"),
        },
        Code::DeadlineExceeded | Code::Unavailable | Code::Cancelled => {
            synctv_media_providers::ProviderClientError::Network(format!(
                "remote transport {} for {}: {}",
                status.code(),
                context,
                message
            ))
        }
        code => {
            if let Some(http_status) = remote_status_to_http_status(code) {
                synctv_media_providers::ProviderClientError::Http {
                    status: http_status,
                    url: format!("http://remote/{context}"),
                    retry_after_secs: None,
                    body: message,
                }
            } else {
                synctv_media_providers::ProviderClientError::Api {
                    code: i64::from(code as i32),
                    message,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestResultExt;
    use reqwest::StatusCode;
    use synctv_media_providers::ProviderClientError;

    #[test]
    fn test_build_remote_request_inserts_x_provider_secret() {
        let request =
            build_remote_request(Some("shared-secret"), 42_u32).checked("request should build");
        assert_eq!(request.get_ref(), &42_u32);
        assert_eq!(
            request.metadata().get("x-provider-secret"),
            Some(&TransportMetadataValue::from_static("shared-secret"))
        );
    }

    #[test]
    fn test_build_remote_request_omits_header_when_secret_is_blank() {
        let request = build_remote_request(Some("   "), 42_u32).checked("request should build");
        assert_eq!(request.get_ref(), &42_u32);
        assert!(
            request.metadata().get("x-provider-secret").is_none(),
            "blank secrets must not produce a malformed header"
        );
    }

    #[test]
    fn test_map_remote_status_unauthenticated_to_auth() {
        let error = map_remote_status(
            "login",
            &TransportStatus::unauthenticated("Invalid provider secret"),
        );
        assert!(matches!(
            error,
            ProviderClientError::Auth(message) if message == "Invalid provider secret"
        ));
    }

    #[test]
    fn test_map_remote_status_invalid_argument_to_invalid_config() {
        let error = map_remote_status(
            "fs_get",
            &TransportStatus::invalid_argument("missing host parameter"),
        );
        assert!(matches!(
            error,
            ProviderClientError::InvalidConfig(message) if message == "missing host parameter"
        ));
    }

    #[test]
    fn test_map_remote_status_not_found_to_http_404() {
        let error = map_remote_status("me", &TransportStatus::not_found("user not found"));
        assert!(matches!(
            error,
            ProviderClientError::Http { status, ref url, ref body, retry_after_secs: None }
                if status == StatusCode::NOT_FOUND
                    && url == "http://remote/me"
                    && body == "user not found"
        ));
    }

    #[test]
    fn test_map_remote_status_unimplemented_to_api_501() {
        let error = map_remote_status(
            "future_method",
            &TransportStatus::unimplemented("rpc unavailable"),
        );
        assert!(matches!(
            error,
            ProviderClientError::Api { code: 501, message }
                if message == "remote transport future_method: rpc unavailable"
        ));
    }
}
