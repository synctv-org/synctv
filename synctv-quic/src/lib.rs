//! HTTP/3 (QUIC) listener with WebTransport support.
//!
//! Serves an existing HTTP service (the SyncTV axum router) natively over
//! HTTP/3 on a UDP socket, next to the TCP listener. Every regular endpoint —
//! REST, SSE streams, gRPC included (request bodies stream, so
//! bidirectional RPCs work) — inherits QUIC's 0-RTT,
//! stream multiplexing, and connection migration.
//!
//! On the same socket, WebTransport sessions (extended CONNECT with the
//! `webtransport` protocol) at [`WebTransportHandler`] paths are handed to
//! the registered handler as bidirectional frame streams; the realtime
//! transport uses them exactly like a WebSocket.
//!
//! This crate is transport-only: no business logic, no protocol types.

#![allow(clippy::module_name_repetitions)]

mod bridge;
mod frame_budget;
mod frames;
mod server;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
mod tls;
mod webtransport;

pub use bridge::{serve_request, H3RecvBody};
pub use frame_budget::{BudgetedConnection, MAX_CONTROL_FRAME_LEN};
pub use frames::{read_frame, write_frame, MAX_FRAME_BYTES, MAX_OUTBOUND_FRAME_BYTES};
pub use server::{serve_quic, QuicServerConfig};

/// Direct peer address of a QUIC connection, attached to every request the
/// HTTP/3 listener serves (plain requests, WebTransport CONNECTs, and
/// requests riding an established session) so peer-derived security controls
/// bind to the real peer — the HTTP/3 equivalent of the TCP path's
/// `ConnectInfo<SocketAddr>`.
#[derive(Clone, Copy, Debug)]
pub struct PeerAddress(pub std::net::SocketAddr);
#[cfg(any(test, feature = "test-support"))]
pub use tls::generate_self_signed_config;
pub use tls::{
    certificate_hash_base64, crypto_provider, load_tls_server_config, quinn_server_config,
    rustls_config_with_alpn,
};
pub use webtransport::{
    is_webtransport_connect, share_session, WebTransportHandler, WtBidiStream, WtSendStream,
    WtSession, WtSessionDriver, WtStreamOpener, WEBTRANSPORT_PROTOCOL,
};

// Re-exported so downstream crates can name stream traits without depending
// on the h3 stack directly.
pub use h3;
pub use h3_quinn;

/// ALPN protocol identifier for HTTP/3.
pub const H3_ALPN: &[&str] = &["h3"];
