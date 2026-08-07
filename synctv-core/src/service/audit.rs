//! Audit logging service
//!
//! Tracks admin actions and permission changes for compliance and debugging.
//!
//! ## Async Buffering
//!
//! Audit events are buffered in memory via a `tokio::sync::mpsc` channel and
//! flushed to the database in batches (every 5 seconds or when 100 events
//! accumulate). This decouples the request path from database write latency.
//! On graceful shutdown the remaining buffer is flushed.
//!
//! # Audit Logging Design Tradeoff
//!
//! ## Availability vs Consistency
//!
//! This service chooses **availability over consistency** for audit logging.
//! When audit log writes fail, operations are **not blocked**.
//!
//! ## Rationale
//!
//! - Audit logs are important for compliance and debugging
//! - Blocking user operations for audit failures would hurt availability
//! - Failed audits are logged at ERROR level for monitoring
//! - Monitoring should alert on audit failures
//!
//! ## Failure Scenarios
//!
//! 1. **Buffer full**: Falls back to synchronous write; if that also fails,
//!    logs ERROR and increments dropped counter
//! 2. **Database unavailable**: Regular flushes retain the batch for the next
//!    interval. Shutdown performs three final attempts before reporting any
//!    unpersisted records
//! 3. **Channel closed**: Logs ERROR and continues operation
//!
//! ## Monitoring
//!
//! Operators should monitor for ERROR logs indicating audit failures:
//! - "Audit sync fallback write also failed, event dropped"
//! - "Failed to flush audit batch; retaining events for retry"
//!
//! ## Recovery Strategies
//!
//! - **Temporary database outage**: Events are retried with backoff; most
//!   outages are recovered automatically
//! - **Extended outage**: Shutdown reports unpersisted events after the final
//!   retry window; check application logs and investigate database connectivity
//! - **Buffer overflow**: Indicates high audit volume; consider increasing
//!   buffer capacity via configuration

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::{
    models::{AuditAction, AuditDetails, AuditTargetType},
    repository::JsonbArray,
    Result,
};

/// Default channel capacity for the audit event buffer
const DEFAULT_BUFFER_CAPACITY: usize = 10_000;
/// Maximum number of events to accumulate before flushing
const FLUSH_BATCH_SIZE: usize = 100;
/// Flush interval in seconds
const FLUSH_INTERVAL_SECS: u64 = 5;

/// Audit log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLog {
    pub id: String,
    pub actor_id: String,
    pub actor_username: String,
    pub action: AuditAction,
    pub target_type: AuditTargetType,
    pub target_id: Option<String>,
    pub details: AuditDetails,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Parameters for logging an audit event
#[derive(Debug, Clone)]
pub struct AuditEventParams {
    pub actor_id: String,
    pub actor_username: String,
    pub action: AuditAction,
    pub target_type: AuditTargetType,
    pub target_id: Option<String>,
    pub details: AuditDetails,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StreamKickAuditRequest {
    pub actor_id: String,
    pub actor_username: String,
    pub room_id: String,
    pub media_id: String,
    pub reason: Option<String>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

fn optional_reason(reason: Option<&str>) -> Option<String> {
    reason
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Internal record sent through the mpsc channel
#[derive(Debug, Clone)]
struct AuditRecord {
    actor_id: String,
    actor_username: String,
    action: AuditAction,
    target_type: AuditTargetType,
    target_id: Option<String>,
    details: AuditDetails,
    ip_address: Option<String>,
    user_agent: Option<String>,
    created_at: DateTime<Utc>,
}

/// Audit logging service
///
/// Records audit logs for security-relevant actions. Events are buffered in
/// memory and flushed to the database in batches for performance.
pub struct AuditService {
    writer: AuditLogWriter,
    /// Sender half of the buffered channel (None when running without background task)
    sender: Option<mpsc::Sender<AuditRecord>>,
    /// Counter of dropped events (channel full)
    dropped_count: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct AuditLogWriter {
    pool: PgPool,
}

impl AuditLogWriter {
    fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn write_single(&self, record: &AuditRecord) -> Result<()> {
        sqlx::query!(
            r"
            INSERT INTO audit_logs (
                actor_id, actor_username, action, target_type, target_id,
                details, ip_address, user_agent, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ",
            parse_actor_id_for_storage(&record.actor_id),
            record.actor_username,
            record.action.as_i16(),
            record.target_type.as_i16(),
            record.target_id.as_deref(),
            &record.details as _,
            record.ip_address.as_deref(),
            record.user_agent.as_deref(),
            record.created_at
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn flush_batch(
        &self,
        buffer: &mut Vec<AuditRecord>,
        dropped_count: &AtomicUsize,
    ) -> bool {
        flush_batch(&self.pool, buffer, dropped_count).await
    }
}

impl AuditService {
    /// Create a new audit service with async buffering.
    ///
    /// Spawns a background task that flushes buffered audit events to the
    /// database every 5 seconds or when 100 events accumulate.
    ///
    /// The returned [`AuditFlushHandle`] must be held for the lifetime of
    /// the service. Dropping it triggers a graceful flush of remaining events.
    #[must_use]
    pub fn new(pool: PgPool) -> (Self, AuditFlushHandle) {
        Self::new_with_capacity(pool, DEFAULT_BUFFER_CAPACITY)
    }

    /// Create a new audit service with async buffering and a custom buffer capacity.
    ///
    /// Use this to override the default buffer capacity (10,000) via configuration.
    /// A capacity of 0 falls back to `DEFAULT_BUFFER_CAPACITY`.
    #[must_use]
    pub fn new_with_capacity(pool: PgPool, capacity: usize) -> (Self, AuditFlushHandle) {
        let capacity = if capacity > 0 {
            capacity
        } else {
            DEFAULT_BUFFER_CAPACITY
        };
        let (tx, rx) = mpsc::channel(capacity);
        let dropped_count = Arc::new(AtomicUsize::new(0));

        let writer = AuditLogWriter::new(pool);
        let handle = AuditFlushHandle::spawn(writer.clone(), rx, Arc::clone(&dropped_count));

        let service = Self {
            writer,
            sender: Some(tx),
            dropped_count,
        };

        (service, handle)
    }

    /// Create a new audit service **without** async buffering.
    ///
    /// Each call to `log()` writes directly to the database. Useful for tests
    /// or environments where a background task is not desired.
    #[must_use]
    pub fn new_unbuffered(pool: PgPool) -> Self {
        Self {
            writer: AuditLogWriter::new(pool),
            sender: None,
            dropped_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn log(&self, params: AuditEventParams) -> Result<()> {
        let AuditEventParams {
            actor_id,
            actor_username,
            action,
            target_type,
            target_id,
            details,
            ip_address,
            user_agent,
        } = params;
        let action_str = action.as_str();
        let target_str = target_type.as_str();
        let created_at = crate::SystemClock.now();

        // If we have a buffered sender, enqueue the event
        if let Some(ref sender) = self.sender {
            let record = AuditRecord {
                actor_id: actor_id.clone(),
                actor_username: actor_username.clone(),
                action,
                target_type,
                target_id: target_id.clone(),
                details: details.clone(),
                ip_address: ip_address.clone(),
                user_agent: user_agent.clone(),
                created_at,
            };

            if let Err(send_err) = sender.try_send(record) {
                // Buffer full: fall back to synchronous DB write instead of dropping.
                // Recover the rejected record from the error instead of rebuilding it.
                let record = send_err.into_inner();
                tracing::warn!(
                    actor_id = %actor_id,
                    action = %action_str,
                    "Audit buffer full, falling back to synchronous DB write"
                );
                if let Err(db_err) = self.writer.write_single(&record).await {
                    self.dropped_count.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(
                        actor_id = %actor_id,
                        action = %action_str,
                        error = %db_err,
                        "Audit sync fallback write also failed, event dropped"
                    );
                }
            } else {
                tracing::debug!(
                    actor_id = %actor_id,
                    action = %action_str,
                    target_type = %target_str,
                    "Audit event buffered"
                );
            }

            return Ok(());
        }

        // Unbuffered mode: write directly to DB
        self.writer
            .write_single(&AuditRecord {
                actor_id: actor_id.clone(),
                actor_username: actor_username.clone(),
                action,
                target_type,
                target_id: target_id.clone(),
                details: details.clone(),
                ip_address: ip_address.clone(),
                user_agent: user_agent.clone(),
                created_at,
            })
            .await?;

        tracing::debug!(
            actor_id = %actor_id,
            action = %action_str,
            target_type = %target_str,
            "Audit log recorded"
        );

        Ok(())
    }

    /// Log user creation
    pub async fn log_user_created(
        &self,
        actor_id: String,
        actor_username: String,
        target_user_id: String,
    ) -> Result<()> {
        self.log(AuditEventParams {
            actor_id,
            actor_username,
            action: AuditAction::UserCreated,
            target_type: AuditTargetType::User,
            target_id: Some(target_user_id),
            details: AuditDetails::reason("User created via admin panel"),
            ip_address: None,
            user_agent: None,
        })
        .await
    }

    /// Log user ban
    pub async fn log_user_banned(
        &self,
        actor_id: String,
        actor_username: String,
        target_user_id: String,
    ) -> Result<()> {
        self.log(AuditEventParams {
            actor_id,
            actor_username,
            action: AuditAction::UserBanned,
            target_type: AuditTargetType::User,
            target_id: Some(target_user_id),
            details: AuditDetails::reason("User banned by admin"),
            ip_address: None,
            user_agent: None,
        })
        .await
    }

    /// Log room deletion
    pub async fn log_room_deleted(
        &self,
        actor_id: String,
        actor_username: String,
        room_id: String,
    ) -> Result<()> {
        self.log(AuditEventParams {
            actor_id,
            actor_username,
            action: AuditAction::RoomDeleted,
            target_type: AuditTargetType::Room,
            target_id: Some(room_id),
            details: AuditDetails::reason("Room deleted by admin"),
            ip_address: None,
            user_agent: None,
        })
        .await
    }

    pub async fn log_stream_kicked(&self, request: StreamKickAuditRequest) -> Result<()> {
        let target_id = format!("{}:{}", request.room_id, request.media_id);
        self.log(AuditEventParams {
            actor_id: request.actor_id,
            actor_username: request.actor_username,
            action: AuditAction::StreamKicked,
            target_type: AuditTargetType::Stream,
            target_id: Some(target_id),
            details: AuditDetails {
                room_id: Some(request.room_id),
                media_id: Some(request.media_id),
                reason: optional_reason(request.reason.as_deref()),
                ..Default::default()
            },
            ip_address: request.ip_address,
            user_agent: request.user_agent,
        })
        .await
    }

    /// Log rate limit reset failure event.
    ///
    /// This is used when a password verification succeeds but the rate limit
    /// counter reset fails (e.g., Redis unavailable). This is security-relevant
    /// because it could lead to legitimate users being locked out if the counter
    /// persists after successful authentication.
    pub async fn log_rate_limit_reset_failed(
        &self,
        target_type: AuditTargetType,
        target_id: String,
        error_message: String,
        ip_address: Option<String>,
    ) -> Result<()> {
        self.log(AuditEventParams {
            actor_id: "system".to_string(),
            actor_username: "system".to_string(),
            action: AuditAction::RateLimitResetFailed,
            target_type,
            target_id: Some(target_id),
            details: AuditDetails {
                error: Some(error_message),
                context: Some("password_verification_succeeded".to_string()),
                ..Default::default()
            },
            ip_address,
            user_agent: None,
        })
        .await
    }

    /// Log a user logout event.
    ///
    /// Records when a user explicitly logs out (access token blacklisted).
    pub async fn log_user_logout(
        &self,
        user_id: String,
        username: String,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<()> {
        self.log(user_logout_event_params(
            user_id, username, ip_address, user_agent,
        ))
        .await
    }
}

fn user_logout_event_params(
    user_id: String,
    username: String,
    ip_address: Option<String>,
    user_agent: Option<String>,
) -> AuditEventParams {
    AuditEventParams {
        actor_id: user_id.clone(),
        actor_username: username,
        action: AuditAction::UserLogout,
        target_type: AuditTargetType::User,
        target_id: Some(user_id),
        details: AuditDetails::default(),
        ip_address,
        user_agent,
    }
}

impl std::fmt::Debug for AuditService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditService")
            .field("buffered", &self.sender.is_some())
            .field("dropped_count", &self.dropped_count.load(Ordering::Relaxed))
            .finish()
    }
}

/// Handle for the background flush task.
///
/// Dropping this handle signals the background task to flush remaining events
/// and shut down gracefully.
pub struct AuditFlushHandle {
    join_handle: Option<tokio::task::JoinHandle<bool>>,
    abort_handle: tokio::task::AbortHandle,
    /// Sender kept alive to control channel lifetime. Dropping it causes the
    /// background receiver loop to terminate after draining remaining items.
    cancel_tx: tokio::sync::watch::Sender<bool>,
    /// Guards against sending the shutdown signal more than once (e.g. when
    /// both `shutdown()` and `Drop` are executed for the same handle).
    has_signaled: AtomicBool,
}

impl AuditFlushHandle {
    fn spawn(
        writer: AuditLogWriter,
        mut rx: mpsc::Receiver<AuditRecord>,
        dropped_count: Arc<AtomicUsize>,
    ) -> Self {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);

        let join_handle = crate::spawn::spawn_monitored("audit_flush", async move {
            let mut buffer: Vec<AuditRecord> = Vec::with_capacity(FLUSH_BATCH_SIZE);
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(FLUSH_INTERVAL_SECS));
            // Don't fire immediately on first tick
            interval.tick().await;

            loop {
                tokio::select! {
                // Receive events
                                   maybe_record = rx.recv() => {
                                       if let Some(record) = maybe_record {
                                           buffer.push(record);
                                           if buffer.len() == FLUSH_BATCH_SIZE {
                                               let _ = writer.flush_batch(&mut buffer, &dropped_count).await;
                                           }
                                       } else {
                // Channel closed, flush remaining and exit
                                           let flushed = flush_with_retries(&writer, &mut buffer, &dropped_count).await;
                                           tracing::info!("Audit flush task: channel closed, exiting");
                                           return flushed;
                                       }
                                   }
                // Periodic flush
                                   _ = interval.tick() => {
                                       if !buffer.is_empty() {
                                           let _ = writer.flush_batch(&mut buffer, &dropped_count).await;
                                       }
                                   }
                // Graceful shutdown signal
                                   _ = cancel_rx.changed() => {
                // Drain remaining items from the channel
                                       rx.close();
                                       while let Some(record) = rx.recv().await {
                                           buffer.push(record);
                                       }
                                       let flushed = flush_with_retries(&writer, &mut buffer, &dropped_count).await;
                                       tracing::info!("Audit flush task: graceful shutdown complete");
                                       return flushed;
                                   }
                               }
            }
        });

        let abort_handle = join_handle.abort_handle();
        Self {
            join_handle: Some(join_handle),
            abort_handle,
            cancel_tx,
            has_signaled: AtomicBool::new(false),
        }
    }

    /// Send the shutdown signal exactly once.
    ///
    /// Returns `true` if this call was the first to send the signal, `false`
    /// if the signal had already been sent (by a previous `shutdown()` or by
    /// `Drop`).
    fn send_shutdown_signal(&self) -> bool {
        // compare_exchange: set true only if currently false.
        // Use AcqRel / Acquire for proper ordering.
        if self
            .has_signaled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            if self.cancel_tx.send(true).is_err() {
                tracing::debug!("Audit flush shutdown signal had no active receiver");
            }
            true
        } else {
            false
        }
    }

    /// Trigger graceful shutdown and wait for the flush to complete.
    pub async fn shutdown(mut self) -> bool {
        self.send_shutdown_signal();
        if let Some(handle) = self.join_handle.take() {
            match handle.await {
                Ok(true) => return true,
                Ok(false) => {
                    tracing::warn!("Audit flush task could not persist all buffered events");
                    return false;
                }
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "Audit flush task ended with join error during shutdown"
                    );
                }
            }
        }
        false
    }
}

impl Drop for AuditFlushHandle {
    fn drop(&mut self) {
        // A plain drop starts a detached graceful flush. If an in-progress
        // shutdown future is cancelled, the second signal attempt identifies
        // that state and aborts the detached task.
        if !self.send_shutdown_signal() {
            self.abort_handle.abort();
        }
    }
}

fn parse_actor_id_for_storage(actor_id: &str) -> Option<i64> {
    actor_id.parse::<i64>().ok().filter(|id| *id > 0)
}

async fn flush_with_retries(
    writer: &AuditLogWriter,
    buffer: &mut Vec<AuditRecord>,
    dropped_count: &AtomicUsize,
) -> bool {
    if buffer.is_empty() {
        return true;
    }

    for attempt in 0..3 {
        if writer.flush_batch(buffer, dropped_count).await {
            return true;
        }
        if attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    false
}

/// Flush a batch of audit records to the database.
async fn flush_batch(
    pool: &PgPool,
    buffer: &mut Vec<AuditRecord>,
    dropped_count: &AtomicUsize,
) -> bool {
    let batch_size = buffer.len();
    tracing::debug!(batch_size = batch_size, "Flushing audit event batch");

    // Build a batch insert using UNNEST for efficiency
    let mut valid_records = Vec::with_capacity(batch_size);
    let mut actor_ids: Vec<i64> = Vec::with_capacity(batch_size);
    let mut actor_usernames = Vec::with_capacity(batch_size);
    let mut actions = Vec::with_capacity(batch_size);
    let mut target_types = Vec::with_capacity(batch_size);
    let mut target_ids: Vec<String> = Vec::with_capacity(batch_size);
    let mut details_list = JsonbArray::<AuditDetails>::with_capacity(batch_size);
    let mut ip_addresses: Vec<String> = Vec::with_capacity(batch_size);
    let mut user_agents: Vec<String> = Vec::with_capacity(batch_size);
    let mut created_ats: Vec<DateTime<Utc>> = Vec::with_capacity(batch_size);

    let pending_records = std::mem::take(buffer);
    for record in pending_records {
        if let Err(error) = details_list.push(&record.details) {
            tracing::error!(
                error = %error,
                "Failed to serialize audit details for batch insert"
            );
            dropped_count.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        valid_records.push(record);
        let record = valid_records.last().expect("record was just inserted");
        actor_ids.push(parse_actor_id_for_storage(&record.actor_id).unwrap_or(0));
        actor_usernames.push(record.actor_username.clone());
        actions.push(record.action.as_i16());
        target_types.push(record.target_type.as_i16());
        target_ids.push(record.target_id.clone().unwrap_or_default());
        ip_addresses.push(record.ip_address.clone().unwrap_or_default());
        user_agents.push(record.user_agent.clone().unwrap_or_default());
        created_ats.push(record.created_at);
    }

    match sqlx::query!(
        r#"
        INSERT INTO audit_logs (
            actor_id, actor_username, action, target_type, target_id,
            details, ip_address, user_agent, created_at
        )
        SELECT NULLIF(actor_id, 0)::bigint,
               actor_username::text,
               action::smallint,
               target_type::smallint,
               NULLIF(target_id, '')::text,
               details::jsonb,
               NULLIF(ip_address, '')::text,
               NULLIF(user_agent, '')::text,
               created_at::timestamptz
        FROM UNNEST(
            $1::bigint[],
            $2::text[],
            $3::smallint[],
            $4::smallint[],
            $5::text[],
            $6::jsonb[],
            $7::text[],
            $8::text[],
            $9::timestamptz[]
        ) AS t(
            actor_id,
            actor_username,
            action,
            target_type,
            target_id,
            details,
            ip_address,
            user_agent,
            created_at
        )
        "#,
        &actor_ids,
        &actor_usernames,
        &actions,
        &target_types,
        &target_ids,
        details_list.as_slice(),
        &ip_addresses,
        &user_agents,
        &created_ats,
    )
    .execute(pool)
    .await
    {
        Ok(_) => {
            tracing::debug!(batch_size = batch_size, "Audit batch flushed successfully");
        }
        Err(e) => {
            tracing::error!(
                batch_size = valid_records.len(),
                error = %e,
                "Failed to flush audit batch; retaining events for retry"
            );
            buffer.extend(valid_records);
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_flush_handle_allows_graceful_task_completion() {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let completed = Arc::new(AtomicBool::new(false));
        let completed_in_task = Arc::clone(&completed);
        let join_handle = tokio::spawn(async move {
            cancel_rx.changed().await.expect("shutdown sender is alive");
            tokio::task::yield_now().await;
            completed_in_task.store(true, Ordering::Release);
            true
        });
        let abort_handle = join_handle.abort_handle();
        let handle = AuditFlushHandle {
            join_handle: Some(join_handle),
            abort_handle,
            cancel_tx,
            has_signaled: AtomicBool::new(false),
        };

        drop(handle);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !completed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("plain drop should allow the flush task to finish");
    }

    #[tokio::test]
    async fn cancelling_shutdown_future_aborts_detached_flush_task() {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let (signal_seen_tx, signal_seen_rx) = tokio::sync::oneshot::channel();
        let join_handle = tokio::spawn(async move {
            cancel_rx.changed().await.expect("shutdown sender is alive");
            let _ = signal_seen_tx.send(());
            std::future::pending::<bool>().await
        });
        let abort_handle = join_handle.abort_handle();
        let observed_abort_handle = abort_handle.clone();
        let handle = AuditFlushHandle {
            join_handle: Some(join_handle),
            abort_handle,
            cancel_tx,
            has_signaled: AtomicBool::new(false),
        };

        let shutdown_task = tokio::spawn(handle.shutdown());
        signal_seen_rx
            .await
            .expect("flush task should observe the shutdown signal");
        shutdown_task.abort();
        let _ = shutdown_task.await;

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !observed_abort_handle.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled shutdown should abort the detached flush task");
    }

    #[test]
    fn optional_reason_preserves_missing_and_trims_present_reason() {
        assert_eq!(optional_reason(None), None);
        assert_eq!(optional_reason(Some("   ")), None);
        assert_eq!(optional_reason(Some("  abuse  ")).as_deref(), Some("abuse"));
    }

    #[test]
    fn user_logout_event_params_preserves_identity_and_metadata() {
        let logout = user_logout_event_params(
            "user_123".to_string(),
            "alice".to_string(),
            Some("192.168.1.1".to_string()),
            Some("Mozilla/5.0".to_string()),
        );
        assert_eq!(logout.actor_id, "user_123");
        assert_eq!(logout.actor_username, "alice");
        assert_eq!(logout.action, AuditAction::UserLogout);
        assert_eq!(logout.target_type, AuditTargetType::User);
        assert_eq!(logout.target_id.as_deref(), Some("user_123"));
        assert!(logout.details.reason.is_none());
        assert!(logout.details.room_id.is_none());
        assert_eq!(logout.ip_address.as_deref(), Some("192.168.1.1"));
        assert_eq!(logout.user_agent.as_deref(), Some("Mozilla/5.0"));
    }
}
