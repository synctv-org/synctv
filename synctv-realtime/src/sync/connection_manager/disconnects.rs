use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use super::metrics::{DisconnectSignalMetrics, ShutdownReport, ShutdownTaskOutcome};
use super::{ConnectionManager, DisconnectSignal, PENDING_DISCONNECT_QUEUE_CAPACITY};
use synctv_core::models::id::{RoomId, UserId};

impl ConnectionManager {
    /// Spawn a background task that retries pending disconnect signals.
    ///
    /// This task periodically checks for disconnect signals that failed to send
    /// (because the broadcast channel was full) and retries them. This ensures
    /// that kick/ban operations are not lost even under high load.
    pub(super) fn spawn_disconnect_retry_task(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let pending_disconnects = self.pending_disconnects.clone();
        let disconnect_tx = self.disconnect_tx.clone();
        let dropped_count = self.dropped_disconnect_signals.clone();
        let retried_count = self.retried_disconnect_signals.clone();

        tokio::spawn(async move {
            /// Interval between retry sweeps for pending disconnect signals.
            const RETRY_INTERVAL: Duration = Duration::from_millis(100);
            /// Maximum age of a pending disconnect signal before it's dropped (5 seconds).
            const MAX_SIGNAL_AGE: Duration = Duration::from_secs(5);

            let mut ticker = tokio::time::interval(RETRY_INTERVAL);
            ticker.tick().await;

            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        info!("Disconnect signal retry task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        let now = Instant::now();
                        let mut to_remove = Vec::new();
                        let mut retry_count = 0u64;

                        for entry in pending_disconnects.iter() {
                            let id = *entry.key();
                            let (signal, created_at) = entry.value();
                            let age = now.duration_since(*created_at);

                            if age > MAX_SIGNAL_AGE {
                                to_remove.push(id);
                                dropped_count.fetch_add(1, Ordering::Relaxed);
                                warn!(
                                    signal = ?signal,
                                    age_ms = age.as_millis(),
                                    "Dropping old disconnect signal after max retries"
                                );
                                continue;
                            }

                            if disconnect_tx.send(signal.clone()).is_ok() {
                                to_remove.push(id);
                                retry_count += 1;
                                debug!(
                                    signal = ?signal,
                                    age_ms = age.as_millis(),
                                    "Successfully retried disconnect signal"
                                );
                            }
                        }

                        for id in to_remove {
                            pending_disconnects.remove(&id);
                        }

                        if retry_count > 0 {
                            retried_count.fetch_add(retry_count, Ordering::Relaxed);
                        }
                    }
                }
            }
        })
    }

    /// Send a disconnect signal, storing it for retry if the channel is full.
    ///
    /// This method ensures that disconnect signals are not lost even when the
    /// broadcast channel is temporarily full. If the send fails, the signal
    /// is stored in `pending_disconnects` and will be retried by the background
    /// task spawned in `new()`.
    fn send_disconnect_signal(&self, signal: &DisconnectSignal) {
        if self.disconnect_tx.send(signal.clone()).is_ok() {
            return;
        }

        let receiver_count = self.disconnect_tx.receiver_count();
        if receiver_count == 0 {
            debug!(
                signal = ?signal,
                "Disconnect signal has no receivers (no active connections)"
            );
            return;
        }

        if self.pending_disconnects.len() >= PENDING_DISCONNECT_QUEUE_CAPACITY {
            self.dropped_disconnect_signals
                .fetch_add(1, Ordering::Relaxed);
            warn!(
                signal = ?signal,
                queue_size = self.pending_disconnects.len(),
                "Disconnect signal queue full, dropping signal. \
                 This indicates severe system overload."
            );
            return;
        }

        let id = self.pending_disconnect_id.fetch_add(1, Ordering::Relaxed);
        self.pending_disconnects
            .insert(id, (signal.clone(), Instant::now()));

        warn!(
            signal = ?signal,
            pending_count = self.pending_disconnects.len(),
            "Disconnect signal queued for retry (broadcast channel full)"
        );
    }

    /// Cancel the auto-spawned background tasks.
    ///
    /// Should be called during graceful shutdown to stop the background tasks.
    pub async fn shutdown(&self) -> ShutdownReport {
        self.ttl_refresh_cancel.cancel();
        self.disconnect_retry_cancel.cancel();

        let mut report = ShutdownReport::new();

        let ttl_refresh_handle = self.ttl_refresh_handle.lock().take();
        if let Some(handle) = ttl_refresh_handle {
            report.ttl_refresh = Some(
                Self::await_shutdown_task("ttl refresh", Duration::from_secs(5), handle).await,
            );
        }

        let pending_retries_handle = self.pending_retries_handle.lock().take();
        if let Some(handle) = pending_retries_handle {
            report.pending_retries = Some(
                Self::await_shutdown_task("pending Redis retries", Duration::from_secs(5), handle)
                    .await,
            );
        }

        let disconnect_retry_handle = self.disconnect_retry_handle.lock().take();
        if let Some(handle) = disconnect_retry_handle {
            report.disconnect_retry = Some(
                Self::await_shutdown_task("disconnect retry", Duration::from_secs(5), handle).await,
            );
        }

        if !report.all_clean() {
            warn!(
                ?report,
                "ConnectionManager shutdown observed background task failures"
            );
        }

        report
    }

    pub(crate) fn abort_background_tasks(&self) {
        self.ttl_refresh_cancel.cancel();
        self.disconnect_retry_cancel.cancel();

        if let Some(handle) = self.ttl_refresh_handle.lock().take() {
            handle.abort();
        }

        if let Some(handle) = self.pending_retries_handle.lock().take() {
            handle.abort();
        }

        if let Some(handle) = self.disconnect_retry_handle.lock().take() {
            handle.abort();
        }
    }

    async fn await_shutdown_task(
        task_name: &'static str,
        timeout_budget: Duration,
        mut handle: tokio::task::JoinHandle<()>,
    ) -> ShutdownTaskOutcome {
        match tokio::time::timeout(timeout_budget, &mut handle).await {
            Ok(Ok(())) => {
                debug!(
                    task = task_name,
                    "ConnectionManager background task stopped"
                );
                ShutdownTaskOutcome::Completed
            }
            Ok(Err(error)) if error.is_cancelled() => {
                debug!(
                    task = task_name,
                    "ConnectionManager background task cancelled"
                );
                ShutdownTaskOutcome::Cancelled
            }
            Ok(Err(error)) => {
                let message = error.to_string();
                warn!(
                    task = task_name,
                    error = %message,
                    "ConnectionManager background task ended with join error during shutdown"
                );
                ShutdownTaskOutcome::Failed(message)
            }
            Err(_) => {
                warn!(
                    task = task_name,
                    timeout_secs = timeout_budget.as_secs(),
                    "ConnectionManager background task did not stop before shutdown timeout; aborting"
                );
                handle.abort();
                match handle.await {
                    Ok(()) => debug!(
                        task = task_name,
                        "ConnectionManager background task completed after abort"
                    ),
                    Err(error) if error.is_cancelled() => debug!(
                        task = task_name,
                        "ConnectionManager background task aborted after timeout"
                    ),
                    Err(error) => warn!(
                        task = task_name,
                        error = %error,
                        "ConnectionManager background task returned join error after timeout abort"
                    ),
                }
                ShutdownTaskOutcome::TimedOut
            }
        }
    }

    /// Subscribe to disconnect signals
    ///
    /// Each connection should subscribe to this and monitor for disconnect signals
    /// that apply to them (by connection ID, user ID, or room ID)
    #[must_use]
    pub fn subscribe_disconnect(&self) -> broadcast::Receiver<DisconnectSignal> {
        self.disconnect_tx.subscribe()
    }

    /// Force disconnect a specific connection
    ///
    /// Sends a signal to the connection to close immediately.
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_connection(&self, connection_id: &str) {
        info!(
            connection_id = %connection_id,
            "Forcing connection disconnect"
        );
        let signal = DisconnectSignal::Connection(connection_id.to_string());
        self.send_disconnect_signal(&signal);
    }

    /// Force disconnect all connections for a user
    ///
    /// Used when a user is banned or kicked from all rooms.
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_user(&self, user_id: &UserId) {
        let conn_count = self.user_connection_count(user_id);
        info!(
            user_id = %user_id,
            connection_count = conn_count,
            "Forcing disconnect of all user connections"
        );
        let signal = DisconnectSignal::User(*user_id);
        self.send_disconnect_signal(&signal);
    }

    /// Force disconnect all connections in a room
    ///
    /// Used when a room is deleted or all users need to be removed.
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_room(&self, room_id: &RoomId) {
        let conn_count = self.room_connection_count(room_id);
        info!(
            room_id = %room_id,
            connection_count = conn_count,
            "Forcing disconnect of all room connections"
        );
        let signal = DisconnectSignal::Room(*room_id);
        self.send_disconnect_signal(&signal);
    }

    /// Force disconnect a specific user from a specific room
    ///
    /// Used when kicking a member from a room (not banning globally).
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_user_from_room(&self, user_id: &UserId, room_id: &RoomId) {
        info!(
            user_id = %user_id,
            room_id = %room_id,
            "Forcing disconnect of user from room"
        );
        let signal = DisconnectSignal::UserFromRoom {
            user_id: *user_id,
            room_id: *room_id,
        };
        self.send_disconnect_signal(&signal);
    }
}

impl ConnectionManager {
    /// Get disconnect signal reliability metrics.
    ///
    /// Returns metrics for monitoring the disconnect signal retry mechanism:
    /// - `pending_count`: Number of signals currently queued for retry
    /// - `dropped_count`: Total signals dropped due to queue overflow or timeout
    /// - `retried_count`: Total signals successfully retried
    #[must_use]
    pub fn disconnect_signal_metrics(&self) -> DisconnectSignalMetrics {
        DisconnectSignalMetrics {
            pending_count: self.pending_disconnects.len(),
            dropped_count: self.dropped_disconnect_signals.load(Ordering::Relaxed),
            retried_count: self.retried_disconnect_signals.load(Ordering::Relaxed),
        }
    }
}
