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
//! 2. **Database unavailable**: Batch flush retries with exponential backoff
//!    (100ms, 200ms, 400ms); after 3 failures, logs ERROR and drops batch
//! 3. **Channel closed**: Logs ERROR and continues operation
//!
//! ## Monitoring
//!
//! Operators should monitor for ERROR logs indicating audit failures:
//! - "Audit sync fallback write also failed, event dropped"
//! - "Failed to flush audit batch after all retries, events dropped"
//!
//! The `dropped_count()` method returns the total number of dropped events.
//!
//! ## Recovery Strategies
//!
//! - **Temporary database outage**: Events are retried with backoff; most
//!   outages are recovered automatically
//! - **Extended outage**: Events may be dropped; check application logs
//!   for ERROR messages and investigate database connectivity
//! - **Buffer overflow**: Indicates high audit volume; consider increasing
//!   buffer capacity via configuration

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::{
    models::{AuditAction, AuditTargetType},
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
    pub details: serde_json::Value,
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
    pub details: serde_json::Value,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PermissionAuditRequest {
    pub actor_id: String,
    pub actor_username: String,
    pub target_type: AuditTargetType,
    pub target_id: String,
    pub old_permissions: u64,
    pub new_permissions: u64,
    pub is_grant: bool,
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

#[derive(Debug, Clone)]
pub struct UserLoginAuditRequest {
    pub user_id: String,
    pub username: String,
    pub login_method: String,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TokenIssuedAuditRequest {
    pub user_id: String,
    pub username: String,
    pub token_type: String,
    pub jti: String,
    pub expires_at: i64,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TokenRefreshedAuditRequest {
    pub user_id: String,
    pub username: String,
    pub old_jti: String,
    pub new_jti: String,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TokenFamilyRevokedAuditRequest {
    pub user_id: String,
    pub username: String,
    pub replayed_jti: String,
    pub ip_address: Option<String>,
}

/// Internal record sent through the mpsc channel
#[derive(Debug, Clone)]
struct AuditRecord {
    actor_id: String,
    actor_username: String,
    action: AuditAction,
    target_type: AuditTargetType,
    target_id: Option<String>,
    details: serde_json::Value,
    ip_address: Option<String>,
    user_agent: Option<String>,
    created_at: DateTime<Utc>,
}

/// Audit logging service
///
/// Records audit logs for security-relevant actions. Events are buffered in
/// memory and flushed to the database in batches for performance.
pub struct AuditService {
    pool: PgPool,
    /// Sender half of the buffered channel (None when running without background task)
    sender: Option<mpsc::Sender<AuditRecord>>,
    /// Counter of dropped events (channel full)
    dropped_count: Arc<AtomicUsize>,
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

        let handle = AuditFlushHandle::spawn(pool.clone(), rx, Arc::clone(&dropped_count));

        let service = Self {
            pool,
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
            pool,
            sender: None,
            dropped_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Return the number of events that were dropped because the buffer was full.
    #[must_use]
    pub fn dropped_count(&self) -> usize {
        self.dropped_count.load(Ordering::Relaxed)
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
        let created_at = Utc::now();

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

            if let Err(_e) = sender.try_send(record) {
                // Buffer full: fall back to synchronous DB write instead of dropping
                tracing::warn!(
                    actor_id = %actor_id,
                    action = %action_str,
                    "Audit buffer full, falling back to synchronous DB write"
                );
                if let Err(db_err) = Self::write_single(
                    &self.pool,
                    &AuditRecord {
                        actor_id: actor_id.clone(),
                        actor_username: actor_username.clone(),
                        action,
                        target_type,
                        target_id: target_id.clone(),
                        details: details.clone(),
                        ip_address: ip_address.clone(),
                        user_agent: user_agent.clone(),
                        created_at,
                    },
                )
                .await
                {
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
        Self::write_single(
            &self.pool,
            &AuditRecord {
                actor_id: actor_id.clone(),
                actor_username: actor_username.clone(),
                action,
                target_type,
                target_id: target_id.clone(),
                details: details.clone(),
                ip_address: ip_address.clone(),
                user_agent: user_agent.clone(),
                created_at,
            },
        )
        .await?;

        tracing::debug!(
            actor_id = %actor_id,
            action = %action_str,
            target_type = %target_str,
            "Audit log recorded"
        );

        Ok(())
    }

    async fn write_single(pool: &PgPool, record: &AuditRecord) -> Result<()> {
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
            record.details,
            record.ip_address.as_deref(),
            record.user_agent.as_deref(),
            record.created_at
        )
        .execute(pool)
        .await?;

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
            details: serde_json::json!({"reason": "User created via admin panel"}),
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
            details: serde_json::json!({"reason": "User banned by admin"}),
            ip_address: None,
            user_agent: None,
        })
        .await
    }

    pub async fn log_permission_changed(&self, request: PermissionAuditRequest) -> Result<()> {
        let action = if request.is_grant {
            AuditAction::PermissionGranted
        } else {
            AuditAction::PermissionRevoked
        };

        self.log(AuditEventParams {
            actor_id: request.actor_id,
            actor_username: request.actor_username,
            action,
            target_type: request.target_type,
            target_id: Some(request.target_id),
            details: serde_json::json!({
                "old_permissions": request.old_permissions,
                "new_permissions": request.new_permissions
            }),
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
            details: serde_json::json!({"reason": "Room deleted by admin"}),
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
            details: serde_json::json!({
                "room_id": request.room_id,
                "media_id": request.media_id,
                "reason": request.reason.unwrap_or_default()
            }),
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
            details: serde_json::json!({
                "error": error_message,
                "context": "password_verification_succeeded"
            }),
            ip_address,
            user_agent: None,
        })
        .await
    }

    pub async fn log_user_login(&self, request: UserLoginAuditRequest) -> Result<()> {
        self.log(AuditEventParams {
            actor_id: request.user_id.clone(),
            actor_username: request.username.clone(),
            action: AuditAction::UserLogin,
            target_type: AuditTargetType::User,
            target_id: Some(request.user_id),
            details: serde_json::json!({
                "login_method": request.login_method,
                "username": request.username
            }),
            ip_address: request.ip_address,
            user_agent: request.user_agent,
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
        self.log(AuditEventParams {
            actor_id: user_id.clone(),
            actor_username: username,
            action: AuditAction::UserLogout,
            target_type: AuditTargetType::User,
            target_id: Some(user_id),
            details: serde_json::json!({}),
            ip_address,
            user_agent,
        })
        .await
    }

    pub async fn log_token_issued(&self, request: TokenIssuedAuditRequest) -> Result<()> {
        let target_id = format!("{}:{}", request.user_id, request.jti);
        self.log(AuditEventParams {
            actor_id: request.user_id,
            actor_username: request.username,
            action: AuditAction::TokenIssued,
            target_type: AuditTargetType::Token,
            target_id: Some(target_id),
            details: serde_json::json!({
                "token_type": request.token_type,
                "jti": request.jti,
                "expires_at": request.expires_at
            }),
            ip_address: request.ip_address,
            user_agent: request.user_agent,
        })
        .await
    }

    pub async fn log_token_refreshed(&self, request: TokenRefreshedAuditRequest) -> Result<()> {
        let target_id = format!("{}:{}", request.user_id, request.new_jti);
        self.log(AuditEventParams {
            actor_id: request.user_id,
            actor_username: request.username,
            action: AuditAction::TokenRefreshed,
            target_type: AuditTargetType::Token,
            target_id: Some(target_id),
            details: serde_json::json!({
                "old_jti": request.old_jti,
                "new_jti": request.new_jti
            }),
            ip_address: request.ip_address,
            user_agent: request.user_agent,
        })
        .await
    }

    pub async fn log_token_family_revoked(
        &self,
        request: TokenFamilyRevokedAuditRequest,
    ) -> Result<()> {
        self.log(AuditEventParams {
            actor_id: request.user_id.clone(),
            actor_username: request.username,
            action: AuditAction::TokenFamilyRevoked,
            target_type: AuditTargetType::Token,
            target_id: Some(request.user_id),
            details: serde_json::json!({
                "replayed_jti": request.replayed_jti,
                "reason": "token_replay_detected"
            }),
            ip_address: request.ip_address,
            user_agent: None,
        })
        .await
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
    join_handle: Option<tokio::task::JoinHandle<()>>,
    /// Sender kept alive to control channel lifetime. Dropping it causes the
    /// background receiver loop to terminate after draining remaining items.
    cancel_tx: tokio::sync::watch::Sender<bool>,
    /// Guards against sending the shutdown signal more than once (e.g. when
    /// both `shutdown()` and `Drop` are executed for the same handle).
    has_signaled: AtomicBool,
}

impl AuditFlushHandle {
    fn spawn(
        pool: PgPool,
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
                                           if buffer.len() >= FLUSH_BATCH_SIZE {
                                               flush_batch(&pool, &mut buffer, &dropped_count).await;
                                           }
                                       } else {
                // Channel closed, flush remaining and exit
                                           if !buffer.is_empty() {
                                               flush_batch(&pool, &mut buffer, &dropped_count).await;
                                           }
                                           tracing::info!("Audit flush task: channel closed, exiting");
                                           return;
                                       }
                                   }
                // Periodic flush
                                   _ = interval.tick() => {
                                       if !buffer.is_empty() {
                                           flush_batch(&pool, &mut buffer, &dropped_count).await;
                                       }
                                   }
                // Graceful shutdown signal
                                   _ = cancel_rx.changed() => {
                // Drain remaining items from the channel
                                       rx.close();
                                       while let Some(record) = rx.recv().await {
                                           buffer.push(record);
                                       }
                                       if !buffer.is_empty() {
                                           flush_batch(&pool, &mut buffer, &dropped_count).await;
                                       }
                                       tracing::info!("Audit flush task: graceful shutdown complete");
                                       return;
                                   }
                               }
            }
        });

        Self {
            join_handle: Some(join_handle),
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
    pub async fn shutdown(mut self) {
        self.send_shutdown_signal();
        if let Some(handle) = self.join_handle.take() {
            match handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "Audit flush task ended with join error during shutdown"
                    );
                }
            }
        }
    }
}

impl Drop for AuditFlushHandle {
    fn drop(&mut self) {
        // Signal shutdown (non-blocking, idempotent). The background task
        // will drain on its next iteration. If shutdown() was already called
        // the signal is not sent again.
        self.send_shutdown_signal();
    }
}

/// Maximum retry attempts for `flush_batch`
const FLUSH_MAX_RETRIES: u32 = 3;
/// Base delay in milliseconds for exponential backoff on flush retries
const FLUSH_RETRY_BASE_MS: u64 = 100;

fn parse_actor_id_for_storage(actor_id: &str) -> Option<i64> {
    actor_id.parse::<i64>().ok().filter(|id| *id > 0)
}

/// Flush a batch of audit records to the database with retry on failure.
///
/// Uses exponential backoff (100ms, 200ms, 400ms) before giving up and
/// counting the batch as dropped.
async fn flush_batch(pool: &PgPool, buffer: &mut Vec<AuditRecord>, dropped_count: &AtomicUsize) {
    let batch_size = buffer.len();
    tracing::debug!(batch_size = batch_size, "Flushing audit event batch");

    // Build a batch insert using UNNEST for efficiency
    let mut actor_ids: Vec<Option<i64>> = Vec::with_capacity(batch_size);
    let mut actor_usernames = Vec::with_capacity(batch_size);
    let mut actions = Vec::with_capacity(batch_size);
    let mut target_types = Vec::with_capacity(batch_size);
    let mut target_ids: Vec<Option<String>> = Vec::with_capacity(batch_size);
    let mut details_list: Vec<serde_json::Value> = Vec::with_capacity(batch_size);
    let mut ip_addresses: Vec<Option<String>> = Vec::with_capacity(batch_size);
    let mut user_agents: Vec<Option<String>> = Vec::with_capacity(batch_size);
    let mut created_ats: Vec<DateTime<Utc>> = Vec::with_capacity(batch_size);

    for record in buffer.iter() {
        actor_ids.push(parse_actor_id_for_storage(&record.actor_id));
        actor_usernames.push(record.actor_username.clone());
        actions.push(record.action.as_i16());
        target_types.push(record.target_type.as_i16());
        target_ids.push(record.target_id.clone());
        details_list.push(record.details.clone());
        ip_addresses.push(record.ip_address.clone());
        user_agents.push(record.user_agent.clone());
        created_ats.push(record.created_at);
    }

    for attempt in 0..FLUSH_MAX_RETRIES {
        let query = r"
            INSERT INTO audit_logs (
                actor_id, actor_username, action, target_type, target_id,
                details, ip_address, user_agent, created_at
            )
            SELECT actor_id::bigint,
                   actor_username::text,
                   action::smallint,
                   target_type::smallint,
                   target_id::text,
                   details::jsonb,
                   ip_address::text,
                   user_agent::text,
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
            ";
        match sqlx::query(query)
            .bind(&actor_ids)
            .bind(&actor_usernames)
            .bind(&actions)
            .bind(&target_types)
            .bind(&target_ids)
            .bind(&details_list)
            .bind(&ip_addresses)
            .bind(&user_agents)
            .bind(&created_ats)
            .execute(pool)
            .await
        {
            Ok(_) => {
                tracing::debug!(batch_size = batch_size, "Audit batch flushed successfully");
                buffer.clear();
                return;
            }
            Err(e) => {
                if attempt + 1 < FLUSH_MAX_RETRIES {
                    let backoff_ms = FLUSH_RETRY_BASE_MS * (1 << attempt);
                    tracing::warn!(
                        batch_size = batch_size,
                        attempt = attempt + 1,
                        max_retries = FLUSH_MAX_RETRIES,
                        backoff_ms = backoff_ms,
                        error = %e,
                        "Audit batch flush failed, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                } else {
                    dropped_count.fetch_add(batch_size, Ordering::Relaxed);
                    tracing::error!(
                        batch_size = batch_size,
                        attempts = FLUSH_MAX_RETRIES,
                        error = %e,
                        "Failed to flush audit batch after all retries, events dropped"
                    );
                }
            }
        }
    }

    buffer.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_unbuffered_service_dropped_count_is_zero() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let service = AuditService::new_unbuffered(pool);
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_buffered_service_enqueues_events() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log(AuditEventParams {
                actor_id: "actor1".to_string(),
                actor_username: "admin".to_string(),
                action: AuditAction::UserCreated,
                target_type: AuditTargetType::User,
                target_id: Some("user1".to_string()),
                details: serde_json::json!({}),
                ip_address: None,
                user_agent: None,
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_audit_record_fields() {
        let record = AuditRecord {
            actor_id: "a1".to_string(),
            actor_username: "admin".to_string(),
            action: AuditAction::RoomDeleted,
            target_type: AuditTargetType::Room,
            target_id: Some("r1".to_string()),
            details: serde_json::json!({"reason": "test"}),
            ip_address: Some("127.0.0.1".to_string()),
            user_agent: None,
            created_at: Utc::now(),
        };

        assert_eq!(record.action.as_str(), "room_deleted");
        assert_eq!(record.target_type.as_str(), "room");
        assert_eq!(record.action.as_i16(), 10);
        assert_eq!(record.target_type.as_i16(), 2);
        assert_eq!(record.actor_id, "a1");
    }

    #[tokio::test]
    async fn test_log_user_login_buffered() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_user_login(UserLoginAuditRequest {
                user_id: "user_123".to_string(),
                username: "alice".to_string(),
                login_method: "password".to_string(),
                ip_address: Some("192.168.1.1".to_string()),
                user_agent: Some("Mozilla/5.0".to_string()),
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_log_user_logout_buffered() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_user_logout(
                "user_123".to_string(),
                "alice".to_string(),
                Some("192.168.1.1".to_string()),
                Some("Mozilla/5.0".to_string()),
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_log_token_issued_buffered() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_token_issued(TokenIssuedAuditRequest {
                user_id: "user_123".to_string(),
                username: "alice".to_string(),
                token_type: "access".to_string(),
                jti: "jti_abc123".to_string(),
                expires_at: 1_735_689_600,
                ip_address: Some("192.168.1.1".to_string()),
                user_agent: Some("Mozilla/5.0".to_string()),
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_log_token_refreshed_buffered() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_token_refreshed(TokenRefreshedAuditRequest {
                user_id: "user_123".to_string(),
                username: "alice".to_string(),
                old_jti: "jti_old_123".to_string(),
                new_jti: "jti_new_456".to_string(),
                ip_address: Some("192.168.1.1".to_string()),
                user_agent: Some("Mozilla/5.0".to_string()),
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_log_token_family_revoked_buffered() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_token_family_revoked(TokenFamilyRevokedAuditRequest {
                user_id: "user_123".to_string(),
                username: "alice".to_string(),
                replayed_jti: "jti_replayed_789".to_string(),
                ip_address: Some("192.168.1.1".to_string()),
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_log_user_login_without_ip_or_user_agent() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_user_login(UserLoginAuditRequest {
                user_id: "user_123".to_string(),
                username: "alice".to_string(),
                login_method: "oauth2".to_string(),
                ip_address: None,
                user_agent: None,
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[test]
    fn test_audit_action_and_target_display_parse_roundtrip() {
        assert_eq!(AuditAction::TokenIssued.to_string(), "token_issued");
        assert_eq!(
            "ROOM_OWNERSHIP_TRANSFERRED".parse::<AuditAction>().unwrap(),
            AuditAction::RoomOwnershipTransferred
        );
        assert!("unknown_action".parse::<AuditAction>().is_err());

        assert_eq!(
            AuditTargetType::ProviderInstance.to_string(),
            "provider_instance"
        );
        assert_eq!(
            "STREAM".parse::<AuditTargetType>().unwrap(),
            AuditTargetType::Stream
        );
        assert_eq!(
            "CHAT_MESSAGE".parse::<AuditTargetType>().unwrap(),
            AuditTargetType::ChatMessage
        );
        assert!("unknown_target".parse::<AuditTargetType>().is_err());
    }
}
