use std::{sync::Arc, time::Duration};

use synctv_api::AdminApiImpl;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

const CLAIM_BATCH_SIZE: i64 = 1;
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const STALE_REQUEUE_INTERVAL: Duration = Duration::from_mins(1);
const PROCESSING_STALE_AFTER_SECS: i64 = 600;
const TERMINAL_CLEANUP_INTERVAL: Duration = Duration::from_hours(1);
const TERMINAL_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;

pub fn start_chat_moderation_dispatcher(
    admin_api: Arc<AdminApiImpl>,
    worker_id: String,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    synctv_core::spawn::spawn_monitored("chat_moderation_dispatcher", async move {
        run_dispatcher(admin_api, worker_id, cancel).await;
    })
}

async fn run_dispatcher(
    admin_api: Arc<AdminApiImpl>,
    worker_id: String,
    cancel: CancellationToken,
) {
    let mut next_stale_requeue = tokio::time::Instant::now();
    let mut next_terminal_cleanup = tokio::time::Instant::now();
    info!(worker_id = %worker_id, "Chat moderation dispatcher started");
    loop {
        if cancel.is_cancelled() {
            info!(worker_id = %worker_id, "Chat moderation dispatcher stopping");
            return;
        }
        if tokio::time::Instant::now() >= next_stale_requeue {
            next_stale_requeue = tokio::time::Instant::now() + STALE_REQUEUE_INTERVAL;
            if let Some(chat_service) = admin_api.chat_service.as_ref() {
                match chat_service
                    .moderation_job_repository()
                    .requeue_stale_processing(PROCESSING_STALE_AFTER_SECS)
                    .await
                {
                    Ok(0) => {}
                    Ok(count) => warn!(count, "Recovered stale chat moderation jobs"),
                    Err(error) => {
                        warn!(error = %error, "Failed to requeue stale chat moderation jobs");
                    }
                }
            }
        }
        if tokio::time::Instant::now() >= next_terminal_cleanup {
            next_terminal_cleanup = tokio::time::Instant::now() + TERMINAL_CLEANUP_INTERVAL;
            if let Some(chat_service) = admin_api.chat_service.as_ref() {
                match chat_service
                    .moderation_job_repository()
                    .delete_terminal_before(TERMINAL_RETENTION_SECS)
                    .await
                {
                    Ok(0) => {}
                    Ok(count) => info!(count, "Deleted expired chat moderation jobs"),
                    Err(error) => {
                        warn!(error = %error, "Failed to delete expired chat moderation jobs");
                    }
                }
            }
        }
        match admin_api
            .process_chat_moderation_jobs(&worker_id, CLAIM_BATCH_SIZE)
            .await
        {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => error!(error = %error, "Failed to process chat moderation jobs"),
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(IDLE_POLL_INTERVAL) => {}
        }
    }
}
