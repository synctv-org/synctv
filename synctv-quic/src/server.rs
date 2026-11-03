//! QUIC endpoint: HTTP/3 serving + WebTransport sessions on one socket.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http::Request;
use http_body::Body as HttpBody;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tower::Service;

use crate::bridge::{serve_request, H3RecvBody};
use crate::webtransport::{is_webtransport_connect, SharedWebTransportHandler};

/// Bound on the unauthenticated establishment phase: after the WebTransport
/// handshake the client must open its control stream (which spawns
/// `run_session` and validates the credential) within this window or the
/// session - and its admission permit - is dropped. A server-side timer, so
/// QUIC keepalive (PING) traffic cannot extend it.
const WT_ESTABLISHMENT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Configuration for the QUIC listener.
pub struct QuicServerConfig {
    /// TLS configuration with the `h3` ALPN.
    pub rustls: rustls::ServerConfig,
    /// Maximum concurrent WebTransport sessions across the whole listener.
    /// Only effective when a handler is installed.
    pub max_webtransport_sessions: u64,
}

/// Binds a QUIC endpoint and serves `service` over HTTP/3 until `shutdown`
/// fires. Returns the bound address (useful when binding port 0 in tests).
///
/// `make_body` wraps each request's streaming [`H3RecvBody`] into the
/// service's request body type; the bridge stays axum-agnostic while callers
/// keep control over buffering policy.
///
/// When `webtransport` is installed, extended-CONNECT WebTransport sessions
/// are handed to it on the same socket; every other request — including SSE
/// and gRPC — flows through the service unchanged.
///
/// # Errors
///
/// Fails when the UDP socket cannot be bound or QUIC rejects the TLS config.
pub async fn serve_quic<S, ReqBody, ResBody, F>(
    bind_addr: SocketAddr,
    config: QuicServerConfig,
    service: S,
    make_body: F,
    webtransport: Option<SharedWebTransportHandler>,
    shutdown: CancellationToken,
) -> anyhow::Result<SocketAddr>
where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: std::fmt::Display + Send,
    F: FnOnce(H3RecvBody) -> ReqBody + Clone + Send + 'static,
    ReqBody: Send + 'static,
    ResBody: HttpBody<Data = Bytes> + Send + Unpin + 'static,
    ResBody::Error: std::fmt::Display + Send,
{
    let quinn_config = crate::tls::quinn_server_config(config.rustls);
    let endpoint = quinn::Endpoint::server(quinn_config, bind_addr)
        .map_err(|error| anyhow::anyhow!("failed to bind QUIC endpoint {bind_addr}: {error}"))?;
    let bound = endpoint.local_addr()?;

    // Listener-wide admission cap: the per-connection h3 setting bounds a
    // single client's sessions; this bounds their sum so many connections
    // cannot exhaust server memory together.
    let wt_session_slots = Arc::new(Semaphore::new(
        usize::try_from(config.max_webtransport_sessions).unwrap_or(usize::MAX),
    ));
    let shutdown_endpoint = endpoint.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = shutdown.cancelled() => {
                shutdown_endpoint.close(0u32.into(), b"server shutting down");
            }
            result = accept_loop(endpoint, service, make_body, webtransport, wt_session_slots) => {
                if let Err(error) = result {
                    tracing::error!(error = %error, "QUIC accept loop failed");
                }
            }
        }
    });

    Ok(bound)
}

async fn accept_loop<S, ReqBody, ResBody, F>(
    endpoint: quinn::Endpoint,
    service: S,
    make_body: F,
    webtransport: Option<SharedWebTransportHandler>,
    wt_session_slots: Arc<Semaphore>,
) -> anyhow::Result<()>
where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: std::fmt::Display + Send,
    F: FnOnce(H3RecvBody) -> ReqBody + Clone + Send + 'static,
    ReqBody: Send + 'static,
    ResBody: HttpBody<Data = Bytes> + Send + Unpin + 'static,
    ResBody::Error: std::fmt::Display + Send,
{
    while let Some(incoming) = endpoint.accept().await {
        // The handshake runs in its own task: a slow or hostile peer that
        // parks the handshake must not stall admission for every other
        // client — the accept loop immediately goes back to the socket.
        // A failed handshake still terminates only that connection.
        let service = service.clone();
        let make_body = make_body.clone();
        let webtransport = webtransport.clone();
        let wt_session_slots = Arc::clone(&wt_session_slots);
        tokio::spawn(async move {
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::warn!(error = %error, "QUIC handshake failed; continuing to accept");
                    return;
                }
            };
            drive_connection(
                connection,
                service,
                make_body,
                webtransport,
                wt_session_slots,
            )
            .await;
        });
    }
    Ok(())
}

/// Drives one QUIC connection: plain HTTP/3 requests go through the service;
/// the first WebTransport CONNECT converts the connection into
/// session-driven mode (further HTTP requests still arrive via
/// `accept_bi` and keep flowing through the service).
async fn drive_connection<S, ReqBody, ResBody, F>(
    connection: quinn::Connection,
    service: S,
    make_body: F,
    webtransport: Option<SharedWebTransportHandler>,
    wt_session_slots: Arc<Semaphore>,
) where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: std::fmt::Display + Send,
    F: FnOnce(H3RecvBody) -> ReqBody + Clone + Send + 'static,
    ReqBody: Send + 'static,
    ResBody: HttpBody<Data = Bytes> + Send + Unpin + 'static,
    ResBody::Error: std::fmt::Display + Send,
{
    let mut builder = h3::server::builder();
    builder
        .enable_webtransport(webtransport.is_some())
        .enable_extended_connect(webtransport.is_some())
        .enable_datagram(webtransport.is_some())
        // A connection converts on its first WebTransport CONNECT (below),
        // so it hosts at most one session; the listener-wide semaphore is
        // the real concurrency bound.
        .max_webtransport_sessions(1);
    // The QUIC connection is consumed by the h3 layer below, so capture the
    // direct peer address now; every request this connection serves carries
    // it so peer-derived security controls (per-IP rate limiting, brute-force
    // lockout, audit attribution) bind to the real peer, like the TCP path's
    // `ConnectInfo`.
    let peer = crate::PeerAddress(connection.remote_address());
    // Wrap every inbound stream with the pre-decode frame budget: h3 0.0.8
    // buffers a non-DATA frame to its declared length before any size
    // check, so the adapter rejects oversized declarations (HEADERS,
    // SETTINGS, ...) at header parse. max_field_section_size additionally
    // bounds the decoded header size and advertises the limit to peers.
    builder.max_field_section_size(crate::frame_budget::MAX_CONTROL_FRAME_LEN as u64);
    let mut h3_conn = match builder
        .build(crate::frame_budget::BudgetedConnection::new(
            h3_quinn::Connection::new(connection),
        ))
        .await
    {
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
        let (mut request, stream) = match resolver.resolve_request().await {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::debug!(error = %error, "HTTP/3 request resolution failed");
                return;
            }
        };
        normalize_authority_host(&mut request);
        request.extensions_mut().insert(peer);

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
                // Listener-wide admission: the per-connection h3 setting
                // cannot bound the sum across connections. Reject with 503
                // instead of queueing — clients retry transport negotiation.
                let Ok(permit) = Arc::clone(&wt_session_slots).try_acquire_owned() else {
                    tracing::warn!(
                        "WebTransport session rejected: listener at max_webtransport_sessions"
                    );
                    let mut stream = stream;
                    let response = http::Response::builder()
                        .status(http::StatusCode::SERVICE_UNAVAILABLE)
                        .body(())
                        .expect("status-only response builds");
                    if let Err(error) = stream.send_response(response).await {
                        tracing::debug!(error = %error, "WebTransport overload response failed");
                    }
                    if let Err(error) = stream.finish().await {
                        tracing::debug!(error = %error, "WebTransport overload finish failed");
                    }
                    continue;
                };
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
                            make_body,
                            Arc::clone(handler),
                            permit,
                            peer,
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
        let make_body = make_body.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_request(stream, request, make_body, &mut service).await {
                tracing::debug!(error = %error, "HTTP/3 request failed");
            }
        });
    }
}

type WtSession =
    h3_webtransport::server::WebTransportSession<crate::frame_budget::BudgetedConnection, Bytes>;

/// HTTP/3 carries the request target in the `:authority` pseudo-header; the
/// h3 crate surfaces it as the request URI's authority, not as a `Host`
/// header. Origin checks and CORS middleware read `Host`, so synthesize it
/// from the authority exactly like an HTTP/2 server stack would.
fn normalize_authority_host<B>(request: &mut http::Request<B>) {
    if request.headers().contains_key(http::header::HOST) {
        return;
    }
    let Some(authority) = request.uri().authority().cloned() else {
        return;
    };
    if let Ok(value) = http::HeaderValue::from_str(authority.as_str()) {
        request.headers_mut().insert(http::header::HOST, value);
    }
}

/// Serves an established WebTransport session: the first bidirectional
/// stream the client opens becomes the control channel for the realtime
/// transport (which may open server-initiated unidirectional push streams);
/// any further HTTP requests on the connection keep going through the
/// service. Client-initiated unidirectional streams are drained defensively
/// so a confused or hostile client cannot wedge stream flow control.
///
/// `permit` is the listener-wide admission slot; it releases when the
/// session ends (this function returns).
async fn drive_webtransport_session<S, ReqBody, ResBody, F>(
    session: WtSession,
    request: Request<()>,
    service: S,
    make_body: F,
    handler: SharedWebTransportHandler,
    // The listener-wide admission slot; the underscore-prefixed binding
    // still lives to end of scope, releasing only when the session ends.
    _permit: OwnedSemaphorePermit,
    peer: crate::PeerAddress,
) where
    S: Service<Request<ReqBody>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: std::fmt::Display + Send,
    F: FnOnce(H3RecvBody) -> ReqBody + Clone + Send + 'static,
    ReqBody: Send + 'static,
    ResBody: HttpBody<Data = Bytes> + Send + Unpin + 'static,
    ResBody::Error: std::fmt::Display + Send,
{
    // Admission already ran before the session handshake (see the accept
    // loop); the CONNECT request is passed through for the session driver.
    let _ = &request;
    let session = Arc::new(session);
    let mut frame_stream_taken = false;
    // Fires when the realtime handler's task finishes: the session must then
    // close instead of lingering established-but-dead.
    let (session_done_tx, session_done) = tokio::sync::oneshot::channel::<()>();
    // The sender moves into the (at most once) session task; the receiver
    // is borrowed by the select each iteration — both need the Option
    // wrapper to satisfy the loop's move check.
    let mut session_done_tx = Some(session_done_tx);
    let mut session_done = Some(session_done);
    let establishment_deadline = tokio::time::sleep(WT_ESTABLISHMENT_DEADLINE);
    tokio::pin!(establishment_deadline);
    loop {
        tokio::select! {
            () = &mut establishment_deadline, if !frame_stream_taken => {
                tracing::info!(
                    "WebTransport session closed: no control stream before the establishment deadline"
                );
                // Drops the permit and the session handles, closing the
                // connection: a permit holder must authenticate within the
                // window or evict itself.
                return;
            }
            accepted = session.accept_bi() => {
                match accepted {
                    Ok(Some(h3_webtransport::server::AcceptedBi::BidiStream(_session_id, stream))) => {
                        if frame_stream_taken {
                            // The realtime protocol uses exactly one control
                            // stream; surplus bidis are dropped (resetting them).
                            continue;
                        }
                        frame_stream_taken = true;
                        let driver = crate::webtransport::share_session(Arc::clone(&session), stream);
                        let handler = Arc::clone(&handler);
                        let request = request.clone();
                        let done_tx = session_done_tx.take();
                        tokio::spawn(async move {
                            handler.run_session(request, driver).await;
                            if let Some(done_tx) = done_tx {
                                let _ = done_tx.send(());
                            }
                        });
                    }
                    Ok(Some(h3_webtransport::server::AcceptedBi::Request(mut request, stream))) => {
                        // Requests arriving on an established WebTransport
                        // session are rebuilt from the h3 parts, so the peer
                        // address must be re-attached for them as well.
                        request.extensions_mut().insert(peer);
                        let mut service = service.clone();
                        let make_body = make_body.clone();
                        tokio::spawn(async move {
                            if let Err(error) = serve_request(stream, request, make_body, &mut service).await {
                                tracing::debug!(error = %error, "HTTP/3 request failed");
                            }
                        });
                    }
                    Ok(None) | Err(_) => return,
                }
            }
            accepted_uni = session.accept_uni() => {
                match accepted_uni {
                    // Drained and dropped: the realtime protocol's uplink is
                    // the control stream, so client uni streams carry nothing.
                    Ok(Some(_stream)) => {}
                    Ok(None) | Err(_) => return,
                }
            }
            // The realtime handler finished (session ended or failed).
            // Without this the WebTransport session would linger
            // established-but-dead — holding its admission slot until the
            // client eventually disconnects. Returning drops the remaining
            // connection handles; the realtime writer's post-cancellation
            // drain is separately bounded (WT_DRAIN_GRACE in
            // synctv-api-http), after which the QUIC connection closes.
            _session_done = async {
                match session_done.as_mut() {
                    Some(done) => {
                        let _ = done.await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {
                tracing::debug!("WebTransport realtime session handler finished; closing session");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_authority_host;

    #[test]
    fn authority_becomes_host_header_when_missing() {
        let uri = "https://example.test:8443/api/public/server-info"
            .parse::<http::Uri>()
            .expect("uri parses");
        let mut request = http::Request::builder()
            .method(http::Method::GET)
            .uri(uri)
            .body(())
            .expect("request builds");
        normalize_authority_host(&mut request);
        assert_eq!(
            request
                .headers()
                .get(http::header::HOST)
                .expect("host synthesized"),
            "example.test:8443"
        );
    }

    #[test]
    fn explicit_host_header_is_preserved() {
        let mut request = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://example.test/")
            .header("host", "kept.example.test")
            .body(())
            .expect("request builds");
        normalize_authority_host(&mut request);
        assert_eq!(
            request
                .headers()
                .get(http::header::HOST)
                .expect("host kept"),
            "kept.example.test"
        );
    }
}
