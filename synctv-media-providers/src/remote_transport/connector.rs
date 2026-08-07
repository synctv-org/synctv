use crate::ProviderClientError;
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use synctv_common::ssrf::SsrfGuard;
use tonic::transport::Uri as TransportUri;

pub(super) fn provider_connection_setup_error(
    message: &'static str,
    error: impl std::fmt::Display,
) -> ProviderClientError {
    tracing::error!(error = %error, "{message}");
    ProviderClientError::InvalidConfig(message.to_string())
}

pub(super) fn resolve_ssrf_validated_address(
    address_overrides: Arc<HashMap<String, SocketAddr>>,
    uri: &TransportUri,
    guard: &SsrfGuard,
) -> impl Future<Output = std::io::Result<(String, SocketAddr)>> + Send {
    let uri = uri.clone();
    let guard = guard.clone();
    async move {
        let host = uri
            .host()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing host"))?;

        if let Some(address) = address_overrides.get(host).copied() {
            tracing::debug!(
                host,
                ip = %address.ip(),
                port = address.port(),
                "Connecting to remote provider via explicit test address override"
            );
            return Ok((host.to_string(), address));
        }

        if guard.is_host_blocked(host) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("SSRF validation: host '{host}' is blocked at connection time"),
            ));
        }

        let port = uri.port_u16().unwrap_or_else(|| {
            if uri.scheme_str() == Some("https") {
                443
            } else {
                80
            }
        });

        let mut resolved = tokio::net::lookup_host((host, port)).await?;
        let address = resolved.find(|addr| {
            let blocked = guard.is_ip_blocked_for_host(host, &addr.ip());
            if blocked {
                tracing::warn!(
                    host,
                    ip = %addr.ip(),
                    "Blocked remote provider connection due to SSRF policy during DNS resolution"
                );
            }
            !blocked
        });

        let address = address.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("SSRF validation: all resolved addresses for '{host}' are blocked"),
            )
        })?;

        tracing::debug!(
            host,
            ip = %address.ip(),
            port = address.port(),
            "Connecting to remote provider after SSRF DNS validation"
        );

        Ok((host.to_string(), address))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_connection_setup_error_hides_invalid_endpoint_details() {
        let err = provider_connection_setup_error(
            "Remote provider endpoint configuration is invalid.",
            "relative URL without a base",
        );

        assert!(matches!(
            err,
            ProviderClientError::InvalidConfig(ref message)
                if message == "Remote provider endpoint configuration is invalid."
        ));
    }

    #[test]
    fn test_provider_connection_setup_error_hides_tls_connect_details() {
        let err = provider_connection_setup_error(
            "Remote provider TLS connection setup failed.",
            "certificate verify failed",
        );

        assert!(matches!(
            err,
            ProviderClientError::InvalidConfig(ref message)
                if message == "Remote provider TLS connection setup failed."
        ));
    }
}
