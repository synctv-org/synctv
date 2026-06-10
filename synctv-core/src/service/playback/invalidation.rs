use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    cache::{CacheInvalidationRuntime, InvalidationMessage, PlaybackStateCache},
    models::{RoomId, RoomPlaybackState},
    service::playback::PlaybackService,
    Error, Result,
};

#[derive(Debug)]
pub(super) struct PlaybackInvalidationRuntime {
    pub(super) started: AtomicBool,
    pub(super) cancel: tokio::sync::Mutex<CancellationToken>,
    pub(super) listener_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl PlaybackInvalidationRuntime {
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
        invalidation_service: Arc<dyn CacheInvalidationRuntime>,
        cache: Arc<moka::future::Cache<String, RoomPlaybackState>>,
        l2_cache: Arc<parking_lot::RwLock<Option<PlaybackStateCache>>>,
    ) -> Result<()> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        if tokio::runtime::Handle::try_current().is_err() {
            self.started.store(false, Ordering::Release);
            return Err(Error::Internal(
                "PlaybackService::start requires a Tokio runtime".to_string(),
            ));
        }

        let mut receiver = invalidation_service.subscribe();
        let listener_cancel = self.cancel.lock().await.child_token();

        let listener_handle = crate::spawn::spawn_monitored(
            "playback_invalidation_listener",
            async move {
                const LAG_FLUSH_MIN_INTERVAL: Duration = Duration::from_secs(5);
                let mut last_lag_flush = Instant::now()
                    .checked_sub(LAG_FLUSH_MIN_INTERVAL)
                    .unwrap_or_else(Instant::now);

                loop {
                    tokio::select! {
                        () = listener_cancel.cancelled() => {
                            tracing::debug!(
                                "Playback cache invalidation listener cancelled, stopping"
                            );
                            break;
                        }
                        recv_result = receiver.recv() => {
                            match recv_result {
                                Ok(message) => {
                                    handle_invalidation_message(
                                        message,
                                        &cache,
                                        &l2_cache,
                                    )
                                    .await;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    tracing::debug!(
                                        "Playback cache invalidation channel closed, stopping listener"
                                    );
                                    break;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                                    handle_lagged_invalidation_messages(
                                        count,
                                        &cache,
                                        &l2_cache,
                                        &mut last_lag_flush,
                                        LAG_FLUSH_MIN_INTERVAL,
                                    )
                                    .await;
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
            PlaybackService::await_invalidation_task_shutdown(
                "playback invalidation listener",
                handle,
            )
            .await;
        }

        self.started.store(false, Ordering::Release);
    }
}

impl PlaybackService {
    pub async fn start(&self) -> Result<()> {
        let Some(invalidation_service) = self.invalidation_service.clone() else {
            return Ok(());
        };

        self.invalidation_runtime
            .start(
                invalidation_service,
                self.playback_cache.clone(),
                Arc::clone(&self.l2_cache),
            )
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

    pub async fn invalidate_playback_cache(&self, room_id: &RoomId) {
        if let Some(ref service) = self.invalidation_service {
            if let Err(error) = service.invalidate_playback_state(room_id).await {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    "Failed to broadcast playback state cache invalidation"
                );
            }
        }

        let cache_key = room_id.to_string();
        self.playback_cache.invalidate(&cache_key).await;

        if let Some(l2_cache) = self.playback_l2_cache() {
            if let Err(error) = l2_cache.invalidate(room_id).await {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    "Failed to invalidate playback state from L2 cache"
                );
            }
        }
    }

    pub(super) async fn broadcast_invalidation(
        &self,
        room_id: &RoomId,
        state: &RoomPlaybackState,
        context: &str,
    ) {
        let Some(ref service) = self.invalidation_service else {
            return;
        };
        if let Err(error) = service.update_playback_state(room_id, state).await {
            tracing::error!(
                error = %error,
                room_id = %room_id,
                "{context}: playback invalidation broadcast failed; replicas may rely on L2/db fallback"
            );
        }
    }
}

async fn handle_invalidation_message(
    message: InvalidationMessage,
    cache: &moka::future::Cache<String, RoomPlaybackState>,
    l2_cache: &Arc<parking_lot::RwLock<Option<PlaybackStateCache>>>,
) {
    match message {
        InvalidationMessage::PlaybackStateUpdate { room_id, state } => {
            let new_version = state.version;
            let new_state = state;
            cache
                .entry(room_id.clone())
                .and_upsert_with(|maybe_entry| {
                    let result = if let Some(entry) = maybe_entry {
                        let current = entry.into_value();
                        let current_version = current.version;
                        if new_version > current_version {
                            tracing::debug!(
                                room_id = %room_id,
                                new_version,
                                current_version,
                                "Playback state cache updated (cross-replica, version upgrade)"
                            );
                            new_state.clone()
                        } else {
                            tracing::debug!(
                                room_id = %room_id,
                                new_version,
                                current_version,
                                "Playback state cache not updated (cross-replica, stale or duplicate version)"
                            );
                            current
                        }
                    } else {
                        tracing::debug!(
                            room_id = %room_id,
                            new_version,
                            "Playback state cache inserted (cross-replica, no prior entry)"
                        );
                        new_state.clone()
                    };
                    std::future::ready(result)
                })
                .await;
        }
        InvalidationMessage::PlaybackState { room_id } => {
            cache.invalidate(&room_id).await;
            invalidate_l2_room(l2_cache, &room_id, "playback state").await;
            tracing::debug!(
                room_id = %room_id,
                "Playback state cache invalidated (cross-replica)"
            );
        }
        InvalidationMessage::Room { room_id } => {
            cache.invalidate(&room_id).await;
            invalidate_l2_room(l2_cache, &room_id, "room-scoped playback state").await;
        }
        InvalidationMessage::All => {
            cache.invalidate_all();
            clear_l2(l2_cache).await;
            tracing::debug!("All playback state cache invalidated (cross-replica)");
        }
        _ => {}
    }
}

async fn handle_lagged_invalidation_messages(
    lagged_count: u64,
    cache: &moka::future::Cache<String, RoomPlaybackState>,
    l2_cache: &Arc<parking_lot::RwLock<Option<PlaybackStateCache>>>,
    last_lag_flush: &mut Instant,
    min_interval: Duration,
) {
    let now = Instant::now();
    let elapsed = now.duration_since(*last_lag_flush);
    if elapsed >= min_interval {
        tracing::warn!(
            lagged_messages = lagged_count,
            "Playback cache invalidation listener lagged, flushing all entries (rate-limited)"
        );
        cache.invalidate_all();
        clear_l2(l2_cache).await;
        crate::metrics::cache::CACHE_LAG_FLUSH_TOTAL
            .with_label_values(&["playback"])
            .inc();
        *last_lag_flush = now;
    } else {
        tracing::warn!(
            lagged_messages = lagged_count,
            "Playback cache invalidation listener lagged, skipping flush (rate-limited)"
        );
    }
}

async fn invalidate_l2_room(
    l2_cache: &Arc<parking_lot::RwLock<Option<PlaybackStateCache>>>,
    room_id: &str,
    context: &'static str,
) {
    let l2_cache = { l2_cache.read().clone() };
    let Some(l2_cache) = l2_cache else {
        return;
    };
    let Ok(room_id) = room_id.parse::<RoomId>() else {
        return;
    };
    if let Err(error) = l2_cache.invalidate(&room_id).await {
        tracing::warn!(
            room_id = %room_id,
            error = %error,
            "Failed to invalidate {context} from L2 cache"
        );
    }
}

async fn clear_l2(l2_cache: &Arc<parking_lot::RwLock<Option<PlaybackStateCache>>>) {
    let l2_cache = { l2_cache.read().clone() };
    if let Some(l2_cache) = l2_cache {
        l2_cache.clear().await;
    }
}
