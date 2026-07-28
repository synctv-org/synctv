use std::{sync::Arc, time::Duration};

use synctv_cluster::leader::LeaderRuntime;
use synctv_core::{
    metrics::email::{
        EMAIL_DELIVERY_DURATION_SECONDS, EMAIL_DELIVERY_IN_FLIGHT, EMAIL_DELIVERY_JOBS_TOTAL,
        EMAIL_DELIVERY_QUEUE_DEPTH,
    },
    models::EmailTokenType,
    repository::{
        email_outbox::EMAIL_OUTBOX_CHANNEL, EmailOutboxJob, EmailOutboxKind, EmailOutboxStatus,
    },
    service::{
        EmailOutboxPayload, EmailOutboxService, EmailService, EmailTokenService, UserService,
    },
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const DELIVERY_CONCURRENCY: usize = 4;
const CLAIM_BATCH_SIZE: i64 = 4;
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const PROCESSING_STALE_AFTER_SECS: i64 = 120;
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);
const TERMINAL_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;
const CLEANUP_BATCH_SIZE: i64 = 100;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(60);

pub fn start_email_outbox_dispatcher(
    outbox: Arc<EmailOutboxService>,
    email_service: Arc<EmailService>,
    email_token_service: Arc<EmailTokenService>,
    user_service: Arc<UserService>,
    leader_runtime: Arc<dyn LeaderRuntime>,
    node_id: String,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    synctv_core::spawn::spawn_monitored("email_outbox_dispatcher", async move {
        run_dispatcher(
            outbox,
            email_service,
            email_token_service,
            user_service,
            leader_runtime,
            node_id,
            cancel,
        )
        .await;
    })
}

async fn run_dispatcher(
    outbox: Arc<EmailOutboxService>,
    email_service: Arc<EmailService>,
    email_token_service: Arc<EmailTokenService>,
    user_service: Arc<UserService>,
    leader_runtime: Arc<dyn LeaderRuntime>,
    node_id: String,
    cancel: CancellationToken,
) {
    let worker_id = format!("{}:{}", node_id, synctv_common::snanoid!(8));
    let mut listener = connect_listener(outbox.repository()).await;
    let mut next_maintenance = Instant::now();
    info!(worker_id = %worker_id, "Email outbox dispatcher started");

    loop {
        if cancel.is_cancelled() {
            break;
        }
        if should_run_maintenance(leader_runtime.is_leader(), Instant::now(), next_maintenance) {
            run_maintenance(&outbox, &email_token_service, &user_service).await;
            next_maintenance = Instant::now() + MAINTENANCE_INTERVAL;
        }

        // Delivery remains active-active. Leadership only suppresses duplicate maintenance work.
        let jobs = match outbox
            .repository()
            .claim_batch(&worker_id, CLAIM_BATCH_SIZE)
            .await
        {
            Ok(jobs) => jobs,
            Err(error) => {
                error!(error = %error, "Failed to claim email outbox jobs");
                Vec::new()
            }
        };
        update_queue_depth(&outbox).await;

        if jobs.is_empty() {
            tokio::select! {
                () = cancel.cancelled() => break,
                () = wait_for_signal(&mut listener) => {}
            }
            continue;
        }

        let mut deliveries = tokio::task::JoinSet::new();
        for job in jobs {
            while deliveries.len() >= DELIVERY_CONCURRENCY {
                observe_delivery_join(deliveries.join_next().await);
            }
            deliveries.spawn(dispatch_job(
                outbox.clone(),
                email_service.clone(),
                email_token_service.clone(),
                user_service.clone(),
                worker_id.clone(),
                job,
            ));
        }
        while let Some(result) = deliveries.join_next().await {
            observe_delivery_join(Some(result));
        }

        if cancel.is_cancelled() {
            break;
        }
    }

    EMAIL_DELIVERY_QUEUE_DEPTH.set(0);
    info!(worker_id = %worker_id, "Email outbox dispatcher stopped");
}

fn should_run_maintenance(is_leader: bool, now: Instant, next_due: Instant) -> bool {
    is_leader && now >= next_due
}

async fn connect_listener(
    repository: &synctv_core::repository::EmailOutboxRepository,
) -> Option<sqlx::postgres::PgListener> {
    match sqlx::postgres::PgListener::connect_with(repository.pool()).await {
        Ok(mut listener) => match listener.listen(EMAIL_OUTBOX_CHANNEL).await {
            Ok(()) => Some(listener),
            Err(error) => {
                warn!(error = %error, "Email outbox LISTEN failed; polling remains active");
                None
            }
        },
        Err(error) => {
            warn!(error = %error, "Email outbox listener connection failed; polling remains active");
            None
        }
    }
}

async fn wait_for_signal(listener: &mut Option<sqlx::postgres::PgListener>) {
    if let Some(pg_listener) = listener.as_mut() {
        match tokio::time::timeout(IDLE_POLL_INTERVAL, pg_listener.recv()).await {
            Ok(Ok(notification)) => {
                debug!(
                    job_id = notification.payload(),
                    "Email outbox notification received"
                );
            }
            Ok(Err(error)) => {
                warn!(error = %error, "Email outbox listener failed; polling remains active");
                *listener = None;
            }
            Err(_) => {}
        }
    } else {
        tokio::time::sleep(IDLE_POLL_INTERVAL).await;
    }
}

fn observe_delivery_join(result: Option<Result<(), tokio::task::JoinError>>) {
    if let Some(Err(error)) = result {
        error!(error = %error, "Email delivery task ended unexpectedly");
    }
}

async fn dispatch_job(
    outbox: Arc<EmailOutboxService>,
    email_service: Arc<EmailService>,
    email_token_service: Arc<EmailTokenService>,
    user_service: Arc<UserService>,
    worker_id: String,
    job: EmailOutboxJob,
) {
    let started = Instant::now();
    let kind = job.kind.as_str();
    EMAIL_DELIVERY_IN_FLIGHT.inc();
    let failure_context = FailureContext {
        outbox: &outbox,
        email_token_service: &email_token_service,
        user_service: &user_service,
        worker_id: &worker_id,
    };

    let payload = match outbox.decrypt_payload(&job.encrypted_payload) {
        Ok(payload) => payload,
        Err(error) => {
            error!(job_id = %job.id, error = %error, "Email outbox payload could not be decrypted");
            finish_failed(
                &failure_context,
                &job,
                None,
                "invalid encrypted payload",
                false,
            )
            .await;
            record_completion(kind, "dead", started);
            return;
        }
    };

    let token = match token_for_job(&job, &payload) {
        Ok(token) => token,
        Err(message) => {
            error!(job_id = %job.id, reason = message, "Email outbox payload does not match job kind");
            finish_failed(
                &failure_context,
                &job,
                Some(&payload),
                "payload kind mismatch",
                false,
            )
            .await;
            record_completion(kind, "dead", started);
            return;
        }
    };

    let message_id = EmailOutboxService::message_id(&job.id);
    let delivery = tokio::time::timeout(
        DELIVERY_TIMEOUT,
        email_service.send_outbox_email_with_control(
            job.kind,
            &job.recipient,
            token,
            &message_id,
            None,
        ),
    )
    .await;

    match delivery {
        Ok(Ok(())) => match outbox
            .repository()
            .mark_sent(&job.id, &worker_id, job.lock_version)
            .await
        {
            Ok(true) => record_completion(kind, "sent", started),
            Ok(false) => {
                warn!(job_id = %job.id, "Email outbox acknowledgement lost its lease fence");
                record_completion(kind, "fenced", started);
            }
            Err(error) => {
                error!(job_id = %job.id, error = %error, "Email was accepted by SMTP but acknowledgement failed");
                record_completion(kind, "ack_failed", started);
            }
        },
        Ok(Err(error)) => {
            let retryable = matches!(
                error,
                synctv_core::Error::ServiceUnavailable(_)
                    | synctv_core::Error::Timeout(_)
                    | synctv_core::Error::Internal(_)
            );
            finish_failed(
                &failure_context,
                &job,
                Some(&payload),
                "smtp delivery failed",
                retryable,
            )
            .await;
            record_completion(kind, if retryable { "retry" } else { "dead" }, started);
        }
        Err(_) => {
            finish_failed(
                &failure_context,
                &job,
                Some(&payload),
                "smtp delivery timed out",
                true,
            )
            .await;
            record_completion(kind, "retry", started);
        }
    }
}

fn token_for_job<'a>(
    job: &EmailOutboxJob,
    payload: &'a EmailOutboxPayload,
) -> Result<&'a str, &'static str> {
    match (job.kind, payload) {
        (
            EmailOutboxKind::PasswordReset,
            EmailOutboxPayload::Token {
                token, token_type, ..
            },
        ) if *token_type == i16::from(EmailTokenType::PasswordReset) => Ok(token),
        (
            EmailOutboxKind::EmailLogin,
            EmailOutboxPayload::Token {
                token, token_type, ..
            },
        ) if *token_type == i16::from(EmailTokenType::EmailLogin) => Ok(token),
        (EmailOutboxKind::EmailBind, EmailOutboxPayload::Bind { token, .. })
        | (EmailOutboxKind::EmailRegistration, EmailOutboxPayload::Registration { token, .. }) => {
            Ok(token)
        }
        _ => Err("job kind and encrypted payload differ"),
    }
}

struct FailureContext<'a> {
    outbox: &'a EmailOutboxService,
    email_token_service: &'a EmailTokenService,
    user_service: &'a UserService,
    worker_id: &'a str,
}

async fn finish_failed(
    context: &FailureContext<'_>,
    job: &EmailOutboxJob,
    payload: Option<&EmailOutboxPayload>,
    safe_error: &str,
    retryable: bool,
) {
    match context
        .outbox
        .repository()
        .mark_failed(job, context.worker_id, safe_error, retryable)
        .await
    {
        Ok(Some(EmailOutboxStatus::Dead)) => {
            if let Some(payload) = payload {
                cleanup_dead_job(
                    context.outbox,
                    context.email_token_service,
                    context.user_service,
                    job,
                    payload,
                )
                .await;
            } else if let Err(error) = context
                .outbox
                .repository()
                .mark_cleanup_completed(&job.id)
                .await
            {
                warn!(job_id = %job.id, error = %error, "Failed to finish irrecoverable email cleanup state");
            }
        }
        Ok(Some(EmailOutboxStatus::Pending)) => {}
        Ok(Some(_)) => {
            error!(job_id = %job.id, "Email outbox failure produced an invalid status transition");
        }
        Ok(None) => {
            warn!(job_id = %job.id, "Email outbox failure update lost its lease fence");
        }
        Err(error) => {
            error!(job_id = %job.id, error = %error, "Failed to persist email delivery failure");
        }
    }
}

async fn cleanup_dead_job(
    outbox: &EmailOutboxService,
    email_token_service: &EmailTokenService,
    user_service: &UserService,
    job: &EmailOutboxJob,
    payload: &EmailOutboxPayload,
) {
    let result = match payload {
        EmailOutboxPayload::Token {
            token,
            user_id,
            token_type,
            ..
        } => match EmailTokenType::try_from(*token_type) {
            Ok(token_type) => {
                email_token_service
                    .invalidate_specific_token(token, user_id, token_type)
                    .await
            }
            Err(error) => Err(synctv_core::Error::Internal(error)),
        },
        EmailOutboxPayload::Registration { token, .. } => user_service
            .delete_unused_email_registration_token(token)
            .await
            .map(|_| ()),
        EmailOutboxPayload::Bind {
            token,
            user_id,
            email,
            ..
        } => user_service
            .delete_pending_email_bind(user_id, email, token)
            .await
            .map(|_| ()),
    };
    match result {
        Ok(()) => {
            if let Err(error) = outbox.repository().mark_cleanup_completed(&job.id).await {
                warn!(job_id = %job.id, error = %error, "Failed to record email cleanup completion");
            }
        }
        Err(error) => {
            warn!(job_id = %job.id, error = %error, "Email dead-letter cleanup will be retried by the leader");
        }
    }
}

async fn run_maintenance(
    outbox: &EmailOutboxService,
    email_token_service: &EmailTokenService,
    user_service: &UserService,
) {
    match outbox
        .repository()
        .requeue_stale_processing(PROCESSING_STALE_AFTER_SECS)
        .await
    {
        Ok(count) if count > 0 => info!(count, "Requeued stale email outbox leases"),
        Ok(_) => {}
        Err(error) => warn!(error = %error, "Failed to requeue stale email outbox leases"),
    }
    if let Err(error) = outbox.repository().expire_pending().await {
        warn!(error = %error, "Failed to expire email outbox jobs");
    }
    match outbox
        .repository()
        .load_cleanup_pending(CLEANUP_BATCH_SIZE)
        .await
    {
        Ok(jobs) => {
            for job in jobs {
                match outbox.decrypt_payload(&job.encrypted_payload) {
                    Ok(payload) => {
                        cleanup_dead_job(outbox, email_token_service, user_service, &job, &payload)
                            .await;
                    }
                    Err(error) => {
                        error!(job_id = %job.id, error = %error, "Dead email job payload is irrecoverable");
                        if let Err(mark_error) =
                            outbox.repository().mark_cleanup_completed(&job.id).await
                        {
                            warn!(job_id = %job.id, error = %mark_error, "Failed to close irrecoverable cleanup job");
                        }
                    }
                }
            }
        }
        Err(error) => warn!(error = %error, "Failed to load email cleanup jobs"),
    }
    if let Err(error) = outbox
        .repository()
        .purge_terminal(TERMINAL_RETENTION_SECS)
        .await
    {
        warn!(error = %error, "Failed to purge terminal email outbox jobs");
    }
    update_queue_depth(outbox).await;
}

async fn update_queue_depth(outbox: &EmailOutboxService) {
    match outbox.repository().pending_count().await {
        Ok(count) => EMAIL_DELIVERY_QUEUE_DEPTH.set(count),
        Err(error) => debug!(error = %error, "Failed to refresh email queue depth metric"),
    }
}

fn record_completion(kind: &str, status: &str, started: Instant) {
    EMAIL_DELIVERY_IN_FLIGHT.dec();
    EMAIL_DELIVERY_JOBS_TOTAL
        .with_label_values(&[kind, status])
        .inc();
    EMAIL_DELIVERY_DURATION_SECONDS
        .with_label_values(&[kind, status])
        .observe(started.elapsed().as_secs_f64());
}

#[cfg(test)]
mod tests {
    use super::should_run_maintenance;
    use tokio::time::Instant;

    #[test]
    fn maintenance_is_due_only_on_the_leader() {
        let now = Instant::now();
        assert!(should_run_maintenance(true, now, now));
        assert!(!should_run_maintenance(false, now, now));
        assert!(!should_run_maintenance(
            true,
            now,
            now + std::time::Duration::from_secs(1)
        ));
    }
}
