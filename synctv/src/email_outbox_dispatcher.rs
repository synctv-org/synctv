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
const JOB_PROCESSING_TIMEOUT: Duration = Duration::from_secs(90);
const LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(30);
const LISTENER_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);

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
    let mut listener = ListenerState::connect(outbox.repository(), &worker_id).await;
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
                () = listener.wait_for_signal(outbox.repository()) => {}
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

struct ListenerState {
    listener: Option<sqlx::postgres::PgListener>,
    reconnect_delay: Duration,
    jitter: Duration,
}

impl ListenerState {
    async fn connect(
        repository: &synctv_core::repository::EmailOutboxRepository,
        worker_id: &str,
    ) -> Self {
        Self {
            listener: connect_listener(repository).await,
            reconnect_delay: IDLE_POLL_INTERVAL,
            jitter: Duration::from_millis(
                worker_id
                    .bytes()
                    .fold(0_u64, |sum, byte| sum + u64::from(byte))
                    % 251,
            ),
        }
    }

    async fn wait_for_signal(
        &mut self,
        repository: &synctv_core::repository::EmailOutboxRepository,
    ) {
        let Some(pg_listener) = self.listener.as_mut() else {
            tokio::time::sleep(self.reconnect_delay + self.jitter).await;
            self.listener = connect_listener(repository).await;
            if self.listener.is_some() {
                self.reconnect_delay = IDLE_POLL_INTERVAL;
            } else {
                self.reconnect_delay = next_reconnect_delay(self.reconnect_delay);
            }
            return;
        };

        match tokio::time::timeout(IDLE_POLL_INTERVAL, pg_listener.recv()).await {
            Ok(Ok(notification)) => {
                debug!(
                    job_id = notification.payload(),
                    "Email outbox notification received"
                );
            }
            Ok(Err(error)) => {
                warn!(error = %error, "Email outbox listener failed; polling remains active");
                self.listener = None;
            }
            Err(_) => {}
        }
    }
}

fn next_reconnect_delay(current: Duration) -> Duration {
    current.saturating_mul(2).min(LISTENER_RECONNECT_MAX_DELAY)
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

    let heartbeat = wait_until_lease_lost(
        &outbox,
        &job.id,
        &worker_id,
        job.lock_version,
        LEASE_RENEW_INTERVAL,
    );
    tokio::pin!(heartbeat);

    let status = tokio::select! {
        biased;
        () = &mut heartbeat => "fenced",
        result = tokio::time::timeout(
            JOB_PROCESSING_TIMEOUT,
            dispatch_job_inner(
                &outbox,
                &email_service,
                &email_token_service,
                &user_service,
                &worker_id,
                &job,
            ),
        ) => if let Ok(status) = result {
            status
        } else {
                warn!(job_id = %job.id, "Email outbox processing exceeded its deadline");
                let context = FailureContext {
                    outbox: &outbox,
                    email_token_service: &email_token_service,
                    user_service: &user_service,
                    worker_id: &worker_id,
                };
                finish_failed(
                    &context,
                    &job,
                    None,
                    "delivery processing timed out",
                    true,
                )
                .await
                .metric_status()
        },
    };

    record_completion(kind, status, started);
}

async fn dispatch_job_inner(
    outbox: &EmailOutboxService,
    email_service: &EmailService,
    email_token_service: &EmailTokenService,
    user_service: &UserService,
    worker_id: &str,
    job: &EmailOutboxJob,
) -> &'static str {
    let failure_context = FailureContext {
        outbox,
        email_token_service,
        user_service,
        worker_id,
    };

    let payload = match outbox.decrypt_payload(job) {
        Ok(payload) => payload,
        Err(error) => {
            error!(job_id = %job.id, error = %error, "Email outbox payload could not be decrypted");
            let outcome = finish_failed(
                &failure_context,
                job,
                None,
                "invalid encrypted payload",
                false,
            )
            .await;
            return outcome.metric_status();
        }
    };

    let token = match token_for_job(job, &payload) {
        Ok(token) => token,
        Err(message) => {
            error!(job_id = %job.id, reason = message, "Email outbox payload does not match job kind");
            let outcome = finish_failed(
                &failure_context,
                job,
                Some(&payload),
                "payload kind mismatch",
                false,
            )
            .await;
            return outcome.metric_status();
        }
    };

    match delivery_token_is_active(email_token_service, user_service, &payload).await {
        Ok(true) => {}
        Ok(false) => {
            let outcome = finish_failed(
                &failure_context,
                job,
                Some(&payload),
                "delivery token was superseded or consumed",
                false,
            )
            .await;
            let status = if outcome == FailureOutcome::Dead {
                "superseded"
            } else {
                outcome.metric_status()
            };
            return status;
        }
        Err(error) => {
            warn!(job_id = %job.id, error = %error, "Failed to verify email delivery token state");
            let outcome = finish_failed(
                &failure_context,
                job,
                Some(&payload),
                "delivery token state check failed",
                true,
            )
            .await;
            return outcome.metric_status();
        }
    }

    match outbox
        .repository()
        .renew_lease(&job.id, worker_id, job.lock_version)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            warn!(job_id = %job.id, "Email outbox delivery lost its lease before SMTP");
            return "fenced";
        }
        Err(error) => {
            warn!(job_id = %job.id, error = %error, "Email outbox lease could not be refreshed before SMTP");
            return "lease_refresh_failed";
        }
    }

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
            .mark_sent(&job.id, worker_id, job.lock_version)
            .await
        {
            Ok(true) => "sent",
            Ok(false) => {
                warn!(job_id = %job.id, "Email outbox acknowledgement lost its lease fence");
                "fenced"
            }
            Err(error) => {
                error!(job_id = %job.id, error = %error, "Email was accepted by SMTP but acknowledgement failed");
                "ack_failed"
            }
        },
        Ok(Err(error)) => {
            let retryable = matches!(
                error,
                synctv_core::Error::ServiceUnavailable(_)
                    | synctv_core::Error::Timeout(_)
                    | synctv_core::Error::Internal(_)
            );
            let outcome = finish_failed(
                &failure_context,
                job,
                Some(&payload),
                "smtp delivery failed",
                retryable,
            )
            .await;
            outcome.metric_status()
        }
        Err(_) => {
            let outcome = finish_failed(
                &failure_context,
                job,
                Some(&payload),
                "smtp delivery timed out",
                true,
            )
            .await;
            outcome.metric_status()
        }
    }
}

async fn wait_until_lease_lost(
    outbox: &EmailOutboxService,
    job_id: &str,
    worker_id: &str,
    lock_version: i64,
    renew_interval: Duration,
) {
    let first_renewal = Instant::now() + renew_interval;
    let mut interval = tokio::time::interval_at(first_renewal, renew_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        match outbox
            .repository()
            .renew_lease(job_id, worker_id, lock_version)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                warn!(job_id = %job_id, "Email outbox lease heartbeat was fenced");
                return;
            }
            Err(error) => {
                warn!(job_id = %job_id, error = %error, "Email outbox lease heartbeat failed");
            }
        }
    }
}

async fn delivery_token_is_active(
    email_token_service: &EmailTokenService,
    user_service: &UserService,
    payload: &EmailOutboxPayload,
) -> synctv_core::Result<bool> {
    match payload {
        EmailOutboxPayload::Token {
            token,
            user_id,
            token_type,
            ..
        } => {
            let token_type =
                EmailTokenType::try_from(*token_type).map_err(synctv_core::Error::Internal)?;
            email_token_service
                .is_token_active(token, user_id, token_type)
                .await
        }
        EmailOutboxPayload::Registration { token, .. } => {
            user_service.is_email_registration_token_active(token).await
        }
        EmailOutboxPayload::Bind {
            token,
            user_id,
            email,
            ..
        } => {
            user_service
                .is_email_bind_token_active(user_id, email, token)
                .await
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureOutcome {
    Retry,
    Dead,
    Fenced,
    PersistFailed,
}

impl FailureOutcome {
    const fn metric_status(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Dead => "dead",
            Self::Fenced => "fenced",
            Self::PersistFailed => "persist_failed",
        }
    }
}

async fn finish_failed(
    context: &FailureContext<'_>,
    job: &EmailOutboxJob,
    payload: Option<&EmailOutboxPayload>,
    safe_error: &str,
    retryable: bool,
) -> FailureOutcome {
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
            FailureOutcome::Dead
        }
        Ok(Some(EmailOutboxStatus::Pending)) => FailureOutcome::Retry,
        Ok(Some(_)) => {
            error!(job_id = %job.id, "Email outbox failure produced an invalid status transition");
            FailureOutcome::PersistFailed
        }
        Ok(None) => {
            warn!(job_id = %job.id, "Email outbox failure update lost its lease fence");
            FailureOutcome::Fenced
        }
        Err(error) => {
            error!(job_id = %job.id, error = %error, "Failed to persist email delivery failure");
            FailureOutcome::PersistFailed
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
    match outbox.repository().close_unknown_dead_cleanup().await {
        Ok(count) if count > 0 => {
            warn!(count, "Closed cleanup for email jobs with unknown kinds");
        }
        Ok(_) => {}
        Err(error) => warn!(error = %error, "Failed to close unknown email job cleanup"),
    }
    match outbox
        .repository()
        .load_cleanup_pending(CLEANUP_BATCH_SIZE)
        .await
    {
        Ok(jobs) => {
            for job in jobs {
                match outbox.decrypt_payload(&job) {
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
    use super::{
        next_reconnect_delay, should_run_maintenance, wait_until_lease_lost, FailureOutcome,
    };
    use chrono::{Duration as ChronoDuration, Utc};
    use std::time::Duration;
    use synctv_core::{
        repository::{EmailOutboxKind, NewEmailOutboxJob},
        service::EmailOutboxService,
    };
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

    #[test]
    fn listener_reconnect_delay_is_capped() {
        assert_eq!(
            next_reconnect_delay(Duration::from_secs(1)),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_reconnect_delay(Duration::from_secs(20)),
            Duration::from_secs(30)
        );
        assert_eq!(
            next_reconnect_delay(Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn failure_metrics_follow_persisted_outcome() {
        assert_eq!(FailureOutcome::Retry.metric_status(), "retry");
        assert_eq!(FailureOutcome::Dead.metric_status(), "dead");
        assert_eq!(FailureOutcome::Fenced.metric_status(), "fenced");
        assert_eq!(
            FailureOutcome::PersistFailed.metric_status(),
            "persist_failed"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker (testcontainers)"]
    async fn cancelling_lease_heartbeat_allows_stale_job_recovery() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let outbox = std::sync::Arc::new(
            EmailOutboxService::new(pool.clone(), &"ab".repeat(32)).expect("create outbox"),
        );
        let now = Utc::now();
        outbox
            .repository()
            .insert(&NewEmailOutboxJob {
                id: "cancelled-heartbeat".to_string(),
                kind: EmailOutboxKind::EmailLogin,
                recipient: "test@example.com".to_string(),
                encrypted_payload: "test-payload".to_string(),
                dedupe_key: "cancelled-heartbeat".to_string(),
                attempts: 0,
                next_attempt_at: now,
                lock_version: 0,
                expires_at: now + ChronoDuration::hours(1),
                created_at: now,
            })
            .await
            .expect("insert job");
        let claimed = outbox
            .repository()
            .claim_batch("worker-a", 1)
            .await
            .expect("claim job")
            .remove(0);

        let heartbeat_outbox = outbox.clone();
        let heartbeat = tokio::spawn(async move {
            wait_until_lease_lost(
                &heartbeat_outbox,
                &claimed.id,
                "worker-a",
                claimed.lock_version,
                Duration::from_millis(20),
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        heartbeat.abort();
        heartbeat.await.expect_err("heartbeat should be cancelled");

        sqlx::query("UPDATE email_outbox SET locked_at = NOW() - INTERVAL '3 minutes'")
            .execute(&pool)
            .await
            .expect("age lease");
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            outbox
                .repository()
                .requeue_stale_processing(120)
                .await
                .expect("requeue stale job"),
            1
        );
    }
}
