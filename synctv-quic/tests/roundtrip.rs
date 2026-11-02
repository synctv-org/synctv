//! Interop round trips for the QUIC listener: plain HTTP/3 through an h3
//! client, and WebTransport through the independent `wtransport` client (the
//! implementation browsers are validated against).

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use synctv_quic::testing::insecure_client_config;
use synctv_quic::{
    generate_self_signed_config, read_frame, serve_quic, write_frame, H3RecvBody, QuicServerConfig,
    WebTransportHandler, WtSessionDriver,
};
use tokio_util::sync::CancellationToken;
use tower::service_fn;

type TestError = Box<dyn std::error::Error + Send + Sync>;

/// Echoes every frame back on the control stream; `/wt/push` additionally
/// mirrors every frame onto a server-initiated unidirectional stream.
struct EchoHandler;

#[async_trait::async_trait]
impl WebTransportHandler for EchoHandler {
    async fn accept(&self, request: &Request<()>) -> Result<(), StatusCode> {
        match request.uri().path() {
            "/wt/echo" | "/wt/push" | "/wt/done" => Ok(()),
            _ => Err(StatusCode::NOT_FOUND),
        }
    }

    async fn run_session(&self, request: Request<()>, session: WtSessionDriver) {
        if request.uri().path() == "/wt/done" {
            // Returns immediately: the listener must close the session once
            // the handler is finished (see
            // `webtransport_session_closes_when_the_handler_finishes`).
            return;
        }
        let push = request.uri().path() == "/wt/push";
        let opener = session.opener();
        let (mut send, mut recv) = h3::quic::BidiStream::split(session.control_stream);
        let mut push_stream = if push { opener.open_uni().await } else { None };
        while let Ok(Some(frame)) = read_frame(&mut recv).await {
            if write_frame(&mut send, &frame).await.is_err() {
                break;
            }
            if let Some(stream) = push_stream.as_mut() {
                if write_frame(stream, &frame).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn start_server() -> (std::net::SocketAddr, CancellationToken) {
    start_server_with_sessions(8).await
}

async fn start_server_with_sessions(
    max_webtransport_sessions: u64,
) -> (std::net::SocketAddr, CancellationToken) {
    let (rustls, _der) = generate_self_signed_config();
    let service = service_fn(|request: Request<H3RecvBody>| async move {
        let path = request.uri().path().to_string();
        let body = request.into_body();
        // `/bidi` replies before reading the request body: a buffered bridge
        // would deadlock here, waiting for an upload that never finishes
        // while the client waits for the response.
        if path == "/bidi" {
            drop(body);
            return Ok::<_, Infallible>(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(Bytes::from_static(b"early response")))
                    .expect("static response body builds"),
            );
        }
        let body = body
            .collect()
            .await
            .expect("request body collects")
            .to_bytes();
        let response = match path.as_str() {
            "/hello" => Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/plain")
                .body(Full::new(Bytes::from_static(b"hello over h3")))
                .expect("response builds"),
            "/echo" => Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(body))
                .expect("response builds"),
            _ => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()))
                .expect("response builds"),
        };
        Ok(response)
    });
    let shutdown = CancellationToken::new();
    let addr = serve_quic(
        "127.0.0.1:0".parse().expect("loopback bind address parses"),
        QuicServerConfig {
            rustls,
            max_webtransport_sessions,
        },
        service,
        |body| body,
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
        quinn::crypto::rustls::QuicClientConfig::try_from(insecure_client_config())
            .expect("client config builds"),
    ));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().expect("bind parses"))
        .expect("client endpoint binds");
    endpoint.set_default_client_config(client_config);
    let connection = endpoint
        .connect(addr, "localhost")
        .expect("connect starts")
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
async fn serves_plain_http3_requests() -> Result<(), TestError> {
    let (addr, shutdown) = start_server().await;
    let (mut send_request, _driver) = h3_client(addr).await;

    let request = Request::builder()
        .uri(format!("https://localhost:{}/hello", addr.port()))
        .body(())?;
    let mut stream = send_request.send_request(request).await?;
    stream.finish().await?;
    let response = stream.recv_response().await?;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Vec::new();
    while let Some(chunk) = stream.recv_data().await? {
        body.extend_from_slice(chunk.chunk());
    }
    assert_eq!(body, b"hello over h3");

    // POST body round trip.
    let request = Request::builder()
        .method("POST")
        .uri(format!("https://localhost:{}/echo", addr.port()))
        .body(())?;
    let mut stream = send_request.send_request(request).await?;
    stream.send_data(Bytes::from_static(b"payload")).await?;
    stream.finish().await?;
    let response = stream.recv_response().await?;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Vec::new();
    while let Some(chunk) = stream.recv_data().await? {
        body.extend_from_slice(chunk.chunk());
    }
    assert_eq!(body, b"payload");

    shutdown.cancel();
    Ok(())
}

/// Regression (buffered-request-body-gRPC-deadlock): the service responds
/// before the client has finished uploading. With a buffered bridge the
/// request path would block forever on the upload's end-of-stream; the
/// streaming bridge must deliver the response immediately.
#[tokio::test]
async fn response_precedes_completed_request_body() -> Result<(), TestError> {
    let (addr, shutdown) = start_server().await;
    let (mut send_request, _driver) = h3_client(addr).await;

    let request = Request::builder()
        .method("POST")
        .uri(format!("https://localhost:{}/bidi", addr.port()))
        .body(())?;
    let mut stream = send_request.send_request(request).await?;
    // Receive the response WITHOUT finishing the request body.
    let response = tokio::time::timeout(Duration::from_secs(10), stream.recv_response())
        .await
        .expect("response must not wait for the upload to finish")?;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Vec::new();
    while let Some(chunk) = stream.recv_data().await? {
        body.extend_from_slice(chunk.chunk());
    }
    assert_eq!(body, b"early response");
    // Finish the still-open upload direction afterwards.
    stream.send_data(Bytes::from_static(b"late upload")).await?;
    stream.finish().await?;

    shutdown.cancel();
    Ok(())
}

#[tokio::test]
async fn webtransport_session_round_trips_frames_with_independent_client() -> Result<(), TestError>
{
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
    write_frame(&mut send, b"first frame").await?;
    write_frame(&mut send, b"second frame").await?;

    let first = read_frame(&mut recv).await?.expect("echo 1");
    assert_eq!(&first[..], b"first frame");
    let second = read_frame(&mut recv).await?.expect("echo 2");
    assert_eq!(&second[..], b"second frame");

    shutdown.cancel();
    Ok(())
}

#[tokio::test]
async fn webtransport_pushes_frames_on_unidirectional_streams() -> Result<(), TestError> {
    let (addr, shutdown) = start_server().await;

    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation()
        .build();
    let endpoint = wtransport::Endpoint::client(config).expect("wtransport client");
    let connection = endpoint
        .connect(format!("https://127.0.0.1:{}/wt/push", addr.port()))
        .await
        .expect("WebTransport session established");

    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .expect("open bi")
        .await
        .expect("bi stream ready");
    write_frame(&mut send, b"mirrored").await?;

    let echoed = read_frame(&mut recv).await?.expect("control echo");
    assert_eq!(&echoed[..], b"mirrored");

    let mut uni = connection.accept_uni().await.expect("push stream");
    let pushed = read_frame(&mut uni).await?.expect("pushed frame");
    assert_eq!(&pushed[..], b"mirrored");

    shutdown.cancel();
    Ok(())
}

/// Regression (lingering-session-after-handler-return): a realtime handler
/// that finishes used to leave the WebTransport session established-but-dead
/// until the client disconnected; the server must close it promptly.
#[tokio::test]
async fn webtransport_session_closes_when_the_handler_finishes() -> Result<(), TestError> {
    let (addr, shutdown) = start_server().await;

    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation()
        .build();
    let endpoint = wtransport::Endpoint::client(config).expect("wtransport client");
    let connection = endpoint
        .connect(format!("https://127.0.0.1:{}/wt/done", addr.port()))
        .await
        .expect("WebTransport session established");

    // The realtime handler starts on the first bidirectional stream (the
    // protocol's control stream) and returns immediately.
    let (_send, _recv) = connection
        .open_bi()
        .await
        .expect("open bi")
        .await
        .expect("bi stream ready");

    tokio::time::timeout(Duration::from_secs(5), connection.closed())
        .await
        .expect("session must close after the handler finishes");

    shutdown.cancel();
    Ok(())
}

/// Regression (per-connection-cap-only-admission): `max_webtransport_sessions`
/// is documented as a listener-wide cap; before the admission semaphore, many
/// connections could each open sessions up to the per-connection h3 setting.
#[tokio::test]
async fn webtransport_sessions_are_capped_listener_wide() -> Result<(), TestError> {
    let (addr, shutdown) = start_server_with_sessions(1).await;

    let build_config = || {
        wtransport::ClientConfig::builder()
            .with_bind_default()
            .with_no_cert_validation()
            .build()
    };

    // First client holds the only slot with an open, idle session.
    let first_endpoint = wtransport::Endpoint::client(build_config()).expect("client 1");
    let first = first_endpoint
        .connect(format!("https://127.0.0.1:{}/wt/echo", addr.port()))
        .await
        .expect("first session admitted");

    // Second client must be rejected with 503 while the slot is held.
    let second_endpoint = wtransport::Endpoint::client(build_config()).expect("client 2");
    let second = second_endpoint
        .connect(format!("https://127.0.0.1:{}/wt/echo", addr.port()))
        .await;
    assert!(
        second.is_err(),
        "the listener cap must reject the second session"
    );

    // Releasing the first session frees the slot for a new client.
    drop(first);
    let third_endpoint = wtransport::Endpoint::client(build_config()).expect("client 3");
    let mut admitted = false;
    for _ in 0..20 {
        if third_endpoint
            .connect(format!("https://127.0.0.1:{}/wt/echo", addr.port()))
            .await
            .is_ok()
        {
            admitted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(admitted, "a released slot must admit a new session");

    shutdown.cancel();
    Ok(())
}

#[tokio::test]
async fn webtransport_rejects_unknown_paths() -> Result<(), TestError> {
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
    Ok(())
}
