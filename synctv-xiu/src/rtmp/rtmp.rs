use crate::streamhub::define::StreamHubEventSender;

use super::auth::AuthCallback;
use super::callbacks::StreamEventCallbacks;
use super::session::server_session;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::Error;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Grace period for existing sessions to complete after shutdown signal.
const SHUTDOWN_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_secs(10);

pub struct RtmpServer {
    address: String,
    event_producer: StreamHubEventSender,
    gop_num: usize,
    auth: Option<Arc<dyn AuthCallback>>,
    shutdown_token: CancellationToken,
    per_stream_max_bytes: Option<usize>,
    callbacks: Arc<StreamEventCallbacks>,
}

impl RtmpServer {
    #[must_use]
    pub fn new(
        address: String,
        event_producer: StreamHubEventSender,
        gop_num: usize,
        auth: Option<Arc<dyn AuthCallback>>,
        per_stream_max_bytes: Option<usize>,
    ) -> Self {
        Self {
            address,
            event_producer,
            gop_num,
            auth,
            shutdown_token: CancellationToken::new(),
            per_stream_max_bytes,
            callbacks: Arc::new(StreamEventCallbacks::default()),
        }
    }

    /// Set stream event callbacks (for metrics, logging, etc.)
    #[must_use]
    pub fn with_callbacks(mut self, callbacks: StreamEventCallbacks) -> Self {
        self.callbacks = Arc::new(callbacks);
        self
    }

    /// Set an external cancellation token. The server's internal shutdown token
    /// becomes a child of `parent`, so cancelling `parent` will also shut down
    /// this RTMP server and all its sessions.
    #[must_use]
    pub fn with_cancellation_token(mut self, parent: CancellationToken) -> Self {
        self.shutdown_token = parent.child_token();
        self
    }

    /// Returns a `CancellationToken` that can be used to signal graceful shutdown.
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        let socket_addr: SocketAddr = self.address.parse().map_err(|e| {
            Error::new(std::io::ErrorKind::InvalidInput, format!("invalid address '{}': {}", self.address, e))
        })?;
        let listener = TcpListener::bind(&socket_addr).await?;
        let session_tracker = tokio_util::task::TaskTracker::new();

        tracing::info!("Rtmp server listening on tcp://{socket_addr}");
        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    let (tcp_stream, _remote_addr) = accept_result?;

                    let mut session = server_session::ServerSession::new(
                        tcp_stream,
                        self.event_producer.clone(),
                        self.gop_num,
                        self.auth.clone(),
                        self.per_stream_max_bytes,
                        Arc::clone(&self.callbacks),
                    );
                    session_tracker.spawn(async move {
                        if let Err(err) = session.run().await {
                            tracing::info!(
                                "session run error: session_type: {}, app_name: {}, stream_name: {}, err: {}",
                                session.common.session_type,
                                session.app_name,
                                session.stream_name,
                                err
                            );
                        }
                    });
                }
                () = self.shutdown_token.cancelled() => {
                    tracing::info!("RTMP server shutting down gracefully");
                    break;
                }
            }
        }

        // Stop accepting new connections; wait for existing sessions with timeout
        session_tracker.close();
        if tokio::time::timeout(SHUTDOWN_GRACE_PERIOD, session_tracker.wait()).await.is_err() {
            tracing::warn!("RTMP shutdown grace period expired, some sessions still active");
        }

        tracing::info!("RTMP server shutdown complete");
        Ok(())
    }
}
