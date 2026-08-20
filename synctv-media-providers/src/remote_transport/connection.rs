use super::connector::{provider_connection_setup_error, resolve_ssrf_validated_address};
use super::endpoint::{
    normalized_transport_endpoint, required_auth_secret, validate_endpoint_ssrf,
};
use super::request::validate_auth_secret;
use crate::ProviderClientError;
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
use synctv_common::ssrf::SsrfGuard;
use synctv_common::ExecutionControl;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
use tonic::transport::{
    Certificate as TransportCertificate, ClientTlsConfig as TransportTlsConfig,
};
use tonic::transport::{Endpoint as TransportEndpoint, Uri as TransportUri};

type TransportChannel = tonic::transport::Channel;

/// Default per-request timeout for remote provider calls.
///
/// Reduced from 30s to 10s: hung requests under load consume threads.
/// Providers that genuinely need longer should use explicit deadlines.
const REMOTE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Match the remote provider server's HTTP/2 frame budget for large provider
/// directory/listing responses.
const PROVIDER_TRANSPORT_FRAME_SIZE_LIMIT: u32 = 4 * 1024 * 1024;

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
fn apply_default_transport_roots(mut tls_config: TransportTlsConfig) -> TransportTlsConfig {
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

#[derive(Clone)]
pub struct RemoteProviderTransportConfig {
    address_overrides: Arc<HashMap<String, SocketAddr>>,
    ssrf_guard: SsrfGuard,
    compression_enabled: bool,
}

impl RemoteProviderTransportConfig {
    #[must_use]
    pub fn new(
        address_overrides: Arc<HashMap<String, SocketAddr>>,
        ssrf_guard: SsrfGuard,
        compression_enabled: bool,
    ) -> Self {
        Self {
            address_overrides,
            ssrf_guard,
            compression_enabled,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RemoteProviderConnectionOptions {
    pub instance_name: String,
    pub endpoint: String,
    pub jwt_secret: Option<String>,
    #[cfg_attr(
        not(any(feature = "tls-webpki-roots", feature = "tls-native-roots")),
        allow(dead_code)
    )]
    pub custom_ca: Option<String>,
    pub timeout: Duration,
    pub tls: bool,
    pub insecure_tls: bool,
}

#[derive(Clone, Debug)]
pub struct RemoteProviderConnection {
    channel: TransportChannel,
    auth_secret: Option<Arc<str>>,
    request_context: Option<ExecutionControl>,
    transport_compression_enabled: bool,
}

impl RemoteProviderConnection {
    #[must_use]
    pub(crate) fn build_provider_client<T>(&self, create: impl FnOnce(TransportChannel) -> T) -> T {
        create(self.channel.clone())
    }

    #[must_use]
    pub fn auth_secret(&self) -> Option<&str> {
        self.auth_secret.as_deref()
    }

    #[must_use]
    pub(crate) const fn transport_compression_enabled(&self) -> bool {
        self.transport_compression_enabled
    }

    #[must_use]
    pub fn with_request_context(mut self, request_context: Option<ExecutionControl>) -> Self {
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

pub fn create_remote_connection(
    options: &RemoteProviderConnectionOptions,
    transport_config: &RemoteProviderTransportConfig,
) -> Result<RemoteProviderConnection, ProviderClientError> {
    let channel = create_transport_channel(
        options,
        Arc::clone(&transport_config.address_overrides),
        &transport_config.ssrf_guard,
    )?;
    let auth_secret = validate_auth_secret(Some(required_auth_secret(
        &options.instance_name,
        options.jwt_secret.as_deref(),
    )?))
    .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
    Ok(RemoteProviderConnection {
        channel,
        auth_secret: auth_secret.map(Arc::<str>::from),
        request_context: None,
        transport_compression_enabled: transport_config.compression_enabled,
    })
}

fn create_transport_channel(
    options: &RemoteProviderConnectionOptions,
    address_overrides: Arc<HashMap<String, SocketAddr>>,
    ssrf_guard: &SsrfGuard,
) -> Result<TransportChannel, ProviderClientError> {
    validate_endpoint_ssrf(&options.endpoint, ssrf_guard)?;

    let timeout = options.timeout;
    let transport_endpoint = normalized_transport_endpoint(&options.endpoint)?;
    let endpoint = TransportEndpoint::from_shared(transport_endpoint)
        .map_err(|error| {
            provider_connection_setup_error(
                "Remote provider endpoint configuration is invalid.",
                error,
            )
        })?
        .timeout(timeout)
        .max_frame_size(PROVIDER_TRANSPORT_FRAME_SIZE_LIMIT)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(10));
    #[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
    let mut endpoint = endpoint;

    if options.tls {
        if options.insecure_tls {
            tracing::warn!(
                "Instance '{}' configured with insecure TLS (skips certificate verification)",
                options.instance_name
            );

            #[cfg(not(any(
                feature = "tls-aws-lc",
                feature = "tls-ring",
                feature = "tls-webpki-roots",
                feature = "tls-native-roots"
            )))]
            {
                drop(endpoint);
                return Err(ProviderClientError::InvalidConfig(
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
                let channel =
                    connect_insecure_tls(&endpoint, address_overrides, ssrf_guard.clone());

                tracing::info!(
                    "Established insecure-TLS remote provider connection to {} (timeout: {:?})",
                    options.endpoint,
                    timeout,
                );

                return Ok(channel);
            }
        }

        #[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
        {
            let mut tls_config = TransportTlsConfig::new();

            if let Some(ref ca_pem) = options.custom_ca {
                let cert = TransportCertificate::from_pem(ca_pem);
                tls_config = tls_config.ca_certificate(cert);
            } else {
                tls_config = apply_default_transport_roots(tls_config);
            }

            endpoint = endpoint.tls_config(tls_config).map_err(|error| {
                provider_connection_setup_error(
                    "Remote provider TLS connection setup failed.",
                    error,
                )
            })?;
        }

        #[cfg(not(any(feature = "tls-webpki-roots", feature = "tls-native-roots")))]
        {
            return Err(ProviderClientError::InvalidConfig(
                "Remote provider TLS requires a TLS root feature".to_string(),
            ));
        }
    }

    let guard = ssrf_guard.clone();
    let connector = tower::service_fn(move |uri: TransportUri| {
        let guard = guard.clone();
        let address_overrides = address_overrides.clone();
        async move {
            let (_, address) =
                resolve_ssrf_validated_address(address_overrides, &uri, &guard).await?;
            let stream = tokio::net::TcpStream::connect(address).await?;
            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
        }
    });

    let channel = endpoint.connect_with_connector_lazy(connector);

    tracing::info!(
        "Established remote provider connection to {} (timeout: {:?}, TLS: {})",
        options.endpoint,
        timeout,
        options.tls
    );

    Ok(channel)
}

#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
fn connect_insecure_tls(
    endpoint: &TransportEndpoint,
    address_overrides: Arc<HashMap<String, SocketAddr>>,
    ssrf_guard: SsrfGuard,
) -> TransportChannel {
    crate::install_process_crypto_provider();

    let tls_config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();

    let connector = tower::service_fn(move |uri: TransportUri| {
        let tls_config = tls_config.clone();
        let guard = ssrf_guard.clone();
        let address_overrides = address_overrides.clone();
        async move {
            let (host, address) =
                resolve_ssrf_validated_address(address_overrides, &uri, &guard).await?;
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
