use std::sync::Arc;
use std::time::Duration;

use synctv_cluster::sync::{ClusterEvent, ClusterManager};
use synctv_core::repository::cluster_outbox::{
    ClusterOutboxEvent, ClusterOutboxRepository, CLUSTER_OUTBOX_CHANNEL,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const CLAIM_BATCH_SIZE: i64 = 100;
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const BUSY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const PROCESSING_STALE_AFTER_SECS: i64 = 120;

pub fn start_cluster_outbox_dispatcher(
    outbox: Arc<ClusterOutboxRepository>,
    cluster_manager: Arc<ClusterManager>,
    node_id: String,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    synctv_core::spawn::spawn_monitored("cluster_outbox_dispatcher", async move {
        run_dispatcher(outbox, cluster_manager, node_id, cancel).await;
    })
}

async fn run_dispatcher(
    outbox: Arc<ClusterOutboxRepository>,
    cluster_manager: Arc<ClusterManager>,
    node_id: String,
    cancel: CancellationToken,
) {
    let worker_id = format!("{}:{}", node_id, synctv_common::snanoid!(8));
    info!(worker_id = %worker_id, "Cluster outbox dispatcher started");

    let mut listener = match sqlx::postgres::PgListener::connect_with(outbox.pool()).await {
        Ok(mut listener) => match listener.listen(CLUSTER_OUTBOX_CHANNEL).await {
            Ok(()) => Some(listener),
            Err(error) => {
                warn!(
                    error = %error,
                    channel = CLUSTER_OUTBOX_CHANNEL,
                    "Cluster outbox dispatcher could not listen for notifications; polling fallback remains active"
                );
                None
            }
        },
        Err(error) => {
            warn!(
                error = %error,
                channel = CLUSTER_OUTBOX_CHANNEL,
                "Cluster outbox dispatcher could not create notification listener; polling fallback remains active"
            );
            None
        }
    };

    loop {
        let dispatched = dispatch_once(outbox.clone(), cluster_manager.clone(), &worker_id).await;
        if dispatched {
            continue;
        }

        tokio::select! {
            () = cancel.cancelled() => {
                info!(worker_id = %worker_id, "Cluster outbox dispatcher stopping");
                return;
            }
            () = wait_for_outbox_signal(&mut listener) => {}
        }
    }
}

async fn dispatch_once(
    outbox: Arc<ClusterOutboxRepository>,
    cluster_manager: Arc<ClusterManager>,
    worker_id: &str,
) -> bool {
    if let Err(error) = outbox
        .requeue_stale_processing(PROCESSING_STALE_AFTER_SECS)
        .await
    {
        warn!(error = %error, "Failed to requeue stale cluster outbox events");
    }

    let events = match outbox.claim_batch(worker_id, CLAIM_BATCH_SIZE).await {
        Ok(events) => events,
        Err(error) => {
            error!(error = %error, "Failed to claim cluster outbox events");
            return false;
        }
    };

    if events.is_empty() {
        return false;
    }

    for event in events {
        dispatch_event(&outbox, &cluster_manager, event).await;
    }

    tokio::time::sleep(BUSY_POLL_INTERVAL).await;
    true
}

async fn wait_for_outbox_signal(listener: &mut Option<sqlx::postgres::PgListener>) {
    if let Some(pg_listener) = listener.as_mut() {
        match tokio::time::timeout(IDLE_POLL_INTERVAL, pg_listener.recv()).await {
            Ok(Ok(notification)) => {
                debug!(
                    channel = notification.channel(),
                    outbox_id = notification.payload(),
                    "Cluster outbox dispatcher woke from database notification"
                );
            }
            Ok(Err(error)) => {
                warn!(
                    error = %error,
                    "Cluster outbox notification listener failed; polling fallback remains active"
                );
                *listener = None;
            }
            Err(_) => {}
        }
        return;
    }

    tokio::time::sleep(IDLE_POLL_INTERVAL).await;
}

async fn dispatch_event(
    outbox: &ClusterOutboxRepository,
    cluster_manager: &ClusterManager,
    event: ClusterOutboxEvent,
) {
    let cluster_event = match serde_json::from_value::<ClusterEvent>(event.payload.clone()) {
        Ok(event) => event,
        Err(error) => {
            let message = format!("Failed to deserialize cluster outbox payload: {error}");
            error!(
                outbox_id = %event.id,
                event_type = %event.event_type,
                error = %error,
                "Dead-lettering malformed cluster outbox event"
            );
            let _ = outbox.mark_failed(&event.id, i32::MAX - 1, &message).await;
            return;
        }
    };

    if cluster_manager.publish_only(cluster_event.clone()) {
        if let Err(error) = outbox.mark_sent(&event.id).await {
            error!(
                outbox_id = %event.id,
                event_type = %cluster_event.event_type(),
                error = %error,
                "Cluster outbox event was published but could not be marked sent"
            );
        } else {
            debug!(
                outbox_id = %event.id,
                event_type = %cluster_event.event_type(),
                "Cluster outbox event published"
            );
        }
        return;
    }

    let message = "Cluster publish queue rejected event";
    if let Err(error) = outbox.mark_failed(&event.id, event.attempts, message).await {
        error!(
            outbox_id = %event.id,
            event_type = %cluster_event.event_type(),
            error = %error,
            "Failed to mark cluster outbox event for retry"
        );
    }
}
