use crate::streamhub::define::StreamHubEventSender;

use super::auth::AuthCallback;
use super::callbacks::StreamEventCallbacks;
use super::session::server_session;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::Error;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Grace period for existing sessions to complete after shutdown signal.
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(10);

pub struct RtmpServer {
    address: String,
    event_producer: StreamHubEventSender,
    gop_num: usize,
    auth: Option<Arc<dyn AuthCallback>>,
    shutdown_token: CancellationToken,
    shutdown_grace_period: Duration,
    per_stream_max_bytes: Option<usize>,
    callbacks: Arc<StreamEventCallbacks>,
    /// Pre-bound listener (optional). When set, the server uses this listener
    /// instead of binding internally. This allows early port conflict detection.
    listener: Option<TcpListener>,
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
            shutdown_grace_period: SHUTDOWN_GRACE_PERIOD,
            per_stream_max_bytes,
            callbacks: Arc::new(StreamEventCallbacks::default()),
            listener: None,
        }
    }

    /// Use a pre-bound TCP listener instead of binding internally.
    /// This allows the caller to detect port conflicts early before
    /// spawning the RTMP server task.
    #[must_use]
    pub fn with_listener(mut self, listener: TcpListener) -> Self {
        self.listener = Some(listener);
        self
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
    pub fn with_cancellation_token(mut self, parent: &CancellationToken) -> Self {
        self.shutdown_token = parent.child_token();
        self
    }

    /// Override the graceful shutdown waiting period before force-aborting
    /// lingering session tasks.
    #[must_use]
    pub const fn with_shutdown_grace_period(mut self, shutdown_grace_period: Duration) -> Self {
        self.shutdown_grace_period = shutdown_grace_period;
        self
    }

    /// Returns a `CancellationToken` that can be used to signal graceful shutdown.
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        let listener = if let Some(pre_bound) = self.listener.take() {
            pre_bound
        } else {
            let socket_addr: SocketAddr = self.address.parse().map_err(|e| {
                Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("invalid address '{}': {}", self.address, e),
                )
            })?;
            TcpListener::bind(&socket_addr).await?
        };

        let local_addr = listener.local_addr()?;
        let session_tracker = tokio_util::task::TaskTracker::new();
        let forced_session_shutdown = CancellationToken::new();

        tracing::info!("Rtmp server listening on tcp://{local_addr}");
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
                    let force_shutdown = forced_session_shutdown.child_token();
                    session_tracker.spawn(async move {
                        tokio::select! {
                            result = session.run() => {
                                if let Err(err) = result {
                                    tracing::info!(
                                        "session run error: session_type: {}, app_name: {}, stream_name: {}, err: {}",
                                        session.common.session_type,
                                        session.app_name,
                                        session.stream_name,
                                        err
                                    );
                                }
                            }
                            () = force_shutdown.cancelled() => {
                                match session.force_shutdown().await {
                                    Ok(()) => {
                                        tracing::warn!(
                                            "force-closing RTMP session during server shutdown: session_type: {}, app_name: {}, stream_name: {}",
                                            session.common.session_type,
                                            session.app_name,
                                            session.stream_name,
                                        );
                                    }
                                    Err(err) => {
                                        tracing::warn!(
                                            "Failed forced RTMP session teardown during server shutdown; force-closing socket anyway: session_type: {}, app_name: {}, stream_name: {}, err: {}",
                                            session.common.session_type,
                                            session.app_name,
                                            session.stream_name,
                                            err
                                        );
                                    }
                                }
                            }
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
        if tokio::time::timeout(self.shutdown_grace_period, session_tracker.wait())
            .await
            .is_err()
        {
            tracing::warn!("RTMP shutdown grace period expired, aborting lingering sessions");
            forced_session_shutdown.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(1), session_tracker.wait()).await;
        }

        tracing::info!("RTMP server shutdown complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_shutdown_force_closes_stuck_sessions_after_grace_period() {
        let (event_tx, _event_rx) = mpsc::channel(1);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let local_addr = listener.local_addr().expect("listener local addr");

        let mut server = RtmpServer::new(local_addr.to_string(), event_tx, 1, None, None)
            .with_listener(listener)
            .with_shutdown_grace_period(Duration::from_millis(50));
        let shutdown = server.shutdown_token();

        let server_handle = tokio::spawn(async move { server.run().await });

        let mut client = TcpStream::connect(local_addr)
            .await
            .expect("client should connect");
        tokio::time::sleep(Duration::from_millis(20)).await;

        shutdown.cancel();

        let run_result = tokio::time::timeout(Duration::from_secs(1), server_handle)
            .await
            .expect("RTMP server should stop within test timeout")
            .expect("RTMP server task should not panic");
        assert!(run_result.is_ok(), "RTMP server should stop cleanly");

        let mut buf = [0_u8; 1];
        match tokio::time::timeout(Duration::from_millis(200), client.read(&mut buf)).await {
            Ok(Ok(0)) => {}
            Ok(Err(err))
                if matches!(
                    err.kind(),
                    ErrorKind::ConnectionReset | ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof
                ) => {}
            Ok(Ok(_)) => panic!("client connection should be closed after shutdown"),
            Ok(Err(err)) => panic!("unexpected read error after shutdown: {err}"),
            Err(timeout_err) => {
                panic!("stuck RTMP session was not force-closed after shutdown: {timeout_err}")
            }
        }
    }
}
