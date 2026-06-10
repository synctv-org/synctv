use std::time::Duration;

use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use super::metrics::{ShutdownReport, ShutdownTaskOutcome};
use super::{ConnectionManager, DisconnectSignal};
use synctv_core::models::id::{RoomId, UserId};

impl ConnectionManager {
    fn send_disconnect_signal(&self, signal: &DisconnectSignal) {
        if self.disconnect_tx.send(signal.clone()).is_ok() {
            return;
        }

        debug!(
            signal = ?signal,
            "Disconnect signal has no receivers"
        );
    }

    /// Cancel the auto-spawned background tasks.
    ///
    /// Should be called during graceful shutdown to stop the background tasks.
    pub async fn shutdown(&self) -> ShutdownReport {
        self.ttl_refresh_cancel.cancel();

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

        if let Some(handle) = self.ttl_refresh_handle.lock().take() {
            handle.abort();
        }

        if let Some(handle) = self.pending_retries_handle.lock().take() {
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

    pub fn disconnect_connection(&self, connection_id: &str) {
        info!(
            connection_id = %connection_id,
            "Forcing connection disconnect"
        );
        let signal = DisconnectSignal::Connection(connection_id.to_string());
        self.send_disconnect_signal(&signal);
    }

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
