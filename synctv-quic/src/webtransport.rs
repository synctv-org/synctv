//! WebTransport session handling over the h3 listener.

use std::sync::Arc;

use bytes::Bytes;

/// The extended-CONNECT protocol identifier for WebTransport (RFC 9297
/// family; see the WebTransport-over-HTTP/3 draft).
pub const WEBTRANSPORT_PROTOCOL: &str = "webtransport";

/// Bidirectional WebTransport stream as driven by the realtime transport.
pub type WtBidiStream =
    h3_webtransport::stream::BidiStream<crate::frame_budget::BudgetedBidiStream<Bytes>, Bytes>;

/// Server-initiated unidirectional WebTransport stream, used for
/// per-observation push lanes.
pub type WtSendStream = h3_webtransport::stream::SendStream<
    crate::frame_budget::BudgetedSendStream<h3_quinn::SendStream<Bytes>>,
    Bytes,
>;

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

/// Handle for opening server-initiated push streams inside an established
/// WebTransport session. Cheap to clone; shared with the session driver.
#[derive(Clone)]
pub struct WtStreamOpener {
    session: Arc<WtSession>,
}

impl WtStreamOpener {
    /// Opens one server-initiated unidirectional stream. `None` when the
    /// session can no longer accept streams; the caller decides whether to
    /// fall back to the control stream or end the session.
    pub async fn open_uni(&self) -> Option<WtSendStream> {
        let id = self.session.session_id();
        self.session.open_uni(id).await.ok()
    }
}

/// An established WebTransport session handed to the application handler:
/// the client's first bidirectional stream plus the ability to open
/// server-initiated unidirectional push streams.
pub struct WtSessionDriver {
    session: Arc<WtSession>,
    /// The client's first bidirectional stream: the control channel and the
    /// only uplink.
    pub control_stream: WtBidiStream,
}

impl WtSessionDriver {
    #[must_use]
    pub fn opener(&self) -> WtStreamOpener {
        WtStreamOpener {
            session: Arc::clone(&self.session),
        }
    }
}

pub type WtSession =
    h3_webtransport::server::WebTransportSession<crate::frame_budget::BudgetedConnection, Bytes>;

/// Wraps a freshly accepted session so the connection driver and the
/// application handler can share it.
#[must_use]
pub fn share_session(session: Arc<WtSession>, control_stream: WtBidiStream) -> WtSessionDriver {
    WtSessionDriver {
        session,
        control_stream,
    }
}

/// Application hook for WebTransport sessions.
///
/// Implemented by the realtime transport: `accept` performs admission
/// (auth, room checks, connection limits) before the session is established,
/// and `run_session` drives the session starting from the client's first
/// bidirectional stream (length-prefixed protobuf frames, mirroring the
/// WebSocket transport). The session ends when this future returns or the
/// underlying connection closes.
#[async_trait::async_trait]
pub trait WebTransportHandler: Send + Sync + 'static {
    /// Admission for the CONNECT request. `Err(status)` rejects the session
    /// with that HTTP status.
    async fn accept(&self, request: &http::Request<()>) -> Result<(), http::StatusCode>;

    /// Drives an accepted session over its first bidirectional stream and
    /// any server-initiated push streams the handler opens.
    async fn run_session(&self, request: http::Request<()>, session: WtSessionDriver);
}

/// Erased alias used by the connection driver.
pub type SharedWebTransportHandler = Arc<dyn WebTransportHandler>;
