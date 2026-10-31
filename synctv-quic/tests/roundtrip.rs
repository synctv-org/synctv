//! Interop round trips for the QUIC listener: plain HTTP/3 through an h3
//! client, and WebTransport through the independent `wtransport` client (the
//! implementation browsers are validated against).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::convert::Infallible;
use std::sync::Arc;

use bytes::{Buf, Bytes};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use synctv_quic::{
    crypto_provider, generate_self_signed_config, read_frame, serve_quic, write_frame,
    QuicServerConfig, WebTransportHandler, WtBidiStream,
};
use tokio_util::sync::CancellationToken;
use tower::service_fn;

/// Accepts every certificate: the server uses an ephemeral self-signed one.
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

fn insecure_client_config() -> rustls::ClientConfig {
    let provider = crypto_provider();
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .expect("TLS 1.3 available")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert(provider)))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h3".to_vec()];
    config
}

/// Echoes every frame back; rejects any path other than `/wt/echo`.
struct EchoHandler;

#[async_trait::async_trait]
impl WebTransportHandler for EchoHandler {
    async fn accept(&self, request: &Request<()>) -> Result<(), StatusCode> {
        if request.uri().path() == "/wt/echo" {
            Ok(())
        } else {
            Err(StatusCode::NOT_FOUND)
        }
    }

    async fn run_session(&self, _request: Request<()>, stream: WtBidiStream) {
        let (mut send, mut recv) = h3::quic::BidiStream::split(stream);
        while let Ok(Some(frame)) = read_frame(&mut recv).await {
            if write_frame(&mut send, &frame).await.is_err() {
                break;
            }
        }
    }
}

async fn start_server() -> (std::net::SocketAddr, CancellationToken) {
    let (rustls, _der) = generate_self_signed_config();
    let service = service_fn(|request: Request<Full<Bytes>>| async move {
        let path = request.uri().path().to_string();
        let body = request.into_body().collect().await.unwrap().to_bytes();
        let response = match path.as_str() {
            "/hello" => Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/plain")
                .body(Full::new(Bytes::from_static(b"hello over h3")))
                .unwrap(),
            "/echo" => Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(body))
                .unwrap(),
            _ => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        };
        Ok::<_, Infallible>(response)
    });
    let shutdown = CancellationToken::new();
    let addr = serve_quic(
        "127.0.0.1:0".parse().unwrap(),
        QuicServerConfig {
            rustls,
            max_webtransport_sessions: 8,
        },
        service,
        Some(Arc::new(EchoHandler)),
        shutdown.clone(),
    )
    .await
    .expect("quic server binds");
    (addr, shutdown)
}

async fn h3_client(
    addr: std::net::SocketAddr,
) -> (
    h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    tokio::task::JoinHandle<()>,
) {
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(insecure_client_config()).unwrap(),
    ));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(client_config);
    let connection = endpoint
        .connect(addr, "localhost")
        .unwrap()
        .await
        .expect("QUIC connect");
    let (mut driver, send_request) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("h3 handshake");
    let driver_task = tokio::spawn(async move {
        let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });
    (send_request, driver_task)
}

#[tokio::test]
async fn serves_plain_http3_requests() {
    let (addr, shutdown) = start_server().await;
    let (mut send_request, _driver) = h3_client(addr).await;

    let request = Request::builder()
        .uri(format!("https://localhost:{}/hello", addr.port()))
        .body(())
        .unwrap();
    let mut stream = send_request.send_request(request).await.unwrap();
    stream.finish().await.unwrap();
    let response = stream.recv_response().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Vec::new();
    while let Some(chunk) = stream.recv_data().await.unwrap() {
        body.extend_from_slice(chunk.chunk());
    }
    assert_eq!(body, b"hello over h3");

    // POST body round trip.
    let request = Request::builder()
        .method("POST")
        .uri(format!("https://localhost:{}/echo", addr.port()))
        .body(())
        .unwrap();
    let mut stream = send_request.send_request(request).await.unwrap();
    stream
        .send_data(Bytes::from_static(b"payload"))
        .await
        .unwrap();
    stream.finish().await.unwrap();
    let response = stream.recv_response().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Vec::new();
    while let Some(chunk) = stream.recv_data().await.unwrap() {
        body.extend_from_slice(chunk.chunk());
    }
    assert_eq!(body, b"payload");

    shutdown.cancel();
}

#[tokio::test]
async fn webtransport_session_round_trips_frames_with_independent_client() {
    let (addr, shutdown) = start_server().await;

    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation()
        .build();
    let endpoint = wtransport::Endpoint::client(config).expect("wtransport client");
    let connection = endpoint
        .connect(format!("https://127.0.0.1:{}/wt/echo", addr.port()))
        .await
        .expect("WebTransport session established");

    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .expect("open bi")
        .await
        .expect("bi stream ready");
    write_frame(&mut send, b"first frame").await.unwrap();
    write_frame(&mut send, b"second frame").await.unwrap();

    let first = read_frame(&mut recv).await.unwrap().expect("echo 1");
    assert_eq!(&first[..], b"first frame");
    let second = read_frame(&mut recv).await.unwrap().expect("echo 2");
    assert_eq!(&second[..], b"second frame");

    shutdown.cancel();
}

#[tokio::test]
async fn webtransport_rejects_unknown_paths() {
    let (addr, shutdown) = start_server().await;

    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation()
        .build();
    let endpoint = wtransport::Endpoint::client(config).expect("wtransport client");
    let result = endpoint
        .connect(format!("https://127.0.0.1:{}/wt/missing", addr.port()))
        .await;
    assert!(
        result.is_err(),
        "unknown WebTransport paths must be rejected"
    );

    shutdown.cancel();
}
