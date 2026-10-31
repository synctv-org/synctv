//! QUIC endpoint: HTTP/3 serving + WebTransport sessions on one socket.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http::Request;
use http_body::Body as HttpBody;
use tokio_util::sync::CancellationToken;
use tower::Service;

use crate::bridge::serve_request;
use crate::webtransport::{is_webtransport_connect, SharedWebTransportHandler};

/// Configuration for the QUIC listener.
pub struct QuicServerConfig {
    /// TLS configuration with the `h3` ALPN.
    pub rustls: rustls::ServerConfig,
    /// Maximum concurrent WebTransport sessions per endpoint. Only effective
    /// when a handler is installed.
    pub max_webtransport_sessions: u64,
}

/// Binds a QUIC endpoint and serves `service` over HTTP/3 until `shutdown`
/// fires. Returns the bound address (useful when binding port 0 in tests).
///
/// When `webtransport` is installed, extended-CONNECT WebTransport sessions
/// are handed to it on the same socket; every other request — including SSE
/// and, best-effort, gRPC — flows through the service unchanged.
///
/// # Errors
///
/// Fails when the UDP socket cannot be bound or QUIC rejects the TLS config.
pub async fn serve_quic<S, ReqBody, ResBody>(
    bind_addr: SocketAddr,
    config: QuicServerConfig,
    service: S,
    webtransport: Option<SharedWebTransportHandler>,
    shutdown: CancellationToken,
) -> anyhow::Result<SocketAddr>
where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: std::fmt::Display + Send,
    ReqBody: From<Bytes> + Send + 'static,
    ResBody: HttpBody<Data = Bytes> + Send + Unpin + 'static,
    ResBody::Error: std::fmt::Display + Send,
{
    let quinn_config = crate::tls::quinn_server_config(config.rustls);
    let endpoint = quinn::Endpoint::server(quinn_config, bind_addr)
        .map_err(|error| anyhow::anyhow!("failed to bind QUIC endpoint {bind_addr}: {error}"))?;
    let bound = endpoint.local_addr()?;

    let max_sessions = config.max_webtransport_sessions;
    let shutdown_endpoint = endpoint.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = shutdown.cancelled() => {
                shutdown_endpoint.close(0u32.into(), b"server shutting down");
            }
            result = accept_loop(endpoint, service, webtransport, max_sessions) => {
                if let Err(error) = result {
                    tracing::error!(error = %error, "QUIC accept loop failed");
                }
            }
        }
    });

    Ok(bound)
}

async fn accept_loop<S, ReqBody, ResBody>(
    endpoint: quinn::Endpoint,
    service: S,
    webtransport: Option<SharedWebTransportHandler>,
    max_webtransport_sessions: u64,
) -> anyhow::Result<()>
where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: std::fmt::Display + Send,
    ReqBody: From<Bytes> + Send + 'static,
    ResBody: HttpBody<Data = Bytes> + Send + Unpin + 'static,
    ResBody::Error: std::fmt::Display + Send,
{
    while let Some(incoming) = endpoint.accept().await {
        let connection = incoming.await?;
        let service = service.clone();
        let webtransport = webtransport.clone();
        tokio::spawn(async move {
            drive_connection(connection, service, webtransport, max_webtransport_sessions).await;
        });
    }
    Ok(())
}

/// Drives one QUIC connection: plain HTTP/3 requests go through the service;
/// the first WebTransport CONNECT converts the connection into
/// session-driven mode (further HTTP requests still arrive via
/// `accept_bi` and keep flowing through the service).
async fn drive_connection<S, ReqBody, ResBody>(
    connection: quinn::Connection,
    service: S,
    webtransport: Option<SharedWebTransportHandler>,
    max_webtransport_sessions: u64,
) where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: std::fmt::Display + Send,
    ReqBody: From<Bytes> + Send + 'static,
    ResBody: HttpBody<Data = Bytes> + Send + Unpin + 'static,
    ResBody::Error: std::fmt::Display + Send,
{
    let mut builder = h3::server::builder();
    builder
        .enable_webtransport(webtransport.is_some())
        .enable_extended_connect(webtransport.is_some())
        .enable_datagram(webtransport.is_some())
        .max_webtransport_sessions(max_webtransport_sessions);
    let mut h3_conn = match builder.build(h3_quinn::Connection::new(connection)).await {
        Ok(connection) => connection,
        Err(error) => {
            tracing::debug!(error = %error, "HTTP/3 handshake failed");
            return;
        }
    };

    loop {
        let resolver = match h3_conn.accept().await {
            Ok(Some(resolver)) => resolver,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(error = %error, "HTTP/3 connection closed");
                return;
            }
        };
        let (request, stream) = match resolver.resolve_request().await {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::debug!(error = %error, "HTTP/3 request resolution failed");
                return;
            }
        };

        if let Some(handler) = &webtransport {
            if is_webtransport_connect(&request) {
                // Admission first: a rejected CONNECT answers with the
                // handler's status and the connection keeps serving.
                if let Err(status) = handler.accept(&request).await {
                    tracing::info!(status = %status, "WebTransport session rejected");
                    let mut stream = stream;
                    let response = http::Response::builder()
                        .status(status)
                        .body(())
                        .expect("status-only response builds");
                    if let Err(error) = stream.send_response(response).await {
                        tracing::debug!(error = %error, "WebTransport reject response failed");
                    }
                    if let Err(error) = stream.finish().await {
                        tracing::debug!(error = %error, "WebTransport reject finish failed");
                    }
                    continue;
                }
                let request_for_session = request;
                match h3_webtransport::server::WebTransportSession::accept(
                    request_for_session.clone(),
                    stream,
                    h3_conn,
                )
                .await
                {
                    Ok(session) => {
                        // The session consumed the h3 connection.
                        drive_webtransport_session(
                            session,
                            request_for_session,
                            service,
                            Arc::clone(handler),
                        )
                        .await;
                        return;
                    }
                    Err(error) => {
                        tracing::debug!(error = ?error, "WebTransport handshake failed");
                        return;
                    }
                }
            }
        }

        if request.method() == http::Method::CONNECT {
            // Plain CONNECT tunnels are not served; answer instead of
            // waiting on a body that never ends.
            let mut stream = stream;
            let response = http::Response::builder()
                .status(http::StatusCode::NOT_IMPLEMENTED)
                .body(())
                .expect("status-only response builds");
            let _ = stream.send_response(response).await;
            let _ = stream.finish().await;
            continue;
        }

        let mut service = service.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_request(stream, request, &mut service).await {
                tracing::debug!(error = %error, "HTTP/3 request failed");
            }
        });
    }
}

type WtSession = h3_webtransport::server::WebTransportSession<h3_quinn::Connection, Bytes>;

/// Serves an established WebTransport session: the first bidirectional
/// stream the client opens becomes the frame channel for the realtime
/// transport; any further HTTP requests on the connection keep going through
/// the service.
async fn drive_webtransport_session<S, ReqBody, ResBody>(
    session: WtSession,
    request: Request<()>,
    service: S,
    handler: SharedWebTransportHandler,
) where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: std::fmt::Display + Send,
    ReqBody: From<Bytes> + Send + 'static,
    ResBody: HttpBody<Data = Bytes> + Send + Unpin + 'static,
    ResBody::Error: std::fmt::Display + Send,
{
    // Admission already ran before the session handshake (see the accept
    // loop); the CONNECT request is passed through for the session driver.
    let _ = &request;
    let mut frame_stream_taken = false;
    loop {
        match session.accept_bi().await {
            Ok(Some(h3_webtransport::server::AcceptedBi::BidiStream(_session_id, stream))) => {
                if frame_stream_taken {
                    // The realtime protocol uses exactly one frame channel;
                    // surplus streams are dropped (resetting them).
                    continue;
                }
                frame_stream_taken = true;
                let handler = Arc::clone(&handler);
                let request = request.clone();
                tokio::spawn(async move {
                    handler.run_session(request, stream).await;
                });
            }
            Ok(Some(h3_webtransport::server::AcceptedBi::Request(request, stream))) => {
                let mut service = service.clone();
                tokio::spawn(async move {
                    if let Err(error) = serve_request(stream, request, &mut service).await {
                        tracing::debug!(error = %error, "HTTP/3 request failed");
                    }
                });
            }
            Ok(None) | Err(_) => return,
        }
    }
}
