//! Regression (handshake-error-terminates-quic-listener): a single failed or
//! hostile QUIC handshake must terminate only that connection — the accept
//! loop keeps serving valid clients afterwards.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::Full;
use synctv_quic::testing::insecure_client_config;
use synctv_quic::{generate_self_signed_config, serve_quic, QuicServerConfig};
use tokio_util::sync::CancellationToken;
use tower::service_fn;

async fn start_server() -> (std::net::SocketAddr, CancellationToken) {
    let (rustls, _der) = generate_self_signed_config();
    let service = service_fn(|_request: Request<synctv_quic::H3RecvBody>| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from_static(b"ok")))
                .expect("static response body builds"),
        )
    });
    let shutdown = CancellationToken::new();
    let addr = serve_quic(
        "127.0.0.1:0".parse().expect("loopback bind address parses"),
        QuicServerConfig {
            rustls,
            max_webtransport_sessions: 8,
        },
        service,
        |body| body,
        None,
        shutdown.clone(),
    )
    .await
    .expect("quic server binds");
    (addr, shutdown)
}

async fn connect_h3(addr: std::net::SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(insecure_client_config())?,
    ));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    endpoint.set_default_client_config(client_config);
    let connection = endpoint.connect(addr, "localhost")?.await?;
    let (_driver, _send_request) = h3::client::new(h3_quinn::Connection::new(connection)).await?;
    Ok(())
}

#[tokio::test]
async fn failed_handshake_does_not_stop_the_accept_loop() -> Result<(), Box<dyn std::error::Error>>
{
    let (addr, shutdown) = start_server().await;

    // Control: a valid h3 handshake succeeds while the loop is healthy.
    tokio::time::timeout(Duration::from_secs(10), connect_h3(addr))
        .await
        .expect("control connect completes")
        .expect("control connect succeeds");

    // Induce exactly one server-side handshake failure: an ALPN the h3-only
    // rustls config rejects.
    let mut bad = insecure_client_config();
    bad.alpn_protocols = vec![b"synctv-test-bad-alpn".to_vec()];
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(bad)?,
    ));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    endpoint.set_default_client_config(client_config);
    let probe = tokio::time::timeout(
        Duration::from_secs(10),
        endpoint.connect(addr, "localhost")?,
    )
    .await
    .expect("probe connect completes");
    assert!(probe.is_err(), "probe handshake must fail client-side too");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Decisive: a valid h3 handshake to the same address afterwards.
    tokio::time::timeout(Duration::from_secs(10), connect_h3(addr))
        .await
        .expect("listener stays alive after a failed handshake")
        .expect("second valid connect succeeds");

    shutdown.cancel();
    Ok(())
}
