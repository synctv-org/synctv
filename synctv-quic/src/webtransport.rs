//! WebTransport session handling over the h3 listener.

use std::sync::Arc;

use bytes::Bytes;

/// The extended-CONNECT protocol identifier for WebTransport (RFC 9297
/// family; see the WebTransport-over-HTTP/3 draft).
pub const WEBTRANSPORT_PROTOCOL: &str = "webtransport";

/// Bidirectional WebTransport stream as driven by the realtime transport.
pub type WtBidiStream = h3_webtransport::stream::BidiStream<h3_quinn::BidiStream<Bytes>, Bytes>;

/// Returns true when the HTTP/3 request is a WebTransport session CONNECT.
///
/// h3 surfaces the `:protocol` pseudo-header as the [`h3::ext::Protocol`]
/// request extension (extended CONNECT, RFC 9220 §3), so that is the
/// authoritative check.
#[must_use]
pub fn is_webtransport_connect(request: &http::Request<()>) -> bool {
    request.method() == http::Method::CONNECT
        && request
            .extensions()
            .get::<h3::ext::Protocol>()
            .is_some_and(|protocol| *protocol == h3::ext::Protocol::WEB_TRANSPORT)
}

/// Application hook for WebTransport sessions.
///
/// Implemented by the realtime transport: `accept` performs admission
/// (auth, room checks, connection limits) before the session is established,
/// and `run_session` drives the first bidirectional stream the client opens
/// as the frame channel (length-prefixed protobuf messages, mirroring the
/// WebSocket transport).
#[async_trait::async_trait]
pub trait WebTransportHandler: Send + Sync + 'static {
    /// Admission for the CONNECT request. `Err(status)` rejects the session
    /// with that HTTP status.
    async fn accept(&self, request: &http::Request<()>) -> Result<(), http::StatusCode>;

    /// Drives an accepted session over its first bidirectional stream. The
    /// session ends when this future returns or the stream closes.
    async fn run_session(&self, request: http::Request<()>, stream: WtBidiStream);
}

/// Erased alias used by the connection driver.
pub type SharedWebTransportHandler = Arc<dyn WebTransportHandler>;
