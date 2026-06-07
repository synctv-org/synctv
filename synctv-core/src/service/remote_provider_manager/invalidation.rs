use super::RemoteProviderManager;
use crate::cache::InvalidationMessage;
use std::sync::Arc;
use tokio::task::JoinHandle;

impl RemoteProviderManager {
    /// Start the durable provider invalidation subscriber for cross-replica cache invalidation.
    ///
    /// Subscribes to the shared `CacheInvalidationService`, which is backed by
    /// Redis Streams in cluster mode and therefore replays pending invalidations
    /// after reconnect/restart.
    /// Returns immediately if cross-replica invalidation is not configured.
    pub async fn start_invalidation_listener(&self) -> crate::Result<()> {
        let Some(ref invalidation_service) = self.cache_invalidation else {
            tracing::debug!("No durable invalidation service configured, skipping listener");
            return Ok(());
        };

        let mut guard = self.invalidation_listener_task.lock().await;
        if guard.is_some() {
            tracing::debug!("Provider invalidation listener already running");
            return Ok(());
        }

        let cache = Arc::clone(&self.channel_cache);
        let cancel = self.invalidation_cancel.child_token();
        let mut receiver = invalidation_service.subscribe();

        // The shared invalidation service may already have consumed durable
        // stream entries before this manager attaches its local broadcast
        // receiver. Drop all cached channels now so the next access reloads the
        // latest DB state instead of serving a stale pre-listener snapshot.
        self.channel_cache.invalidate_all();

        let handle = crate::spawn::spawn_monitored("provider_invalidation_listener", async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        tracing::info!("Provider invalidation listener shutting down");
                        break;
                    }
                    result = receiver.recv() => {
                        match result {
                            Ok(InvalidationMessage::ProviderInstance { instance_name }) => {
                                tracing::info!(
                                    "Received provider change notification for '{}', invalidating cache",
                                    instance_name
                                );
                                cache.invalidate(&instance_name).await;
                            }
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                tracing::warn!(
                                    "cache invalidation service closed provider invalidation subscription"
                                );
                                break;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(
                                    skipped,
                                    "Provider invalidation listener lagged; invalidating all cached provider channels"
                                );
                                cache.invalidate_all();
                            }
                        }
                    }
                }
            }
        });
        *guard = Some(handle);
        drop(guard);

        tracing::info!("Provider instance cache invalidation listener started (durable stream)");
        Ok(())
    }

    /// Cancel and join the provider invalidation listener.
    pub async fn shutdown(&self) {
        self.invalidation_cancel.cancel();

        let mut guard = self.invalidation_listener_task.lock().await;
        if let Some(handle) = guard.take() {
            match handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "Provider invalidation listener ended with join error during shutdown"
                    );
                }
            }
        }
    }

    #[must_use]
    pub fn invalidation_listener_task(&self) -> Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>> {
        Arc::clone(&self.invalidation_listener_task)
    }

    #[must_use]
    pub fn invalidation_cancel_token(&self) -> tokio_util::sync::CancellationToken {
        self.invalidation_cancel.clone()
    }

    /// Publish a durable cache invalidation notification so other replicas
    /// evict the stale entry for `instance_name`.
    pub(super) async fn notify_change(
        &self,
        operation: &'static str,
        instance_name: &str,
    ) -> crate::Result<()> {
        let Some(ref invalidation_service) = self.cache_invalidation else {
            return Ok(());
        };

        invalidation_service
            .invalidate_provider_instance(instance_name)
            .await
            .map_err(|e| {
                tracing::error!(
                    operation,
                    instance_name,
                    error = %e,
                    "Failed to publish provider change notification"
                );
                crate::Error::ServiceUnavailable(format!(
                    "Failed to publish provider invalidation for {operation} '{instance_name}': {e}"
                ))
            })
    }
}
