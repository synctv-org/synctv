use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use std::sync::Arc;

use crate::cache::{CacheInvalidationRuntime, InvalidationMessage, RoomSettingsCache};
use crate::models::RoomId;
use crate::{Error, Result};

use super::RoomSettingsService;

#[derive(Debug)]
pub(super) struct RoomSettingsInvalidationRuntime {
    started: AtomicBool,
    cancel: tokio::sync::Mutex<CancellationToken>,
    listener_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl RoomSettingsInvalidationRuntime {
    pub(super) fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            cancel: tokio::sync::Mutex::new(CancellationToken::new()),
            listener_handle: tokio::sync::Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(super) fn is_started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    pub(super) async fn start(
        &self,
        inv_service: Arc<dyn CacheInvalidationRuntime>,
        cache: RoomSettingsCache,
    ) -> Result<()> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        if tokio::runtime::Handle::try_current().is_err() {
            self.started.store(false, Ordering::Release);
            return Err(Error::Internal(
                "RoomSettingsService::start requires a Tokio runtime".to_string(),
            ));
        }

        let mut receiver = inv_service.subscribe();
        let cancel = self.cancel.lock().await.child_token();

        let listener_handle = crate::spawn::spawn_monitored(
            "room_settings_invalidation_listener",
            async move {
                const LAG_FLUSH_MIN_INTERVAL: Duration = Duration::from_secs(5);
                let mut last_lag_flush = std::time::Instant::now()
                    .checked_sub(LAG_FLUSH_MIN_INTERVAL)
                    .unwrap_or_else(std::time::Instant::now);

                loop {
                    tokio::select! {
                        () = cancel.cancelled() => {
                            tracing::info!("Room settings invalidation listener shutting down");
                            break;
                        }
                        result = receiver.recv() => {
                            match result {
                                Ok(InvalidationMessage::RoomSettings { ref room_id }) => {
                                    let Ok(room_id) = room_id.parse::<RoomId>() else {
                                        tracing::warn!(room_id = %room_id, "Invalid room settings invalidation room id");
                                        continue;
                                    };
                                    if let Err(error) = cache.invalidate(&room_id).await {
                                        tracing::warn!(
                                            room_id = %room_id,
                                            error = %error,
                                            "Failed to invalidate room settings cache"
                                        );
                                    }
                                    tracing::debug!(
                                        room_id = %room_id,
                                        "Room settings cache invalidated (cross-replica)"
                                    );
                                }
                                Ok(InvalidationMessage::All) => {
                                    cache.clear().await;
                                    tracing::debug!("All room settings cache cleared (cross-replica)");
                                }
                                Ok(_) => {}
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    tracing::debug!("Room settings invalidation channel closed");
                                    break;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    let now = std::time::Instant::now();
                                    let elapsed = now.duration_since(last_lag_flush);
                                    if elapsed >= LAG_FLUSH_MIN_INTERVAL {
                                        tracing::warn!(
                                            lagged_messages = n,
                                            "Room settings invalidation listener lagged, flushing all cache (rate-limited)"
                                        );
                                        cache.clear().await;
                                        crate::metrics::cache::CACHE_LAG_FLUSH_TOTAL
                                            .with_label_values(&["room_settings"])
                                            .inc();
                                        last_lag_flush = now;
                                    } else {
                                        tracing::warn!(
                                            lagged_messages = n,
                                            "Room settings invalidation listener lagged, skipping flush (rate-limited)"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            },
        );

        *self.listener_handle.lock().await = Some(listener_handle);
        Ok(())
    }

    pub(super) async fn shutdown(&self) {
        let cancel = {
            let mut runtime_cancel = self.cancel.lock().await;
            std::mem::replace(&mut *runtime_cancel, CancellationToken::new())
        };
        cancel.cancel();

        let listener_handle = self.listener_handle.lock().await.take();
        if let Some(handle) = listener_handle {
            RoomSettingsService::await_invalidation_task_shutdown(
                "room settings invalidation listener",
                handle,
            )
            .await;
        }

        self.started.store(false, Ordering::Release);
    }
}

impl RoomSettingsService {
    /// Maximum time to wait for the invalidation listener to stop.
    const INVALIDATION_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

    pub async fn start(&self) -> Result<()> {
        let Some(inv_service) = self.invalidation_service.clone() else {
            return Ok(());
        };

        self.invalidation_runtime
            .start(inv_service, self.cache.clone())
            .await
    }

    pub async fn shutdown(&self) {
        self.invalidation_runtime.shutdown().await;
    }

    async fn await_invalidation_task_shutdown(name: &'static str, mut handle: JoinHandle<()>) {
        match tokio::time::timeout(Self::INVALIDATION_TASK_SHUTDOWN_TIMEOUT, &mut handle).await {
            Ok(Ok(())) => info!("{name} stopped"),
            Ok(Err(error)) => warn!(%error, "{name} panicked during shutdown"),
            Err(_) => {
                warn!(
                    timeout_secs = Self::INVALIDATION_TASK_SHUTDOWN_TIMEOUT.as_secs(),
                    "{name} did not stop before timeout; aborting task"
                );
                handle.abort();
                match handle.await {
                    Ok(()) => info!("{name} aborted cleanly"),
                    Err(error) if error.is_cancelled() => info!("{name} aborted"),
                    Err(error) => warn!(%error, "{name} failed after abort"),
                }
            }
        }
    }
}
