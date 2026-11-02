//! Test-support helpers: an h3 client that trusts anything, for exercising a
//! self-signed QUIC endpoint from integration tests.

use std::sync::Arc;

use bytes::Buf;

/// Accepts every server certificate (test endpoints use ephemeral
/// self-signed certificates).
#[derive(Debug)]
struct AcceptAnyServerCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// A rustls client config that accepts any certificate and negotiates `h3`.
#[must_use]
pub fn insecure_client_config() -> rustls::ClientConfig {
    let provider = crate::tls::crypto_provider();
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS 1.3 available")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert(
            crate::tls::crypto_provider(),
        )))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h3".to_vec()];
    config
}

/// Performs one HTTP/3 GET and returns `(status, headers, body)`.
///
/// # Panics
///
/// Panics on transport or protocol errors: this helper exists for
/// integration tests.
pub async fn h3_get(
    addr: std::net::SocketAddr,
    path: &str,
) -> (http::StatusCode, http::HeaderMap, Vec<u8>) {
    h3_request(
        addr,
        http::Request::builder().uri(format!("https://localhost{path}")),
    )
    .await
}

/// Performs one HTTP/3 request built from `builder` (no body) and returns
/// `(status, headers, body)`.
///
/// # Panics
///
/// Panics on transport or protocol errors: this helper exists for
/// integration tests.
pub async fn h3_request(
    addr: std::net::SocketAddr,
    builder: http::request::Builder,
) -> (http::StatusCode, http::HeaderMap, Vec<u8>) {
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(insecure_client_config())
            .expect("valid client config"),
    ));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().expect("client bind"))
        .expect("client endpoint");
    endpoint.set_default_client_config(client_config);
    let connection = endpoint
        .connect(addr, "localhost")
        .expect("connect request")
        .await
        .expect("QUIC handshake");

    let (mut driver, mut send_request) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("h3 handshake");
    tokio::spawn(async move {
        let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    let request = builder.body(()).expect("request builds");
    let mut stream = send_request.send_request(request).await.expect("send");
    stream.finish().await.expect("finish request body");
    let response = stream.recv_response().await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    while let Some(chunk) = stream.recv_data().await.expect("body chunk") {
        body.extend_from_slice(chunk.chunk());
    }
    (status, headers, body)
}
