use super::RemoteProviderManager;
use crate::models::ProviderInstance;
#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
use tonic::transport::{Certificate, ClientTlsConfig};
use tonic::transport::{Channel, Endpoint, Uri};

/// Match the remote provider server's HTTP/2 frame budget for large provider
/// directory/listing responses.
const PROVIDER_GRPC_FRAME_SIZE_LIMIT: u32 = 4 * 1024 * 1024;

/// A certificate verifier that accepts any server certificate.
#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
#[derive(Debug)]
struct NoVerifier;

#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}

#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
fn apply_default_grpc_roots(mut tls_config: ClientTlsConfig) -> ClientTlsConfig {
    #[cfg(feature = "tls-webpki-roots")]
    {
        tls_config = tls_config.with_webpki_roots();
    }

    #[cfg(feature = "tls-native-roots")]
    {
        tls_config = tls_config.with_native_roots();
    }

    tls_config
}

impl RemoteProviderManager {
    fn resolve_ssrf_validated_address(
        address_overrides: Arc<HashMap<String, SocketAddr>>,
        uri: &Uri,
        guard: &synctv_common::ssrf::SsrfGuard,
    ) -> impl std::future::Future<Output = std::io::Result<(String, SocketAddr)>> + Send {
        let uri = uri.clone();
        let guard = guard.clone();
        async move {
            let host = uri.host().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing host")
            })?;

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

    /// Create a gRPC channel for the given provider instance.
    pub(super) fn create_grpc_channel(&self, config: &ProviderInstance) -> crate::Result<Channel> {
        Self::validate_endpoint_ssrf(&config.endpoint, &self.ssrf_guard)?;

        let timeout = config.parse_timeout().map_err(crate::Error::Internal)?;
        let transport_endpoint = Self::normalized_transport_endpoint(config)?;
        let endpoint = Endpoint::from_shared(transport_endpoint)
            .map_err(|error| {
                Self::provider_connection_setup_error(
                    "Remote provider endpoint configuration is invalid.",
                    error,
                )
            })?
            .timeout(timeout)
            .max_frame_size(PROVIDER_GRPC_FRAME_SIZE_LIMIT)
            .tcp_keepalive(Some(Duration::from_secs(30)))
            .http2_keep_alive_interval(Duration::from_secs(30))
            .keep_alive_timeout(Duration::from_secs(10));
        #[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
        let mut endpoint = endpoint;

        if config.tls {
            if config.insecure_tls {
                tracing::warn!(
                    "Instance '{}' configured with insecure TLS (skips certificate verification)",
                    config.name
                );

                #[cfg(not(any(
                    feature = "tls-aws-lc",
                    feature = "tls-ring",
                    feature = "tls-webpki-roots",
                    feature = "tls-native-roots"
                )))]
                {
                    drop(endpoint);
                    return Err(crate::Error::InvalidInput(
                        "Remote provider insecure TLS requires a TLS provider feature".to_string(),
                    ));
                }

                #[cfg(any(
                    feature = "tls-aws-lc",
                    feature = "tls-ring",
                    feature = "tls-webpki-roots",
                    feature = "tls-native-roots"
                ))]
                {
                    let channel = self.connect_insecure_tls(&endpoint);

                    tracing::info!(
                        "Established insecure-TLS gRPC connection to {} (timeout: {:?})",
                        config.endpoint,
                        timeout,
                    );

                    return Ok(channel);
                }
            }

            #[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
            {
                let mut tls_config = ClientTlsConfig::new();

                if let Some(ref ca_pem) = config.custom_ca {
                    let cert = Certificate::from_pem(ca_pem);
                    tls_config = tls_config.ca_certificate(cert);
                } else {
                    tls_config = apply_default_grpc_roots(tls_config);
                }

                endpoint = endpoint.tls_config(tls_config).map_err(|error| {
                    Self::provider_connection_setup_error(
                        "Remote provider TLS connection setup failed.",
                        error,
                    )
                })?;
            }

            #[cfg(not(any(feature = "tls-webpki-roots", feature = "tls-native-roots")))]
            {
                return Err(crate::Error::InvalidInput(
                    "Remote provider TLS requires a TLS root feature".to_string(),
                ));
            }
        }

        let guard = self.ssrf_guard.clone();
        let address_overrides = Arc::clone(&self.address_overrides);
        let connector = tower::service_fn(move |uri: Uri| {
            let guard = guard.clone();
            let address_overrides = address_overrides.clone();
            async move {
                let (_, address) =
                    Self::resolve_ssrf_validated_address(address_overrides, &uri, &guard).await?;
                let stream = tokio::net::TcpStream::connect(address).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        });

        let channel = endpoint.connect_with_connector_lazy(connector);

        tracing::info!(
            "Established gRPC connection to {} (timeout: {:?}, TLS: {})",
            config.endpoint,
            timeout,
            config.tls
        );

        Ok(channel)
    }

    /// Connect to a gRPC endpoint with TLS certificate verification disabled.
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    fn connect_insecure_tls(&self, endpoint: &Endpoint) -> Channel {
        crate::install_process_crypto_provider();

        let guard = self.ssrf_guard.clone();
        let address_overrides = Arc::clone(&self.address_overrides);

        let tls_config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();

        let connector = tower::service_fn(move |uri: Uri| {
            let tls_config = tls_config.clone();
            let guard = guard.clone();
            let address_overrides = address_overrides.clone();
            async move {
                let (host, address) =
                    Self::resolve_ssrf_validated_address(address_overrides, &uri, &guard).await?;
                let tcp = tokio::net::TcpStream::connect(address).await?;
                let server_name = rustls::pki_types::ServerName::try_from(host)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                let tls = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
                let stream = tls.connect(server_name, tcp).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        });

        endpoint.clone().connect_with_connector_lazy(connector)
    }
}
