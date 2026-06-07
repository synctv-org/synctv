use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    cache::{CacheInvalidationRuntime, InvalidationMessage, MemberPermissionKey},
    models::{RoomId, UserId},
    service::permission::PermissionService,
    Error, Result,
};

#[derive(Debug)]
pub(crate) struct PermissionInvalidationRuntime {
    pub(crate) started: AtomicBool,
    pub(crate) cancel: tokio::sync::Mutex<CancellationToken>,
    pub(crate) listener_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    pub(crate) recovery_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl PermissionInvalidationRuntime {
    pub(crate) fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            cancel: tokio::sync::Mutex::new(CancellationToken::new()),
            listener_handle: tokio::sync::Mutex::new(None),
            recovery_handle: tokio::sync::Mutex::new(None),
        }
    }
}

#[derive(Default)]
pub(crate) struct SharedInvalidationService {
    pub(crate) service: parking_lot::RwLock<Option<std::sync::Arc<dyn CacheInvalidationRuntime>>>,
}

impl std::fmt::Debug for SharedInvalidationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedInvalidationService")
            .field("configured", &self.service.read().is_some())
            .finish()
    }
}

impl PermissionService {
    pub fn has_invalidation_service(&self) -> bool {
        self.invalidation_service().is_some()
    }

    #[cfg(test)]
    pub(crate) fn invalidation_tasks_started(&self) -> bool {
        self.invalidation_runtime.started.load(Ordering::Acquire)
    }

    pub async fn start(&self) -> Result<()> {
        let Some(invalidation_service) = self.invalidation_service() else {
            return Ok(());
        };

        if self
            .invalidation_runtime
            .started
            .swap(true, Ordering::AcqRel)
        {
            return Ok(());
        }

        if tokio::runtime::Handle::try_current().is_err() {
            self.invalidation_runtime
                .started
                .store(false, Ordering::Release);
            return Err(Error::Internal(
                "PermissionService::start requires a Tokio runtime".to_string(),
            ));
        }

        let mut receiver = invalidation_service.subscribe();
        let member_permission_cache = self.member_permission_cache.clone();
        let room_settings_cache = self.room_settings_cache.clone();
        let cache_degraded = self.cache_degraded.clone();
        let last_flush_time = self.last_flush_time.clone();
        let degradation_started = self.degradation_started.clone();
        let listener_cancel = self.invalidation_runtime.cancel.lock().await.child_token();

        let listener_handle = crate::spawn::spawn_monitored(
            "permission_invalidation_listener",
            async move {
                loop {
                    tokio::select! {
                        () = listener_cancel.cancelled() => {
                            tracing::info!("Permission invalidation listener shutting down");
                            break;
                        }
                        result = receiver.recv() => {
                            match result {
                                Ok(msg) => {
                                    if cache_degraded.swap(false, Ordering::Release) {
                                        tracing::info!("Permission cache recovered from degraded state");
                                    }
                                    *degradation_started.lock() = None;

                                    match msg {
                                        InvalidationMessage::UserPermission { room_id, user_id } => {
                                            match (room_id.parse::<RoomId>(), user_id.parse::<UserId>()) {
                                                (Ok(room_id), Ok(user_id)) => {
                                                    let cache_key = MemberPermissionKey::new(room_id, user_id);
                                                    if let Err(error) = member_permission_cache.invalidate(&cache_key).await {
                                                        tracing::warn!(
                                                            room_id = %room_id,
                                                            user_id = %user_id,
                                                            error = %error,
                                                            "Failed to invalidate member permission source cache"
                                                        );
                                                    }
                                                }
                                                _ => {
                                                    tracing::warn!(
                                                        room_id = %room_id,
                                                        user_id = %user_id,
                                                        "Ignoring invalid member permission invalidation key"
                                                    );
                                                }
                                            }
                                            tracing::debug!(
                                                room_id = %room_id,
                                                user_id = %user_id,
                                                "Member permission source cache invalidated (cross-replica)"
                                            );
                                        }
                                        InvalidationMessage::RoomPermission { room_id } => {
                                            member_permission_cache.clear().await;
                                            match room_id.parse::<RoomId>() {
                                                Ok(parsed_room_id) => {
                                                    if let Err(error) = room_settings_cache.invalidate(&parsed_room_id).await {
                                                        tracing::warn!(
                                                            room_id = %parsed_room_id,
                                                            error = %error,
                                                            "Failed to invalidate permission room settings source cache"
                                                        );
                                                    }
                                                }
                                                Err(_) => {
                                                    tracing::warn!(
                                                        room_id = %room_id,
                                                        "Ignoring invalid room permission invalidation key"
                                                    );
                                                }
                                            }
                                            tracing::debug!(
                                                room_id = %room_id,
                                                "Room permission source caches invalidated (cross-replica)"
                                            );
                                        }
                                        InvalidationMessage::All => {
                                            member_permission_cache.clear().await;
                                            room_settings_cache.clear().await;
                                            tracing::debug!("All permission source caches invalidated (cross-replica)");
                                        }
                                        _ => {}
                                    }
                                }
                                Err(broadcast::error::RecvError::Closed) => {
                                    tracing::debug!("Invalidation channel closed, stopping listener");
                                    break;
                                }
                                Err(broadcast::error::RecvError::Lagged(n)) => {
                                    let was_degraded = cache_degraded.swap(true, Ordering::Release);
                                    if !was_degraded {
                                        *degradation_started.lock() = Some(Instant::now());
                                        tracing::warn!(
                                            lagged_messages = n,
                                            "Permission cache entered degraded state due to Pub/Sub lag"
                                        );
                                    }

                                    std::sync::atomic::fence(Ordering::SeqCst);

                                    let should_flush = {
                                        let mut last = last_flush_time.lock();
                                        if last.elapsed() >= Duration::from_secs(Self::FLUSH_RATE_LIMIT_SECS) {
                                            *last = Instant::now();
                                            true
                                        } else {
                                            false
                                        }
                                    };

                                    if should_flush {
                                        tracing::warn!(
                                            lagged_messages = n,
                                        "Invalidation listener lagged, flushing all permission source caches"
                                    );
                                        member_permission_cache.clear().await;
                                        room_settings_cache.clear().await;
                                    } else {
                                        tracing::debug!(
                                            lagged_messages = n,
                                            "Invalidation listener lagged, cache flush rate-limited (already degraded)"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            },
        );

        let member_cache_for_recovery = self.member_permission_cache.clone();
        let room_settings_cache_for_recovery = self.room_settings_cache.clone();
        let cache_degraded_for_recovery = self.cache_degraded.clone();
        let degradation_started_for_recovery = self.degradation_started.clone();
        let recovery_cancel = self.invalidation_runtime.cancel.lock().await.child_token();
        let recovery_handle = crate::spawn::spawn_monitored(
            "permission_cache_recovery",
            async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(10));
                loop {
                    tokio::select! {
                        () = recovery_cancel.cancelled() => {
                            tracing::info!("Permission cache recovery task shutting down");
                            break;
                        }
                        _ = ticker.tick() => {
                            if !cache_degraded_for_recovery.load(Ordering::Acquire) {
                                continue;
                            }

                            let should_recover = {
                                let started = degradation_started_for_recovery.lock();
                                started.is_some_and(|start_time| {
                                    start_time.elapsed()
                                        >= Duration::from_secs(Self::MAX_DEGRADATION_DURATION_SECS)
                                })
                            };

                            if !should_recover {
                                continue;
                            }

                            tracing::warn!(
                                "Permission cache degraded for {} seconds, forcing cache refresh before recovery",
                                Self::MAX_DEGRADATION_DURATION_SECS
                            );
                            member_cache_for_recovery.clear().await;
                            room_settings_cache_for_recovery.clear().await;
                            cache_degraded_for_recovery.store(false, Ordering::Release);
                            *degradation_started_for_recovery.lock() = None;
                            tracing::info!(
                                "Permission cache auto-recovered after full cache refresh"
                            );
                        }
                    }
                }
            },
        );

        *self.invalidation_runtime.listener_handle.lock().await = Some(listener_handle);
        *self.invalidation_runtime.recovery_handle.lock().await = Some(recovery_handle);

        Ok(())
    }

    pub async fn shutdown(&self) {
        let cancel = {
            let mut runtime_cancel = self.invalidation_runtime.cancel.lock().await;
            std::mem::replace(&mut *runtime_cancel, CancellationToken::new())
        };
        cancel.cancel();

        let listener_handle = self
            .invalidation_runtime
            .listener_handle
            .lock()
            .await
            .take();
        if let Some(handle) = listener_handle {
            Self::await_invalidation_task_shutdown("permission invalidation listener", handle)
                .await;
        }

        let recovery_handle = self
            .invalidation_runtime
            .recovery_handle
            .lock()
            .await
            .take();
        if let Some(handle) = recovery_handle {
            Self::await_invalidation_task_shutdown("permission cache recovery", handle).await;
        }
        self.invalidation_runtime
            .started
            .store(false, Ordering::Release);
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
