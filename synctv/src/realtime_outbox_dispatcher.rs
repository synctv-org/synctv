use std::sync::Arc;
use std::time::Duration;

use synctv_core::repository::realtime_outbox::{
    RealtimeOutboxEvent, RealtimeOutboxRepository, REALTIME_OUTBOX_CHANNEL,
};
use synctv_realtime::sync::{RealtimeEvent, RealtimeManager};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const CLAIM_BATCH_SIZE: i64 = 100;
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const BUSY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const PROCESSING_STALE_AFTER_SECS: i64 = 120;

pub fn start_realtime_outbox_dispatcher(
    outbox: Arc<RealtimeOutboxRepository>,
    realtime_manager: Arc<RealtimeManager>,
    node_id: String,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    synctv_core::spawn::spawn_monitored("realtime_outbox_dispatcher", async move {
        run_dispatcher(outbox, realtime_manager, node_id, cancel).await;
    })
}

async fn run_dispatcher(
    outbox: Arc<RealtimeOutboxRepository>,
    realtime_manager: Arc<RealtimeManager>,
    node_id: String,
    cancel: CancellationToken,
) {
    let worker_id = format!("{}:{}", node_id, synctv_common::snanoid!(8));
    info!(worker_id = %worker_id, "Realtime outbox dispatcher started");

    let mut listener = match sqlx::postgres::PgListener::connect_with(outbox.pool()).await {
        Ok(mut listener) => match listener.listen(REALTIME_OUTBOX_CHANNEL).await {
            Ok(()) => Some(listener),
            Err(error) => {
                warn!(
                    error = %error,
                    channel = REALTIME_OUTBOX_CHANNEL,
                    "Realtime outbox dispatcher could not listen for notifications; polling fallback remains active"
                );
                None
            }
        },
        Err(error) => {
            warn!(
                error = %error,
                channel = REALTIME_OUTBOX_CHANNEL,
                "Realtime outbox dispatcher could not create notification listener; polling fallback remains active"
            );
            None
        }
    };

    loop {
        let dispatched = dispatch_once(outbox.clone(), realtime_manager.clone(), &worker_id).await;
        if dispatched {
            continue;
        }

        tokio::select! {
            () = cancel.cancelled() => {
                info!(worker_id = %worker_id, "Realtime outbox dispatcher stopping");
                return;
            }
            () = wait_for_outbox_signal(&mut listener) => {}
        }
    }
}

async fn dispatch_once(
    outbox: Arc<RealtimeOutboxRepository>,
    realtime_manager: Arc<RealtimeManager>,
    worker_id: &str,
) -> bool {
    if let Err(error) = outbox
        .requeue_stale_processing(PROCESSING_STALE_AFTER_SECS)
        .await
    {
        warn!(error = %error, "Failed to requeue stale realtime outbox events");
    }

    let events = match outbox.claim_batch(worker_id, CLAIM_BATCH_SIZE).await {
        Ok(events) => events,
        Err(error) => {
            error!(error = %error, "Failed to claim realtime outbox events");
            return false;
        }
    };

    if events.is_empty() {
        return false;
    }

    for event in events {
        dispatch_event(&outbox, &realtime_manager, event).await;
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
                    "Realtime outbox dispatcher woke from database notification"
                );
            }
            Ok(Err(error)) => {
                warn!(
                    error = %error,
                    "Realtime outbox notification listener failed; polling fallback remains active"
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
    outbox: &RealtimeOutboxRepository,
    realtime_manager: &RealtimeManager,
    event: RealtimeOutboxEvent,
) {
    let realtime_event = match serde_json::from_value::<RealtimeEvent>(event.payload.clone()) {
        Ok(event) => event,
        Err(error) => {
            let message = format!("Failed to deserialize realtime outbox payload: {error}");
            error!(
                outbox_id = %event.id,
                event_type = %event.event_type,
                error = %error,
                "Dead-lettering malformed realtime outbox event"
            );
            let _ = outbox.mark_failed(&event.id, i32::MAX - 1, &message).await;
            return;
        }
    };

    if realtime_manager.publish_only(realtime_event.clone()) {
        if let Err(error) = outbox.mark_sent(&event.id).await {
            error!(
                outbox_id = %event.id,
                event_type = %realtime_event.event_type(),
                error = %error,
                "Realtime outbox event was published but could not be marked sent"
            );
        } else {
            debug!(
                outbox_id = %event.id,
                event_type = %realtime_event.event_type(),
                "Realtime outbox event published"
            );
        }
        return;
    }

    let message = "Realtime publish queue rejected event";
    if let Err(error) = outbox.mark_failed(&event.id, event.attempts, message).await {
        error!(
            outbox_id = %event.id,
            event_type = %realtime_event.event_type(),
            error = %error,
            "Failed to mark realtime outbox event for retry"
        );
    }
}
